//! Rip 值图的循环回收与堆内存统计。
//!
//! Rip 的容器是 `Rc<RefCell<...>>`：没有环时引用计数立即释放内存，但
//! 「容器 → 闭包 → 捕获的容器」这类环会一直存活。这里在线程内登记所有容器与
//! 闭包节点，用「外部引用数 = strong_count − 内部引用数」的试验删除法
//! （Bacon–Rajan）找出只被环引用的节点，清空容器内容让引用计数归零。
//!
//! 判定引用的关键是「外部引用」：解释器栈帧、全局变量、Rust 侧临时变量持有的
//! `Value` 都会增加 `strong_count`，但不会被计入内部引用，因此它们天然是根。
//! 只有完全由已登记节点互相引用、且从任何外部引用都到不了的节点才会被回收。
//!
//! 值全部是 `Rc`（非 `Send`），登记表用线程局部存储，不会跨线程混淆。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::value::{Ary, Env, FuncKind, Function, MapData, Value};

/// 两次回收之间至少要新增多少个节点。堆越大，阈值越高，避免全量扫描过于频繁。
const MIN_COLLECT_INTERVAL: usize = 10_000;

thread_local! {
    static REGISTRY: RefCell<Vec<WeakNode>> = const { RefCell::new(Vec::new()) };
    static SINCE_COLLECT: Cell<usize> = const { Cell::new(0) };
    static LIVE_AT_LAST: Cell<usize> = const { Cell::new(0) };
}

/// 回收统计。
#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    /// 扫描到的存活节点数（含本轮被回收的）。
    pub scanned: usize,
    /// 本轮回收的节点数。
    pub collected: usize,
}

/// 登记表里的弱引用；指向的节点被释放后自动失效。
enum WeakNode {
    Ary(Weak<RefCell<Ary>>),
    Vec(Weak<RefCell<Vec<Value>>>),
    Map(Weak<RefCell<MapData>>),
    Func(Weak<Function>),
}

impl WeakNode {
    /// 对应节点的强引用数；0 表示节点已销毁（弱引用条目可以清理）。
    fn strong_count(&self) -> usize {
        match self {
            WeakNode::Ary(weak) => weak.strong_count(),
            WeakNode::Vec(weak) => weak.strong_count(),
            WeakNode::Map(weak) => weak.strong_count(),
            WeakNode::Func(weak) => weak.strong_count(),
        }
    }

    fn upgrade(&self) -> Option<Node> {
        match self {
            WeakNode::Ary(weak) => weak.upgrade().map(Node::Ary),
            WeakNode::Vec(weak) => weak.upgrade().map(Node::Vec),
            WeakNode::Map(weak) => weak.upgrade().map(Node::Map),
            WeakNode::Func(weak) => weak.upgrade().map(Node::Func),
        }
    }
}

/// 回收器持有的强引用；函数返回时统一释放，回收后的节点随之销毁。
enum Node {
    Ary(Rc<RefCell<Ary>>),
    Vec(Rc<RefCell<Vec<Value>>>),
    Map(Rc<RefCell<MapData>>),
    Func(Rc<Function>),
}

impl Node {
    /// 节点地址：用于把子引用映射回登记表下标。
    fn key(&self) -> usize {
        match self {
            Node::Ary(value) => Rc::as_ptr(value) as *const () as usize,
            Node::Vec(value) => Rc::as_ptr(value) as *const () as usize,
            Node::Map(value) => Rc::as_ptr(value) as *const () as usize,
            Node::Func(value) => Rc::as_ptr(value) as *const () as usize,
        }
    }

    /// 当前强引用数（包含回收器自己持有的这一份）。
    fn strong(&self) -> usize {
        match self {
            Node::Ary(value) => Rc::strong_count(value),
            Node::Vec(value) => Rc::strong_count(value),
            Node::Map(value) => Rc::strong_count(value),
            Node::Func(value) => Rc::strong_count(value),
        }
    }

    /// 子节点地址列表；返回 `false` 表示引用无法枚举（当作根处理）。
    fn children(&self, out: &mut Vec<usize>) -> bool {
        match self {
            Node::Ary(value) => match value.try_borrow() {
                Ok(ary) => {
                    for (_, field) in &ary.fields {
                        push_value(field, out);
                    }
                    true
                }
                Err(_) => false,
            },
            Node::Vec(value) => match value.try_borrow() {
                Ok(items) => {
                    for item in items.iter() {
                        push_value(item, out);
                    }
                    true
                }
                Err(_) => false,
            },
            Node::Map(value) => match value.try_borrow() {
                Ok(map) => {
                    for (key, item) in &map.entries {
                        push_value(key, out);
                        push_value(item, out);
                    }
                    true
                }
                Err(_) => false,
            },
            Node::Func(func) => match &func.kind {
                FuncKind::Closure { captured, .. } => {
                    push_env(captured, out);
                    true
                }
                FuncKind::Router(table) => {
                    for route in table.routes() {
                        push_value(&route.handler, out);
                    }
                    true
                }
                FuncKind::Middleware(_) => true,
                FuncKind::MiddlewareApplied { next, .. } => {
                    push_value(next, out);
                    true
                }
                FuncKind::Chain {
                    middlewares,
                    handler,
                    composed,
                } => {
                    for middleware in middlewares {
                        push_value(middleware, out);
                    }
                    push_value(handler, out);
                    if let Some(value) = composed.get() {
                        push_value(value, out);
                    }
                    true
                }
                FuncKind::Mounted { inner, .. } => {
                    push_value(inner, out);
                    true
                }
                FuncKind::Decl(_) | FuncKind::Static { .. } | FuncKind::SseSend(_) => true,
                // 原生闭包捕获的 Rust 值无法枚举：保守地当作根。
                FuncKind::Native { .. } => false,
            },
        }
    }

    /// 清空容器内容，打断环；闭包本身不可变，但清空它引用的容器同样能断开环。
    fn clear(&self) -> bool {
        match self {
            Node::Ary(value) => match value.try_borrow_mut() {
                Ok(mut ary) => {
                    ary.fields.clear();
                    true
                }
                Err(_) => false,
            },
            Node::Vec(value) => match value.try_borrow_mut() {
                Ok(mut items) => {
                    items.clear();
                    true
                }
                Err(_) => false,
            },
            Node::Map(value) => match value.try_borrow_mut() {
                Ok(mut map) => {
                    map.entries.clear();
                    true
                }
                Err(_) => false,
            },
            Node::Func(_) => true,
        }
    }
}

fn push_value(value: &Value, out: &mut Vec<usize>) {
    match value {
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => push_value(inner, out),
        Value::Ary(value) => out.push(Rc::as_ptr(value) as *const () as usize),
        Value::Vec(value) => out.push(Rc::as_ptr(value) as *const () as usize),
        Value::Map(value) => out.push(Rc::as_ptr(value) as *const () as usize),
        Value::Func(func) => out.push(Rc::as_ptr(func) as *const () as usize),
        _ => {}
    }
}

fn push_env(env: &Env, out: &mut Vec<usize>) {
    for scope in &env.scopes {
        for value in scope.values() {
            push_value(value, out);
        }
    }
}

pub(crate) fn track_ary(value: &Rc<RefCell<Ary>>) {
    register(WeakNode::Ary(Rc::downgrade(value)));
}

pub(crate) fn track_vec(value: &Rc<RefCell<Vec<Value>>>) {
    register(WeakNode::Vec(Rc::downgrade(value)));
}

pub(crate) fn track_map(value: &Rc<RefCell<MapData>>) {
    register(WeakNode::Map(Rc::downgrade(value)));
}

pub(crate) fn track_function(value: &Rc<Function>) {
    register(WeakNode::Func(Rc::downgrade(value)));
}

fn register(node: WeakNode) {
    REGISTRY.with(|registry| {
        registry.borrow_mut().push(node);
    });
    SINCE_COLLECT.with(|count| count.set(count.get() + 1));
}

/// 当前线程已到回收间隔时返回 true；检查本身只有两次线程局部读取。
pub fn should_collect() -> bool {
    let live = LIVE_AT_LAST.with(|value| value.get());
    let interval = live.max(MIN_COLLECT_INTERVAL);
    SINCE_COLLECT.with(|count| count.get() >= interval)
}

/// 当前登记表里的节点数（含未被回收的环）。
pub fn tracked_nodes() -> usize {
    REGISTRY.with(|registry| registry.borrow().len())
}

/// 执行一次全量回收，返回本轮统计。
pub fn collect() -> GcStats {
    SINCE_COLLECT.with(|count| count.set(0));
    let nodes: Vec<Node> = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let mut nodes = Vec::with_capacity(registry.len());
        registry.retain(|node| match node.upgrade() {
            Some(node) => {
                nodes.push(node);
                true
            }
            None => false,
        });
        // 回收后通常只剩少量存活节点；把登记表容量收回来，避免长期占用。
        if registry.capacity() > registry.len().saturating_mul(4) {
            registry.shrink_to_fit();
        }
        nodes
    });
    let count = nodes.len();
    LIVE_AT_LAST.with(|value| value.set(count));
    if count == 0 {
        return GcStats::default();
    }

    let mut index: HashMap<usize, usize> = HashMap::with_capacity(count);
    for (position, node) in nodes.iter().enumerate() {
        index.insert(node.key(), position);
    }
    // 先记录强引用数，再遍历子引用；遍历过程中复制 Value 不会影响已记录的数字。
    let strong: Vec<usize> = nodes.iter().map(Node::strong).collect();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut internal = vec![0usize; count];
    let mut roots = vec![false; count];
    let mut children = Vec::new();
    for (position, node) in nodes.iter().enumerate() {
        children.clear();
        if !node.children(&mut children) {
            roots[position] = true;
            continue;
        }
        for child in &children {
            if let Some(&target) = index.get(child) {
                internal[target] += 1;
                edges[position].push(target);
            }
        }
    }
    for position in 0..count {
        // 减去回收器自己持有的那一份，再减去来自其他节点的引用。
        let external = strong[position].saturating_sub(1 + internal[position]);
        if external > 0 {
            roots[position] = true;
        }
    }

    let mut reachable = vec![false; count];
    let mut stack: Vec<usize> = (0..count).filter(|&position| roots[position]).collect();
    while let Some(position) = stack.pop() {
        if std::mem::replace(&mut reachable[position], true) {
            continue;
        }
        for &target in &edges[position] {
            if !reachable[target] {
                stack.push(target);
            }
        }
    }

    let mut collected = 0;
    for position in 0..count {
        if !reachable[position] && nodes[position].clear() {
            collected += 1;
        }
    }
    // 回收后的节点要等回收器释放自己持有的引用才会真正销毁；
    // 提前 drop 掉再清理登记表，弱引用才不会继续占着分配块。
    drop(nodes);
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.retain(|node| node.strong_count() > 0);
        if registry.capacity() > registry.len().saturating_mul(4) {
            registry.shrink_to_fit();
        }
    });
    GcStats {
        scanned: count,
        collected,
    }
}

/// 到间隔时自动回收；在请求 / 测试的边界调用，避免每条语句都做扫描。
pub fn collect_if_due() -> Option<GcStats> {
    if should_collect() {
        Some(collect())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{FuncKind, Function};
    use rp_ast::{TypeExpr, TypeExprKind};

    fn ary(fields: Vec<(&str, Value)>) -> Rc<RefCell<Ary>> {
        let value = Rc::new(RefCell::new(Ary {
            class: "test".to_string(),
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect(),
        }));
        track_ary(&value);
        value
    }

    #[test]
    fn collects_self_referencing_container() {
        let node = ary(vec![]);
        let shared = Value::Ary(node.clone());
        // 环：ary 的一个字段指向包含它自己的 vec。
        node.borrow_mut()
            .fields
            .push(("self".to_string(), Value::vec(vec![shared.clone()])));
        let weak = Rc::downgrade(&node);
        drop(shared);
        drop(node);
        // 没有外部引用后，只剩环；collect 应该把它清掉。
        let stats = collect();
        assert!(stats.collected >= 1, "{:?}", stats);
        assert!(weak.upgrade().is_none(), "cycle must be reclaimed");
    }

    #[test]
    fn keeps_externally_referenced_values() {
        let node = ary(vec![("x", Value::Int(1))]);
        let weak = Rc::downgrade(&node);
        let keep = Value::Ary(node.clone());
        node.borrow_mut()
            .fields
            .push(("self".to_string(), Value::vec(vec![keep.clone()])));
        collect();
        assert!(weak.upgrade().is_some(), "live value must survive");
        drop(keep);
        drop(node);
        collect();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn collects_cycle_through_closure() {
        let node = ary(vec![("x", Value::Int(1))]);
        let mut env = Env::new();
        env.define("node", Value::Ary(node.clone()));
        let closure = Value::func_rc(Rc::new(Function {
            name: "<closure>".to_string(),
            kind: FuncKind::Closure {
                params: Vec::new(),
                ret: TypeExpr {
                    kind: TypeExprKind::Void,
                    line: 1,
                },
                body: Rc::new(Vec::new()),
                captured: env,
            },
        }));
        // 环：ary 持有闭包，闭包捕获了同一个 ary。
        node.borrow_mut()
            .fields
            .push(("cb".to_string(), closure.clone()));
        let weak = Rc::downgrade(&node);
        drop(closure);
        drop(node);
        let stats = collect();
        assert!(stats.collected >= 1, "{:?}", stats);
        assert!(weak.upgrade().is_none(), "closure cycle must be reclaimed");
    }
}

//! Rip 运行时值模型。

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use rp_ast::{FuncDecl, IntSuffix, Param, Stmt, TypeExpr};

#[derive(Clone)]
pub enum Value {
    Null,
    None,
    Bool(bool),
    Int(i64),
    /// 无符号整数（`u8` / `u16` / `u32` / `u64`）。位宽由类型检查器保证，这里只区分有无符号。
    UInt(u64),
    Float(f64),
    Str(Arc<String>),
    Char(String),
    Some(Box<Value>),
    Ok(Box<Value>),
    Err(Box<Value>),
    Vec(Rc<RefCell<Vec<Value>>>),
    Ary(Rc<RefCell<Ary>>),
    Map(Rc<RefCell<MapData>>),
    Func(Rc<Function>),
    /// `async` 调用的临时结果，只能被 `await` 消费。
    Task(Arc<crate::async_rt::TaskSlot>),
}

impl Value {
    pub fn string(s: impl Into<String>) -> Value {
        Value::Str(Arc::new(s.into()))
    }

    pub fn vec(items: Vec<Value>) -> Value {
        let value = Rc::new(RefCell::new(items));
        crate::gc::track_vec(&value);
        Value::Vec(value)
    }

    pub fn map(entries: Vec<(Value, Value)>) -> Value {
        Value::map_data(MapData { entries })
    }

    /// 由 `MapData` 构造 map 值并登记到环回收器。
    pub fn map_data(map: MapData) -> Value {
        let value = Rc::new(RefCell::new(map));
        crate::gc::track_map(&value);
        Value::Map(value)
    }

    /// 由 `Ary` 构造结构体值并登记到环回收器。
    pub fn ary_owned(ary: Ary) -> Value {
        let value = Rc::new(RefCell::new(ary));
        crate::gc::track_ary(&value);
        Value::Ary(value)
    }

    pub fn empty_map() -> Value {
        Value::map(Vec::new())
    }

    pub fn ary(class: impl Into<String>, fields: Vec<(String, Value)>) -> Value {
        Value::ary_owned(Ary {
            class: class.into(),
            fields,
        })
    }

    /// 包装函数值并登记到循环回收器。
    pub fn func_rc(func: Rc<Function>) -> Value {
        crate::gc::track_function(&func);
        Value::Func(func)
    }

    pub fn as_str(&self) -> Option<String> {
        match self {
            Value::Str(s) => Some((**s).clone()),
            Value::Char(c) => Some(c.clone()),
            _ => None,
        }
    }

    /// 借用版字符串读取：热路径避免克隆 `String`。
    pub fn as_str_ref(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_ref()),
            Value::Char(c) => Some(c.as_str()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::UInt(v) if *v <= i64::MAX as u64 => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

#[derive(Clone, Default)]
pub struct Ary {
    pub class: String,
    pub fields: Vec<(String, Value)>,
}

impl Ary {
    pub fn get(&self, name: &str) -> Option<Value> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    /// 借用版查找：HTTP 路由匹配等热路径避免克隆整个 `Value`。
    pub fn get_ref(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    pub fn set(&mut self, name: &str, value: Value) {
        if let Some((_, slot)) = self.fields.iter_mut().find(|(n, _)| n == name) {
            *slot = value;
        } else {
            self.fields.push((name.to_string(), value));
        }
    }
}

#[derive(Clone, Default)]
pub struct MapData {
    pub entries: Vec<(Value, Value)>,
}

impl MapData {
    pub fn get(&self, key: &Value) -> Option<Value> {
        self.entries
            .iter()
            .find(|(k, _)| value_eq(k, key))
            .map(|(_, v)| v.clone())
    }

    pub fn set(&mut self, key: Value, value: Value) {
        if let Some((_, slot)) = self.entries.iter_mut().find(|(k, _)| value_eq(k, &key)) {
            *slot = value;
        } else {
            self.entries.push((key, value));
        }
    }

    pub fn has(&self, key: &Value) -> bool {
        self.entries.iter().any(|(k, _)| value_eq(k, key))
    }

    pub fn delete(&mut self, key: &Value) -> Option<Value> {
        if let Some(index) = self.entries.iter().position(|(k, _)| value_eq(k, key)) {
            Some(self.entries.remove(index).1)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct Function {
    pub name: String,
    pub kind: FuncKind,
}

/// SSE 帧接收端：直接持有连接的写半部，逐帧同步写出。
///
/// thread-per-core 后生产函数与连接在同一线程执行，不再需要跨线程通道；
/// 写失败（客户端断开或写超时）以 `io::Error` 返回，由生产函数结束本次推送。
pub struct SseSink {
    writer: std::cell::RefCell<Box<dyn std::io::Write>>,
}

impl SseSink {
    pub(crate) fn new(writer: impl std::io::Write + 'static) -> Self {
        SseSink {
            writer: std::cell::RefCell::new(Box::new(writer)),
        }
    }

    /// 写出一帧并立即 flush：SSE 要求客户端尽快看到事件，不能等缓冲区满。
    pub(crate) fn send(&self, frame: &str) -> std::io::Result<()> {
        let mut writer = self.writer.borrow_mut();
        writer.write_all(frame.as_bytes())?;
        writer.flush()
    }
}

#[derive(Clone)]
pub enum FuncKind {
    Decl(Arc<FuncDecl>),
    Closure {
        params: Vec<Param>,
        ret: TypeExpr,
        body: Rc<Vec<Stmt>>,
        captured: Env,
    },
    Router(Rc<RouteTable>),
    /// 中间件工厂：`func(http.handler) -> http.handler`。
    Middleware(MiddlewareSpec),
    /// 已绑定下一级处理器的中间件。
    MiddlewareApplied {
        spec: MiddlewareSpec,
        next: Value,
    },
    /// 中间件链：第一次调用时按顺序包裹成处理函数并缓存。
    Chain {
        middlewares: Vec<Value>,
        handler: Value,
        composed: std::cell::OnceCell<Value>,
    },
    /// 路由分组：匹配到前缀后剥掉前缀，再把请求交给内部处理器。
    Mounted {
        prefix: String,
        inner: Value,
    },
    Static {
        dir: String,
        prefix: String,
    },
    /// SSE 事件发送器：由服务端注入给 `http.response.sse` 的生产函数。
    SseSend(Rc<SseSink>),
    /// AOT 生成的原生函数/闭包：不经过 AST 解释。
    Native {
        arity: usize,
        body: Rc<dyn Fn(&mut crate::Runtime, Vec<Value>, u32) -> FlowResult<Value>>,
    },
}

#[derive(Clone)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub handler: Value,
}

/// 内建中间件配置。可跨线程重建，不依赖闭包捕获。
#[derive(Clone)]
pub enum MiddlewareSpec {
    /// CORS：自动处理 OPTIONS 预检，普通响应补 `Access-Control-*`。
    Cors {
        origin: String,
    },
    /// 给每个响应追加固定响应头。
    Headers {
        headers: Vec<(String, String)>,
    },
    /// 按客户端 IP 限流：与 `[server] rate_limit` 独立，可细到某个路由分组。
    RateLimit {
        limiter: std::sync::Arc<crate::server::RateLimiter>,
    },
}

/// 路由前缀树的一个节点。
///
/// 路径按 `/` 切分后逐段下沉：静态段走哈希索引，`:param` 走唯一的参数分支，
/// `*` 表示“从该节点起前缀匹配”，记录在该节点上并就地收尾（与原来的线性
/// 匹配语义一致：命中 `*` 后不再校验剩余路径）。
#[derive(Default)]
struct RouteNode {
    statics: HashMap<String, RouteNode>,
    param: Option<Box<RouteNode>>,
    /// 正好在该节点结束的路由下标（按注册顺序追加）。
    exact: Vec<u32>,
    /// 以 `*` 在该节点收尾的路由下标（按注册顺序追加）。
    wildcard: Vec<u32>,
}

/// 路由表：注册顺序的原始列表 + 按路径段组织的前缀树。
///
/// 匹配复杂度由 O(路由数) 降到 O(路径段数)（每层最多比较静态段、参数段两条分支）。
/// 树只负责挑出候选，最终仍取“注册序号最小”的那条，保证与线性扫描的
/// 语义完全一致——先注册的路由优先，静态路由与通配路由的相对优先级不受树结构影响。
pub struct RouteTable {
    routes: Vec<Route>,
    /// 每条路由模式里 `:param` 的名字，按出现顺序；与匹配时收集的段值一一对应。
    param_names: Vec<Vec<String>>,
    /// 预计算的指标标签（`"METHOD /path"`），避免每请求 `format!`。
    labels: Vec<String>,
    root: RouteNode,
}

impl RouteTable {
    pub fn new(routes: Vec<Route>) -> RouteTable {
        let mut root = RouteNode::default();
        let mut param_names = Vec::with_capacity(routes.len());
        for (index, route) in routes.iter().enumerate() {
            let index = index as u32;
            let mut node = &mut root;
            let mut names = Vec::new();
            let mut wildcard = false;
            for segment in route.path.split('/').filter(|segment| !segment.is_empty()) {
                if segment == "*" {
                    node.wildcard.push(index);
                    wildcard = true;
                    break;
                }
                match segment.strip_prefix(':') {
                    Some(name) => {
                        names.push(name.to_string());
                        node = node
                            .param
                            .get_or_insert_with(|| Box::new(RouteNode::default()));
                    }
                    None => {
                        node = node.statics.entry(segment.to_string()).or_default();
                    }
                }
            }
            if !wildcard {
                node.exact.push(index);
            }
            param_names.push(names);
        }
        let labels: Vec<String> = routes
            .iter()
            .map(|route| format!("{} {}", route.method, route.path))
            .collect();
        RouteTable {
            routes,
            param_names,
            labels,
            root,
        }
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// 路由的指标标签（`"METHOD /path"`），构建时预计算。
    pub fn label(&self, index: usize) -> &str {
        &self.labels[index]
    }

    pub fn route(&self, index: usize) -> &Route {
        &self.routes[index]
    }

    /// 查找命中的路由，返回其下标与 `:param` 取值（名字取自该路由的模式）。
    pub fn find(&self, method: &str, path: &str) -> Option<(usize, Vec<(String, String)>)> {
        let parts: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
        let mut values = Vec::new();
        let mut best: Option<(usize, Vec<String>)> = None;
        self.search(&self.root, &parts, method, &mut values, &mut best);
        best.map(|(index, values)| {
            let params = self.param_names[index]
                .iter()
                .cloned()
                .zip(values)
                .collect();
            (index, params)
        })
    }

    fn search(
        &self,
        node: &RouteNode,
        parts: &[&str],
        method: &str,
        values: &mut Vec<String>,
        best: &mut Option<(usize, Vec<String>)>,
    ) {
        // 注册序号 0 已是最小，无需继续深搜。
        if let Some((0, _)) = best {
            return;
        }
        for &index in &node.wildcard {
            self.offer(index, method, values, best);
        }
        match parts.split_first() {
            None => {
                for &index in &node.exact {
                    self.offer(index, method, values, best);
                }
            }
            Some((segment, rest)) => {
                if let Some(child) = node.statics.get(*segment) {
                    self.search(child, rest, method, values, best);
                }
                if let Some(child) = &node.param {
                    values.push((*segment).to_string());
                    self.search(child, rest, method, values, best);
                    values.pop();
                }
            }
        }
    }

    /// 候选路由：方法匹配且注册序号比当前最优更小才替换。
    fn offer(
        &self,
        index: u32,
        method: &str,
        values: &[String],
        best: &mut Option<(usize, Vec<String>)>,
    ) {
        if let Some((current, _)) = best {
            if *current <= index as usize {
                return;
            }
        }
        let route = &self.routes[index as usize];
        if route.method == "*" || route.method.eq_ignore_ascii_case(method) {
            *best = Some((index as usize, values.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_table(items: &[(&str, &str)]) -> RouteTable {
        let routes = items
            .iter()
            .map(|(method, path)| Route {
                method: method.to_string(),
                path: path.to_string(),
                handler: Value::Null,
            })
            .collect();
        RouteTable::new(routes)
    }

    fn hit(table: &RouteTable, method: &str, path: &str) -> Option<(String, Vec<(String, String)>)> {
        table
            .find(method, path)
            .map(|(index, params)| (table.route(index).path.clone(), params))
    }

    #[test]
    fn trie_matches_static_param_and_wildcard() {
        let table = make_table(&[
            ("GET", "/users/:id"),
            ("GET", "/users/:id/posts"),
            ("GET", "/assets/*"),
            ("GET", "/"),
        ]);
        assert_eq!(
            hit(&table, "GET", "/users/42"),
            Some(("/users/:id".to_string(), vec![("id".to_string(), "42".to_string())]))
        );
        assert_eq!(
            hit(&table, "GET", "/users/42/posts"),
            Some((
                "/users/:id/posts".to_string(),
                vec![("id".to_string(), "42".to_string())]
            ))
        );
        assert_eq!(
            hit(&table, "GET", "/assets/css/app.css"),
            Some(("/assets/*".to_string(), Vec::new()))
        );
        assert_eq!(hit(&table, "GET", "/assets"), Some(("/assets/*".to_string(), Vec::new())));
        assert_eq!(hit(&table, "GET", "/"), Some(("/".to_string(), Vec::new())));
        assert_eq!(hit(&table, "GET", "/missing/path/deeper"), None);
        assert_eq!(hit(&table, "POST", "/users/42"), None);
    }

    #[test]
    fn trie_preserves_registration_order() {
        // 先注册的参数路由要遮蔽后注册的静态路由，与线性扫描一致。
        let table = make_table(&[("GET", "/:id"), ("GET", "/about")]);
        assert_eq!(
            hit(&table, "GET", "/about"),
            Some(("/:id".to_string(), vec![("id".to_string(), "about".to_string())]))
        );
        // 反过来先注册静态路由，静态路由优先命中。
        let table = make_table(&[("GET", "/about"), ("GET", "/:id")]);
        assert_eq!(hit(&table, "GET", "/about"), Some(("/about".to_string(), Vec::new())));
    }

    #[test]
    fn trie_uses_per_route_param_names() {
        let table = make_table(&[("GET", "/:id/a"), ("GET", "/:name/b")]);
        assert_eq!(
            hit(&table, "GET", "/x/a"),
            Some(("/:id/a".to_string(), vec![("id".to_string(), "x".to_string())]))
        );
        assert_eq!(
            hit(&table, "GET", "/x/b"),
            Some(("/:name/b".to_string(), vec![("name".to_string(), "x".to_string())]))
        );
    }

    #[test]
    fn trie_matches_any_method_and_prefers_earlier_route() {
        let table = make_table(&[("*", "/ping"), ("GET", "/ping")]);
        assert_eq!(hit(&table, "GET", "/ping"), Some(("/ping".to_string(), Vec::new())));
        let table = make_table(&[("GET", "/ping"), ("*", "/ping")]);
        assert_eq!(hit(&table, "PUT", "/ping"), Some(("/ping".to_string(), Vec::new())));
        assert_eq!(hit(&table, "PUT", "/nope"), None);
    }

    #[test]
    fn enum_value_formats_and_serializes() {
        let value = Value::ary(
            "status",
            vec![
                ("#variant".to_string(), Value::string("banned")),
                ("reason".to_string(), Value::string("spam")),
            ],
        );
        assert_eq!(format_value(&value), "status.banned[reason = spam]");
        let unit = Value::ary(
            "status",
            vec![("#variant".to_string(), Value::string("active"))],
        );
        assert_eq!(format_value(&unit), "status.active");

        let json = value_to_json(&value);
        assert_eq!(json["variant"], serde_json::json!("banned"));
        assert_eq!(json["reason"], serde_json::json!("spam"));
        assert!(json.get("#variant").is_none());
    }

    /// 快速序列化路径必须与旧路径逐字节一致（响应体与 json.encode 共用此保证）。
    #[test]
    fn value_write_json_matches_serde_path() {
        let cases = vec![
            Value::Null,
            Value::None,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(-42),
            Value::UInt(255),
            Value::Float(1.5),
            Value::Float(2.0),
            Value::Float(f64::NAN),
            Value::string("he\"llo\n\t\\中"),
            Value::Char("a".to_string()),
            Value::vec(vec![Value::Int(1), Value::string("x"), Value::Null]),
            Value::empty_map(),
            Value::map(vec![
                (Value::string("updated_ms"), Value::Null),
                (Value::string("id"), Value::Int(7)),
                (Value::string("name"), Value::string("u\"1")),
            ]),
            Value::Ok(Box::new(Value::string("inner"))),
            Value::ary(
                "status",
                vec![
                    ("#variant".to_string(), Value::string("banned")),
                    ("reason".to_string(), Value::string("spam")),
                ],
            ),
            Value::ary("point", vec![("x".to_string(), Value::Int(1))]),
        ];
        for value in &cases {
            let expected = serde_json::to_vec(&value_to_json(value)).unwrap();
            let mut actual = Vec::new();
            value_write_json(value, &mut actual);
            assert_eq!(actual, expected, "mismatch for {:?}", format_value(value));
        }
    }
}

#[derive(Clone, Default)]
pub struct Env {
    pub scopes: Vec<HashMap<String, Value>>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    pub fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn define(&mut self, name: &str, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), value);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(value) = scope.get(name) {
                return Some(value);
            }
        }
        None
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(value) = scope.get_mut(name) {
                return Some(value);
            }
        }
        None
    }

    pub fn set(&mut self, name: &str, value: Value) -> bool {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), value);
                return true;
            }
        }
        false
    }

    /// 调试快照：由内到外收集可见变量，值转成展示文本；同名的外层变量被遮蔽。
    pub fn snapshot(&self) -> Vec<(String, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            let mut names: Vec<&String> = scope.keys().collect();
            names.sort();
            for name in names {
                if seen.insert(name.clone()) {
                    out.push((name.clone(), format_value(&scope[name])));
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeError {
    pub message: String,
    pub line: u32,
    /// 调用栈（函数名, 定义行号），按「最外层 → 最内层」顺序；AOT 构建暂不填充。
    pub stack: Vec<(String, u32)>,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>, line: u32) -> Self {
        Self {
            message: message.into(),
            line,
            stack: Vec::new(),
        }
    }

    /// 附带调用栈（由解释器在出错时快照）。
    pub fn with_stack(mut self, stack: Vec<(String, u32)>) -> Self {
        self.stack = stack;
        self
    }

    /// 供 CLI / 日志打印的栈帧行（最内层在前）。
    pub fn stack_lines(&self) -> Vec<String> {
        self.stack
            .iter()
            .rev()
            .map(|(name, line)| format!("  at {} (line {})", name, line))
            .collect()
    }
}

pub enum Flow {
    Error(RuntimeError),
    Return(Value),
    Break,
    Continue,
    Propagate(Value),
}

pub type FlowResult<T = Value> = Result<T, Flow>;

/// 字面量到运行时值：无后缀时按 `int` 存放，由绑定处的声明类型再转换成具体位宽。
#[cfg(feature = "interp")]
pub fn int_literal_value(value: i128, suffix: Option<IntSuffix>) -> Value {
    match suffix {
        Some(suffix) if !suffix.is_signed() => Value::UInt(value as u64),
        _ => Value::Int(value as i64),
    }
}

/// 按声明类型名（`int` / `i8`…`u64`）归一整数并检查范围；非整数类型名原样返回。
///
/// 解释器与 AOT 产物共用：`let x: u8 = 300;` 在两种执行方式下都报同一个错误。
pub fn coerce_int_name(value: Value, ty_name: &str, line: u32) -> Result<Value, String> {
    let suffix = match ty_name {
        "int" | "i64" => IntSuffix::I64,
        other => match IntSuffix::from_name(other) {
            Some(suffix) => suffix,
            None => return Ok(value),
        },
    };
    let (min, max) = suffix.range();
    let wide = match &value {
        Value::Int(v) => *v as i128,
        Value::UInt(v) => *v as i128,
        _ => return Ok(value),
    };
    if wide < min || wide > max {
        let _ = line;
        return Err(format!("integer overflow: {} does not fit {}", wide, suffix.name()));
    }
    Ok(match value {
        Value::Int(v) => {
            if suffix.is_signed() {
                Value::Int(v)
            } else {
                Value::UInt(v as u64)
            }
        }
        Value::UInt(v) => {
            if suffix.is_signed() {
                Value::Int(v as i64)
            } else {
                Value::UInt(v)
            }
        }
        other => other,
    })
}

pub fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::None, Value::None) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::UInt(x), Value::UInt(y)) => x == y,
        // 无后缀字面量可能以 `Int` 位模式出现，与 `UInt` 比较时按无符号解释。
        (Value::Int(x), Value::UInt(y)) | (Value::UInt(y), Value::Int(x)) => *x as u64 == *y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Some(x), Value::Some(y)) => value_eq(x, y),
        (Value::Ok(x), Value::Ok(y)) => value_eq(x, y),
        (Value::Err(x), Value::Err(y)) => value_eq(x, y),
        (Value::Vec(x), Value::Vec(y)) => {
            let x = x.borrow();
            let y = y.borrow();
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| value_eq(a, b))
        }
        (Value::Ary(x), Value::Ary(y)) => {
            let x = x.borrow();
            let y = y.borrow();
            x.fields.len() == y.fields.len()
                && x.fields.iter().all(|(name, value)| {
                    y.get(name)
                        .map(|other| value_eq(value, &other))
                        .unwrap_or(false)
                })
        }
        (Value::Map(x), Value::Map(y)) => {
            let x = x.borrow();
            let y = y.borrow();
            x.entries.len() == y.entries.len()
                && x.entries.iter().all(|(k, v)| {
                    y.get(k).map(|other| value_eq(v, &other)).unwrap_or(false)
                })
        }
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Task(x), Value::Task(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

pub fn deep_clone(value: &Value) -> Value {
    match value {
        Value::Str(s) => Value::Str(s.clone()),
        Value::Some(v) => Value::Some(Box::new(deep_clone(v))),
        Value::Ok(v) => Value::Ok(Box::new(deep_clone(v))),
        Value::Err(v) => Value::Err(Box::new(deep_clone(v))),
        Value::Vec(items) => Value::vec(items.borrow().iter().map(deep_clone).collect()),
        Value::Ary(ary) => {
            let ary = ary.borrow();
            Value::ary(
                ary.class.clone(),
                ary.fields
                    .iter()
                    .map(|(n, v)| (n.clone(), deep_clone(v)))
                    .collect(),
            )
        }
        Value::Map(map) => {
            let map = map.borrow();
            Value::map(
                map.entries
                    .iter()
                    .map(|(k, v)| (deep_clone(k), deep_clone(v)))
                    .collect(),
            )
        }
        Value::Func(f) => Value::Func(f.clone()),
        other => other.clone(),
    }
}

pub fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::None => "none".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::Float(v) => {
            if v.is_nan() {
                "NaN".to_string()
            } else if v.is_infinite() {
                if *v > 0.0 { "inf" } else { "-inf" }.to_string()
            } else {
                let mut s = format!("{}", v);
                if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                    s.push_str(".0");
                }
                s
            }
        }
        Value::Str(s) => (**s).clone(),
        Value::Char(c) => c.clone(),
        Value::Some(v) => format!("some({})", format_value(v)),
        Value::Ok(v) => format!("ok({})", format_value(v)),
        Value::Err(v) => format!("err({})", format_value(v)),
        Value::Vec(items) => {
            let items = items.borrow();
            let inner: Vec<String> = items.iter().map(format_value).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Ary(ary) => {
            let ary = ary.borrow();
            // 枚举值：`status.banned[reason = "x"]`，隐藏内部 `#variant` 字段。
            if let Some(variant) = ary.get("#variant").and_then(|value| value.as_str()) {
                let payload: Vec<String> = ary
                    .fields
                    .iter()
                    .filter(|(name, _)| name != "#variant")
                    .map(|(name, v)| format!("{} = {}", name, format_value(v)))
                    .collect();
                if payload.is_empty() {
                    return format!("{}.{}", ary.class, variant);
                }
                return format!("{}.{}[{}]", ary.class, variant, payload.join(", "));
            }
            let inner: Vec<String> = ary
                .fields
                .iter()
                .map(|(name, v)| format!("{} = {}", name, format_value(v)))
                .collect();
            if ary.class.is_empty() {
                format!("[{}]", inner.join(", "))
            } else {
                format!("{}[{}]", ary.class, inner.join(", "))
            }
        }
        Value::Map(map) => {
            let map = map.borrow();
            let inner: Vec<String> = map
                .entries
                .iter()
                .map(|(k, v)| format!("{:?} = {}", format_value(k), format_value(v)))
                .collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Func(f) => format!("<func {}>", f.name),
        Value::Task(_) => "<task>".to_string(),
    }
}

pub fn value_to_json(value: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Value::Null | Value::None => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(v) => J::Number((*v).into()),
        Value::UInt(v) => J::Number((*v).into()),
        Value::Float(v) => serde_json::Number::from_f64(*v)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Str(s) => J::String((**s).clone()),
        Value::Char(c) => J::String(c.clone()),
        Value::Some(v) | Value::Ok(v) | Value::Err(v) => value_to_json(v),
        Value::Vec(items) => {
            J::Array(items.borrow().iter().map(value_to_json).collect())
        }
        Value::Ary(ary) => {
            let ary = ary.borrow();
            let mut map = serde_json::Map::new();
            // 枚举值序列化为 `{"variant": "banned", ...字段}`。
            if let Some(variant) = ary.get("#variant").and_then(|value| value.as_str()) {
                map.insert("variant".to_string(), J::String(variant));
                for (name, v) in &ary.fields {
                    if name != "#variant" {
                        map.insert(name.clone(), value_to_json(v));
                    }
                }
                return J::Object(map);
            }
            for (name, v) in &ary.fields {
                map.insert(name.clone(), value_to_json(v));
            }
            J::Object(map)
        }
        Value::Map(map) => {
            let map = map.borrow();
            let mut object = serde_json::Map::new();
            for (k, v) in &map.entries {
                let key = match k {
                    Value::Str(s) => (**s).clone(),
                    other => format_value(other),
                };
                object.insert(key, value_to_json(v));
            }
            J::Object(object)
        }
        Value::Func(_) => J::Null,
        Value::Task(_) => J::Null,
    }
}

/// 把 `Value` 直接序列化进字节缓冲：跳过 `serde_json::Value` 中间树，
/// 一次分配、零克隆。输出与 `serde_json::to_vec(&value_to_json(v))` 逐字节一致
/// （键序同为 map/fields 的插入序，浮点同为 ryu 最短路表示）。
/// HTTP JSON 响应与 `json.encode` 走这条快速路径。
pub fn value_write_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null | Value::None => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Int(v) => {
            out.extend_from_slice(itoa::Buffer::new().format(*v).as_bytes());
        }
        Value::UInt(v) => {
            out.extend_from_slice(itoa::Buffer::new().format(*v).as_bytes());
        }
        Value::Float(v) => match serde_json::Number::from_f64(*v) {
            Some(number) => out.extend_from_slice(number.to_string().as_bytes()),
            None => out.extend_from_slice(b"null"),
        },
        Value::Str(s) => write_json_string(s, out),
        Value::Char(c) => write_json_string(c, out),
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => {
            value_write_json(inner, out)
        }
        Value::Vec(items) => {
            out.push(b'[');
            let items = items.borrow();
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                value_write_json(item, out);
            }
            out.push(b']');
        }
        Value::Ary(ary) => {
            let ary = ary.borrow();
            // 与旧路径一致：serde_json::Map 默认 BTreeMap，键按字典序输出。
            // 枚举 `#variant` 重命名为 `variant` 后与其它字段一起排序。
            let mut entries: Vec<(String, JsonField)> = Vec::with_capacity(ary.fields.len());
            if let Some(variant) = ary.get("#variant").and_then(|value| value.as_str()) {
                entries.push(("variant".to_string(), JsonField::Text(variant)));
            }
            for (name, inner) in &ary.fields {
                if name == "#variant" {
                    continue;
                }
                entries.push((name.clone(), JsonField::Node(inner)));
            }
            write_sorted_json_object(entries, out);
        }
        Value::Map(map) => {
            let map = map.borrow();
            let mut entries: Vec<(String, JsonField)> = Vec::with_capacity(map.entries.len());
            for (key, inner) in &map.entries {
                let key = match key {
                    Value::Str(s) => (**s).clone(),
                    other => format_value(other),
                };
                entries.push((key, JsonField::Node(inner)));
            }
            write_sorted_json_object(entries, out);
        }
        Value::Func(_) | Value::Task(_) => out.extend_from_slice(b"null"),
    }
}

/// `write_sorted_json_object` 的字段值：直接字符串或需递归序列化的节点。
enum JsonField<'a> {
    Text(String),
    Node(&'a Value),
}

/// 按键字典序写出 JSON 对象；重复键保留最后一次（与 `BTreeMap::insert` 覆盖语义一致）。
fn write_sorted_json_object(mut entries: Vec<(String, JsonField)>, out: &mut Vec<u8>) {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    out.push(b'{');
    let mut first = true;
    for index in 0..entries.len() {
        // 去重：排序后相同键相邻，只在最后一次出现处写出（覆盖语义）。
        if entries
            .get(index + 1)
            .is_some_and(|next| next.0 == entries[index].0)
        {
            continue;
        }
        if !first {
            out.push(b',');
        }
        first = false;
        let (key, value) = &entries[index];
        write_json_string(key, out);
        out.push(b':');
        match value {
            JsonField::Text(text) => write_json_string(text, out),
            JsonField::Node(inner) => value_write_json(inner, out),
        }
    }
    out.push(b'}');
}

/// 直接序列化并返回 `String`（响应体需要字节时优先用 `value_write_json`）。
pub fn value_to_json_string(value: &Value) -> String {
    let mut out = Vec::with_capacity(64);
    value_write_json(value, &mut out);
    String::from_utf8(out).unwrap_or_default()
}

/// JSON 字符串转义，规则与 serde_json 一致：控制字符用 `\u00xx`（小写十六进制），
/// 常用控制符走单字符转义，非 ASCII 原样输出 UTF-8。
pub(crate) fn write_json_string(text: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in text.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            other if (other as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", other as u32).as_bytes());
            }
            other => {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    out.push(b'"');
}

pub fn json_to_value(value: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match value {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                Value::UInt(u)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => Value::string(s.clone()),
        J::Array(items) => Value::vec(items.iter().map(json_to_value).collect()),
        J::Object(map) => Value::map(
            map.iter()
                .map(|(k, v)| (Value::string(k.clone()), json_to_value(v)))
                .collect(),
        ),
    }
}

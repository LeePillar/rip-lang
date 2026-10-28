//! 字节码快速路径：把「纯标量函数」编译成栈式字节码。
//!
//! 只编译安全子集：标量局部变量、算术/比较/逻辑、if/for、函数调用、
//! 内建模块与方法的调用。遇到任何不支持的语法（结构体、容器、match、
//! 闭包、全局变量等），整个函数回退到树遍历解释器，语义保持不变。
//!
//! 目的是让计算密集型热点（循环、递归）不再为每次变量读写做 HashMap 查找。

use std::collections::HashMap;
use std::sync::Arc;

use rp_ast::*;

use crate::value::{format_value, int_literal_value, value_eq, Flow, FlowResult, Value};
use crate::Runtime;

/// 一条栈式指令。行号只用于运行时错误定位。
#[derive(Clone)]
pub(crate) enum Op {
    PushInt(i64),
    PushFloat(f64),
    PushStr(Arc<String>),
    PushChar(String),
    PushBool(bool),
    PushNull,
    /// 读取局部变量槽位。
    Load(u16),
    /// 把栈顶写入局部变量槽位。
    Store(u16),
    /// 槽位 op int 常量，结果压栈（替代 Load + PushInt + Binary）。
    SlotConst(u16, i64, BinOp, u32),
    /// 槽位 op 槽位，结果压栈。
    SlotSlot(u16, u16, BinOp, u32),
    /// `x = x op <栈顶>`：读槽位、与栈顶运算、写回。
    Accumulate(u16, BinOp, u32),
    /// `x = x op int常量`：原地自更新，不经过操作数栈。
    SlotSelfConst(u16, i64, BinOp, u32),
    /// `(slot op1 int常量) op2 slot`：把 `x * k + y` 这类热点表达式压成一条指令。
    SlotConstSlot(u16, i64, u16, BinOp, BinOp, u32),
    /// `slot = <栈顶> % int常量`：取模写回，省掉 Push/Mod/Store 三条。
    ModStore(u16, i64, u32),
    /// 槽位 op int 常量 为假时跳转（替代 Load + PushInt + Binary + JumpIfFalse）。
    JumpUnlessSlotConst(u16, i64, BinOp, usize, u32),
    /// 槽位 op 槽位 为假时跳转。
    JumpUnlessSlotSlot(u16, u16, BinOp, usize, u32),
    Unary(UnOp, u32),
    /// 非短路二元运算；`&&` / `||` 在编译期展开为跳转。
    Binary(BinOp, u32),
    Jump(usize),
    JumpIfFalse(usize, u32),
    JumpIfTrue(usize, u32),
    CallPrint(u16),
    CallFunc(Arc<String>, u16, u32),
    /// 已编译函数的直接下标调用，省掉每次调用一次名字哈希。
    CallFuncId(u16, u16, u32),
    CallModule(Arc<String>, Arc<String>, u16, u32),
    CallMethod(Arc<String>, u16, u32),
    Pop,
    Return,
    ReturnVoid,
    /// 非 void 函数走到末尾：与解释器一致地报错。
    End(u32),
    /// 覆盖率标记：只有 `rip test --coverage` 时才会被编译进字节码。
    Cover(u32),
}

pub(crate) struct CompiledFunc {
    pub code: Vec<Op>,
    /// 参数个数（用于调用时的实参校验）。
    pub params: u16,
    /// 本函数需要的局部槽位总数。
    pub slots: u16,
    pub name: String,
    /// 函数定义行号，用于错误栈定位。
    pub line: u32,
}

struct Unsupported;

type Compiled = Result<(), Unsupported>;

/// 局部变量声明为 `int`（= `i64`）时才使用槽位专用指令。
fn is_int_type(ty: &TypeExpr) -> bool {
    matches!(&ty.kind, TypeExprKind::Named(path) if path.len() == 1 && (path[0] == "int" || path[0] == "i64"))
}

/// 需要运行期范围检查的整数位宽（`u8`…`u64`、`i8`…`i32`）：VM 不做位宽校验，
/// 遇到这类类型整体退回解释器，保证「越界 panic」语义一致。
fn needs_width_check(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Named(path) => match path.last().map(|name| name.as_str()) {
            Some("int") | Some("i64") => false,
            Some(name) => IntSuffix::from_name(name).is_some(),
            None => false,
        },
        _ => false,
    }
}

/// `int` 字面量：无后缀或 `i64` 后缀，且能装进 64 位有符号整数。
fn int_const(expr: &Expr) -> Option<i64> {
    match &expr.kind {
        ExprKind::Int {
            value,
            suffix: None | Some(IntSuffix::I64),
            ..
        } => i64::try_from(*value).ok(),
        _ => None,
    }
}

/// 编译期常量：只认字面量，确保折叠结果与运行时完全一致。
fn const_value(expr: &Expr) -> Option<Value> {
    match &expr.kind {
        ExprKind::Int { value, suffix, .. } => Some(int_literal_value(*value, *suffix)),
        ExprKind::Float(value) => Some(Value::Float(*value)),
        ExprKind::Str(value) => Some(Value::string(value.clone())),
        ExprKind::Char(value) => Some(Value::Char(value.clone())),
        ExprKind::Bool(value) => Some(Value::Bool(*value)),
        _ => None,
    }
}

fn fold_int(op: BinOp, a: i64, b: i64, wrap: fn(i64) -> Value) -> Option<Value> {
    let result = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Div => {
            if b == 0 {
                return None;
            }
            a.checked_div(b)
        }
        BinOp::Mod => {
            if b == 0 {
                return None;
            }
            a.checked_rem(b)
        }
        _ => return None,
    }?;
    Some(wrap(result))
}

fn fold_arithmetic(op: BinOp, left: &Value, right: &Value) -> Option<Value> {
    match (left, right) {
        (Value::Float(a), Value::Float(b)) => Some(Value::Float(match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => a / b,
            BinOp::Mod => a % b,
            _ => return None,
        })),
        (Value::Int(a), Value::Int(b)) => fold_int(op, *a, *b, Value::Int),
        _ => None,
    }
}

fn fold_compare(op: BinOp, left: &Value, right: &Value) -> Option<Value> {
    use std::cmp::Ordering;
    let ordering = match (left, right) {
        (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
        _ => return None,
    }?;
    Some(Value::Bool(match op {
        BinOp::Lt => ordering == Ordering::Less,
        BinOp::Gt => ordering == Ordering::Greater,
        BinOp::Le => ordering != Ordering::Greater,
        BinOp::Ge => ordering != Ordering::Less,
        _ => return None,
    }))
}

/// 常量折叠；任何无法安全求值的情况都返回 None，留给运行时按原语义报错。
fn fold_binary(op: BinOp, left: &Value, right: &Value) -> Option<Value> {
    match op {
        BinOp::Eq => Some(Value::Bool(value_eq(left, right))),
        BinOp::Ne => Some(Value::Bool(!value_eq(left, right))),
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => fold_compare(op, left, right),
        BinOp::Add => {
            if let (Some(a), Some(b)) = (left.as_str(), right.as_str()) {
                return Some(Value::string(format!("{}{}", a, b)));
            }
            fold_arithmetic(op, left, right)
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => fold_arithmetic(op, left, right),
        BinOp::And | BinOp::Or => None,
    }
}

fn fold_unary(op: UnOp, value: &Value) -> Option<Value> {
    match (op, value) {
        (UnOp::Not, Value::Bool(b)) => Some(Value::Bool(!*b)),
        (UnOp::Neg, Value::Int(v)) => Some(Value::Int(v.checked_neg()?)),
        (UnOp::Neg, Value::Float(v)) => Some(Value::Float(-v)),
        _ => None,
    }
}

struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

/// 编译期作用域：记录名字到槽位的映射，以及作用域结束时要回收的槽位水位。
struct Scope {
    names: HashMap<String, u16>,
    base_slot: u16,
}

struct Compiler<'a> {
    funcs: &'a HashMap<String, Arc<FuncDecl>>,
    imports: &'a HashMap<String, String>,
    code: Vec<Op>,
    scopes: Vec<Scope>,
    next_slot: u16,
    /// 槽位水位：作用域回收会让 `next_slot` 下降，但函数帧必须按峰值分配。
    max_slot: u16,
    /// 每个槽位是否为 `int`：专用指令只用于 int，避免类型语义漂移。
    slot_int: Vec<bool>,
    loops: Vec<LoopCtx>,
}

/// 编译结果：名字表用于解释器入口，下标表用于字节码内的直接调用。
///
/// 同一个编译单元只编译一次，结果通过 `Arc` 在 HTTP 工作线程、`parallel` 分支
/// 与并行测试之间共享；对外只暴露 `SharedVm` 句柄。
pub(crate) struct VmCache {
    pub by_name: HashMap<String, Arc<CompiledFunc>>,
    pub by_index: Vec<Arc<CompiledFunc>>,
}

impl VmCache {
    fn empty() -> Self {
        Self {
            by_name: HashMap::new(),
            by_index: Vec::new(),
        }
    }
}

/// 编译所有函数；返回可在 VM 中执行的子集，其余留空走解释器。
pub(crate) fn compile_all(
    funcs: &HashMap<String, Arc<FuncDecl>>,
    imports: &HashMap<String, String>,
) -> VmCache {
    // 逃生开关：RIP_NO_VM=1 时完全回退到树遍历解释器，便于对比与排障。
    if std::env::var_os("RIP_NO_VM").is_some() {
        return VmCache::empty();
    }
    let mut compiled: HashMap<String, CompiledFunc> = HashMap::new();
    for (name, decl) in funcs {
        if let Some(func) = compile(decl, funcs, imports) {
            compiled.insert(name.clone(), func);
        }
    }
    if compiled.len() > u16::MAX as usize {
        return VmCache::empty();
    }
    // 函数下标按名字排序，保证同一次编译内稳定。
    let mut names: Vec<String> = compiled.keys().cloned().collect();
    names.sort();
    let mut index_of: HashMap<String, u16> = HashMap::new();
    for (index, name) in names.iter().enumerate() {
        index_of.insert(name.clone(), index as u16);
    }
    for func in compiled.values_mut() {
        for op in func.code.iter_mut() {
            if let Op::CallFunc(name, argc, line) = op {
                if let Some(index) = index_of.get(name.as_str()) {
                    *op = Op::CallFuncId(*index, *argc, *line);
                }
            }
        }
    }
    let mut by_name = HashMap::new();
    let mut by_index = Vec::with_capacity(names.len());
    for name in names {
        if let Some(func) = compiled.remove(&name) {
            let func = Arc::new(func);
            by_index.push(func.clone());
            by_name.insert(name, func);
        }
    }
    VmCache { by_name, by_index }
}

fn compile(
    decl: &FuncDecl,
    funcs: &HashMap<String, Arc<FuncDecl>>,
    imports: &HashMap<String, String>,
) -> Option<CompiledFunc> {
    // async 函数在独立线程解释执行，字节码路径不参与。
    if decl.is_async {
        return None;
    }
    let mut compiler = Compiler {
        funcs,
        imports,
        code: Vec::new(),
        scopes: vec![Scope {
            names: HashMap::new(),
            base_slot: 0,
        }],
        next_slot: 0,
        max_slot: 0,
        slot_int: Vec::new(),
        loops: Vec::new(),
    };
    for param in &decl.params {
        if needs_width_check(&param.ty) {
            return None;
        }
        compiler
            .declare(&param.name, is_int_type(&param.ty))
            .ok()?;
    }
    if needs_width_check(&decl.ret) {
        return None;
    }
    // 函数体单独一层作用域：允许 `let` 遮蔽参数。
    compiler.push_scope();
    for stmt in &decl.body {
        compiler.stmt(stmt).ok()?;
    }
    let ret_void = decl.ret.kind == TypeExprKind::Void;
    if ret_void {
        compiler.code.push(Op::ReturnVoid);
    } else {
        compiler.code.push(Op::End(decl.line));
    }
    Some(CompiledFunc {
        code: compiler.code,
        params: decl.params.len() as u16,
        slots: compiler.max_slot,
        name: decl.name.clone(),
        line: decl.line,
    })
}

impl<'a> Compiler<'a> {
    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            names: HashMap::new(),
            base_slot: self.next_slot,
        });
    }

    /// 结束作用域：回收该层声明的槽位，让后面的局部变量复用同一批槽。
    fn pop_scope(&mut self) {
        if let Some(scope) = self.scopes.pop() {
            self.next_slot = scope.base_slot;
        }
    }

    fn declare(&mut self, name: &str, is_int: bool) -> Result<u16, Unsupported> {
        if self.next_slot == u16::MAX {
            return Err(Unsupported);
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.max_slot = self.max_slot.max(self.next_slot);
        if self.slot_int.len() <= slot as usize {
            self.slot_int.resize(slot as usize + 1, false);
        }
        self.slot_int[slot as usize] = is_int;
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name.to_string(), slot);
        }
        Ok(slot)
    }

    fn is_int_slot(&self, slot: u16) -> bool {
        self.slot_int.get(slot as usize).copied().unwrap_or(false)
    }

    fn lookup(&self, name: &str) -> Option<u16> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.names.get(name) {
                return Some(*slot);
            }
        }
        None
    }

    /// 条件为假时跳转；int 槽位与常量/槽位的比较直接生成一条跳转指令。
    fn cond_jump_if_false(&mut self, cond: &Expr, line: u32) -> Result<usize, Unsupported> {
        if let ExprKind::Binary { op, left, right } = &cond.kind {
            if matches!(
                op,
                BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
            ) {
                if let ExprKind::Ident(name) = &left.kind {
                    if let Some(slot) = self.lookup(name) {
                        if self.is_int_slot(slot) {
                            if let Some(constant) = int_const(right) {
                                return Ok(self.emit(Op::JumpUnlessSlotConst(
                                    slot, constant, *op, 0, line,
                                )));
                            }
                            if let ExprKind::Ident(other) = &right.kind {
                                if let Some(right_slot) = self.lookup(other) {
                                    if self.is_int_slot(right_slot) {
                                        return Ok(self.emit(Op::JumpUnlessSlotSlot(
                                            slot, right_slot, *op, 0, line,
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.expr(cond)?;
        Ok(self.emit(Op::JumpIfFalse(0, line)))
    }

    fn stmt(&mut self, stmt: &Stmt) -> Compiled {
        // 采集覆盖率时插入标记指令；关闭时字节码与原来完全一致。
        if crate::coverage::enabled() {
            self.code.push(Op::Cover(stmt.line));
        }
        match &stmt.kind {
            StmtKind::Let { name, ty, init, .. } => {
                if needs_width_check(ty) {
                    return Err(Unsupported);
                }
                self.expr(init)?;
                let slot = self.declare(name, is_int_type(ty))?;
                self.code.push(Op::Store(slot));
                Ok(())
            }
            StmtKind::Expr(expr) => {
                self.expr(expr)?;
                self.code.push(Op::Pop);
                Ok(())
            }
            StmtKind::Assign { target, value } => {
                let slot = match &target.kind {
                    ExprKind::Ident(name) => self.lookup(name).ok_or(Unsupported)?,
                    _ => return Err(Unsupported),
                };
                // `x = x op rhs`：只对 int 槽位做融合，读/写顺序与解释器一致。
                if self.is_int_slot(slot) {
                    if let ExprKind::Binary { op, left, right } = &value.kind {
                        if !matches!(op, BinOp::And | BinOp::Or) {
                            if let ExprKind::Ident(name) = &left.kind {
                                if self.lookup(name) == Some(slot) {
                                    if let Some(constant) = int_const(right) {
                                        self.code.push(Op::SlotSelfConst(
                                            slot, constant, *op, stmt.line,
                                        ));
                                        return Ok(());
                                    }
                                    self.expr(right)?;
                                    self.code.push(Op::Accumulate(slot, *op, stmt.line));
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                // `x = <表达式> % 常量`：表达式结果直接取模写回，不经过 Push/Mod/Store。
                if self.is_int_slot(slot) {
                    if let ExprKind::Binary {
                        op: BinOp::Mod,
                        left,
                        right,
                    } = &value.kind
                    {
                        if let Some(constant) = int_const(right) {
                            self.expr(left)?;
                            self.code.push(Op::ModStore(slot, constant, stmt.line));
                            return Ok(());
                        }
                    }
                }
                self.expr(value)?;
                self.code.push(Op::Store(slot));
                Ok(())
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let jump_else = self.cond_jump_if_false(cond, stmt.line)?;
                self.push_scope();
                for inner in then_body {
                    self.stmt(inner)?;
                }
                self.pop_scope();
                match else_body {
                    Some(body) => {
                        let jump_end = self.emit(Op::Jump(0));
                        let else_at = self.code.len();
                        self.patch(jump_else, else_at);
                        self.push_scope();
                        for inner in body {
                            self.stmt(inner)?;
                        }
                        self.pop_scope();
                        let end = self.code.len();
                        self.patch(jump_end, end);
                    }
                    None => {
                        let end = self.code.len();
                        self.patch(jump_else, end);
                    }
                }
                Ok(())
            }
            StmtKind::Loop { body } => {
                let start = self.code.len();
                self.push_scope();
                self.loops.push(LoopCtx {
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                for inner in body {
                    self.stmt(inner)?;
                }
                let ctx = self.loops.pop().ok_or(Unsupported)?;
                self.pop_scope();
                for jump in ctx.continues {
                    self.patch(jump, start);
                }
                self.emit(Op::Jump(start));
                let end = self.code.len();
                for jump in ctx.breaks {
                    self.patch(jump, end);
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                let start = self.code.len();
                let exit = self.cond_jump_if_false(cond, stmt.line)?;
                self.push_scope();
                self.loops.push(LoopCtx {
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                for inner in body {
                    self.stmt(inner)?;
                }
                let ctx = self.loops.pop().ok_or(Unsupported)?;
                self.pop_scope();
                for jump in ctx.continues {
                    self.patch(jump, start);
                }
                self.emit(Op::Jump(start));
                let end = self.code.len();
                self.patch(exit, end);
                for jump in ctx.breaks {
                    self.patch(jump, end);
                }
                Ok(())
            }
            StmtKind::Return(value) => {
                match value {
                    Some(expr) => self.expr(expr)?,
                    None => self.code.push(Op::PushNull),
                }
                self.code.push(Op::Return);
                Ok(())
            }
            StmtKind::Break => {
                if self.loops.is_empty() {
                    return Err(Unsupported);
                }
                let at = self.emit(Op::Jump(0));
                if let Some(ctx) = self.loops.last_mut() {
                    ctx.breaks.push(at);
                }
                Ok(())
            }
            StmtKind::Continue => {
                if self.loops.is_empty() {
                    return Err(Unsupported);
                }
                let at = self.emit(Op::Jump(0));
                if let Some(ctx) = self.loops.last_mut() {
                    ctx.continues.push(at);
                }
                Ok(())
            }
            StmtKind::WhileLet { .. }
            | StmtKind::ForIn { .. }
            | StmtKind::Match { .. }
            | StmtKind::Defer { .. }
            | StmtKind::Parallel { .. } => Err(Unsupported),
        }
    }

    fn expr(&mut self, expr: &Expr) -> Compiled {
        match &expr.kind {
            ExprKind::Int { value, suffix, .. } => {
                // VM 只处理 int（i64）：带无符号或窄位宽后缀的字面量退回解释器。
                if !matches!(suffix, None | Some(IntSuffix::I64)) {
                    return Err(Unsupported);
                }
                let value = i64::try_from(*value).map_err(|_| Unsupported)?;
                self.code.push(Op::PushInt(value));
                Ok(())
            }
            ExprKind::Float(value) => {
                self.code.push(Op::PushFloat(*value));
                Ok(())
            }
            ExprKind::Str(value) => {
                self.code.push(Op::PushStr(Arc::new(value.clone())));
                Ok(())
            }
            ExprKind::Char(value) => {
                self.code.push(Op::PushChar(value.clone()));
                Ok(())
            }
            ExprKind::Bool(value) => {
                self.code.push(Op::PushBool(*value));
                Ok(())
            }
            ExprKind::Ident(name) => {
                let slot = self.lookup(name).ok_or(Unsupported)?;
                self.code.push(Op::Load(slot));
                Ok(())
            }
            ExprKind::Unary { op, expr: inner } => {
                if let Some(value) = const_value(inner) {
                    if let Some(folded) = fold_unary(*op, &value) {
                        self.push_const(folded);
                        return Ok(());
                    }
                }
                self.expr(inner)?;
                self.code.push(Op::Unary(*op, expr.line));
                Ok(())
            }
            ExprKind::Binary { op, left, right } => match op {
                BinOp::And => {
                    self.expr(left)?;
                    let fail = self.emit(Op::JumpIfFalse(0, expr.line));
                    self.expr(right)?;
                    let fail2 = self.emit(Op::JumpIfFalse(0, expr.line));
                    self.code.push(Op::PushBool(true));
                    let end = self.emit(Op::Jump(0));
                    let false_at = self.code.len();
                    self.patch(fail, false_at);
                    self.patch(fail2, false_at);
                    self.code.push(Op::PushBool(false));
                    let end_at = self.code.len();
                    self.patch(end, end_at);
                    Ok(())
                }
                BinOp::Or => {
                    self.expr(left)?;
                    let pass = self.emit(Op::JumpIfTrue(0, expr.line));
                    self.expr(right)?;
                    let pass2 = self.emit(Op::JumpIfTrue(0, expr.line));
                    self.code.push(Op::PushBool(false));
                    let end = self.emit(Op::Jump(0));
                    let true_at = self.code.len();
                    self.patch(pass, true_at);
                    self.patch(pass2, true_at);
                    self.code.push(Op::PushBool(true));
                    let end_at = self.code.len();
                    self.patch(end, end_at);
                    Ok(())
                }
                other => {
                    if let (Some(left_value), Some(right_value)) =
                        (const_value(left), const_value(right))
                    {
                        if let Some(folded) = fold_binary(*other, &left_value, &right_value) {
                            self.push_const(folded);
                            return Ok(());
                        }
                    }
                    // int 槽位融合：`x op 常量` / `x op y` 各压成一条指令。
                    // 先尝试更长的 `(x op1 常量) op2 y`，命中后一次完成三段计算。
                    if let ExprKind::Binary {
                        op: inner_op,
                        left: inner_left,
                        right: inner_right,
                    } = &left.kind
                    {
                        if !matches!(inner_op, BinOp::And | BinOp::Or) {
                            if let ExprKind::Ident(inner_name) = &inner_left.kind {
                                if let Some(inner_slot) = self.lookup(inner_name) {
                                    if self.is_int_slot(inner_slot) {
                                        if let Some(constant) = int_const(inner_right) {
                                            if let ExprKind::Ident(right_name) = &right.kind {
                                                if let Some(right_slot) =
                                                    self.lookup(right_name)
                                                {
                                                    if self.is_int_slot(right_slot) {
                                                        self.code.push(Op::SlotConstSlot(
                                                            inner_slot,
                                                            constant,
                                                            right_slot,
                                                            *inner_op,
                                                            *other,
                                                            expr.line,
                                                        ));
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let ExprKind::Ident(name) = &left.kind {
                        if let Some(slot) = self.lookup(name) {
                            if self.is_int_slot(slot) {
                                if let Some(constant) = int_const(right) {
                                    self.code.push(Op::SlotConst(
                                        slot, constant, *other, expr.line,
                                    ));
                                    return Ok(());
                                }
                                if let ExprKind::Ident(right_name) = &right.kind {
                                    if let Some(right_slot) = self.lookup(right_name) {
                                        if self.is_int_slot(right_slot) {
                                            self.code.push(Op::SlotSlot(
                                                slot, right_slot, *other, expr.line,
                                            ));
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    self.expr(left)?;
                    self.expr(right)?;
                    self.code.push(Op::Binary(*other, expr.line));
                    Ok(())
                }
            },
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Ident(name) = &callee.kind {
                    if name == "print" {
                        for arg in args {
                            self.expr(arg)?;
                        }
                        self.code.push(Op::CallPrint(args.len() as u16));
                        return Ok(());
                    }
                    if self.funcs.contains_key(name) {
                        for arg in args {
                            self.expr(arg)?;
                        }
                        self.code.push(Op::CallFunc(
                            Arc::new(name.clone()),
                            args.len() as u16,
                            expr.line,
                        ));
                        return Ok(());
                    }
                    return Err(Unsupported);
                }
                if let ExprKind::Field { base, name } = &callee.kind {
                    if let ExprKind::Ident(alias) = &base.kind {
                        if let Some(path) = self.imports.get(alias) {
                            for arg in args {
                                self.expr(arg)?;
                            }
                            self.code.push(Op::CallModule(
                                Arc::new(path.clone()),
                                Arc::new(name.clone()),
                                args.len() as u16,
                                expr.line,
                            ));
                            return Ok(());
                        }
                    }
                }
                Err(Unsupported)
            }
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                if let ExprKind::Ident(alias) = &receiver.kind {
                    if let Some(path) = self.imports.get(alias) {
                        for arg in args {
                            self.expr(arg)?;
                        }
                        self.code.push(Op::CallModule(
                            Arc::new(path.clone()),
                            Arc::new(name.clone()),
                            args.len() as u16,
                            expr.line,
                        ));
                        return Ok(());
                    }
                }
                self.expr(receiver)?;
                for arg in args {
                    self.expr(arg)?;
                }
                self.code.push(Op::CallMethod(
                    Arc::new(name.clone()),
                    args.len() as u16,
                    expr.line,
                ));
                Ok(())
            }
            // 结构体、容器、option/result、闭包、? 等走解释器。
            _ => Err(Unsupported),
        }
    }

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    fn push_const(&mut self, value: Value) {
        self.code.push(match value {
            Value::Int(v) => Op::PushInt(v),
            Value::Float(v) => Op::PushFloat(v),
            Value::Str(v) => Op::PushStr(v),
            Value::Char(v) => Op::PushChar(v),
            Value::Bool(v) => Op::PushBool(v),
            // const_value / fold_* 只会产出以上类型，这里兜底保持可执行。
            _ => Op::PushNull,
        });
    }

    fn patch(&mut self, at: usize, target: usize) {
        match &mut self.code[at] {
            Op::Jump(slot)
            | Op::JumpIfFalse(slot, _)
            | Op::JumpIfTrue(slot, _)
            | Op::JumpUnlessSlotConst(_, _, _, slot, _)
            | Op::JumpUnlessSlotSlot(_, _, _, slot, _) => *slot = target,
            _ => {}
        }
    }
}

struct Frame {
    func: Arc<CompiledFunc>,
    pc: usize,
    base: usize,
}

fn new_slots(slots: &mut Vec<Value>, func: &CompiledFunc, args: Vec<Value>) -> usize {
    let base = slots.len();
    slots.extend(args);
    slots.resize(base + func.slots as usize, Value::Null);
    base
}

fn pop_args(stack: &mut Vec<Value>, argc: u16) -> Vec<Value> {
    let at = stack.len().saturating_sub(argc as usize);
    stack.split_off(at)
}

impl Runtime {
    /// 字节码用的二元运算：int 与 int 直接内联，其余回落到统一实现。
    fn vm_binary(
        &mut self,
        op: BinOp,
        left: Value,
        right: Value,
        line: u32,
    ) -> FlowResult<Value> {
        if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
            let fast = match op {
                BinOp::Add => a.checked_add(*b).map(Value::Int),
                BinOp::Sub => a.checked_sub(*b).map(Value::Int),
                BinOp::Mul => a.checked_mul(*b).map(Value::Int),
                BinOp::Div => a.checked_div(*b).map(Value::Int),
                BinOp::Mod => a.checked_rem(*b).map(Value::Int),
                BinOp::Eq => Some(Value::Bool(a == b)),
                BinOp::Ne => Some(Value::Bool(a != b)),
                BinOp::Lt => Some(Value::Bool(a < b)),
                BinOp::Gt => Some(Value::Bool(a > b)),
                BinOp::Le => Some(Value::Bool(a <= b)),
                BinOp::Ge => Some(Value::Bool(a >= b)),
                BinOp::And | BinOp::Or => None,
            };
            if let Some(value) = fast {
                return Ok(value);
            }
        }
        self.binary_values(op, left, right, line)
    }

    /// 执行一段已编译的字节码；调用其他函数时仍复用统一的调用入口。
    ///
    /// 执行循环不复制指令：`op` 只借用当前函数的代码，调用/返回通过
    /// 循环末尾统一切换帧，避免每条指令一次 `Op` clone。
    pub(crate) fn run_vm(
        &mut self,
        entry: Arc<CompiledFunc>,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // 补充调用帧：字节码函数不经过 call_decl，出错时仍能在栈里看到函数名。
        self.call_stack.push((entry.name.clone(), entry.line));
        let mut stack: Vec<Value> = Vec::with_capacity(32);
        let mut slots: Vec<Value> = Vec::with_capacity(entry.slots as usize + 8);
        let mut frames: Vec<Frame> = Vec::new();
        let mut func = entry;
        let mut pc = 0usize;
        let mut base = new_slots(&mut slots, &func, args);
        loop {
            let op = match func.code.get(pc) {
                Some(op) => op,
                None => {
                    return Err(Flow::Error(
                        self.runtime_error("bytecode ran past the end", line),
                    ))
                }
            };
            pc += 1;
            let mut pending: Option<(Arc<CompiledFunc>, usize)> = None;
            let mut ret: Option<Value> = None;
            match op {
                Op::PushInt(value) => stack.push(Value::Int(*value)),
                Op::PushFloat(value) => stack.push(Value::Float(*value)),
                Op::PushStr(value) => stack.push(Value::Str(value.clone())),
                Op::PushChar(value) => stack.push(Value::Char(value.clone())),
                Op::PushBool(value) => stack.push(Value::Bool(*value)),
                Op::PushNull => stack.push(Value::Null),
                Op::Load(slot) => stack.push(slots[base + *slot as usize].clone()),
                Op::Store(slot) => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    slots[base + *slot as usize] = value;
                }
                Op::SlotConst(slot, constant, op, line) => {
                    let left = slots[base + *slot as usize].clone();
                    let result = self.vm_binary(*op, left, Value::Int(*constant), *line)?;
                    stack.push(result);
                }
                Op::SlotSlot(left_slot, right_slot, op, line) => {
                    let left = slots[base + *left_slot as usize].clone();
                    let right = slots[base + *right_slot as usize].clone();
                    let result = self.vm_binary(*op, left, right, *line)?;
                    stack.push(result);
                }
                Op::Accumulate(slot, op, line) => {
                    let right = stack.pop().unwrap_or(Value::Null);
                    let left = slots[base + *slot as usize].clone();
                    let result = self.vm_binary(*op, left, right, *line)?;
                    slots[base + *slot as usize] = result;
                }
                Op::SlotSelfConst(slot, constant, op, line) => {
                    let left = slots[base + *slot as usize].clone();
                    let result = self.vm_binary(*op, left, Value::Int(*constant), *line)?;
                    slots[base + *slot as usize] = result;
                }
                Op::SlotConstSlot(left_slot, constant, right_slot, first, second, line) => {
                    let left = slots[base + *left_slot as usize].clone();
                    let middle = self.vm_binary(*first, left, Value::Int(*constant), *line)?;
                    let right = slots[base + *right_slot as usize].clone();
                    let result = self.vm_binary(*second, middle, right, *line)?;
                    stack.push(result);
                }
                Op::ModStore(slot, constant, line) => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    let result = self.vm_binary(BinOp::Mod, value, Value::Int(*constant), *line)?;
                    slots[base + *slot as usize] = result;
                }
                Op::JumpUnlessSlotConst(slot, constant, op, target, line) => {
                    let left = slots[base + *slot as usize].clone();
                    let result = self.vm_binary(*op, left, Value::Int(*constant), *line)?;
                    if !self.truthy(result, *line)? {
                        pc = *target;
                    }
                }
                Op::JumpUnlessSlotSlot(left_slot, right_slot, op, target, line) => {
                    let left = slots[base + *left_slot as usize].clone();
                    let right = slots[base + *right_slot as usize].clone();
                    let result = self.vm_binary(*op, left, right, *line)?;
                    if !self.truthy(result, *line)? {
                        pc = *target;
                    }
                }
                Op::Unary(op, line) => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    let result = self.unary_value(*op, value, *line)?;
                    stack.push(result);
                }
                Op::Binary(op, line) => {
                    let right = stack.pop().unwrap_or(Value::Null);
                    let left = stack.pop().unwrap_or(Value::Null);
                    let result = self.vm_binary(*op, left, right, *line)?;
                    stack.push(result);
                }
                Op::Jump(target) => pc = *target,
                Op::JumpIfFalse(target, line) => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    if !self.truthy(value, *line)? {
                        pc = *target;
                    }
                }
                Op::JumpIfTrue(target, line) => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    if self.truthy(value, *line)? {
                        pc = *target;
                    }
                }
                Op::Pop => {
                    stack.pop();
                }
                Op::CallPrint(argc) => {
                    let values = pop_args(&mut stack, *argc);
                    let text: Vec<String> = values.iter().map(format_value).collect();
                    println!("{}", text.join(" "));
                    stack.push(Value::Null);
                }
                Op::CallFunc(name, argc, line) => {
                    let argc = *argc as usize;
                    let arg_start = stack.len().saturating_sub(argc);
                    if let Some(callee) = self.vm.by_name.get(name.as_str()).cloned() {
                        if callee.params as usize != argc {
                            return Err(Flow::Error(self.runtime_error(
                                format!(
                                    "function '{}' expects {} arguments, found {}",
                                    callee.name, callee.params, argc
                                ),
                                *line,
                            )));
                        }
                        // 参数直接从栈搬进新帧的槽位，避免每次调用分配临时 Vec。
                        frames.push(Frame {
                            func: func.clone(),
                            pc,
                            base,
                        });
                        self.call_stack.push((callee.name.clone(), callee.line));
                        let new_base = slots.len();
                        slots.extend(stack.drain(arg_start..));
                        slots.resize(new_base + callee.slots as usize, Value::Null);
                        pending = Some((callee, new_base));
                    } else {
                        let mut args = Vec::with_capacity(argc);
                        args.extend(stack.drain(arg_start..));
                        let decl = match self.funcs.get(name.as_str()).cloned() {
                            Some(decl) => decl,
                            None => {
                                return Err(Flow::Error(self.runtime_error(
                                    format!("unknown function '{}'", name),
                                    *line,
                                )))
                            }
                        };
                        let value = self.call_decl(&decl, args, *line)?;
                        stack.push(value);
                    }
                }
                Op::CallFuncId(index, argc, line) => {
                    let argc = *argc as usize;
                    let arg_start = stack.len().saturating_sub(argc);
                    let callee = match self.vm.by_index.get(*index as usize).cloned() {
                        Some(callee) => callee,
                        None => {
                            return Err(Flow::Error(
                                self.runtime_error("stale bytecode function index", *line),
                            ))
                        }
                    };
                    if callee.params as usize != argc {
                        return Err(Flow::Error(self.runtime_error(
                            format!(
                                "function '{}' expects {} arguments, found {}",
                                callee.name, callee.params, argc
                            ),
                            *line,
                        )));
                    }
                    frames.push(Frame {
                        func: func.clone(),
                        pc,
                        base,
                    });
                    self.call_stack.push((callee.name.clone(), callee.line));
                    let new_base = slots.len();
                    slots.extend(stack.drain(arg_start..));
                    slots.resize(new_base + callee.slots as usize, Value::Null);
                    pending = Some((callee, new_base));
                }
                Op::CallModule(path, name, argc, line) => {
                    let args = pop_args(&mut stack, *argc);
                    let value = self.call_module(path, name, args, *line)?;
                    stack.push(value);
                }
                Op::CallMethod(name, argc, line) => {
                    let args = pop_args(&mut stack, *argc);
                    let receiver = stack.pop().unwrap_or(Value::Null);
                    let value = self.call_method(receiver, name, args, *line)?;
                    stack.push(value);
                }
                Op::Return => {
                    let value = stack.pop().unwrap_or(Value::Null);
                    ret = Some(value);
                }
                Op::ReturnVoid => ret = Some(Value::Null),
                Op::End(line) => {
                    return Err(Flow::Error(self.runtime_error(
                        format!("function '{}' ended without returning a value", func.name),
                        *line,
                    )))
                }
                Op::Cover(line) => {
                    if self.coverage {
                        crate::coverage::record(&func.name, *line);
                    }
                }
            }
            if let Some((callee, new_base)) = pending {
                func = callee;
                pc = 0;
                base = new_base;
                continue;
            }
            if let Some(value) = ret {
                slots.truncate(base);
                match frames.pop() {
                    Some(frame) => {
                        func = frame.func;
                        pc = frame.pc;
                        base = frame.base;
                        stack.push(value);
                        self.call_stack.pop();
                    }
                    None => return Ok(value),
                }
            }
        }
    }
}

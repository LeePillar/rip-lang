//! Rip 所有权 / 借用 / 移动检查。
//!
//! 规则：Copy 类型复制；Move 类型赋值 / 传参默认转移；`read` 借用，`write`
//! 可变借用；禁止使用已移动变量；禁止部分移动；禁止修改不可变绑定。

use std::collections::HashMap;

use rp_ast::*;
use rp_types::{CheckResult, Diagnostic, EnumInfo, FuncSig, Level, Type, TypeTable};

pub fn check_ownership(program: &Program, checked: &CheckResult) -> Vec<Diagnostic> {
    let mut checker = Checker {
        table: &checked.type_table,
        funcs: &checked.funcs,
        interfaces: &checked.interfaces,
        enums: &checked.enums,
        type_bounds: &checked.type_bounds,
        current_bounds: HashMap::new(),
        expr_types: &checked.expr_types,
        scopes: Vec::new(),
        diagnostics: Vec::new(),
    };
    checker.run(program);
    checker.diagnostics
}

struct VarState {
    mutable: bool,
    moved: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Read,
    Consume,
    Write,
}

struct Checker<'a> {
    table: &'a TypeTable,
    funcs: &'a HashMap<String, FuncSig>,
    interfaces: &'a HashMap<String, Vec<rp_types::InterfaceMethodSig>>,
    /// 枚举名 → 变体与字段类型。
    enums: &'a HashMap<String, EnumInfo>,
    /// 函数全名 → 类型参数约束。
    type_bounds: &'a HashMap<String, HashMap<String, String>>,
    /// 当前函数的类型参数约束。
    current_bounds: HashMap<String, String>,
    expr_types: &'a HashMap<NodeId, Type>,
    scopes: Vec<HashMap<String, VarState>>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Checker<'a> {
    fn run(&mut self, program: &Program) {
        self.scopes.push(HashMap::new());
        for item in &program.items {
            if let Item::Const(decl) | Item::Static(decl) = item {
                self.scopes.last_mut().unwrap().insert(
                    decl.name.clone(),
                    VarState {
                        mutable: false,
                        moved: false,
                    },
                );
            }
        }
        for item in &program.items {
            if let Item::Func(func) = item {
                self.check_func(func);
            }
        }
    }

    fn check_func(&mut self, func: &FuncDecl) {
        self.current_bounds = self
            .type_bounds
            .get(&func.name)
            .cloned()
            .unwrap_or_default();
        self.scopes.push(HashMap::new());
        for p in &func.params {
            self.declare(
                &p.name,
                rp_types::type_from_expr(&p.ty),
                p.mode == ParamMode::Write,
                p.line,
            );
        }
        self.check_stmts(&func.body);
        self.scopes.pop();
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.check_stmt(stmt);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                name,
                mutable,
                ty,
                init,
            } => {
                self.use_expr(init, Mode::Consume);
                self.declare(name, rp_types::type_from_expr(ty), *mutable, stmt.line);
            }
            StmtKind::Expr(expr) => {
                self.use_expr(expr, Mode::Read);
            }
            StmtKind::Assign { target, value } => {
                match &target.kind {
                    ExprKind::Ident(name) => {
                        self.require_mutable(name, target.line);
                        self.set_moved(name, false);
                    }
                    ExprKind::Field { base, .. } => {
                        if let Some(root) = root_ident(target) {
                            self.require_mutable(&root, target.line);
                        }
                        self.use_expr(base, Mode::Read);
                    }
                    _ => {
                        self.use_expr(target, Mode::Read);
                    }
                }
                self.use_expr(value, Mode::Consume);
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                self.use_expr(cond, Mode::Read);
                self.scopes.push(HashMap::new());
                self.check_stmts(then_body);
                self.scopes.pop();
                if let Some(body) = else_body {
                    self.scopes.push(HashMap::new());
                    self.check_stmts(body);
                    self.scopes.pop();
                }
            }
            StmtKind::Loop { body } => {
                self.scopes.push(HashMap::new());
                self.check_stmts(body);
                self.scopes.pop();
            }
            StmtKind::While { cond, body } => {
                self.use_expr(cond, Mode::Read);
                self.scopes.push(HashMap::new());
                self.check_stmts(body);
                self.scopes.pop();
            }
            StmtKind::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                self.use_expr(scrutinee, Mode::Consume);
                let subject_ty = self.table.resolve(&self.ty_of(scrutinee));
                self.scopes.push(HashMap::new());
                match (pattern, &subject_ty) {
                    (Pattern::Some(name), Type::Option(inner)) => {
                        self.declare(name, (**inner).clone(), false, stmt.line);
                    }
                    (Pattern::Ok(name), Type::Result(ok, _)) => {
                        self.declare(name, (**ok).clone(), false, stmt.line);
                    }
                    (Pattern::Err(name), Type::Result(_, err)) => {
                        self.declare(name, (**err).clone(), false, stmt.line);
                    }
                    (Pattern::Ary(fields), Type::Ary(ary_fields)) => {
                        for field in fields {
                            if field.name == "_" {
                                continue;
                            }
                            if let Some((_, ty)) =
                                ary_fields.iter().find(|(n, _)| n == &field.name)
                            {
                                self.declare(&field.name, ty.clone(), false, stmt.line);
                            }
                        }
                    }
                    _ => {}
                }
                self.check_stmts(body);
                self.scopes.pop();
            }
            StmtKind::ForIn { name, iter, body } => {
                self.use_expr(iter, Mode::Read);
                let elem = self.elem_type(iter);
                self.scopes.push(HashMap::new());
                self.declare(name, elem, false, stmt.line);
                self.check_stmts(body);
                self.scopes.pop();
            }
            StmtKind::Defer { call } => {
                // 参数在 defer 处立即求值（Go 语义），因此这里就完成借用/移动检查。
                self.use_expr(call, Mode::Consume);
            }
            StmtKind::Return(value) => {
                if let Some(expr) = value {
                    self.use_expr(expr, Mode::Consume);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Match { subject, arms } => {
                self.use_expr(subject, Mode::Consume);
                let subject_ty = self.table.resolve(&self.ty_of(subject));
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    match (&arm.pattern, &subject_ty) {
                        (Pattern::Some(name), Type::Option(inner)) => {
                            self.declare(name, (**inner).clone(), false, arm.line);
                        }
                        (Pattern::Ok(name), Type::Result(ok, _)) => {
                            self.declare(name, (**ok).clone(), false, arm.line);
                        }
                        (Pattern::Err(name), Type::Result(_, err)) => {
                            self.declare(name, (**err).clone(), false, arm.line);
                        }
                        (Pattern::Ary(fields), Type::Ary(ary_fields)) => {
                            for field in fields {
                                if field.name == "_" {
                                    continue;
                                }
                                if let Some((_, ty)) =
                                    ary_fields.iter().find(|(n, _)| n == &field.name)
                                {
                                    self.declare(&field.name, ty.clone(), false, arm.line);
                                }
                            }
                        }
                        (
                            Pattern::Enum {
                                name: enum_name,
                                variant,
                                fields,
                            },
                            Type::Named(actual),
                        ) => {
                            if enum_name == actual {
                                if let Some(info) = self.enums.get(actual) {
                                    if let Some(variant_info) =
                                        info.variants.iter().find(|item| &item.name == variant)
                                    {
                                        for field in fields {
                                            if field.name == "_" {
                                                continue;
                                            }
                                            if let Some((_, ty)) = variant_info
                                                .fields
                                                .iter()
                                                .find(|(n, _)| n == &field.name)
                                            {
                                                self.declare(
                                                    &field.name,
                                                    ty.clone(),
                                                    false,
                                                    arm.line,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    self.check_stmts(&arm.body);
                    self.scopes.pop();
                }
            }
            StmtKind::Parallel { branches } => {
                // 分支在各自线程重建运行时，块外局部变量不可见：只保留模块级作用域。
                for branch in branches {
                    let locals = self.scopes.split_off(1);
                    self.scopes.push(HashMap::new());
                    self.check_stmt(branch);
                    self.scopes.pop();
                    self.scopes.extend(locals);
                }
            }
        }
    }

    /// 用户方法 / 接口方法的接收者是否为 `write`（需要可变借用）。
    fn is_write_method(&self, ty: &Type, name: &str) -> bool {
        let Type::Named(owner) = ty else {
            return false;
        };
        let full = format!("{}.{}", owner, name);
        if let Some(sig) = self.funcs.get(&full) {
            return sig
                .params
                .first()
                .map(|param| param.mode == ParamMode::Write)
                .unwrap_or(false);
        }
        // 类型参数按约束接口判断；普通命名类型直接查接口表。
        let iface = self
            .current_bounds
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.clone());
        self.interfaces
            .get(&iface)
            .and_then(|methods| methods.iter().find(|method| method.name == name))
            .map(|method| method.mode == ParamMode::Write)
            .unwrap_or(false)
    }

    fn use_expr(&mut self, expr: &Expr, mode: Mode) {
        match &expr.kind {
            ExprKind::Ident(name) => {
                let ty = self.ty_of(expr);
                let is_moved = self.is_moved(name);
                if is_moved {
                    self.error(
                        format!("use of moved value '{}'", name),
                        expr.line,
                    );
                    return;
                }
                if mode == Mode::Consume && self.is_move_type(&ty) {
                    self.set_moved(name, true);
                } else if mode == Mode::Write {
                    self.require_mutable(name, expr.line);
                }
            }
            ExprKind::Field { base, .. } => {
                // `枚举.变体` 的 base 是类型名而不是变量：只做模块成员 / 枚举构造处理。
                if let ExprKind::Ident(name) = &base.kind {
                    if self.lookup(name).is_none() {
                        return;
                    }
                }
                self.use_expr(base, Mode::Read);
                if mode == Mode::Consume {
                    let ty = self.ty_of(expr);
                    if self.is_move_type(&ty) {
                        self.error(
                            "cannot move a field out of a composite value; use .clone()",
                            expr.line,
                        );
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.use_callee(callee, args, expr.line);
            }
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                let receiver_ty = self.table.resolve(&self.ty_of(receiver));
                let mutating = self.is_write_method(&self.ty_of(receiver), name)
                    || is_mutating_method(&receiver_ty, name);
                if mutating {
                    if let Some(root) = root_ident(receiver) {
                        self.require_mutable(&root, receiver.line);
                        if self.is_moved(&root) {
                            self.error(
                                format!("use of moved value '{}'", root),
                                receiver.line,
                            );
                        }
                    } else {
                        self.use_expr(receiver, Mode::Read);
                    }
                } else {
                    self.use_expr(receiver, Mode::Read);
                }
                for arg in args {
                    self.use_expr(arg, Mode::Read);
                }
            }
            ExprKind::Unary { expr: inner, .. } => {
                self.use_expr(inner, Mode::Read);
            }
            ExprKind::Binary { left, right, .. } => {
                self.use_expr(left, Mode::Read);
                self.use_expr(right, Mode::Read);
            }
            ExprKind::List(items) => {
                for item in items {
                    match item {
                        ListItem::Value(e)
                        | ListItem::Named(_, e)
                        | ListItem::Key(_, e)
                        | ListItem::Typed(_, e) => self.use_expr(e, Mode::Read),
                    }
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, value) in fields {
                    self.use_expr(value, Mode::Read);
                }
            }
            ExprKind::Some(inner) | ExprKind::Ok(inner) | ExprKind::Err(inner) => {
                self.use_expr(inner, Mode::Consume);
            }
            ExprKind::None => {}
            ExprKind::Closure { params, body, .. } => {
                self.scopes.push(HashMap::new());
                for p in params {
                    self.declare(
                        &p.name,
                        rp_types::type_from_expr(&p.ty),
                        p.mode == ParamMode::Write,
                        p.line,
                    );
                }
                self.check_stmts(body);
                self.scopes.pop();
            }
            ExprKind::Try(inner) => {
                self.use_expr(inner, Mode::Read);
            }
            ExprKind::Await(inner) => {
                self.use_expr(inner, Mode::Consume);
            }
            ExprKind::Int { .. }
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Char(_)
            | ExprKind::Bool(_)
            | ExprKind::Null => {}
        }
    }

    fn use_callee(&mut self, callee: &Expr, args: &[Expr], line: u32) {
        if let ExprKind::Ident(name) = &callee.kind {
            if name == "print" {
                for arg in args {
                    self.use_expr(arg, Mode::Read);
                }
                return;
            }
            if let Some(sig) = self.funcs.get(name) {
                let modes: Vec<Mode> = sig
                    .params
                    .iter()
                    .map(|p| match p.mode {
                        ParamMode::Value => Mode::Consume,
                        ParamMode::Read => Mode::Read,
                        ParamMode::Write => Mode::Write,
                    })
                    .collect();
                self.check_args(args, &modes, line);
                return;
            }
        }
        if let ExprKind::Field { base, .. } = &callee.kind {
            if matches!(&base.kind, ExprKind::Ident(_)) {
                // 标准库 / Rust 包函数按只读借用处理，避免误移动用户变量。
                for arg in args {
                    self.use_expr(arg, Mode::Read);
                }
                return;
            }
        }
        self.use_expr(callee, Mode::Read);
        for arg in args {
            self.use_expr(arg, Mode::Read);
        }
    }

    fn check_args(&mut self, args: &[Expr], modes: &[Mode], line: u32) {
        for (i, arg) in args.iter().enumerate() {
            let mode = modes.get(i).copied().unwrap_or(Mode::Read);
            match mode {
                Mode::Read => self.use_expr(arg, Mode::Read),
                Mode::Consume => self.use_expr(arg, Mode::Consume),
                Mode::Write => {
                    match &arg.kind {
                        ExprKind::Ident(name) => {
                            self.require_mutable(name, arg.line);
                            if self.is_moved(name) {
                                self.error(
                                    format!("use of moved value '{}'", name),
                                    arg.line,
                                );
                            }
                        }
                        ExprKind::Field { .. } => {
                            if let Some(root) = root_ident(arg) {
                                self.require_mutable(&root, arg.line);
                            }
                            self.use_expr(arg, Mode::Read);
                        }
                        _ => {
                            self.error(
                                "write argument requires a mutable variable",
                                line,
                            );
                            self.use_expr(arg, Mode::Read);
                        }
                    }
                }
            }
        }
    }

    fn elem_type(&self, iter: &Expr) -> Type {
        match self.table.resolve(&self.ty_of(iter)) {
            Type::Vec(t) => *t,
            Type::Map(k, _) => *k,
            _ => Type::Unknown,
        }
    }

    fn ty_of(&self, expr: &Expr) -> Type {
        self.expr_types
            .get(&expr.id)
            .cloned()
            .unwrap_or(Type::Unknown)
    }

    fn is_move_type(&self, ty: &Type) -> bool {
        match self.table.resolve(ty) {
            Type::Unknown => false,
            other => !self.table.is_copy(&other),
        }
    }

    fn declare(&mut self, name: &str, _ty: Type, mutable: bool, _line: u32) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                VarState {
                    mutable,
                    moved: false,
                },
            );
        }
    }

    fn lookup(&self, name: &str) -> Option<&VarState> {
        for scope in self.scopes.iter().rev() {
            if let Some(state) = scope.get(name) {
                return Some(state);
            }
        }
        None
    }

    fn is_moved(&self, name: &str) -> bool {
        self.lookup(name).map(|s| s.moved).unwrap_or(false)
    }

    fn set_moved(&mut self, name: &str, moved: bool) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(state) = scope.get_mut(name) {
                state.moved = moved;
                return;
            }
        }
    }

    fn require_mutable(&mut self, name: &str, line: u32) {
        match self.lookup(name) {
            Some(state) if state.mutable => {}
            Some(_) => self.error(
                format!("cannot mutate immutable variable '{}'", name),
                line,
            ),
            None => {}
        }
    }

    fn error(&mut self, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic {
            level: Level::Error,
            message: message.into(),
            line,
        });
    }
}

fn is_mutating_method(receiver: &Type, name: &str) -> bool {
    match receiver {
        Type::Vec(_) => matches!(
            name,
            "push" | "pop" | "insert" | "remove" | "clear" | "reverse" | "sort"
        ),
        Type::Map(_, _) => matches!(name, "set" | "delete" | "merge" | "clear"),
        _ => false,
    }
}

fn root_ident(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(name.clone()),
        ExprKind::Field { base, .. } => root_ident(base),
        _ => None,
    }
}

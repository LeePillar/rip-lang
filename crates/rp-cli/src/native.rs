//! 实验性原生编译（AOT）：把「标量 int 子集」翻译成 Rust，再用 rustc -O 编译成原生可执行文件。
//!
//! 支持范围（超出即报错，不会静默生成错误代码）：
//! - 函数参数/返回值：`int` 或 `void`，函数之间直接调用（含递归）；
//! - 语句：`let`（int）、赋值、表达式语句、`if`/`else`、`for` 的
//!   `初始化; 条件; 步进` 与 `条件` 两种形式、`return`、`break`、`continue`；
//! - 表达式：十进制 int 字面量、`int` 常量、局部变量、算术/比较/逻辑运算、
//!   字符串字面量与 `+` 拼接、`int.to_string()`、`print`（单个 int/string）。
//!
//! 其它语法（结构体、vec/map、match、闭包、模块调用、可变全局变量等）不在本子集内。

use std::collections::HashMap;
use std::fmt::Write as _;

use rp_ast::*;
use rp_types::{CheckResult, Type};

pub(crate) fn generate(program: &Program, checked: &CheckResult) -> Result<String, String> {
    let mut generator = Generator::new(program, checked);
    generator.run()
}

struct Generator<'a> {
    program: &'a Program,
    checked: &'a CheckResult,
    consts: HashMap<String, i64>,
    functions: Vec<(&'a FuncDecl, String)>,
    main: Option<&'a FuncDecl>,
    scopes: Vec<HashMap<String, String>>,
    /// 循环栈：`Some(step)` 表示 C 风格 for（continue 需要先执行步进）。
    loops: Vec<Option<String>>,
    counter: usize,
    indent: usize,
    body: String,
}

impl<'a> Generator<'a> {
    fn new(program: &'a Program, checked: &'a CheckResult) -> Self {
        Self {
            program,
            checked,
            consts: HashMap::new(),
            functions: Vec::new(),
            main: None,
            scopes: Vec::new(),
            loops: Vec::new(),
            counter: 0,
            indent: 0,
            body: String::new(),
        }
    }

    fn run(&mut self) -> Result<String, String> {
        for item in &self.program.items {
            match item {
                Item::Const(decl) => {
                    let value = match &decl.init.kind {
                        ExprKind::Int {
                            value,
                            suffix: None,
                            ..
                        } => i64::try_from(*value).map_err(|_| {
                            format!(
                                "native build only supports 64-bit signed int constants ('{}')",
                                decl.name
                            )
                        })?,
                        _ => {
                            return Err(format!(
                                "native build does not support non-literal constant '{}'",
                                decl.name
                            ))
                        }
                    };
                    if !is_int_type(&decl.ty) {
                        return Err(format!(
                            "native build only supports int constants ('{}')",
                            decl.name
                        ));
                    }
                    self.consts.insert(decl.name.clone(), value);
                }
                Item::Static(decl) => {
                    return Err(format!(
                        "native build does not support static '{}' (mutable globals)",
                        decl.name
                    ))
                }
                Item::Func(decl) => {
                    if decl.name == "main" {
                        self.main = Some(decl);
                    } else {
                        let rust_name = format!("rip_fn_{}", self.functions.len());
                        self.functions.push((decl, rust_name));
                    }
                }
                _ => {}
            }
        }
        let main = match self.main {
            Some(main) => main,
            None => return Err("native build requires func main() -> void".to_string()),
        };

        let mut definitions = String::new();
        let functions = self.functions.clone();
        for (decl, rust_name) in &functions {
            definitions.push_str(&self.emit_function(decl, rust_name)?);
        }
        definitions.push_str(&self.emit_main(main)?);

        let mut out = String::new();
        out.push_str(RUST_RUNTIME);
        out.push('\n');
        out.push_str(&definitions);
        Ok(out)
    }

    fn emit_function(&mut self, decl: &FuncDecl, rust_name: &str) -> Result<String, String> {
        self.scopes.clear();
        self.loops.clear();
        self.body.clear();
        self.indent = 1;
        let mut scope = HashMap::new();
        let mut params = Vec::new();
        for (index, param) in decl.params.iter().enumerate() {
            if !is_int_type(&param.ty) {
                return Err(format!(
                    "native build only supports int parameters ('{}' in '{}')",
                    param.name, decl.name
                ));
            }
            let rust_param = format!("a{}", index);
            scope.insert(param.name.clone(), rust_param.clone());
            params.push(format!("{}: i64", rust_param));
        }
        self.scopes.push(scope);
        for stmt in &decl.body {
            self.stmt(stmt)?;
        }
        let signature = if is_void_type(&decl.ret) {
            format!("fn {}({}) {{", rust_name, params.join(", "))
        } else if is_int_type(&decl.ret) {
            format!("fn {}({}) -> i64 {{", rust_name, params.join(", "))
        } else {
            return Err(format!(
                "native build only supports int/void return ('{}')",
                decl.name
            ));
        };
        let mut text = String::new();
        let _ = writeln!(text, "{}", signature);
        text.push_str(&self.body);
        text.push_str("}\n\n");
        Ok(text)
    }

    fn emit_main(&mut self, decl: &FuncDecl) -> Result<String, String> {
        if !decl.params.is_empty() || !is_void_type(&decl.ret) {
            return Err("native build requires func main() -> void".to_string());
        }
        self.scopes.clear();
        self.loops.clear();
        self.body.clear();
        self.indent = 1;
        self.scopes.push(HashMap::new());
        for stmt in &decl.body {
            self.stmt(stmt)?;
        }
        let mut text = String::from("fn rip_main() {\n");
        text.push_str(&self.body);
        text.push_str("}\n\nfn main() {\n    rip_main();\n}\n");
        Ok(text)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.kind {
            StmtKind::Let { name, ty, init, .. } => {
                if !is_int_type(ty) {
                    return Err(format!(
                        "native build only supports int locals ('{}', line {})",
                        name, stmt.line
                    ));
                }
                let value = self.expr(init)?;
                let rust_name = self.new_var(name);
                self.line(&format!("let mut {}: i64 = {};", rust_name, value));
                Ok(())
            }
            StmtKind::Assign { target, value } => {
                if let Some(code) = self.assign_code(target, value)? {
                    self.line(&format!("{};", code));
                    return Ok(());
                }
                Err(format!(
                    "native build only supports assignment to local variables (line {})",
                    stmt.line
                ))
            }
            StmtKind::Expr(expr) => {
                let code = self.expr(expr)?;
                self.line(&format!("{};", code));
                Ok(())
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_code = self.expr(cond)?;
                self.line(&format!("if {} {{", cond_code));
                self.block(then_body)?;
                if let Some(body) = else_body {
                    self.line("} else {");
                    self.block(body)?;
                }
                self.line("}");
                Ok(())
            }
            StmtKind::Loop { body } => {
                self.line("loop {");
                self.indent += 1;
                self.loops.push(None);
                self.block(body)?;
                self.loops.pop();
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            StmtKind::While { cond, body } => {
                let cond_code = self.expr(cond)?;
                self.line(&format!("while {} {{", cond_code));
                self.loops.push(None);
                self.block(body)?;
                self.loops.pop();
                self.line("}");
                Ok(())
            }
            StmtKind::WhileLet { .. } => Err(format!(
                "native build does not support while let (line {})",
                stmt.line
            )),
            StmtKind::Defer { .. } => Err(format!(
                "native build does not support defer (line {})",
                stmt.line
            )),
            StmtKind::Return(value) => {
                match value {
                    Some(expr) => {
                        let code = self.expr(expr)?;
                        self.line(&format!("return {};", code));
                    }
                    None => self.line("return;"),
                }
                Ok(())
            }
            StmtKind::Break => {
                self.line("break;");
                Ok(())
            }
            StmtKind::Continue => {
                if let Some(Some(step)) = self.loops.last().cloned() {
                    self.line(&format!("{};", step));
                }
                self.line("continue;");
                Ok(())
            }
            StmtKind::ForIn { .. } => {
                Err(format!("native build does not support for-in (line {})", stmt.line))
            }
            StmtKind::Match { .. } => Err(format!(
                "native build does not support match (line {})",
                stmt.line
            )),
            StmtKind::Parallel { .. } => Err(format!(
                "native build does not support parallel (line {})",
                stmt.line
            )),
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        self.indent += 1;
        self.scopes.push(HashMap::new());
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        self.scopes.pop();
        self.indent -= 1;
        Ok(())
    }

    /// 赋值表达式（用于语句与 for 步进）；不支持的目标返回 None。
    fn assign_code(&mut self, target: &Expr, value: &Expr) -> Result<Option<String>, String> {
        let name = match &target.kind {
            ExprKind::Ident(name) => name,
            _ => return Ok(None),
        };
        let var = match self.lookup(name) {
            Some(var) => var,
            None => return Ok(None),
        };
        let value = self.expr(value)?;
        Ok(Some(format!("{} = {}", var, value)))
    }

    fn expr(&mut self, expr: &Expr) -> Result<String, String> {
        match &expr.kind {
            ExprKind::Int {
                value,
                suffix: None,
                ..
            } => Ok(format!("{}i64", value)),
            ExprKind::Int { .. } => Err(format!(
                "native build only supports plain int literals (line {})",
                expr.line
            )),
            ExprKind::Bool(value) => Ok(if *value { "true".to_string() } else { "false".to_string() }),
            ExprKind::Str(value) => Ok(format!("{:?}", value)),
            ExprKind::Ident(name) => {
                if let Some(var) = self.lookup(name) {
                    return Ok(var);
                }
                if let Some(value) = self.consts.get(name) {
                    return Ok(format!("{}i64", value));
                }
                Err(format!(
                    "native build cannot resolve '{}' (line {}); globals are not supported",
                    name, expr.line
                ))
            }
            ExprKind::Unary { op, expr: inner } => {
                let inner = self.expr(inner)?;
                Ok(match op {
                    UnOp::Neg => format!("(-{})", inner),
                    UnOp::Not => format!("(!{})", inner),
                })
            }
            ExprKind::Binary { op, left, right } => self.binary(*op, left, right, expr.line),
            ExprKind::Call { callee, args, .. } => self.call(callee, args, expr.line),
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                if name == "to_string" && args.is_empty() && !self.is_str(receiver) {
                    let inner = self.expr(receiver)?;
                    return Ok(format!("({}).to_string()", inner));
                }
                Err(format!(
                    "native build only supports int.to_string() method calls (line {})",
                    expr.line
                ))
            }
            _ => Err(format!(
                "native build does not support this expression yet (line {})",
                expr.line
            )),
        }
    }

    fn binary(&mut self, op: BinOp, left: &Expr, right: &Expr, line: u32) -> Result<String, String> {
        let left_code = self.expr(left)?;
        let right_code = self.expr(right)?;
        let left_str = self.is_str(left);
        let right_str = self.is_str(right);
        if left_str || right_str {
            if op == BinOp::Add && left_str && right_str {
                return Ok(format!("format!(\"{{}}{{}}\", {}, {})", left_code, right_code));
            }
            return Err(format!(
                "native build only supports string '+' concatenation (line {})",
                line
            ));
        }
        let code = match op {
            BinOp::Add => format!("rip_add({}, {})", left_code, right_code),
            BinOp::Sub => format!("rip_sub({}, {})", left_code, right_code),
            BinOp::Mul => format!("rip_mul({}, {})", left_code, right_code),
            BinOp::Div => format!("rip_div({}, {})", left_code, right_code),
            BinOp::Mod => format!("rip_mod({}, {})", left_code, right_code),
            BinOp::Eq => format!("({} == {})", left_code, right_code),
            BinOp::Ne => format!("({} != {})", left_code, right_code),
            BinOp::Lt => format!("({} < {})", left_code, right_code),
            BinOp::Gt => format!("({} > {})", left_code, right_code),
            BinOp::Le => format!("({} <= {})", left_code, right_code),
            BinOp::Ge => format!("({} >= {})", left_code, right_code),
            BinOp::And => format!("({} && {})", left_code, right_code),
            BinOp::Or => format!("({} || {})", left_code, right_code),
        };
        Ok(code)
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], line: u32) -> Result<String, String> {
        let name = match &callee.kind {
            ExprKind::Ident(name) => name,
            _ => {
                return Err(format!(
                    "native build only supports direct function calls (line {})",
                    line
                ))
            }
        };
        if name == "print" {
            if args.len() != 1 {
                return Err("native build only supports print with one argument".to_string());
            }
            let code = self.expr(&args[0])?;
            return Ok(format!("println!(\"{{}}\", {})", code));
        }
        let target = self
            .functions
            .iter()
            .find(|(decl, _)| &decl.name == name)
            .map(|(_, rust_name)| rust_name.clone());
        if let Some(rust_name) = target {
            let mut values = Vec::new();
            for arg in args {
                values.push(self.expr(arg)?);
            }
            return Ok(format!("{}({})", rust_name, values.join(", ")));
        }
        Err(format!(
            "native build cannot call '{}' (line {}); only local native functions are supported",
            name, line
        ))
    }

    fn is_str(&self, expr: &Expr) -> bool {
        self.checked
            .expr_types
            .get(&expr.id)
            .map(|ty| matches!(self.checked.type_table.resolve(ty), Type::Str))
            .unwrap_or(false)
    }

    fn lookup(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(value) = scope.get(name) {
                return Some(value.clone());
            }
        }
        None
    }

    fn new_var(&mut self, name: &str) -> String {
        self.counter += 1;
        let rust_name = format!("v{}_{}", self.counter, sanitize(name));
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), rust_name.clone());
        }
        rust_name
    }

    fn line(&mut self, code: &str) {
        for _ in 0..self.indent {
            self.body.push_str("    ");
        }
        self.body.push_str(code);
        self.body.push('\n');
    }
}

fn is_int_type(ty: &TypeExpr) -> bool {
    matches!(&ty.kind, TypeExprKind::Named(path) if path.len() == 1 && path[0] == "int")
}

fn is_void_type(ty: &TypeExpr) -> bool {
    matches!(ty.kind, TypeExprKind::Void)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

const RUST_RUNTIME: &str = r#"#[allow(dead_code)]
fn rip_fail(message: &str) -> ! {
    eprintln!("runtime error: {}", message);
    std::process::exit(1);
}

#[inline]
fn rip_add(a: i64, b: i64) -> i64 {
    match a.checked_add(b) {
        Some(value) => value,
        None => rip_fail("integer overflow"),
    }
}

#[inline]
fn rip_sub(a: i64, b: i64) -> i64 {
    match a.checked_sub(b) {
        Some(value) => value,
        None => rip_fail("integer overflow"),
    }
}

#[inline]
fn rip_mul(a: i64, b: i64) -> i64 {
    match a.checked_mul(b) {
        Some(value) => value,
        None => rip_fail("integer overflow"),
    }
}

#[inline]
fn rip_div(a: i64, b: i64) -> i64 {
    if b == 0 {
        rip_fail("division by zero");
    }
    match a.checked_div(b) {
        Some(value) => value,
        None => rip_fail("integer overflow"),
    }
}

#[inline]
fn rip_mod(a: i64, b: i64) -> i64 {
    if b == 0 {
        rip_fail("modulo by zero");
    }
    match a.checked_rem(b) {
        Some(value) => value,
        None => rip_fail("integer overflow"),
    }
}
"#;

//! Rip 类型系统与类型检查（全显式、零推断）。

pub mod builtins;
pub mod native;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use rp_ast::*;

use builtins as bi;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Unknown,
    Void,
    /// `int`，等价 `i64`（默认整数类型）。
    Int,
    I8,
    I16,
    I32,
    U8,
    U16,
    U32,
    U64,
    /// `float`，等价 `f64`（默认浮点类型）。
    Float,
    Str,
    Bool,
    Any,
    Named(String),
    Ary(Vec<(String, Type)>),
    Vec(Box<Type>),
    VecHetero(Vec<Type>),
    VecDynamic,
    Map(Box<Type>, Box<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Share(Box<Type>),
    Task(Box<Type>),
    Func(Vec<Type>, Box<Type>),
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unknown => write!(f, "unknown"),
            Type::Void => write!(f, "void"),
            Type::Int => write!(f, "int"),
            Type::I8 => write!(f, "i8"),
            Type::I16 => write!(f, "i16"),
            Type::I32 => write!(f, "i32"),
            Type::U8 => write!(f, "u8"),
            Type::U16 => write!(f, "u16"),
            Type::U32 => write!(f, "u32"),
            Type::U64 => write!(f, "u64"),
            Type::Float => write!(f, "float"),
            Type::Str => write!(f, "string"),
            Type::Bool => write!(f, "bool"),
            Type::Any => write!(f, "any"),
            Type::Named(name) => write!(f, "{}", name),
            Type::Ary(fields) => {
                write!(f, "[")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", name, ty)?;
                }
                write!(f, "]")
            }
            Type::Vec(t) => write!(f, "vec<{}>", t),
            Type::VecHetero(ts) => {
                write!(f, "vec<")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, ">")
            }
            Type::VecDynamic => write!(f, "vec"),
            Type::Map(k, v) => write!(f, "map<{}, {}>", k, v),
            Type::Option(t) => write!(f, "option<{}>", t),
            Type::Result(t, e) => write!(f, "result<{}, {}>", t, e),
            Type::Share(t) => write!(f, "share<{}>", t),
            Type::Task(t) => write!(f, "task<{}>", t),
            Type::Func(args, ret) => {
                write!(f, "func(")?;
                for (i, t) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, ") -> {}", ret)
            }
        }
    }
}

impl Type {
    /// 所有整数类型：`int`、`i8`…`i32`、`u8`…`u64`。
    pub fn is_int(&self) -> bool {
        matches!(
            self,
            Type::Int
                | Type::I8
                | Type::I16
                | Type::I32
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
        )
    }

    /// 整数类型对应的位宽；`int` = `i64`，float 与其它类型返回 `None`。
    pub fn int_suffix(&self) -> Option<IntSuffix> {
        Some(match self {
            Type::Int => IntSuffix::I64,
            Type::I8 => IntSuffix::I8,
            Type::I16 => IntSuffix::I16,
            Type::I32 => IntSuffix::I32,
            Type::U8 => IntSuffix::U8,
            Type::U16 => IntSuffix::U16,
            Type::U32 => IntSuffix::U32,
            Type::U64 => IntSuffix::U64,
            _ => return None,
        })
    }

    /// 位宽对应回类型；`i64` 归一为 `int`。
    pub fn from_int_suffix(suffix: IntSuffix) -> Type {
        match suffix {
            IntSuffix::I8 => Type::I8,
            IntSuffix::I16 => Type::I16,
            IntSuffix::I32 => Type::I32,
            IntSuffix::I64 => Type::Int,
            IntSuffix::U8 => Type::U8,
            IntSuffix::U16 => Type::U16,
            IntSuffix::U32 => Type::U32,
            IntSuffix::U64 => Type::U64,
        }
    }
}

pub fn type_from_expr(te: &TypeExpr) -> Type {
    match &te.kind {
        TypeExprKind::Void => Type::Void,
        TypeExprKind::Read(inner) => type_from_expr(inner),
        TypeExprKind::Share(inner) => Type::Share(Box::new(type_from_expr(inner))),
        TypeExprKind::Task(inner) => Type::Task(Box::new(type_from_expr(inner))),
        TypeExprKind::Func(args, ret) => Type::Func(
            args.iter().map(type_from_expr).collect(),
            Box::new(type_from_expr(ret)),
        ),
        TypeExprKind::Ary(fields) => Type::Ary(
            fields
                .iter()
                .map(|f| (f.name.clone(), type_from_expr(&f.ty)))
                .collect(),
        ),
        TypeExprKind::Vec(args) => match args.len() {
            0 => Type::VecDynamic,
            1 => Type::Vec(Box::new(type_from_expr(&args[0]))),
            _ => Type::VecHetero(args.iter().map(type_from_expr).collect()),
        },
        TypeExprKind::Map(k, v) => Type::Map(
            Box::new(type_from_expr(k)),
            Box::new(type_from_expr(v)),
        ),
        TypeExprKind::Option(t) => Type::Option(Box::new(type_from_expr(t))),
        TypeExprKind::Result(t, e) => Type::Result(
            Box::new(type_from_expr(t)),
            Box::new(type_from_expr(e)),
        ),
        TypeExprKind::Named(path) => {
            let name = path.join(".");
            match name.as_str() {
                "int" => Type::Int,
                "i8" => Type::I8,
                "i16" => Type::I16,
                "i32" => Type::I32,
                "i64" => Type::Int,
                "u8" => Type::U8,
                "u16" => Type::U16,
                "u32" => Type::U32,
                "u64" => Type::U64,
                "float" | "f64" => Type::Float,
                "string" => Type::Str,
                "bool" => Type::Bool,
                "void" => Type::Void,
                "any" => Type::Any,
                _ => Type::Named(name),
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TypeTable {
    pub defs: BTreeMap<String, Type>,
}

impl TypeTable {
    pub fn new(program: &Program) -> Self {
        let mut defs = BTreeMap::new();
        for item in &program.items {
            if let Item::Type(decl) = item {
                defs.insert(decl.name.clone(), type_from_expr(&decl.ty));
            }
        }
        Self { defs }
    }

    pub fn resolve(&self, ty: &Type) -> Type {
        let mut stack = Vec::new();
        self.resolve_depth(ty, 0, &mut stack)
    }

    /// 展开类型别名。
    ///
    /// `stack` 记录当前正在展开的别名：再次遇到同一个名字时保留 `Named` 引用，
    /// 因此递归类型（如 `type node = [next: option<node>]`）只展开一层就停住，
    /// 不会指数膨胀，也不会死循环。
    fn resolve_depth(&self, ty: &Type, depth: usize, stack: &mut Vec<String>) -> Type {
        if depth > 48 {
            return ty.clone();
        }
        match ty {
            Type::Named(name) => {
                if stack.iter().any(|active| active == name) {
                    return Type::Named(name.clone());
                }
                if let Some(def) = self.defs.get(name) {
                    stack.push(name.clone());
                    let resolved = self.resolve_depth(def, depth + 1, stack);
                    stack.pop();
                    resolved
                } else if let Some(builtin) = bi::builtin_type(name) {
                    stack.push(name.clone());
                    let resolved = self.resolve_depth(&builtin, depth + 1, stack);
                    stack.pop();
                    resolved
                } else {
                    Type::Named(name.clone())
                }
            }
            Type::Ary(fields) => Type::Ary(
                fields
                    .iter()
                    .map(|(n, t)| (n.clone(), self.resolve_depth(t, depth + 1, stack)))
                    .collect(),
            ),
            Type::Vec(t) => Type::Vec(Box::new(self.resolve_depth(t, depth + 1, stack))),
            Type::VecHetero(ts) => Type::VecHetero(
                ts.iter()
                    .map(|t| self.resolve_depth(t, depth + 1, stack))
                    .collect(),
            ),
            Type::VecDynamic => Type::VecDynamic,
            Type::Map(k, v) => Type::Map(
                Box::new(self.resolve_depth(k, depth + 1, stack)),
                Box::new(self.resolve_depth(v, depth + 1, stack)),
            ),
            Type::Option(t) => Type::Option(Box::new(self.resolve_depth(t, depth + 1, stack))),
            Type::Result(t, e) => Type::Result(
                Box::new(self.resolve_depth(t, depth + 1, stack)),
                Box::new(self.resolve_depth(e, depth + 1, stack)),
            ),
            Type::Share(t) => Type::Share(Box::new(self.resolve_depth(t, depth + 1, stack))),
            Type::Task(t) => Type::Task(Box::new(self.resolve_depth(t, depth + 1, stack))),
            Type::Func(args, ret) => Type::Func(
                args.iter()
                    .map(|t| self.resolve_depth(t, depth + 1, stack))
                    .collect(),
                Box::new(self.resolve_depth(ret, depth + 1, stack)),
            ),
            other => other.clone(),
        }
    }

    pub fn is_known_named(&self, name: &str) -> bool {
        self.defs.contains_key(name)
            || bi::builtin_type(name).is_some()
            || bi::is_opaque_named(name)
    }

    pub fn is_copy(&self, ty: &Type) -> bool {
        matches!(
            self.resolve(ty),
            Type::Int
                | Type::I8
                | Type::I16
                | Type::I32
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
                | Type::Float
                | Type::Bool
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: Level,
    pub message: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ParamSig {
    pub name: String,
    pub mode: ParamMode,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct FuncSig {
    pub params: Vec<ParamSig>,
    pub ret: Type,
    /// 泛型类型参数名；空表示普通函数。
    pub type_params: Vec<String>,
    /// 类型参数约束：`T: shape`（T → 接口名）。
    pub type_bounds: HashMap<String, String>,
    /// 声明时的参数类型表达式与返回类型，供泛型实例化使用。
    pub param_exprs: Vec<TypeExpr>,
    pub ret_expr: TypeExpr,
    /// `async func`：调用必须写成 `await f()`。
    pub is_async: bool,
}

impl FuncSig {
    pub fn func_type(&self) -> Type {
        Type::Func(
            self.params.iter().map(|p| p.ty.clone()).collect(),
            Box::new(self.ret.clone()),
        )
    }
}

/// 接口方法签名：接收者固定是接口自身。
#[derive(Debug, Clone)]
pub struct InterfaceMethodSig {
    pub name: String,
    /// 接收者模式：`read` 或 `write`。
    pub mode: ParamMode,
    pub params: Vec<Type>,
    pub ret: Type,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub diagnostics: Vec<Diagnostic>,
    pub expr_types: HashMap<NodeId, Type>,
    pub funcs: HashMap<String, FuncSig>,
    pub type_table: TypeTable,
    pub imports: HashMap<String, String>,
    /// 接口名 → 方法签名（所有权检查判断接收者是否可变）。
    pub interfaces: HashMap<String, Vec<InterfaceMethodSig>>,
    /// 枚举名 → 变体与字段类型。
    pub enums: HashMap<String, EnumInfo>,
    /// 函数全名 → 类型参数约束。
    pub type_bounds: HashMap<String, HashMap<String, String>>,
}

/// 枚举类型信息：变体列表（按声明顺序，用于穷尽匹配）。
#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub variants: Vec<EnumVariantInfo>,
}

#[derive(Debug, Clone)]
pub struct EnumVariantInfo {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

impl CheckResult {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.level == Level::Error)
    }
}

pub fn check_program(program: &Program) -> CheckResult {
    TypeChecker::new(program).run(program)
}

/// 整数转换方法（支持进制参数的那 9 个）。
fn is_int_convert(name: &str) -> bool {
    matches!(
        name,
        "to_int" | "to_i8" | "to_i16" | "to_i32" | "to_i64" | "to_u8" | "to_u16" | "to_u32" | "to_u64"
    )
}

/// 字面量表达式：转换方法不接受字面量直接作为接收者。
fn is_literal_expr(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Int { .. } | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Char(_)
    )
}

/// map 字面量的键必须是可比较的标量（运行时按键值线性查找）。
fn is_map_key_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str
            | Type::Int
            | Type::I8
            | Type::I16
            | Type::I32
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Float
            | Type::Bool
            | Type::Any
    )
}

/// 沿「纯别名」边（`type a = b`）行走，遇到重复名字说明存在无法解析的环。
fn alias_cycle_from(defs: &BTreeMap<String, Type>, start: &str) -> bool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current = start.to_string();
    loop {
        if !seen.insert(current.clone()) {
            return true;
        }
        match defs.get(&current) {
            Some(Type::Named(next)) => current = next.clone(),
            _ => return false,
        }
    }
}

fn route_method(attribute: &str) -> Option<&'static str> {
    match attribute {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        "options" => Some("OPTIONS"),
        "head" => Some("HEAD"),
        _ => None,
    }
}

struct VarInfo {
    ty: Type,
    mutable: bool,
    /// 声明行号，用于「未使用变量」告警。
    line: u32,
    /// 是否已被读取（仅 let 局部变量参与检查）。
    used: bool,
    /// 只在 `let` 声明上检查未使用；参数、循环变量、match 绑定不检查。
    check_unused: bool,
}

struct ReturnCtx {
    ret: Type,
    result_err: Option<Type>,
}

struct TypeChecker {
    table: TypeTable,
    imports: HashMap<String, String>,
    funcs: HashMap<String, FuncSig>,
    diagnostics: Vec<Diagnostic>,
    expr_types: HashMap<NodeId, Type>,
    scopes: Vec<HashMap<String, VarInfo>>,
    returns: Vec<ReturnCtx>,
    loop_depth: usize,
    /// 当前所在 `parallel` 分支的嵌套层数：分支内禁止 `return`。
    parallel_depth: usize,
    routes: HashMap<String, u32>,
    private_funcs: HashMap<String, String>,
    current_package: String,
    default_package: String,
    /// 已使用过的 import 别名与函数名，用于「未使用」告警。
    used_imports: HashSet<String>,
    used_funcs: HashSet<String>,
    import_lines: HashMap<String, u32>,
    func_lines: HashMap<String, u32>,
    /// 带注解（路由/测试/定时）或由运行时调用的函数，不算「未使用的私有函数」。
    entry_funcs: HashSet<String>,
    /// 正在检查的函数声明的类型参数；仅用于放行 `T` 这类泛型名字。
    current_type_params: Vec<String>,
    /// 当前函数的类型参数约束：`T → 接口名`。
    current_type_bounds: HashMap<String, String>,
    /// 函数全名 → 类型参数约束（所有权检查据此判断接口方法的接收者是否可变）。
    func_bounds: HashMap<String, HashMap<String, String>>,
    /// 接口名 → 方法签名。
    interfaces: HashMap<String, Vec<InterfaceMethodSig>>,
    /// 枚举名 → 变体与字段类型。
    enums: HashMap<String, EnumInfo>,
    /// 具体类型名 → (方法名 → 完整函数名 `类型.方法`)。
    methods: HashMap<String, HashMap<String, String>>,
}

impl TypeChecker {
    fn new(program: &Program) -> Self {
        Self {
            table: TypeTable::new(program),
            imports: HashMap::new(),
            funcs: HashMap::new(),
            diagnostics: Vec::new(),
            expr_types: HashMap::new(),
            scopes: Vec::new(),
            returns: Vec::new(),
            loop_depth: 0,
            parallel_depth: 0,
            routes: HashMap::new(),
            private_funcs: HashMap::new(),
            current_package: program.package.clone(),
            default_package: program.package.clone(),
            used_imports: HashSet::new(),
            used_funcs: HashSet::new(),
            import_lines: HashMap::new(),
            func_lines: HashMap::new(),
            entry_funcs: HashSet::new(),
            current_type_params: Vec::new(),
            current_type_bounds: HashMap::new(),
            func_bounds: HashMap::new(),
            interfaces: HashMap::new(),
            enums: HashMap::new(),
            methods: HashMap::new(),
        }
    }

    fn package_of(&self, func: &FuncDecl) -> String {
        if func.package.is_empty() {
            self.default_package.clone()
        } else {
            func.package.clone()
        }
    }

    // `self func` 只能在同一个包内调用；跨包调用（含作为函数值引用）直接报错。
    fn check_private_access(&mut self, name: &str, line: u32) {
        if let Some(owner) = self.private_funcs.get(name).cloned() {
            if owner != self.current_package {
                self.error(
                    format!(
                        "function '{}' is package-private to '{}' and cannot be used from package '{}'",
                        name, owner, self.current_package
                    ),
                    line,
                );
            }
        }
    }

    fn run(mut self, program: &Program) -> CheckResult {
        self.collect_imports(program);
        self.collect_enums(program);
        self.collect_interfaces(program);
        self.check_type_decls(program);
        self.collect_funcs(program);

        self.scopes.push(HashMap::new());
        for item in &program.items {
            match item {
                Item::Const(decl) | Item::Static(decl) => {
                    let ty = type_from_expr(&decl.ty);
                    self.validate_type(&ty, decl.line);
                    self.check_expr(&decl.init, Some(&ty));
                    self.warn_name(&decl.name, NameKind::Const, decl.line);
                    self.declare(&decl.name, ty, false, decl.line);
                }
                _ => {}
            }
        }

        for item in &program.items {
            if let Item::Func(func) = item {
                self.check_func(func);
            }
        }

        // 未使用的 import 与包内私有函数：只告警，不影响编译。
        // 第三方包在编译期已与项目源码合并，调用走全局函数名而不是别名，
        // 因此不参与“未使用”判定（否则每个第三方包 import 都会误报）。
        let mut unused_imports: Vec<(u32, String, String)> = self
            .imports
            .iter()
            .filter(|(alias, path)| {
                !self.used_imports.contains(*alias)
                    && (path.starts_with("std/") || path.starts_with("rust/"))
            })
            .map(|(alias, path)| {
                (
                    self.import_lines.get(alias).copied().unwrap_or(0),
                    alias.clone(),
                    path.clone(),
                )
            })
            .collect();
        unused_imports.sort_by(|a, b| a.0.cmp(&b.0));
        for (line, alias, path) in unused_imports {
            self.warning(format!("unused import \"{}\" ({})", path, alias), line);
        }
        let mut unused_funcs: Vec<(u32, String)> = self
            .private_funcs
            .keys()
            .filter(|name| !self.used_funcs.contains(*name) && !self.entry_funcs.contains(*name))
            .map(|name| (self.func_lines.get(name).copied().unwrap_or(0), name.clone()))
            .collect();
        unused_funcs.sort_by(|a, b| a.0.cmp(&b.0));
        for (line, name) in unused_funcs {
            self.warning(format!("unused private function '{}'", name), line);
        }

        let mut result = CheckResult {
            diagnostics: self.diagnostics,
            expr_types: self.expr_types,
            funcs: self.funcs,
            type_table: self.table,
            imports: self.imports,
            interfaces: self.interfaces,
            enums: self.enums,
            type_bounds: self.func_bounds,
        };
        result
            .diagnostics
            .sort_by(|a, b| a.line.cmp(&b.line).then(a.message.cmp(&b.message)));
        result
    }

    fn collect_imports(&mut self, program: &Program) {
        self.warn_name(
            &program.package,
            NameKind::Package,
            program.package_line,
        );
        for item in &program.items {
            if let Item::Import(import) = item {
                let alias = import.alias.clone().unwrap_or_else(|| {
                    import
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&import.path)
                        .to_string()
                });
                if let Some(previous) = self.imports.get(&alias) {
                    if previous != &import.path {
                        self.error(
                            format!(
                                "import alias '{}' already refers to \"{}\"",
                                alias, previous
                            ),
                            import.line,
                        );
                    }
                }
                self.imports.insert(alias.clone(), import.path.clone());
                self.import_lines.insert(alias, import.line);
            }
        }
    }

    /// 收集接口声明。接口不是 `TypeTable` 里的结构类型，单独保存方法表。
    fn collect_interfaces(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Interface(decl) = item {
                self.warn_name(&decl.name, NameKind::Type, decl.line);
                if self.table.defs.contains_key(&decl.name)
                    || self.interfaces.contains_key(&decl.name)
                    || self.enums.contains_key(&decl.name)
                {
                    self.error(
                        format!("type '{}' is already defined", decl.name),
                        decl.line,
                    );
                    continue;
                }
                let mut methods: Vec<InterfaceMethodSig> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                for method in &decl.methods {
                    if !seen.insert(method.name.clone()) {
                        self.error(
                            format!(
                                "interface '{}' declares '{}' more than once",
                                decl.name, method.name
                            ),
                            method.line,
                        );
                        continue;
                    }
                    self.warn_name(&method.name, NameKind::Function, method.line);
                    let mut params = Vec::new();
                    for param in &method.params {
                        self.warn_name(&param.name, NameKind::Variable, param.line);
                        let ty = type_from_expr(&param.ty);
                        self.validate_type(&ty, param.line);
                        params.push(ty);
                    }
                    let ret = type_from_expr(&method.ret);
                    self.validate_type(&ret, method.line);
                    methods.push(InterfaceMethodSig {
                        name: method.name.clone(),
                        mode: method.mode,
                        params,
                        ret,
                    });
                }
                self.interfaces.insert(decl.name.clone(), methods);
            }
        }
    }

    /// 收集枚举声明。先登记全部名字，再校验字段类型（字段可以引用后面的枚举）。
    fn collect_enums(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Enum(decl) = item {
                self.warn_name(&decl.name, NameKind::Type, decl.line);
                if self.table.defs.contains_key(&decl.name)
                    || self.interfaces.contains_key(&decl.name)
                    || self.enums.contains_key(&decl.name)
                {
                    self.error(
                        format!("type '{}' is already defined", decl.name),
                        decl.line,
                    );
                    continue;
                }
                if decl.variants.is_empty() {
                    self.error(
                        format!("enum '{}' must declare at least one variant", decl.name),
                        decl.line,
                    );
                    continue;
                }
                let mut variants: Vec<EnumVariantInfo> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                for variant in &decl.variants {
                    self.warn_name(&variant.name, NameKind::Variable, variant.line);
                    if !seen.insert(variant.name.clone()) {
                        self.error(
                            format!(
                                "enum '{}' declares variant '{}' more than once",
                                decl.name, variant.name
                            ),
                            variant.line,
                        );
                        continue;
                    }
                    let mut fields: Vec<(String, Type)> = Vec::new();
                    let mut field_seen: HashSet<String> = HashSet::new();
                    for field in &variant.fields {
                        if !field_seen.insert(field.name.clone()) {
                            self.error(
                                format!(
                                    "variant '{}.{}' declares field '{}' more than once",
                                    decl.name, variant.name, field.name
                                ),
                                variant.line,
                            );
                            continue;
                        }
                        fields.push((field.name.clone(), type_from_expr(&field.ty)));
                    }
                    variants.push(EnumVariantInfo {
                        name: variant.name.clone(),
                        fields,
                    });
                }
                self.enums
                    .insert(decl.name.clone(), EnumInfo { variants });
            }
        }
        // 第二遍：此时所有枚举名都已就位，字段里的枚举引用可以正常校验。
        for item in &program.items {
            if let Item::Enum(decl) = item {
                if !self.enums.contains_key(&decl.name) {
                    continue;
                }
                for variant in &decl.variants {
                    for field in &variant.fields {
                        self.validate_type(&type_from_expr(&field.ty), field.ty.line);
                    }
                }
            }
        }
    }

    fn check_type_decls(&mut self, program: &Program) {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in &program.items {
            if let Item::Type(decl) = item {
                self.warn_name(&decl.name, NameKind::Type, decl.line);
                if !seen.insert(decl.name.clone())
                    || self.interfaces.contains_key(&decl.name)
                    || self.enums.contains_key(&decl.name)
                {
                    self.error(
                        format!("type '{}' is already defined", decl.name),
                        decl.line,
                    );
                    continue;
                }
                // 纯别名环（`type a = b; type b = a`）没有可用的结构，直接报错；
                // 经过容器的递归（`[next: option<node>]`）是合法的，不在此列。
                if alias_cycle_from(&self.table.defs, &decl.name) {
                    self.error(
                        format!("type '{}' is a recursive alias and never resolves", decl.name),
                        decl.line,
                    );
                    continue;
                }
                let ty = type_from_expr(&decl.ty);
                self.validate_type(&ty, decl.line);
            }
        }
    }

    fn collect_funcs(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::Func(func) = item {
                match &func.owner {
                    Some(_) => {
                        let method = func.name.rsplit('.').next().unwrap_or(&func.name);
                        self.warn_name(method, NameKind::Function, func.line);
                    }
                    None => self.warn_name(&func.name, NameKind::Function, func.line),
                }
                if self.funcs.contains_key(&func.name) {
                    self.error(
                        format!("function '{}' is already defined", func.name),
                        func.line,
                    );
                    continue;
                }
                let mut params = Vec::new();
                self.current_type_params = func.type_params.clone();
                for p in &func.params {
                    self.warn_name(&p.name, NameKind::Variable, p.line);
                    let ty = type_from_expr(&p.ty);
                    self.validate_type(&ty, p.line);
                    if p.mode != ParamMode::Value && self.table.is_copy(&ty) {
                        self.warning(
                            format!(
                                "read/write has no effect on Copy type parameter '{}'",
                                p.name
                            ),
                            p.line,
                        );
                    }
                    params.push(ParamSig {
                        name: p.name.clone(),
                        mode: p.mode,
                        ty,
                    });
                }
                let ret = type_from_expr(&func.ret);
                self.validate_type(&ret, func.line);
                self.current_type_params.clear();
                for (param, bound) in &func.type_bounds {
                    if !func.type_params.iter().any(|name| name == param) {
                        self.error(
                            format!("constraint refers to unknown type parameter '{}'", param),
                            func.line,
                        );
                    }
                    if !self.interfaces.contains_key(bound) {
                        self.error(
                            format!("constraint '{}' must be an interface", bound),
                            func.line,
                        );
                    }
                }
                self.func_bounds
                    .insert(func.name.clone(), func.type_bounds.clone());
                if func.is_async {
                    if func.owner.is_some() {
                        self.error("async is not allowed on methods", func.line);
                    }
                    if !func.attributes.is_empty() {
                        self.error(
                            "async functions cannot have attributes; use a sync wrapper",
                            func.line,
                        );
                    }
                    if func.name == "main" {
                        self.error("main cannot be async", func.line);
                    }
                }
                if let Some(owner) = func.owner.clone() {
                    self.check_method(func, &owner, &params, func.line);
                } else {
                    self.check_route_attributes(func, &params, &ret);
                }
                if func.self_only && func.owner.is_none() {
                    self.private_funcs
                        .insert(func.name.clone(), self.package_of(func));
                }
                // 带注解的函数由运行时调用（路由、测试、定时），不算未使用。
                if !func.attributes.is_empty() || func.name == "main" {
                    self.entry_funcs.insert(func.name.clone());
                }
                self.func_lines.insert(func.name.clone(), func.line);
                let param_exprs: Vec<TypeExpr> =
                    func.params.iter().map(|p| p.ty.clone()).collect();
                self.funcs.insert(
                    func.name.clone(),
                    FuncSig {
                        params,
                        ret,
                        type_params: func.type_params.clone(),
                        type_bounds: func.type_bounds.clone(),
                        param_exprs,
                        ret_expr: func.ret.clone(),
                        is_async: func.is_async,
                    },
                );
            }
        }
    }

    /// 方法声明校验：owner 必须是具体类型；第一个参数是接收者，只能是 `read` / `write`。
    fn check_method(&mut self, func: &FuncDecl, owner: &str, params: &[ParamSig], line: u32) {
        if !func.attributes.is_empty() {
            self.error("methods cannot have attributes", func.line);
        }
        if func.self_only {
            self.error("`self` is not allowed on methods", func.line);
        }
        if self.interfaces.contains_key(owner) {
            self.error(
                format!("cannot define methods on interface '{}'", owner),
                func.line,
            );
            return;
        }
        if !func.type_params.is_empty() {
            self.error("methods cannot be generic", func.line);
        }
        if !self.table.defs.contains_key(owner) && !self.enums.contains_key(owner) {
            self.error(
                format!("unknown method owner type '{}'", owner),
                func.line,
            );
            return;
        }
        let Some(receiver) = params.first() else {
            self.error(
                "method needs a receiver as its first parameter",
                func.line,
            );
            return;
        };
        if receiver.mode == ParamMode::Value {
            self.error(
                format!(
                    "method receiver '{}' must be `read` or `write`",
                    receiver.name
                ),
                func.params[0].line,
            );
        }
        if !self.same_type(&receiver.ty, &Type::Named(owner.to_string())) {
            self.error(
                format!("method receiver must be '{}', found {}", owner, receiver.ty),
                func.params[0].line,
            );
        }
        let method = func
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&func.name)
            .to_string();
        let entry = self.methods.entry(owner.to_string()).or_default();
        if entry.insert(method.clone(), func.name.clone()).is_some() {
            self.error(
                format!("method '{}.{}' is already defined", owner, method),
                line,
            );
        }
    }

    // 路由注解：只认标准库方法名，路径必须以 / 开头，处理器签名固定为
    // func(http.request) -> http.response，同程序内 (方法, 路径) 不得重复。
    fn check_route_attributes(&mut self, func: &FuncDecl, params: &[ParamSig], ret: &Type) {
        for attribute in &func.attributes {
            if attribute.name == "test" {
                if !attribute.args.is_empty() {
                    self.error("#[test] takes no arguments", attribute.line);
                }
                if !params.is_empty() {
                    self.error("#[test] function must take no parameters", attribute.line);
                }
                if !matches!(ret, Type::Void) {
                    self.error("#[test] function must return void", attribute.line);
                }
                continue;
            }
            if attribute.name == "bench" {
                if !attribute.args.is_empty() {
                    self.error("#[bench] takes no arguments", attribute.line);
                }
                if !params.is_empty() {
                    self.error("#[bench] function must take no parameters", attribute.line);
                }
                if !matches!(ret, Type::Void) {
                    self.error("#[bench] function must return void", attribute.line);
                }
                continue;
            }
            if attribute.name == "interval" {
                self.check_interval_attribute(func, attribute);
                continue;
            }
            let method = match route_method(&attribute.name) {
                Some(method) => method,
                None => {
                    self.error(
                        format!("unknown attribute '#[{}]'", attribute.name),
                        attribute.line,
                    );
                    continue;
                }
            };
            if attribute.args.len() != 1 {
                self.error(
                    format!("#[{}] requires exactly one path argument", attribute.name),
                    attribute.line,
                );
                continue;
            }
            let path = &attribute.args[0];
            if !path.starts_with('/') {
                self.error(
                    format!("#[{}] path must start with '/'", attribute.name),
                    attribute.line,
                );
            }
            if func.params.len() != 1 || func.params[0].mode == ParamMode::Write {
                self.error(
                    format!(
                        "route handler '{}' must take exactly one http.request parameter",
                        func.name
                    ),
                    func.line,
                );
            } else if params[0].ty != bi::http_request() {
                self.error(
                    format!(
                        "route handler '{}' parameter must be http.request",
                        func.name
                    ),
                    func.params[0].line,
                );
            }
            if ret != &bi::http_response() {
                self.error(
                    format!("route handler '{}' must return http.response", func.name),
                    func.line,
                );
            }
            let key = format!("{} {}", method, path);
            match self.routes.get(&key) {
                Some(line) => self.error(
                    format!(
                        "duplicate route '{}' (first declared at line {})",
                        key, line
                    ),
                    attribute.line,
                ),
                None => {
                    self.routes.insert(key, attribute.line);
                }
            }
        }
    }

    /// 定时任务注解：`#[interval("30s")]`，函数必须无参，间隔为正整数加 ms/s/m/h/d 单位。
    fn check_interval_attribute(&mut self, func: &FuncDecl, attribute: &Attribute) {
        if attribute.args.len() != 1 {
            self.error(
                "#[interval] requires exactly one interval argument".to_string(),
                attribute.line,
            );
            return;
        }
        if !func.params.is_empty() {
            self.error(
                format!(
                    "scheduled task '{}' must take no parameters",
                    func.name
                ),
                func.line,
            );
        }
        let spec = &attribute.args[0];
        let amount = spec
            .trim()
            .trim_end_matches("ms")
            .trim_end_matches(['s', 'm', 'h', 'd']);
        let valid = (spec.ends_with("ms")
            || spec.ends_with('s')
            || spec.ends_with('m')
            || spec.ends_with('h')
            || spec.ends_with('d'))
            && amount.parse::<u64>().map(|value| value > 0).unwrap_or(false);
        if !valid {
            self.error(
                format!(
                    "invalid interval '{}' (expected e.g. \"500ms\", \"30s\", \"5m\", \"1h\", \"1d\")",
                    spec
                ),
                attribute.line,
            );
        }
    }

    fn check_func(&mut self, func: &FuncDecl) {
        let sig = match self.funcs.get(&func.name) {
            Some(sig) => sig.clone(),
            None => return,
        };
        self.current_package = self.package_of(func);
        self.current_type_bounds = func.type_bounds.clone();
        self.scopes.push(HashMap::new());
        for (p, ps) in func.params.iter().zip(sig.params.iter()) {
            self.declare(&p.name, ps.ty.clone(), p.mode == ParamMode::Write, p.line);
        }
        let result_err = match self.table.resolve(&sig.ret) {
            Type::Result(_, e) => Some(*e),
            _ => None,
        };
        self.returns.push(ReturnCtx {
            ret: sig.ret.clone(),
            result_err,
        });
        self.loop_depth = 0;
        self.check_stmts(&func.body);
        self.returns.pop();
        self.end_scope();
        self.current_type_bounds.clear();
        if self.table.resolve(&sig.ret) != Type::Void && !always_returns(&func.body) {
            self.error(
                format!(
                    "function '{}' must return {} on all paths",
                    func.name, sig.ret
                ),
                func.line,
            );
        }
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
                let declared = type_from_expr(ty);
                self.validate_type(&declared, stmt.line);
                let actual = self.check_expr(init, Some(&declared));
                if !self.is_assignable(&actual, &declared) {
                    self.error(
                        format!(
                            "type mismatch for '{}': expected {}, found {}",
                            name, declared, actual
                        ),
                        stmt.line,
                    );
                }
                self.warn_name(name, NameKind::Variable, stmt.line);
                self.declare_let(name, declared, *mutable, stmt.line);
            }
            StmtKind::Expr(expr) => {
                self.check_expr(expr, None);
            }
            StmtKind::Assign { target, value } => {
                let target_ty = self.check_assign_target(target);
                let actual = self.check_expr(value, Some(&target_ty));
                if !self.is_assignable(&actual, &target_ty) {
                    self.error(
                        format!(
                            "type mismatch in assignment: expected {}, found {}",
                            target_ty, actual
                        ),
                        stmt.line,
                    );
                }
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                self.expect_bool(cond, "if condition");
                self.scopes.push(HashMap::new());
                self.check_stmts(then_body);
                self.end_scope();
                if let Some(body) = else_body {
                    self.scopes.push(HashMap::new());
                    self.check_stmts(body);
                    self.end_scope();
                }
            }
            StmtKind::Loop { body } => {
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                self.check_stmts(body);
                self.end_scope();
                self.loop_depth -= 1;
            }
            StmtKind::While { cond, body } => {
                self.expect_bool(cond, "while condition");
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                self.check_stmts(body);
                self.end_scope();
                self.loop_depth -= 1;
            }
            StmtKind::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                let ty = self.check_expr(scrutinee, None);
                let resolved = self.table.resolve(&ty);
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                self.bind_pattern(pattern, &resolved, stmt.line);
                self.check_stmts(body);
                self.end_scope();
                self.loop_depth -= 1;
            }
            StmtKind::ForIn { name, iter, body } => {
                let iter_ty = self.check_expr(iter, None);
                let elem = self.iter_elem(&iter_ty, stmt.line);
                self.loop_depth += 1;
                self.scopes.push(HashMap::new());
                self.warn_name(name, NameKind::Variable, stmt.line);
                self.declare(name, elem, false, stmt.line);
                self.check_stmts(body);
                self.end_scope();
                self.loop_depth -= 1;
            }
            StmtKind::Defer { call } => {
                if self.returns.is_empty() {
                    self.error("defer outside of function", stmt.line);
                    return;
                }
                if self.parallel_depth > 0 {
                    self.error(
                        "parallel branch cannot use 'defer'; it has no enclosing function scope",
                        stmt.line,
                    );
                    return;
                }
                // 参数在 defer 处立即求值，返回值丢弃，因此只检查调用本身。
                self.check_expr(call, None);
            }
            StmtKind::Return(value) => {
                if self.parallel_depth > 0 {
                    self.error(
                        "parallel branch cannot use 'return'; move the logic into a function",
                        stmt.line,
                    );
                    return;
                }
                let ctx = match self.returns.last() {
                    Some(ctx) => ReturnCtx {
                        ret: ctx.ret.clone(),
                        result_err: ctx.result_err.clone(),
                    },
                    None => {
                        self.error("return outside of function", stmt.line);
                        return;
                    }
                };
                let ret = self.table.resolve(&ctx.ret);
                if ret == Type::Void {
                    if value.is_some() {
                        self.error("void function cannot return a value", stmt.line);
                    }
                    return;
                }
                match value {
                    Some(expr) => {
                        let actual = self.check_expr(expr, Some(&ctx.ret));
                        if !self.is_assignable(&actual, &ctx.ret) {
                            self.error(
                                format!(
                                    "return type mismatch: expected {}, found {}",
                                    ctx.ret, actual
                                ),
                                stmt.line,
                            );
                        }
                    }
                    None => self.error(
                        format!("function must return {}", ctx.ret),
                        stmt.line,
                    ),
                }
            }
            StmtKind::Break => {
                if self.loop_depth == 0 {
                    self.error("break outside of loop", stmt.line);
                }
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.error("continue outside of loop", stmt.line);
                }
            }
            StmtKind::Match { subject, arms } => {
                self.check_match(subject, arms, stmt.line);
            }
            StmtKind::Parallel { branches } => {
                // 分支各自在本线程重建运行时，看不到块外的局部变量：
                // 只保留模块级作用域（const/static），丢弃全部函数局部作用域。
                for branch in branches {
                    let locals = self.scopes.split_off(1);
                    let saved_loop_depth = self.loop_depth;
                    self.loop_depth = 0;
                    self.parallel_depth += 1;
                    self.scopes.push(HashMap::new());
                    self.check_stmt(branch);
                    self.end_scope();
                    self.parallel_depth -= 1;
                    self.loop_depth = saved_loop_depth;
                    self.scopes.extend(locals);
                }
            }
        }
    }

    fn check_match(&mut self, subject: &Expr, arms: &[MatchArm], line: u32) {
        let subject_ty = self.check_expr(subject, None);
        let resolved = self.table.resolve(&subject_ty);
        let mut has_some = false;
        let mut has_none = false;
        let mut has_ok = false;
        let mut has_err = false;
        let mut covered: HashSet<String> = HashSet::new();

        for arm in arms {
            match &arm.pattern {
                Pattern::Some(_) => has_some = true,
                Pattern::None => has_none = true,
                Pattern::Ok(_) => has_ok = true,
                Pattern::Err(_) => has_err = true,
                Pattern::Ary(_) => {}
                Pattern::Enum {
                    name: enum_name,
                    variant,
                    ..
                } => {
                    if let Type::Named(subject_name) = &resolved {
                        if subject_name == enum_name {
                            covered.insert(variant.clone());
                        }
                    }
                }
            }
            self.scopes.push(HashMap::new());
            self.bind_pattern(&arm.pattern, &resolved, arm.line);
            self.check_stmts(&arm.body);
            self.end_scope();
        }

        match &resolved {
            Type::Option(_) => {
                if !has_some || !has_none {
                    self.error("match on option must cover some(x) and none", line);
                }
            }
            Type::Result(_, _) => {
                if !has_ok || !has_err {
                    self.error("match on result must cover ok(v) and err(e)", line);
                }
            }
            Type::Ary(_) | Type::Unknown => {}
            Type::Named(name) => {
                if let Some(info) = self.enums.get(name).cloned() {
                    let name = name.clone();
                    for variant in &info.variants {
                        if !covered.contains(&variant.name) {
                            self.error(
                                format!(
                                    "match on enum '{}' must cover variant '{}'",
                                    name, variant.name
                                ),
                                line,
                            );
                        }
                    }
                }
            }
            other => {
                self.error(
                    format!("match supports option/result/ary/enum, found {}", other),
                    line,
                );
            }
        }
    }

    /// 校验模式与主体类型是否匹配，并把模式绑定的变量声明进当前作用域。
    ///
    /// `match` 分支与 `while let` 共用：前者随后检查分支是否穷尽，后者不要求穷尽。
    fn bind_pattern(&mut self, pattern: &Pattern, resolved: &Type, line: u32) {
        if matches!(resolved, Type::Unknown) {
            return;
        }
        match (pattern, resolved) {
            (Pattern::Some(name), Type::Option(inner)) => {
                self.declare(name, (**inner).clone(), false, line);
            }
            (Pattern::None, Type::Option(_)) => {}
            (Pattern::Ok(name), Type::Result(ok, _)) => {
                self.declare(name, (**ok).clone(), false, line);
            }
            (Pattern::Err(name), Type::Result(_, err)) => {
                self.declare(name, (**err).clone(), false, line);
            }
            (Pattern::Ary(fields), Type::Ary(ary_fields)) => {
                for field in fields {
                    match ary_fields.iter().find(|(n, _)| n == &field.name) {
                        Some((_, ty)) => {
                            if let Some(annotated) = &field.ty {
                                let annotated_ty = type_from_expr(annotated);
                                self.validate_type(&annotated_ty, line);
                                if !self.same_type(&annotated_ty, ty) {
                                    self.error(
                                        format!(
                                            "pattern type for '{}' is {}, but field is {}",
                                            field.name, annotated_ty, ty
                                        ),
                                        line,
                                    );
                                }
                            }
                            if field.name != "_" {
                                self.declare(&field.name, ty.clone(), false, line);
                            }
                        }
                        None => {
                            self.error(format!("type has no field '{}'", field.name), line)
                        }
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
                if enum_name != actual {
                    self.error(
                        format!(
                            "pattern '{}.{}' requires enum '{}', found '{}'",
                            enum_name, variant, enum_name, actual
                        ),
                        line,
                    );
                    return;
                }
                let info = match self.enums.get(actual).cloned() {
                    Some(info) => info,
                    None => {
                        self.error(format!("'{}' is not an enum", actual), line);
                        return;
                    }
                };
                let variant_info = match info.variants.iter().find(|item| item.name == *variant) {
                    Some(variant_info) => variant_info.clone(),
                    None => {
                        self.error(
                            format!("enum '{}' has no variant '{}'", actual, variant),
                            line,
                        );
                        return;
                    }
                };
                for field in fields {
                    match variant_info
                        .fields
                        .iter()
                        .find(|(name, _)| name == &field.name)
                    {
                        Some((_, ty)) => {
                            if let Some(annotated) = &field.ty {
                                let annotated_ty = type_from_expr(annotated);
                                self.validate_type(&annotated_ty, line);
                                if !self.same_type(&annotated_ty, ty) {
                                    self.error(
                                        format!(
                                            "pattern type for '{}' is {}, but field is {}",
                                            field.name, annotated_ty, ty
                                        ),
                                        line,
                                    );
                                }
                            }
                            if field.name != "_" {
                                self.declare(&field.name, ty.clone(), false, line);
                            }
                        }
                        None => self.error(
                            format!(
                                "variant '{}.{}' has no field '{}'",
                                actual, variant, field.name
                            ),
                            line,
                        ),
                    }
                }
            }
            (Pattern::Enum { name: enum_name, .. }, other) => {
                self.error(
                    format!(
                        "pattern '{}.…' requires enum '{}', found {}",
                        enum_name, enum_name, other
                    ),
                    line,
                );
            }
            (Pattern::Some(_), other) => {
                self.error(
                    format!("pattern some(x) requires option<T>, found {}", other),
                    line,
                );
            }
            (Pattern::None, other) => {
                self.error(
                    format!("pattern none requires option<T>, found {}", other),
                    line,
                );
            }
            (Pattern::Ok(_), other) => {
                self.error(
                    format!("pattern ok(x) requires result<T, E>, found {}", other),
                    line,
                );
            }
            (Pattern::Err(_), other) => {
                self.error(
                    format!("pattern err(e) requires result<T, E>, found {}", other),
                    line,
                );
            }
            (Pattern::Ary(_), other) => {
                self.error(format!("pattern [...] requires ary, found {}", other), line);
            }
        }
    }

    fn check_assign_target(&mut self, target: &Expr) -> Type {
        match &target.kind {
            ExprKind::Ident(name) => {
                let found = self.lookup(name).map(|info| (info.mutable, info.ty.clone()));
                if let Some((mutable, ty)) = found {
                    if !mutable {
                        self.error(
                            format!("cannot assign to immutable variable '{}'", name),
                            target.line,
                        );
                    }
                    ty
                } else {
                    self.error(format!("unknown variable '{}'", name), target.line);
                    Type::Unknown
                }
            }
            ExprKind::Field { .. } => {
                if let Some(root) = root_ident(target) {
                    match self.lookup(&root) {
                        Some(info) if info.mutable => {}
                        Some(_) => self.error(
                            format!("cannot mutate immutable variable '{}'", root),
                            target.line,
                        ),
                        None => self.error(format!("unknown variable '{}'", root), target.line),
                    }
                }
                self.check_expr(target, None)
            }
            _ => {
                self.error("assignment target must be a variable or field", target.line);
                Type::Unknown
            }
        }
    }

    fn expect_bool(&mut self, expr: &Expr, what: &str) {
        let ty = self.check_expr(expr, Some(&Type::Bool));
        if !self.is_assignable(&ty, &Type::Bool) {
            self.error(format!("{} must be bool, found {}", what, ty), expr.line);
        }
    }

    fn iter_elem(&mut self, ty: &Type, line: u32) -> Type {
        match self.table.resolve(ty) {
            Type::Vec(t) => *t,
            Type::VecDynamic => Type::Any,
            Type::VecHetero(ts) if ts.len() == 1 => ts[0].clone(),
            Type::VecHetero(_) => Type::Unknown,
            Type::Map(k, _) => *k,
            Type::Unknown => Type::Unknown,
            other => {
                self.error(format!("for-in requires vec or map, found {}", other), line);
                Type::Unknown
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, expected: Option<&Type>) -> Type {
        let ty = match &expr.kind {
            ExprKind::Int {
                value,
                base,
                suffix,
            } => self.int_literal(*value, *base, *suffix, expected, expr.line),
            ExprKind::Float(_) => match self.expected_kind(expected) {
                Some(k) if k.is_int() => {
                    self.error("float literal cannot be used as an integer", expr.line);
                    Type::Unknown
                }
                Some(Type::Str) => {
                    self.error("float literal cannot be used as string", expr.line);
                    Type::Unknown
                }
                _ => Type::Float,
            },
            ExprKind::Str(_) | ExprKind::Char(_) => {
                if let Some(k) = self.expected_kind(expected) {
                    if !matches!(k, Type::Str | Type::Any) {
                        self.error(
                            format!("string literal cannot be used as {}", k),
                            expr.line,
                        );
                        return self.record(expr.id, Type::Unknown);
                    }
                }
                Type::Str
            }
            ExprKind::Bool(_) => {
                if let Some(k) = self.expected_kind(expected) {
                    if !matches!(k, Type::Bool | Type::Any) {
                        self.error(format!("bool literal cannot be used as {}", k), expr.line);
                        return self.record(expr.id, Type::Unknown);
                    }
                }
                Type::Bool
            }
            ExprKind::Null => match self.expected_kind(expected) {
                Some(Type::Option(_)) | Some(Type::Any) => {
                    expected.map(|t| t.clone()).unwrap_or(Type::Option(Box::new(Type::Unknown)))
                }
                _ => {
                    self.error("null is only allowed for option<T>", expr.line);
                    Type::Unknown
                }
            },
            ExprKind::Ident(name) => self.ident_type(name, expr.line),
            ExprKind::Field { base, name } => self.field_type(expr, base, name),
            ExprKind::Call {
                callee,
                args,
                type_args,
            } => self.call_type(expr, callee, args, type_args),
            ExprKind::Method {
                receiver,
                name,
                args,
            } => self.method_type(expr, receiver, name, args),
            ExprKind::Unary { op, expr: inner } => {
                // `-128i8` 这类字面量取负：按负值检查范围，否则 128 会被误判为超出 i8。
                if matches!(op, UnOp::Neg) {
                    if let ExprKind::Int {
                        value,
                        base,
                        suffix,
                    } = &inner.kind
                    {
                        let ty = self.int_literal(-*value, *base, *suffix, expected, inner.line);
                        self.record(inner.id, ty.clone());
                        return self.record(expr.id, ty);
                    }
                }
                let t = self.check_expr(inner, None);
                let r = self.table.resolve(&t);
                match op {
                    UnOp::Not => {
                        if !matches!(r, Type::Bool | Type::Unknown) {
                            self.error(format!("! requires bool, found {}", r), expr.line);
                            Type::Unknown
                        } else {
                            Type::Bool
                        }
                    }
                    UnOp::Neg => match r {
                        ref t if t.is_int() || matches!(t, Type::Float) => r,
                        Type::Unknown => Type::Unknown,
                        other => {
                            self.error(format!("unary - requires number, found {}", other), expr.line);
                            Type::Unknown
                        }
                    },
                }
            }
            ExprKind::Binary { op, left, right } => self.binary_type(expr, *op, left, right),
            ExprKind::List(items) => self.list_type(items, expected, expr.line),
            ExprKind::Struct { path, fields } => {
                self.struct_type(path, fields, expr.line)
            }
            ExprKind::Some(inner) => {
                let expected_inner = match self.expected_kind(expected) {
                    Some(Type::Option(t)) => Some(*t),
                    Some(Type::Any) => Some(Type::Any),
                    _ => None,
                };
                let t = self.check_expr(inner, expected_inner.as_ref());
                if let Some(k) = self.expected_kind(expected) {
                    if !matches!(k, Type::Option(_) | Type::Any) {
                        self.error(format!("some(...) cannot be used as {}", k), expr.line);
                        return self.record(expr.id, Type::Unknown);
                    }
                }
                Type::Option(Box::new(t))
            }
            ExprKind::None => match self.expected_kind(expected) {
                Some(Type::Option(t)) => Type::Option(t),
                Some(Type::Any) => Type::Option(Box::new(Type::Any)),
                _ => {
                    self.error("none requires option<T> context", expr.line);
                    Type::Option(Box::new(Type::Unknown))
                }
            },
            ExprKind::Ok(inner) => {
                let expected_ok = match self.expected_kind(expected) {
                    Some(Type::Result(t, _)) => Some(*t),
                    Some(Type::Any) => Some(Type::Any),
                    _ => None,
                };
                let t = self.check_expr(inner, expected_ok.as_ref());
                match self.expected_kind(expected) {
                    Some(Type::Result(_, e)) => Type::Result(Box::new(t), e),
                    Some(Type::Any) => Type::Result(Box::new(t), Box::new(Type::Any)),
                    _ => Type::Result(Box::new(t), Box::new(Type::Unknown)),
                }
            }
            ExprKind::Err(inner) => {
                let expected_err = match self.expected_kind(expected) {
                    Some(Type::Result(_, e)) => Some(*e),
                    Some(Type::Any) => Some(Type::Any),
                    _ => None,
                };
                let t = self.check_expr(inner, expected_err.as_ref());
                match self.expected_kind(expected) {
                    Some(Type::Result(ok, _)) => Type::Result(ok, Box::new(t)),
                    Some(Type::Any) => Type::Result(Box::new(Type::Any), Box::new(t)),
                    _ => Type::Result(Box::new(Type::Unknown), Box::new(t)),
                }
            }
            ExprKind::Closure { params, ret, body } => {
                self.closure_type(expr, params, ret, body, expected)
            }
            ExprKind::Try(inner) => {
                let t = self.check_expr(inner, None);
                match self.table.resolve(&t) {
                    Type::Result(ok, err) => {
                        let ctx_err = self
                            .returns
                            .last()
                            .and_then(|c| c.result_err.clone());
                        match ctx_err {
                            Some(expected_err) => {
                                if !self.is_assignable(&err, &expected_err) {
                                    self.error(
                                        format!(
                                            "'?' error type {} does not match function error type {}",
                                            err, expected_err
                                        ),
                                        expr.line,
                                    );
                                }
                                *ok
                            }
                            None => {
                                self.error(
                                    "'?' can only be used in a function returning result",
                                    expr.line,
                                );
                                *ok
                            }
                        }
                    }
                    Type::Unknown => Type::Unknown,
                    other => {
                        self.error(
                            format!("'?' requires result<T, E>, found {}", other),
                            expr.line,
                        );
                        Type::Unknown
                    }
                }
            }
            ExprKind::Await(inner) => self.await_type(expr, inner),
        };
        self.record(expr.id, ty)
    }

    /// `await`：操作数必须是 `task<T>`，结果类型是 `T`。
    fn await_type(&mut self, whole: &Expr, inner: &Expr) -> Type {
        let ty = self.check_expr(inner, None);
        match self.table.resolve(&ty) {
            Type::Task(value) => *value,
            Type::Unknown => Type::Unknown,
            other => {
                self.error(
                    format!(
                        "await requires task<T> (the result of an async call), found {}",
                        other
                    ),
                    whole.line,
                );
                Type::Unknown
            }
        }
    }

    fn record(&mut self, id: NodeId, ty: Type) -> Type {
        self.expr_types.insert(id, ty.clone());
        ty
    }

    /// 整数字面量定型：后缀优先，其次上下文期望的整数类型，最后是 `int`。
    ///
    /// 前缀（`0x` / `0o` / `0b`）只表示进制，不参与定型；范围按目标类型检查。
    fn int_literal(
        &mut self,
        value: i128,
        _base: IntBase,
        suffix: Option<IntSuffix>,
        expected: Option<&Type>,
        line: u32,
    ) -> Type {
        let kind = self.expected_kind(expected);
        let target = match suffix {
            Some(suffix) => Type::from_int_suffix(suffix),
            None => match &kind {
                Some(k) if k.is_int() => k.clone(),
                _ => Type::Int,
            },
        };
        if let Some(suffix) = suffix {
            if let Some(k) = &kind {
                if !matches!(k, Type::Any | Type::Unknown) && *k != target {
                    self.error(
                        format!("integer literal suffix {} does not match {}", suffix.name(), k),
                        line,
                    );
                    return Type::Unknown;
                }
            }
        }
        let (min, max) = match target.int_suffix() {
            Some(suffix) => suffix.range(),
            None => (i64::MIN as i128, i64::MAX as i128),
        };
        if value < min || value > max {
            self.error(
                format!("integer literal {} does not fit {}", value, target),
                line,
            );
            return Type::Unknown;
        }
        target
    }

    fn ident_type(&mut self, name: &str, line: u32) -> Type {
        if let Some(info) = self.lookup(name) {
            let ty = info.ty.clone();
            self.mark_used(name);
            return ty;
        }
        if let Some(sig) = self.funcs.get(name) {
            let sig = sig.clone();
            self.used_funcs.insert(name.to_string());
            self.check_private_access(name, line);
            if sig.is_async {
                self.error(
                    format!(
                        "async function '{}' can only be used as `await {}()`",
                        name, name
                    ),
                    line,
                );
            }
            return sig.func_type();
        }
        if self.imports.contains_key(name) {
            self.used_imports.insert(name.to_string());
            self.error(
                format!("module alias '{}' can only be used as a call base", name),
                line,
            );
            return Type::Unknown;
        }
        self.error(format!("unknown identifier '{}'", name), line);
        Type::Unknown
    }

    fn field_type(&mut self, whole: &Expr, base: &Expr, name: &str) -> Type {
        if let ExprKind::Ident(alias) = &base.kind {
            if self.imports.contains_key(alias) {
                self.used_imports.insert(alias.clone());
                self.error(
                    format!("module member '{}' can only appear in a call", alias),
                    whole.line,
                );
                return Type::Unknown;
            }
            // 枚举单元变体：`status.active`（不是变量访问）。
            if self.lookup(alias).is_none() {
                if let Some(info) = self.enums.get(alias).cloned() {
                    match info.variants.iter().find(|variant| variant.name == name) {
                        Some(variant) => {
                            if !variant.fields.is_empty() {
                                self.error(
                                    format!(
                                        "variant '{}.{}' has fields; build it with '{}.{}[field = value]'",
                                        alias, name, alias, name
                                    ),
                                    whole.line,
                                );
                                return Type::Unknown;
                            }
                            return Type::Named(alias.clone());
                        }
                        None => {
                            self.error(
                                format!("enum '{}' has no variant '{}'", alias, name),
                                whole.line,
                            );
                            return Type::Unknown;
                        }
                    }
                }
            }
        }
        let base_ty = self.check_expr(base, None);
        let resolved = self.table.resolve(&base_ty);
        match &resolved {
            Type::Ary(fields) => match fields.iter().find(|(n, _)| n == name) {
                Some((_, ty)) => ty.clone(),
                None => {
                    self.error(format!("type {} has no field '{}'", resolved, name), whole.line);
                    Type::Unknown
                }
            },
            Type::Vec(t) => {
                if is_numeric(name) {
                    (**t).clone()
                } else {
                    self.error(format!("vec has no field '{}'", name), whole.line);
                    Type::Unknown
                }
            }
            Type::VecHetero(ts) => {
                if let Ok(index) = name.parse::<usize>() {
                    match ts.get(index) {
                        Some(t) => t.clone(),
                        None => {
                            self.error(
                                format!("vec index {} out of bounds for {}", index, resolved),
                                whole.line,
                            );
                            Type::Unknown
                        }
                    }
                } else {
                    self.error(format!("vec has no field '{}'", name), whole.line);
                    Type::Unknown
                }
            }
            Type::VecDynamic => Type::Any,
            Type::Unknown | Type::Any => Type::Unknown,
            Type::Named(n) if self.enums.contains_key(n) => {
                self.error(
                    format!(
                        "enum values are read through `match`, e.g. `{}.variant[field] => ...`",
                        n
                    ),
                    whole.line,
                );
                Type::Unknown
            }
            other => {
                self.error(format!("type {} has no fields", other), whole.line);
                Type::Unknown
            }
        }
    }

    fn call_type(
        &mut self,
        whole: &Expr,
        callee: &Expr,
        args: &[Expr],
        type_args: &[TypeExpr],
    ) -> Type {
        if let ExprKind::Ident(name) = &callee.kind {
            if name == "print" {
                if !type_args.is_empty() {
                    self.error("print is not generic", whole.line);
                }
                if args.len() != 1 {
                    self.error("print takes exactly 1 argument", whole.line);
                }
                for arg in args {
                    self.check_expr(arg, Some(&Type::Any));
                }
                self.record(callee.id, Type::Func(vec![Type::Any], Box::new(Type::Void)));
                return Type::Void;
            }
            if let Some(sig) = self.funcs.get(name).cloned() {
                self.used_funcs.insert(name.clone());
                self.check_private_access(name, whole.line);
                self.record(callee.id, sig.func_type());
                let ret = self.call_sig(&sig, type_args, args, whole.line);
                return if sig.is_async {
                    Type::Task(Box::new(ret))
                } else {
                    ret
                };
            }
            let local_ty = self.lookup(name).map(|info| info.ty.clone());
            if let Some(local_ty) = local_ty {
                let ty = self.table.resolve(&local_ty);
                if let Type::Func(params, ret) = ty {
                    if !type_args.is_empty() {
                        self.error(
                            format!("'{}' is a function value, not a generic function", name),
                            whole.line,
                        );
                    }
                    self.mark_used(name);
                    self.record(callee.id, local_ty);
                    let sig_params: Vec<ParamSig> = params
                        .iter()
                        .map(|t| ParamSig {
                            name: String::new(),
                            mode: ParamMode::Value,
                            ty: t.clone(),
                        })
                        .collect();
                    return self.check_call_args(&sig_params, &ret, args, whole.line);
                }
            }
        }

        if let ExprKind::Field { base, name } = &callee.kind {
            if let ExprKind::Ident(alias) = &base.kind {
                if let Some(path) = self.imports.get(alias).cloned() {
                    self.used_imports.insert(alias.clone());
                    if let Some(sig) = bi::builtin_function(&path, name) {
                        return self.check_builtin_call(&path, name, &sig, callee, args, whole.line);
                    }
                    // 非系统包（`packages/` 下的第三方包）编译时与项目源码合并，
                    // 包内函数按全局函数名参与类型检查，这里按名字解析。
                    if let Some(sig) = self.funcs.get(name).cloned() {
                        self.used_funcs.insert(name.clone());
                        self.check_private_access(name, whole.line);
                        self.record(callee.id, sig.func_type());
                        let ret = self.call_sig(&sig, type_args, args, whole.line);
                        return if sig.is_async {
                            Type::Task(Box::new(ret))
                        } else {
                            ret
                        };
                    }
                    self.error(
                        format!("package \"{}\" has no function '{}' in this build", path, name),
                        whole.line,
                    );
                    return Type::Unknown;
                }
            }
        }

        let callee_ty = self.check_expr(callee, None);
        match self.table.resolve(&callee_ty) {
            Type::Func(params, ret) => {
                let sig_params: Vec<ParamSig> = params
                    .iter()
                    .map(|t| ParamSig {
                        name: String::new(),
                        mode: ParamMode::Value,
                        ty: t.clone(),
                    })
                    .collect();
                self.check_call_args(&sig_params, &ret, args, whole.line)
            }
            Type::Unknown => Type::Unknown,
            other => {
                self.error(format!("cannot call value of type {}", other), whole.line);
                Type::Unknown
            }
        }
    }

    fn check_builtin_call(
        &mut self,
        path: &str,
        name: &str,
        sig: &bi::BuiltinSig,
        callee: &Expr,
        args: &[Expr],
        line: u32,
    ) -> Type {
        let _ = (path, name);
        self.record(callee.id, Type::Unknown);
        // 变参内建（如 `dev.log`）：参数个数不固定，逐个按首参类型检查即可。
        if sig.variadic_any {
            let expected = sig.params.first().cloned().unwrap_or(Type::Any);
            for arg in args {
                self.check_expr(arg, Some(&expected));
            }
            return sig.ret.clone();
        }
        let params: Vec<ParamSig> = sig
            .params
            .iter()
            .map(|t| ParamSig {
                name: String::new(),
                mode: ParamMode::Value,
                ty: t.clone(),
            })
            .collect();
        self.check_call_args(&params, &sig.ret, args, line)
    }

    fn check_call_args(
        &mut self,
        params: &[ParamSig],
        ret: &Type,
        args: &[Expr],
        line: u32,
    ) -> Type {
        if params.len() != args.len() {
            self.error(
                format!("expected {} arguments, found {}", params.len(), args.len()),
                line,
            );
        }
        for (i, arg) in args.iter().enumerate() {
            if let Some(param) = params.get(i) {
                let expected = param.ty.clone();
                let actual = self.check_expr(arg, Some(&expected));
                if !self.is_assignable(&actual, &expected) {
                    self.error(
                        format!(
                            "argument {} type mismatch: expected {}, found {}",
                            i + 1,
                            expected,
                            actual
                        ),
                        arg.line,
                    );
                }
            } else {
                self.check_expr(arg, None);
            }
        }
        ret.clone()
    }

    /// 调用函数：泛型函数先按实参推断类型参数，再代入形参与返回值。
    ///
    /// 支持两种实例化方式：从实参推断，或显式 `f<int>(...)`（数量须与声明一致）；
    /// 推断不出的类型参数按 `any` 处理，行为与写死 `any` 一致。
    fn call_sig(
        &mut self,
        sig: &FuncSig,
        type_args: &[TypeExpr],
        args: &[Expr],
        line: u32,
    ) -> Type {
        if sig.type_params.is_empty() {
            if !type_args.is_empty() {
                self.error(
                    format!(
                        "function is not generic, but {} type argument(s) were given",
                        type_args.len()
                    ),
                    line,
                );
            }
            return self.check_call_args(&sig.params, &sig.ret, args, line);
        }
        if sig.params.len() != args.len() {
            self.error(
                format!("expected {} arguments, found {}", sig.params.len(), args.len()),
                line,
            );
            return sig.ret.clone();
        }
        let mut arg_types = Vec::with_capacity(args.len());
        for arg in args {
            arg_types.push(self.check_expr(arg, None));
        }
        let mut bindings: HashMap<String, Type> = HashMap::new();
        if !type_args.is_empty() {
            // 显式类型实参：逐个绑定，不再从实参推断。
            if type_args.len() != sig.type_params.len() {
                self.error(
                    format!(
                        "expected {} type argument(s), found {}",
                        sig.type_params.len(),
                        type_args.len()
                    ),
                    line,
                );
            }
            for (index, param) in sig.type_params.iter().enumerate() {
                if let Some(expr) = type_args.get(index) {
                    let ty = type_from_expr(expr);
                    self.validate_type(&ty, expr.line);
                    bindings.insert(param.clone(), ty);
                }
            }
        } else {
            for (index, arg_ty) in arg_types.iter().enumerate() {
                if let Some(expr) = sig.param_exprs.get(index) {
                    self.unify_type_expr(expr, arg_ty, &sig.type_params, &mut bindings);
                }
            }
        }
        for (param, bound) in &sig.type_bounds {
            if let Some(binding) = bindings.get(param).cloned() {
                self.check_type_bound(param, &binding, bound, line);
            }
        }
        for (index, expr) in sig.param_exprs.iter().enumerate() {
            let expected = self.instantiate_type_expr(expr, &bindings);
            let actual = &arg_types[index];
            if !self.is_assignable(actual, &expected) {
                self.error(
                    format!(
                        "argument {} type mismatch: expected {}, found {}",
                        index + 1,
                        expected,
                        actual
                    ),
                    args[index].line,
                );
            }
        }
        self.instantiate_type_expr(&sig.ret_expr, &bindings)
    }

    /// 类型实参必须满足声明里的接口约束。
    fn check_type_bound(&mut self, param: &str, ty: &Type, bound: &str, line: u32) {
        match ty {
            Type::Unknown | Type::Any => {}
            Type::Named(owner) => {
                // 类型参数透传：`S` 自己的约束满足即可。
                if let Some(existing) = self.current_type_bounds.get(owner) {
                    if existing == bound {
                        return;
                    }
                }
                if !self.implements(owner, bound) {
                    self.error(
                        format!("type '{}' does not implement '{}' required by '{}'", owner, bound, param),
                        line,
                    );
                }
            }
            other => self.error(
                format!("type {} cannot satisfy '{}: {}'", other, param, bound),
                line,
            ),
        }
    }

    /// 从「声明形状 + 实参类型」推断类型参数；冲突时保留第一次的绑定，
    /// 由后续的赋值检查给出可读错误。
    ///
    /// 先按未展开的实参类型匹配，保留 `circle` 这类命名类型身份（接口约束依赖它）；
    /// 匹配不上再展开别名重试。
    fn unify_type_expr(
        &self,
        expr: &TypeExpr,
        actual: &Type,
        type_params: &[String],
        bindings: &mut HashMap<String, Type>,
    ) {
        if self.unify_once(expr, actual, type_params, bindings) {
            return;
        }
        let resolved = self.table.resolve(actual);
        if &resolved != actual {
            self.unify_once(expr, &resolved, type_params, bindings);
        }
    }

    /// 按声明形状匹配一次；匹配成功返回 `true`。
    fn unify_once(
        &self,
        expr: &TypeExpr,
        actual: &Type,
        type_params: &[String],
        bindings: &mut HashMap<String, Type>,
    ) -> bool {
        match &expr.kind {
            TypeExprKind::Named(path) => {
                if path.len() == 1 && type_params.iter().any(|param| param == &path[0]) {
                    if matches!(actual, Type::Unknown) {
                        return true;
                    }
                    let name = path[0].clone();
                    match bindings.get(&name) {
                        None => {
                            bindings.insert(name, actual.clone());
                        }
                        Some(existing) if matches!(self.table.resolve(existing), Type::Any) => {
                            bindings.insert(name, actual.clone());
                        }
                        // 第二个实参给出了不同绑定：保留首个，交给赋值检查报错。
                        Some(_) => {}
                    }
                }
                true
            }
            TypeExprKind::Vec(items) if items.len() == 1 => match actual {
                Type::Vec(inner) => {
                    self.unify_type_expr(&items[0], inner, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Map(key, value) => match actual {
                Type::Map(actual_key, actual_value) => {
                    self.unify_type_expr(key, actual_key, type_params, bindings);
                    self.unify_type_expr(value, actual_value, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Option(inner) => match actual {
                Type::Option(actual_inner) => {
                    self.unify_type_expr(inner, actual_inner, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Result(ok, err) => match actual {
                Type::Result(actual_ok, actual_err) => {
                    self.unify_type_expr(ok, actual_ok, type_params, bindings);
                    self.unify_type_expr(err, actual_err, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Task(inner) => match actual {
                Type::Task(actual_inner) => {
                    self.unify_type_expr(inner, actual_inner, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Ary(fields) => match actual {
                Type::Ary(actual_fields) => {
                    for field in fields {
                        if let Some((_, actual_field)) =
                            actual_fields.iter().find(|(other, _)| other == &field.name)
                        {
                            self.unify_type_expr(
                                &field.ty,
                                actual_field,
                                type_params,
                                bindings,
                            );
                        }
                    }
                    true
                }
                _ => false,
            },
            TypeExprKind::Func(params, ret) => match actual {
                Type::Func(actual_params, actual_ret) => {
                    for (param, actual_param) in params.iter().zip(actual_params.iter()) {
                        self.unify_type_expr(param, actual_param, type_params, bindings);
                    }
                    self.unify_type_expr(ret, actual_ret, type_params, bindings);
                    true
                }
                _ => false,
            },
            TypeExprKind::Read(inner) | TypeExprKind::Share(inner) => {
                self.unify_type_expr(inner, actual, type_params, bindings);
                true
            }
            _ => true,
        }
    }

    /// 把声明中的类型参数替换成推断结果；未绑定的按 `any` 处理。
    fn instantiate_type_expr(&self, expr: &TypeExpr, bindings: &HashMap<String, Type>) -> Type {
        match &expr.kind {
            TypeExprKind::Named(path) => {
                if path.len() == 1 {
                    if let Some(bound) = bindings.get(&path[0]) {
                        return bound.clone();
                    }
                }
                // 基础类型名（`int` / `string` …）必须走 `type_from_expr`
                // 映射到真实类型，`resolve` 只展开用户别名与内建命名类型。
                let named = type_from_expr(expr);
                let resolved = self.table.resolve(&named);
                // 只出现在返回位置的类型参数推断不出绑定，按 `any` 处理。
                match &resolved {
                    Type::Named(name) if !self.table.is_known_named(name) => Type::Any,
                    _ => resolved,
                }
            }
            TypeExprKind::Ary(fields) => Type::Ary(
                fields
                    .iter()
                    .map(|field| {
                        (
                            field.name.clone(),
                            self.instantiate_type_expr(&field.ty, bindings),
                        )
                    })
                    .collect(),
            ),
            TypeExprKind::Vec(items) => match items.len() {
                0 => Type::VecDynamic,
                1 => Type::Vec(Box::new(self.instantiate_type_expr(&items[0], bindings))),
                _ => Type::VecHetero(
                    items
                        .iter()
                        .map(|item| self.instantiate_type_expr(item, bindings))
                        .collect(),
                ),
            },
            TypeExprKind::Map(key, value) => Type::Map(
                Box::new(self.instantiate_type_expr(key, bindings)),
                Box::new(self.instantiate_type_expr(value, bindings)),
            ),
            TypeExprKind::Option(inner) => {
                Type::Option(Box::new(self.instantiate_type_expr(inner, bindings)))
            }
            TypeExprKind::Result(ok, err) => Type::Result(
                Box::new(self.instantiate_type_expr(ok, bindings)),
                Box::new(self.instantiate_type_expr(err, bindings)),
            ),
            TypeExprKind::Share(inner) | TypeExprKind::Read(inner) => {
                self.instantiate_type_expr(inner, bindings)
            }
            TypeExprKind::Task(inner) => {
                Type::Task(Box::new(self.instantiate_type_expr(inner, bindings)))
            }
            TypeExprKind::Func(params, ret) => Type::Func(
                params
                    .iter()
                    .map(|param| self.instantiate_type_expr(param, bindings))
                    .collect(),
                Box::new(self.instantiate_type_expr(ret, bindings)),
            ),
            TypeExprKind::Void => Type::Void,
        }
    }

    fn method_type(
        &mut self,
        whole: &Expr,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
    ) -> Type {
        if let ExprKind::Ident(alias) = &receiver.kind {
            if let Some(path) = self.imports.get(alias).cloned() {
                self.used_imports.insert(alias.clone());
                if let Some(sig) = bi::builtin_function(&path, name) {
                    return self.check_builtin_call(&path, name, &sig, receiver, args, whole.line);
                }
                // 第三方包函数与项目源码合并编译，按全局函数名解析。
                if let Some(sig) = self.funcs.get(name).cloned() {
                    self.used_funcs.insert(name.to_string());
                    self.check_private_access(name, whole.line);
                    return self.check_call_args(&sig.params, &sig.ret, args, whole.line);
                }
                self.error(
                    format!("package \"{}\" has no function '{}' in this build", path, name),
                    whole.line,
                );
                return Type::Unknown;
            }
        }
        let recv_ty = self.check_expr(receiver, None);
        // 用户方法：接收者静态类型是命名类型。普通类型静态分派，接口按运行时类名分派。
        if let Type::Named(type_name) = &recv_ty {
            if let Some(full) = self
                .methods
                .get(type_name)
                .and_then(|methods| methods.get(name))
                .cloned()
            {
                return self.check_user_method(&full, receiver, args, whole.line);
            }
            if let Some(methods) = {
                // 受接口约束的类型参数按约束接口解析（`func f<S: shape>` 里 `s.area()`）。
                let iface = self
                    .current_type_bounds
                    .get(type_name)
                    .cloned()
                    .unwrap_or_else(|| type_name.clone());
                self.interfaces.get(&iface).cloned()
            } {
                if let Some(method) = methods.iter().find(|method| method.name == name).cloned() {
                    if method.mode == ParamMode::Write {
                        self.require_mutable_receiver(receiver);
                    }
                    let expected = method.params.clone();
                    self.expect_args(args, &expected, name, whole.line);
                    return method.ret.clone();
                }
                return self.unknown_method(&recv_ty, name, whole.line);
            }
        }
        let resolved = self.table.resolve(&recv_ty);
        // 整数转换方法只接受变量接收者：`11.to_i16()`、`"11".to_int(2)` 都是编译错误，
        // 必须先 `let` 绑定（与「零推断、全显式」一致）。
        if is_int_convert(name) && is_literal_expr(receiver) {
            self.error(
                format!(
                    "conversion method '{}' requires a variable receiver; bind the value first, e.g. `let n: int = 11; n.{}(...)`",
                    name, name
                ),
                whole.line,
            );
        }
        match resolved {
            ref t if t.is_int() => self.int_method(&resolved, name, args, whole.line),
            Type::Float => self.float_method(name, args, whole.line),
            Type::Bool => match name {
                "to_string" => self.no_args(name, args, whole.line, Type::Str),
                _ => self.unknown_method(&resolved, name, whole.line),
            },
            Type::Str => self.string_method(name, args, whole.line),
            Type::Vec(t) => self.vec_method(*t, name, args, whole.line),
            Type::VecHetero(_) | Type::VecDynamic => match name {
                "len" => self.no_args(name, args, whole.line, Type::Int),
                "is_empty" => self.no_args(name, args, whole.line, Type::Bool),
                "clone" => self.no_args(name, args, whole.line, resolved.clone()),
                "at" => {
                    self.expect_args(args, &[Type::Int], name, whole.line);
                    Type::Result(Box::new(Type::Any), Box::new(Type::Str))
                }
                _ => self.unknown_method(&resolved, name, whole.line),
            },
            Type::Map(k, v) => self.map_method(*k, *v, name, args, whole.line),
            Type::Option(t) => self.option_method(*t, name, args, whole.line),
            Type::Result(t, e) => self.result_method(*t, *e, name, args, whole.line),
            Type::Any => self.any_method(name, args, whole.line),
            Type::Named(ref n) if n == "time.datetime" => match name {
                "format" => {
                    self.expect_args(args, &[Type::Str], name, whole.line);
                    Type::Str
                }
                "to_string" => self.no_args(name, args, whole.line, Type::Str),
                "timestamp_ms" => self.no_args(name, args, whole.line, Type::Int),
                _ => self.unknown_method(&resolved, name, whole.line),
            },
            Type::Named(n) if n == "llm.client" => self.llm_client_method(name, args, whole.line),
            Type::Named(n) if n == "db.conn" => self.db_conn_method(name, args, whole.line),
            Type::Named(n) if n == "redis.conn" => self.redis_conn_method(name, args, whole.line),
            Type::Named(n) if n == "sync.mutex" => self.sync_mutex_method(name, args, whole.line),
            Type::Named(n) if n == "sync.atomic" => self.sync_atomic_method(name, args, whole.line),
            Type::Named(n) if n == "sync.channel" => {
                self.sync_channel_method(name, args, whole.line)
            }
            Type::Named(n) if n == "db.query" => self.db_query_method(name, args, whole.line),
            Type::Named(ref n) if self.enums.contains_key(n) => match name {
                "clone" => self.no_args(name, args, whole.line, resolved.clone()),
                _ => self.unknown_method(&resolved, name, whole.line),
            },
            Type::Ary(_) => match name {
                "clone" => self.no_args(name, args, whole.line, resolved.clone()),
                _ => self.unknown_method(&resolved, name, whole.line),
            },
            Type::Unknown => Type::Unknown,
            other => self.unknown_method(&other, name, whole.line),
        }
    }

    /// 用户方法调用：第一个形参是接收者，实参从第二个形参开始匹配。
    fn check_user_method(
        &mut self,
        full: &str,
        receiver: &Expr,
        args: &[Expr],
        line: u32,
    ) -> Type {
        let Some(sig) = self.funcs.get(full).cloned() else {
            return Type::Unknown;
        };
        self.used_funcs.insert(full.to_string());
        self.check_private_access(full, line);
        if let Some(receiver_param) = sig.params.first() {
            if receiver_param.mode == ParamMode::Write {
                self.require_mutable_receiver(receiver);
            }
        }
        let params = if sig.params.is_empty() {
            &sig.params[..]
        } else {
            &sig.params[1..]
        };
        self.check_call_args(params, &sig.ret, args, line)
    }

    /// `write` 接收者要求可变：变量/字段链的可变性统一由所有权检查报错，
    /// 这里只拦截临时值（`make().scale(2.0)` 改了也看不到）。
    fn require_mutable_receiver(&mut self, receiver: &Expr) {
        fn root(expr: &Expr) -> Option<(String, u32)> {
            match &expr.kind {
                ExprKind::Ident(name) => Some((name.clone(), expr.line)),
                ExprKind::Field { base, .. } => root(base),
                _ => None,
            }
        }
        if root(receiver).is_none() {
            self.error(
                "write method requires a mutable variable receiver".to_string(),
                receiver.line,
            );
        }
    }

    fn int_method(&mut self, ty: &Type, name: &str, args: &[Expr], line: u32) -> Type {
        match name {
            "to_int" | "to_i64" => self.int_convert(name, args, line, Type::Int),
            "to_i8" => self.int_convert(name, args, line, Type::I8),
            "to_i16" => self.int_convert(name, args, line, Type::I16),
            "to_i32" => self.int_convert(name, args, line, Type::I32),
            "to_u8" => self.int_convert(name, args, line, Type::U8),
            "to_u16" => self.int_convert(name, args, line, Type::U16),
            "to_u32" => self.int_convert(name, args, line, Type::U32),
            "to_u64" => self.int_convert(name, args, line, Type::U64),
            "to_float" => self.no_args(name, args, line, Type::Float),
            "to_string" => self.no_args(name, args, line, Type::Str),
            "to_string_base" => {
                self.expect_args(args, &[Type::Int], name, line);
                Type::Str
            }
            "abs" => self.no_args(name, args, line, ty.clone()),
            "min" | "max" | "mod" => {
                self.expect_args(args, &[ty.clone()], name, line);
                ty.clone()
            }
            "pow" => {
                self.expect_args(args, &[Type::Int], name, line);
                ty.clone()
            }
            "clamp" => {
                self.expect_args(args, &[ty.clone(), ty.clone()], name, line);
                ty.clone()
            }
            _ => self.unknown_method(ty, name, line),
        }
    }

    /// 整数转换方法：可带一个进制参数（2..=36，缺省 10），用于把字符串按进制解析。
    fn int_convert(&mut self, name: &str, args: &[Expr], line: u32, ret: Type) -> Type {
        if !args.is_empty() {
            self.expect_args(args, &[Type::Int], name, line);
        }
        ret
    }

    fn float_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        match name {
            "to_int" => self.no_args(name, args, line, Type::Int),
            "to_string" => self.no_args(name, args, line, Type::Str),
            "to_float" => self.no_args(name, args, line, Type::Float),
            "abs" => self.no_args(name, args, line, Type::Float),
            "min" | "max" | "pow" => {
                self.expect_args(args, &[Type::Float], name, line);
                Type::Float
            }
            "clamp" => {
                self.expect_args(args, &[Type::Float, Type::Float], name, line);
                Type::Float
            }
            _ => self.unknown_method(&Type::Float, name, line),
        }
    }

    fn string_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let s = Type::Str;
        match name {
            "len" => self.no_args(name, args, line, Type::Int),
            "at" => {
                self.expect_args(args, &[Type::Int], name, line);
                Type::Result(Box::new(Type::Str), Box::new(Type::Str))
            }
            "slice" => {
                self.expect_args(args, &[Type::Int, Type::Int], name, line);
                Type::Result(Box::new(Type::Str), Box::new(Type::Str))
            }
            "first" | "last" => self.no_args(name, args, line, Type::Str),
            "contains" | "starts_with" | "ends_with" => {
                self.expect_args(args, &[s.clone()], name, line);
                Type::Bool
            }
            "index_of" | "last_index_of" => {
                self.expect_args(args, &[s.clone()], name, line);
                Type::Option(Box::new(Type::Int))
            }
            "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end" | "reverse"
            | "remove_whitespace" => self.no_args(name, args, line, Type::Str),
            // 模板插值会把每个 `${…}` 统一转成 `.to_string()`；字符串自身就地返回。
            "to_string" => self.no_args(name, args, line, Type::Str),
            "replace" | "replace_all" => {
                self.expect_args(args, &[s.clone(), s.clone()], name, line);
                Type::Str
            }
            "split" => {
                self.expect_args(args, &[s.clone()], name, line);
                Type::Vec(Box::new(Type::Str))
            }
            "split_lines" | "chars" => self.no_args(name, args, line, Type::Vec(Box::new(Type::Str))),
            "bytes" => self.no_args(name, args, line, Type::Vec(Box::new(Type::U8))),
            "filter" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![s.clone()], Box::new(Type::Bool))],
                    name,
                    line,
                );
                Type::Str
            }
            "keep_chars" | "remove_chars" => {
                self.expect_args(args, &[s.clone()], name, line);
                Type::Str
            }
            "to_int" => {
                self.int_convert(name, args, line, Type::Result(Box::new(Type::Int), Box::new(s)))
            }
            "to_i8" | "to_i16" | "to_i32" | "to_i64" | "to_u8" | "to_u16" | "to_u32"
            | "to_u64" => {
                let target = Type::from_int_suffix(
                    IntSuffix::from_name(name.trim_start_matches("to_")).unwrap_or(IntSuffix::I64),
                );
                self.int_convert(name, args, line, Type::Result(Box::new(target), Box::new(s)))
            }
            "to_float" => self.no_args(
                name,
                args,
                line,
                Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
            ),
            "to_bool" => self.no_args(
                name,
                args,
                line,
                Type::Result(Box::new(Type::Bool), Box::new(Type::Str)),
            ),
            "pad_start" | "pad_end" => {
                self.expect_args(args, &[Type::Int, Type::Str], name, line);
                Type::Str
            }
            "repeat" => {
                self.expect_args(args, &[Type::Int], name, line);
                Type::Str
            }
            "join" => {
                self.expect_args(args, &[Type::Vec(Box::new(Type::Str))], name, line);
                Type::Str
            }
            "clone" => self.no_args(name, args, line, Type::Str),
            "normalize" => {
                self.expect_args(args, &[s.clone()], name, line);
                Type::Result(Box::new(Type::Str), Box::new(Type::Str))
            }
            "graphemes" => self.no_args(name, args, line, Type::Vec(Box::new(Type::Str))),
            "grapheme_len" | "width" => self.no_args(name, args, line, Type::Int),
            _ => self.unknown_method(&Type::Str, name, line),
        }
    }

    fn vec_method(&mut self, t: Type, name: &str, args: &[Expr], line: u32) -> Type {
        let vec_ty = Type::Vec(Box::new(t.clone()));
        match name {
            "len" => self.no_args(name, args, line, Type::Int),
            "is_empty" => self.no_args(name, args, line, Type::Bool),
            "push" => {
                self.expect_args(args, &[t.clone()], name, line);
                Type::Void
            }
            "pop" => self.no_args(name, args, line, Type::Option(Box::new(t.clone()))),
            "insert" => {
                self.expect_args(args, &[Type::Int, t.clone()], name, line);
                Type::Void
            }
            "remove" => {
                self.expect_args(args, &[Type::Int], name, line);
                t.clone()
            }
            "clear" | "reverse" => self.no_args(name, args, line, Type::Void),
            "take" | "skip" => {
                self.expect_args(args, &[Type::Int], name, line);
                vec_ty.clone()
            }
            "sort" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![t.clone(), t.clone()], Box::new(Type::Int))],
                    name,
                    line,
                );
                Type::Void
            }
            "map" => {
                if args.len() != 1 {
                    self.error("map takes exactly 1 argument", line);
                    for arg in args {
                        self.check_expr(arg, None);
                    }
                    return Type::Vec(Box::new(Type::Unknown));
                }
                let fty = self.check_expr(&args[0], None);
                match self.table.resolve(&fty) {
                    Type::Func(params, ret) if params.len() == 1 && self.same_type(&params[0], &t) => {
                        Type::Vec(ret)
                    }
                    Type::Func(_, _) => {
                        self.error(
                            format!("vec.map callback must be func({}) -> U", t),
                            line,
                        );
                        Type::Vec(Box::new(Type::Unknown))
                    }
                    _ => {
                        self.error("vec.map requires a function", line);
                        Type::Vec(Box::new(Type::Unknown))
                    }
                }
            }
            "filter" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![t.clone()], Box::new(Type::Bool))],
                    name,
                    line,
                );
                vec_ty
            }
            "reduce" => {
                self.expect_args(
                    args,
                    &[
                        Type::Func(vec![t.clone(), t.clone()], Box::new(t.clone())),
                        t.clone(),
                    ],
                    name,
                    line,
                );
                t
            }
            "find" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![t.clone()], Box::new(Type::Bool))],
                    name,
                    line,
                );
                Type::Option(Box::new(t))
            }
            "any" | "all" | "count" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![t.clone()], Box::new(Type::Bool))],
                    name,
                    line,
                );
                if name == "count" {
                    Type::Int
                } else {
                    Type::Bool
                }
            }
            "slice" => {
                self.expect_args(args, &[Type::Int, Type::Int], name, line);
                vec_ty
            }
            "concat" => {
                self.expect_args(args, &[vec_ty.clone()], name, line);
                vec_ty
            }
            "unique" => self.no_args(name, args, line, vec_ty),
            "flatten" => match self.table.resolve(&t) {
                Type::Vec(inner) => Type::Vec(inner),
                other => {
                    self.error(
                        format!("flatten requires vec<vec<T>>, found vec<{}>", other),
                        line,
                    );
                    Type::Vec(Box::new(Type::Unknown))
                }
            },
            "join" => {
                if self.table.resolve(&t) != Type::Str {
                    self.error("join requires vec<string>", line);
                }
                self.expect_args(args, &[Type::Str], name, line);
                Type::Str
            }
            "chunk" => {
                self.expect_args(args, &[Type::Int], name, line);
                Type::Vec(Box::new(vec_ty))
            }
            "zip" => {
                if args.len() != 1 {
                    self.error("zip takes exactly 1 argument", line);
                }
                if let Some(arg) = args.first() {
                    let other = self.check_expr(arg, None);
                    if !matches!(self.table.resolve(&other), Type::Vec(_) | Type::Unknown) {
                        self.error("zip requires vec", line);
                    }
                }
                Type::VecDynamic
            }
            "at" => {
                self.expect_args(args, &[Type::Int], name, line);
                Type::Result(Box::new(t), Box::new(Type::Str))
            }
            "clone" => self.no_args(name, args, line, vec_ty),
            _ => self.unknown_method(&vec_ty, name, line),
        }
    }

    fn map_method(&mut self, k: Type, v: Type, name: &str, args: &[Expr], line: u32) -> Type {
        let map_ty = Type::Map(Box::new(k.clone()), Box::new(v.clone()));
        match name {
            "set" => {
                self.expect_args(args, &[k.clone(), v.clone()], name, line);
                Type::Void
            }
            "get" => {
                self.expect_args(args, &[k], name, line);
                Type::Option(Box::new(v))
            }
            "has" => {
                self.expect_args(args, &[k], name, line);
                Type::Bool
            }
            "delete" => {
                self.expect_args(args, &[k], name, line);
                Type::Option(Box::new(v))
            }
            "len" => self.no_args(name, args, line, Type::Int),
            "keys" => self.no_args(name, args, line, Type::Vec(Box::new(k))),
            "values" => self.no_args(name, args, line, Type::Vec(Box::new(v))),
            "items" => self.no_args(
                name,
                args,
                line,
                Type::Vec(Box::new(Type::Ary(vec![
                    ("key".to_string(), k),
                    ("value".to_string(), v),
                ]))),
            ),
            "merge" => {
                self.expect_args(args, &[map_ty], name, line);
                Type::Void
            }
            "filter" => {
                self.expect_args(
                    args,
                    &[Type::Func(vec![k.clone(), v.clone()], Box::new(Type::Bool))],
                    name,
                    line,
                );
                Type::Map(Box::new(k), Box::new(v))
            }
            "clone" => self.no_args(name, args, line, map_ty),
            _ => self.unknown_method(&map_ty, name, line),
        }
    }

    fn option_method(&mut self, t: Type, name: &str, args: &[Expr], line: u32) -> Type {
        let opt = Type::Option(Box::new(t.clone()));
        match name {
            "unwrap_or" => {
                self.expect_args(args, &[t.clone()], name, line);
                t
            }
            "is_some" | "is_none" => self.no_args(name, args, line, Type::Bool),
            "unwrap" => self.no_args(name, args, line, t),
            "clone" => self.no_args(name, args, line, opt.clone()),
            _ => self.unknown_method(&opt, name, line),
        }
    }

    fn result_method(&mut self, t: Type, e: Type, name: &str, args: &[Expr], line: u32) -> Type {
        let res = Type::Result(Box::new(t.clone()), Box::new(e.clone()));
        match name {
            "unwrap_or" => {
                self.expect_args(args, &[t.clone()], name, line);
                t
            }
            "is_ok" | "is_err" => self.no_args(name, args, line, Type::Bool),
            "unwrap" => self.no_args(name, args, line, t),
            "unwrap_err" => self.no_args(name, args, line, e),
            "clone" => self.no_args(name, args, line, res.clone()),
            _ => self.unknown_method(&res, name, line),
        }
    }

    fn any_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let ret = match name {
            "as_string" => Type::Option(Box::new(Type::Str)),
            "as_int" => Type::Option(Box::new(Type::Int)),
            "as_float" => Type::Option(Box::new(Type::Float)),
            "as_bool" => Type::Option(Box::new(Type::Bool)),
            "as_vec" => Type::Option(Box::new(Type::VecDynamic)),
            "as_map" => Type::Option(Box::new(Type::Map(
                Box::new(Type::Str),
                Box::new(Type::Any),
            ))),
            _ => return self.unknown_method(&Type::Any, name, line),
        };
        self.no_args(name, args, line, ret)
    }

    fn llm_client_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let response = bi::result_of(bi::named("llm.response"), bi::string());
        match name {
            "chat" => {
                self.expect_args(
                    args,
                    &[bi::vec_of(bi::llm_message()), bi::llm_options()],
                    name,
                    line,
                );
                response
            }
            "chat_stream" => {
                self.expect_args(
                    args,
                    &[
                        bi::vec_of(bi::llm_message()),
                        bi::llm_options(),
                        Type::Func(vec![bi::string()], Box::new(bi::void())),
                    ],
                    name,
                    line,
                );
                response
            }
            "chat_json" => {
                if args.len() != 3 {
                    self.error("chat_json takes 3 arguments", line);
                }
                for arg in args.iter().take(2) {
                    self.check_expr(arg, None);
                }
                if let Some(third) = args.get(2) {
                    self.check_expr(third, Some(&Type::Any));
                }
                bi::result_of(bi::any(), bi::string())
            }
            _ => self.unknown_method(&bi::named("llm.client"), name, line),
        }
    }

    fn redis_conn_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let text = bi::result_of(bi::string(), bi::string());
        let flag = bi::result_of(bi::boolean(), bi::string());
        let count = bi::result_of(bi::int(), bi::string());
        let optional = bi::result_of(bi::opt_of(bi::string()), bi::string());
        let any = bi::result_of(bi::any(), bi::string());
        match name {
            "ping" => self.no_args(name, args, line, text),
            "get" | "lpop" | "rpop" | "hget" => {
                self.expect_args(args, &[bi::string()], name, line);
                optional
            }
            "brpop" => {
                self.expect_args(args, &[bi::string(), bi::int()], name, line);
                optional
            }
            "set" | "setnx" | "hset" | "lpush" | "rpush" => {
                self.expect_args(args, &[bi::string(), bi::string()], name, line);
                flag
            }
            "setex" => {
                self.expect_args(args, &[bi::string(), bi::int(), bi::string()], name, line);
                flag
            }
            "expire" => {
                self.expect_args(args, &[bi::string(), bi::int()], name, line);
                flag
            }
            "del" | "hdel" => {
                self.expect_args(args, &[bi::string()], name, line);
                count
            }
            "exists" => {
                self.expect_args(args, &[bi::string()], name, line);
                flag
            }
            "incr" | "decr" | "llen" => {
                self.expect_args(args, &[bi::string()], name, line);
                count
            }
            "eval" => {
                self.expect_args(
                    args,
                    &[
                        bi::string(),
                        bi::vec_of(bi::string()),
                        bi::vec_of(bi::string()),
                    ],
                    name,
                    line,
                );
                any
            }
            "close" => self.no_args(name, args, line, bi::void()),
            _ => self.unknown_method(&bi::redis_conn(), name, line),
        }
    }

    fn sync_mutex_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        match name {
            "lock" | "unlock" => self.no_args(name, args, line, bi::void()),
            "try_lock" | "is_locked" => self.no_args(name, args, line, bi::boolean()),
            "lock_timeout" => {
                self.expect_args(args, &[bi::int()], name, line);
                bi::boolean()
            }
            "name" => self.no_args(name, args, line, bi::string()),
            _ => self.unknown_method(&bi::sync_mutex(), name, line),
        }
    }

    fn sync_atomic_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        match name {
            "load" => self.no_args(name, args, line, bi::int()),
            "store" => {
                self.expect_args(args, &[bi::int()], name, line);
                bi::void()
            }
            "add" | "sub" | "swap" => {
                self.expect_args(args, &[bi::int()], name, line);
                bi::int()
            }
            "compare_swap" => {
                self.expect_args(args, &[bi::int(), bi::int()], name, line);
                bi::boolean()
            }
            "name" => self.no_args(name, args, line, bi::string()),
            _ => self.unknown_method(&bi::sync_atomic(), name, line),
        }
    }

    fn sync_channel_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let item = bi::opt_of(bi::any());
        match name {
            "send" => {
                self.expect_args(args, &[bi::any()], name, line);
                bi::void()
            }
            "try_send" => {
                self.expect_args(args, &[bi::any()], name, line);
                bi::boolean()
            }
            "send_timeout" => {
                self.expect_args(args, &[bi::any(), bi::int()], name, line);
                bi::boolean()
            }
            "recv" | "try_recv" => self.no_args(name, args, line, item),
            "recv_timeout" => {
                self.expect_args(args, &[bi::int()], name, line);
                item
            }
            "close" => self.no_args(name, args, line, bi::void()),
            "is_closed" => self.no_args(name, args, line, bi::boolean()),
            "len" | "capacity" => self.no_args(name, args, line, bi::int()),
            "name" => self.no_args(name, args, line, bi::string()),
            _ => self.unknown_method(&bi::sync_channel(), name, line),
        }
    }

    fn db_conn_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let affected = bi::result_of(bi::int(), bi::string());
        match name {
            "execute" => {
                self.db_call_args(args, name, line);
                affected
            }
            "execute_many" => {
                self.db_call_args(args, name, line);
                affected
            }
            "query" => {
                self.db_call_args(args, name, line);
                bi::result_of(bi::db_rows(), bi::string())
            }
            "query_one" => {
                self.db_call_args(args, name, line);
                bi::result_of(
                    bi::opt_of(bi::map_of(bi::string(), bi::any())),
                    bi::string(),
                )
            }
            "row_json" => {
                self.db_call_args(args, name, line);
                bi::result_of(bi::string(), bi::string())
            }
            "last_insert_id" => self.no_args(name, args, line, bi::int()),
            "begin" | "commit" | "rollback" => self.no_args(name, args, line, affected),
            "close" => self.no_args(name, args, line, bi::void()),
            _ => self.unknown_method(&bi::db_conn(), name, line),
        }
    }

    fn db_call_args(&mut self, args: &[Expr], name: &str, line: u32) {
        if args.is_empty() || args.len() > 2 {
            self.error(
                format!(
                    "method '{}' expects 1 or 2 arguments, found {}",
                    name,
                    args.len()
                ),
                line,
            );
        }
        if let Some(first) = args.first() {
            let actual = self.check_expr(first, None);
            if !self.is_assignable(&actual, &bi::string()) {
                self.error(
                    format!(
                        "argument 1 of '{}' expected string, found {}",
                        name, actual
                    ),
                    line,
                );
            }
        }
        if let Some(params) = args.get(1) {
            self.check_expr(params, Some(&bi::any()));
        }
    }

    fn db_query_method(&mut self, name: &str, args: &[Expr], line: u32) -> Type {
        let affected = bi::result_of(bi::int(), bi::string());
        let rows = bi::result_of(bi::db_rows(), bi::string());
        let row = bi::result_of(
            bi::opt_of(bi::map_of(bi::string(), bi::any())),
            bi::string(),
        );
        match name {
            "field" => {
                self.expect_args(args, &[bi::string()], name, line);
                bi::db_query()
            }
            "where" => {
                self.expect_args(
                    args,
                    &[bi::string(), bi::string(), bi::any()],
                    name,
                    line,
                );
                bi::db_query()
            }
            "where_in" => {
                self.expect_args(args, &[bi::string(), bi::any()], name, line);
                bi::db_query()
            }
            "where_null" => {
                self.expect_args(args, &[bi::string()], name, line);
                bi::db_query()
            }
            "order" => {
                self.expect_args(args, &[bi::string(), bi::string()], name, line);
                bi::db_query()
            }
            "limit" | "offset" => {
                self.expect_args(args, &[bi::int()], name, line);
                bi::db_query()
            }
            "join" => {
                self.expect_args(args, &[bi::string(), bi::string()], name, line);
                bi::db_query()
            }
            "select" => self.no_args(name, args, line, rows),
            "find" => self.no_args(name, args, line, row),
            "count" => self.no_args(
                name,
                args,
                line,
                bi::result_of(bi::int(), bi::string()),
            ),
            "value" => {
                self.expect_args(args, &[bi::string()], name, line);
                bi::result_of(bi::opt_of(bi::any()), bi::string())
            }
            "column" => {
                self.expect_args(args, &[bi::string()], name, line);
                bi::result_of(bi::vec_of(bi::any()), bi::string())
            }
            "insert" | "update" => {
                self.expect_args(args, &[bi::any()], name, line);
                affected
            }
            "delete" => self.no_args(name, args, line, affected),
            "to_sql" => self.no_args(name, args, line, bi::string()),
            "clone" => self.no_args(name, args, line, bi::db_query()),
            _ => self.unknown_method(&bi::db_query(), name, line),
        }
    }

    fn no_args(&mut self, name: &str, args: &[Expr], line: u32, ret: Type) -> Type {
        if !args.is_empty() {
            self.error(format!("method '{}' takes no arguments", name), line);
            for arg in args {
                self.check_expr(arg, None);
            }
        }
        ret
    }

    fn expect_args(&mut self, args: &[Expr], expected: &[Type], name: &str, line: u32) -> bool {
        if args.len() != expected.len() {
            self.error(
                format!(
                    "method '{}' expects {} arguments, found {}",
                    name,
                    expected.len(),
                    args.len()
                ),
                line,
            );
        }
        for (i, arg) in args.iter().enumerate() {
            if let Some(exp) = expected.get(i) {
                let actual = self.check_expr(arg, Some(exp));
                if !self.is_assignable(&actual, exp) {
                    self.error(
                        format!(
                            "argument {} of '{}' expected {}, found {}",
                            i + 1,
                            name,
                            exp,
                            actual
                        ),
                        arg.line,
                    );
                }
            } else {
                self.check_expr(arg, None);
            }
        }
        args.len() == expected.len()
    }

    fn unknown_method(&mut self, ty: &Type, name: &str, line: u32) -> Type {
        self.error(format!("type {} has no method '{}'", ty, name), line);
        Type::Unknown
    }

    fn binary_type(&mut self, whole: &Expr, op: BinOp, left: &Expr, right: &Expr) -> Type {
        let left_ty = self.check_expr(left, None);
        let resolved_left = self.table.resolve(&left_ty);
        let right_ty = self.check_expr(right, Some(&resolved_left));
        let resolved_right = self.table.resolve(&right_ty);

        if matches!(op, BinOp::And | BinOp::Or) {
            if !matches!(resolved_left, Type::Bool | Type::Unknown)
                || !matches!(resolved_right, Type::Bool | Type::Unknown)
            {
                self.error(
                    format!(
                        "logical operator requires bool, found {} and {}",
                        resolved_left, resolved_right
                    ),
                    whole.line,
                );
                return Type::Unknown;
            }
            return Type::Bool;
        }

        if matches!(op, BinOp::Eq | BinOp::Ne) {
            if !self.same_type(&resolved_left, &resolved_right) {
                self.error(
                    format!(
                        "comparison requires same types, found {} and {}",
                        resolved_left, resolved_right
                    ),
                    whole.line,
                );
                return Type::Unknown;
            }
            return Type::Bool;
        }

        if matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) {
            if !self.same_type(&resolved_left, &resolved_right) {
                self.error(
                    format!(
                        "comparison requires same types, found {} and {}",
                        resolved_left, resolved_right
                    ),
                    whole.line,
                );
                return Type::Unknown;
            }
            if !matches!(
                resolved_left,
                Type::Float | Type::Str | Type::Unknown
            ) && !resolved_left.is_int()
            {
                self.error(
                    format!("ordered comparison not supported for {}", resolved_left),
                    whole.line,
                );
                return Type::Unknown;
            }
            return Type::Bool;
        }

        if matches!(resolved_left, Type::Unknown) || matches!(resolved_right, Type::Unknown) {
            return Type::Unknown;
        }

        if op == BinOp::Add && resolved_left == Type::Str && resolved_right == Type::Str {
            return Type::Str;
        }

        if !self.same_type(&resolved_left, &resolved_right) {
            self.error(
                format!(
                    "operator requires same types, found {} and {}",
                    resolved_left, resolved_right
                ),
                whole.line,
            );
            return Type::Unknown;
        }

        if resolved_left.is_int() || matches!(resolved_left, Type::Float) {
            return resolved_left;
        }

        self.error(
            format!(
                "operator does not support type {}",
                resolved_left
            ),
            whole.line,
        );
        Type::Unknown
    }

    fn list_type(&mut self, items: &[ListItem], expected: Option<&Type>, line: u32) -> Type {
        let raw = expected.cloned();
        let exp = expected.map(|t| {
            let r = self.table.resolve(t);
            if let Type::Option(inner) = &r {
                if self.table.resolve(inner) == Type::Any {
                    return Type::Any;
                }
            }
            r
        });

        let inferred = match exp {
            Some(Type::Ary(fields)) => self.ary_literal(items, &fields, line),
            Some(Type::Map(k, v)) => self.map_literal(items, &k, &v, line),
            Some(Type::Vec(t)) => self.vec_literal(items, &t, line),
            Some(Type::VecHetero(ts)) => self.vec_hetero_literal(items, &ts, line),
            Some(Type::VecDynamic) => {
                self.dynamic_vec_literal(items, line);
                Type::VecDynamic
            }
            Some(Type::Any) | None => self.infer_literal(items, line),
            Some(other) => {
                let inferred = self.infer_literal(items, line);
                if !self.is_assignable(&inferred, &other) {
                    self.error(
                        format!("literal cannot be used as {}; found {}", other, inferred),
                        line,
                    );
                }
                inferred
            }
        };
        // `let c: circle = [radius = 1.0]` 记录为 `circle` 而不是展开后的结构：
        // 运行时据此写入类名，接口调用才能按类名分派。
        if let Some(Type::Named(name)) = raw {
            if self.table.defs.contains_key(&name)
                && self.is_assignable(&inferred, &Type::Named(name.clone()))
            {
                return Type::Named(name);
            }
        }
        inferred
    }

    fn ary_literal(&mut self, items: &[ListItem], fields: &[(String, Type)], line: u32) -> Type {
        let mut seen: Vec<String> = Vec::new();
        for item in items {
            match item {
                ListItem::Named(name, value) => {
                    seen.push(name.clone());
                    match fields.iter().find(|(n, _)| n == name) {
                        Some((_, ty)) => {
                            let actual = self.check_expr(value, Some(ty));
                            if !self.is_assignable(&actual, ty) {
                                self.error(
                                    format!(
                                        "field '{}' expected {}, found {}",
                                        name, ty, actual
                                    ),
                                    value.line,
                                );
                            }
                        }
                        None => self.error(
                            format!("type has no field '{}'", name),
                            value.line,
                        ),
                    }
                }
                ListItem::Key(..) | ListItem::Value(_) | ListItem::Typed(..) => {
                    self.error(
                        "ary literal requires named fields: [field = value, ...]",
                        line,
                    );
                }
            }
        }
        for (name, _) in fields {
            if !seen.iter().any(|n| n == name) {
                self.error(format!("missing field '{}' in ary literal", name), line);
            }
        }
        Type::Ary(fields.to_vec())
    }

    fn map_literal(&mut self, items: &[ListItem], k: &Type, v: &Type, line: u32) -> Type {
        for item in items {
            match item {
                ListItem::Named(_name, value) => {
                    if self.table.resolve(k) != Type::Str {
                        self.error("map literal without quoted keys requires string keys", line);
                    }
                    self.check_expr(value, Some(v));
                }
                ListItem::Key(key, value) => {
                    let key_ty = self.check_expr(key, Some(k));
                    if !self.is_assignable(&key_ty, k) {
                        self.error(
                            format!("map key expected {}, found {}", k, key_ty),
                            key.line,
                        );
                    }
                    let value_ty = self.check_expr(value, Some(v));
                    if !self.is_assignable(&value_ty, v) {
                        self.error(
                            format!("map value expected {}, found {}", v, value_ty),
                            value.line,
                        );
                    }
                }
                ListItem::Value(_) | ListItem::Typed(..) => {
                    self.error("map literal requires key = value entries", line);
                }
            }
        }
        Type::Map(Box::new(k.clone()), Box::new(v.clone()))
    }

    fn vec_literal(&mut self, items: &[ListItem], t: &Type, line: u32) -> Type {
        for item in items {
            match item {
                ListItem::Value(value) => {
                    let actual = self.check_expr(value, Some(t));
                    if !self.is_assignable(&actual, t) {
                        self.error(
                            format!("vec element expected {}, found {}", t, actual),
                            value.line,
                        );
                    }
                }
                ListItem::Typed(te, value) => {
                    let declared = type_from_expr(te);
                    if !self.same_type(&declared, t) {
                        self.error(
                            format!(
                                "typed element declares {} but vec element is {}",
                                declared, t
                            ),
                            value.line,
                        );
                    }
                    self.check_expr(value, Some(t));
                }
                ListItem::Named(..) | ListItem::Key(..) => {
                    self.error("vec literal does not allow named elements", line);
                }
            }
        }
        Type::Vec(Box::new(t.clone()))
    }

    fn vec_hetero_literal(&mut self, items: &[ListItem], ts: &[Type], line: u32) -> Type {
        if items.len() != ts.len() {
            self.error(
                format!(
                    "heterogeneous vec requires {} elements, found {}",
                    ts.len(),
                    items.len()
                ),
                line,
            );
        }
        for (i, item) in items.iter().enumerate() {
            if let Some(t) = ts.get(i) {
                match item {
                    ListItem::Value(value) => {
                        self.check_expr(value, Some(t));
                    }
                    ListItem::Typed(te, value) => {
                        let declared = type_from_expr(te);
                        if !self.same_type(&declared, t) {
                            self.error(
                                format!(
                                    "typed element declares {} but position {} is {}",
                                    declared,
                                    i,
                                    t
                                ),
                                value.line,
                            );
                        }
                        self.check_expr(value, Some(t));
                    }
                    ListItem::Named(..) | ListItem::Key(..) => {
                        self.error("vec literal does not allow named elements", line);
                    }
                }
            }
        }
        Type::VecHetero(ts.to_vec())
    }

    fn dynamic_vec_literal(&mut self, items: &[ListItem], line: u32) {
        for item in items {
            match item {
                ListItem::Typed(te, value) => {
                    let declared = type_from_expr(te);
                    self.validate_type(&declared, value.line);
                    self.check_expr(value, Some(&declared));
                }
                _ => self.error(
                    "bare vec literal requires explicit element types: [:int = 1, ...]",
                    line,
                ),
            }
        }
    }

    fn infer_literal(&mut self, items: &[ListItem], line: u32) -> Type {
        if items.is_empty() {
            self.error("cannot infer type of empty literal; annotate the variable", line);
            return Type::Unknown;
        }
        let all_named = items
            .iter()
            .all(|i| matches!(i, ListItem::Named(..) | ListItem::Key(..)));
        if all_named {
            // 推断键类型：`[name = value]` 的键是字符串，`[1 = value]` 是整数键；
            // 混用不同键类型或使用不可比较的键（数组 / 记录 / 函数）直接报错。
            let mut key: Option<Type> = None;
            for item in items {
                match item {
                    ListItem::Named(_, value) => {
                        self.check_expr(value, None);
                        key = self.merge_map_key(key, Type::Str, line);
                    }
                    ListItem::Key(key_expr, value) => {
                        let ty = self.check_expr(key_expr, None);
                        let resolved = self.table.resolve(&ty);
                        if !is_map_key_type(&resolved) {
                            self.error(
                                format!("map keys must be string, int, float or bool, found {}", ty),
                                key_expr.line,
                            );
                            key = Some(Type::Unknown);
                        }
                        self.check_expr(value, None);
                        key = self.merge_map_key(key, ty, line);
                    }
                    _ => {}
                }
            }
            return Type::Map(
                Box::new(key.unwrap_or(Type::Str)),
                Box::new(Type::Any),
            );
        }
        if items.iter().all(|i| matches!(i, ListItem::Typed(..))) {
            self.dynamic_vec_literal(items, line);
            return Type::VecDynamic;
        }
        if items.iter().any(|i| matches!(i, ListItem::Typed(..))) {
            self.error("cannot mix typed and shorthand vec elements", line);
            return Type::Unknown;
        }
        let mut elem: Option<Type> = None;
        for item in items {
            if let ListItem::Value(value) = item {
                let ty = self.check_expr(value, None);
                match &elem {
                    None => elem = Some(ty),
                    Some(prev) if self.same_type(prev, &ty) => {}
                    Some(prev) => {
                        self.error(
                            format!(
                                "vec elements have mixed types {} and {}; use explicit element types",
                                prev, ty
                            ),
                            value.line,
                        );
                        return Type::Unknown;
                    }
                }
            }
        }
        match elem {
            Some(t) => Type::Vec(Box::new(t)),
            None => Type::Unknown,
        }
    }

    /// 合并 map 字面量的键类型：不同键类型视为错误。
    fn merge_map_key(&mut self, current: Option<Type>, next: Type, line: u32) -> Option<Type> {
        match current {
            None => Some(next),
            Some(previous) if matches!(previous, Type::Unknown) => Some(next),
            Some(previous) if self.same_type(&previous, &next) => Some(previous),
            Some(previous) => {
                self.error(
                    format!(
                        "map keys have mixed types {} and {}; annotate the value to disambiguate",
                        previous, next
                    ),
                    line,
                );
                Some(Type::Unknown)
            }
        }
    }

    fn closure_type(
        &mut self,
        whole: &Expr,
        params: &[Param],
        ret_expr: &TypeExpr,
        body: &[Stmt],
        expected: Option<&Type>,
    ) -> Type {
        let ret = type_from_expr(ret_expr);
        self.validate_type(&ret, whole.line);
        let mut param_types = Vec::new();
        for p in params {
            let ty = type_from_expr(&p.ty);
            self.validate_type(&ty, p.line);
            param_types.push(ty);
        }
        if let Some(exp) = expected {
            if let Type::Func(expected_params, expected_ret) = self.table.resolve(exp) {
                if expected_params.len() != param_types.len() {
                    self.error(
                        format!(
                            "closure expects {} parameters, found {}",
                            expected_params.len(),
                            param_types.len()
                        ),
                        whole.line,
                    );
                }
                for (i, (actual, expected)) in
                    param_types.iter().zip(expected_params.iter()).enumerate()
                {
                    if !self.same_type(actual, expected) {
                        self.error(
                            format!(
                                "closure parameter {} expected {}, found {}",
                                i + 1,
                                expected,
                                actual
                            ),
                            whole.line,
                        );
                    }
                }
                if !self.is_assignable(&ret, &expected_ret) {
                    self.error(
                        format!(
                            "closure return type expected {}, found {}",
                            expected_ret, ret
                        ),
                        whole.line,
                    );
                }
            }
        }

        self.scopes.push(HashMap::new());
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.warn_name(&p.name, NameKind::Variable, p.line);
            self.declare(&p.name, ty.clone(), p.mode == ParamMode::Write, p.line);
        }
        let result_err = match self.table.resolve(&ret) {
            Type::Result(_, e) => Some(*e),
            _ => None,
        };
        self.returns.push(ReturnCtx {
            ret: ret.clone(),
            result_err,
        });
        let saved_loop = self.loop_depth;
        self.loop_depth = 0;
        self.check_stmts(body);
        self.loop_depth = saved_loop;
        self.returns.pop();
        self.end_scope();

        Type::Func(param_types, Box::new(ret))
    }

    fn struct_type(&mut self, path: &[String], fields: &[(String, Expr)], line: u32) -> Type {
        let name = path.join(".");
        // `枚举.变体[字段 = 值]`：精确的结构类型名优先（允许 `type a.b = [...]`）。
        if path.len() == 2
            && !self.table.defs.contains_key(&name)
            && bi::builtin_type(&name).is_none()
        {
            if let Some(info) = self.enums.get(&path[0]).cloned() {
                let enum_name = path[0].clone();
                let variant_name = path[1].clone();
                let variant = match info
                    .variants
                    .iter()
                    .find(|variant| variant.name == variant_name)
                {
                    Some(variant) => variant.clone(),
                    None => {
                        self.error(
                            format!("enum '{}' has no variant '{}'", enum_name, variant_name),
                            line,
                        );
                        return Type::Unknown;
                    }
                };
                let mut seen: Vec<String> = Vec::new();
                for (field_name, value) in fields {
                    seen.push(field_name.clone());
                    match variant.fields.iter().find(|(n, _)| n == field_name) {
                        Some((_, ty)) => {
                            let actual = self.check_expr(value, Some(ty));
                            if !self.is_assignable(&actual, ty) {
                                self.error(
                                    format!(
                                        "field '{}' of '{}.{}' expected {}, found {}",
                                        field_name, enum_name, variant_name, ty, actual
                                    ),
                                    value.line,
                                );
                            }
                        }
                        None => self.error(
                            format!(
                                "variant '{}.{}' has no field '{}'",
                                enum_name, variant_name, field_name
                            ),
                            value.line,
                        ),
                    }
                }
                for (field_name, _) in &variant.fields {
                    if !seen.iter().any(|seen| seen == field_name) {
                        self.error(
                            format!(
                                "missing field '{}' in '{}.{}' literal",
                                field_name, enum_name, variant_name
                            ),
                            line,
                        );
                    }
                }
                return Type::Named(enum_name);
            }
        }
        let named = if path.len() == 1 {
            Type::Named(name.clone())
        } else {
            Type::Named(name.clone())
        };
        let resolved = self.table.resolve(&named);
        let ary_fields = match &resolved {
            Type::Ary(fields) => fields.clone(),
            other => {
                self.error(
                    format!("\"{}\" is not a struct type (found {})", name, other),
                    line,
                );
                return Type::Unknown;
            }
        };
        let is_response = name == "http.response";
        let mut seen: Vec<String> = Vec::new();
        for (field_name, value) in fields {
            seen.push(field_name.clone());
            match ary_fields.iter().find(|(n, _)| n == field_name) {
                Some((_, ty)) => {
                    let actual = self.check_expr(value, Some(ty));
                    if !self.is_assignable(&actual, ty) {
                        self.error(
                            format!(
                                "field '{}' expected {}, found {}",
                                field_name, ty, actual
                            ),
                            value.line,
                        );
                    }
                }
                None => self.error(
                    format!("type {} has no field '{}'", name, field_name),
                    value.line,
                ),
            }
        }
        if is_response {
            let exclusive = ["body", "json", "sse"]
                .iter()
                .filter(|f| seen.iter().any(|s| s == *f))
                .count();
            if exclusive > 1 {
                self.error(
                    "http.response body/json/sse are mutually exclusive",
                    line,
                );
            }
        } else {
            for (field_name, _) in &ary_fields {
                if !seen.iter().any(|s| s == field_name) {
                    self.error(
                        format!("missing field '{}' in {} literal", field_name, name),
                        line,
                    );
                }
            }
        }
        // 用户声明的类型保留名字：方法分派与接口实现判定都依赖命名类型身份。
        if self.table.defs.contains_key(&name) {
            named
        } else {
            resolved
        }
    }

    fn is_assignable(&self, actual: &Type, expected: &Type) -> bool {
        self.assignable_inner(actual, expected, 0)
    }

    /// 赋值兼容性（含协变到 `any` 与递归类型的结构化比较）。
    fn assignable_inner(&self, actual: &Type, expected: &Type, depth: usize) -> bool {
        if depth > 32 {
            return true;
        }
        // 受接口约束的类型参数可以当作该接口使用（`S: shape` → 可传给 `shape` 参数）。
        if let (Type::Named(param), Type::Named(target)) = (actual, expected) {
            if self
                .current_type_bounds
                .get(param)
                .map(|bound| bound == target)
                .unwrap_or(false)
            {
                return true;
            }
        }
        // 具体类型 → 接口：隐式实现（拥有接口要求的全部同名同签名方法）。
        if let Type::Named(target) = expected {
            if self.interfaces.contains_key(target) {
                if let Type::Named(owner) = actual {
                    if owner != target && self.implements(owner, target) {
                        return true;
                    }
                }
            }
        }
        let a = self.table.resolve(actual);
        let b = self.table.resolve(expected);
        if matches!(a, Type::Unknown) || matches!(b, Type::Unknown) {
            return true;
        }
        if b == Type::Any {
            return true;
        }
        // `option<any>` 接受任意值（写入时自动包成 some），与既有的 json 字段约定一致。
        if let Type::Option(inner) = &b {
            if self.table.resolve(inner) == Type::Any {
                return true;
            }
        }
        if a == b {
            return true;
        }
        match (&a, &b) {
            (Type::Vec(x), Type::Vec(y))
            | (Type::Option(x), Type::Option(y))
            | (Type::Share(x), Type::Share(y))
            | (Type::Task(x), Type::Task(y)) => self.assignable_inner(x, y, depth + 1),
            (Type::Map(ak, av), Type::Map(bk, bv))
            | (Type::Result(ak, av), Type::Result(bk, bv)) => {
                self.assignable_inner(ak, bk, depth + 1)
                    && self.assignable_inner(av, bv, depth + 1)
            }
            (Type::Func(ap, ar), Type::Func(bp, br)) => {
                ap.len() == bp.len()
                    && ap
                        .iter()
                        .zip(bp.iter())
                        .all(|(x, y)| self.assignable_inner(x, y, depth + 1))
                    && self.assignable_inner(ar, br, depth + 1)
            }
            // 递归类型展开层数可能不同：退化为结构比较。
            _ => self.structurally_equal(&a, &b, 0),
        }
    }

    /// 结构相等：同名类型别名视为相等，容器逐层比较，深度上限防止无限展开。
    fn structurally_equal(&self, a: &Type, b: &Type, depth: usize) -> bool {
        if depth > 32 {
            return true;
        }
        if a == b {
            return true;
        }
        match (a, b) {
            (Type::Named(x), Type::Named(y)) => x == y,
            (Type::Ary(fa), Type::Ary(fb)) => {
                fa.len() == fb.len()
                    && fa.iter().all(|(name, ty)| {
                        fb.iter()
                            .find(|(other, _)| other == name)
                            .is_some_and(|(_, other)| {
                                self.structurally_equal(ty, other, depth + 1)
                            })
                    })
            }
            (Type::Vec(x), Type::Vec(y))
            | (Type::Option(x), Type::Option(y))
            | (Type::Share(x), Type::Share(y))
            | (Type::Task(x), Type::Task(y)) => self.structurally_equal(x, y, depth + 1),
            (Type::Map(ak, av), Type::Map(bk, bv))
            | (Type::Result(ak, av), Type::Result(bk, bv)) => {
                self.structurally_equal(ak, bk, depth + 1)
                    && self.structurally_equal(av, bv, depth + 1)
            }
            (Type::Func(ap, ar), Type::Func(bp, br)) => {
                ap.len() == bp.len()
                    && ap
                        .iter()
                        .zip(bp.iter())
                        .all(|(x, y)| self.structurally_equal(x, y, depth + 1))
                    && self.structurally_equal(ar, br, depth + 1)
            }
            // 一侧还是别名：展开一层再比。
            (Type::Named(name), other) => {
                let expanded = self.table.resolve(&Type::Named(name.clone()));
                if &expanded == a {
                    false
                } else {
                    self.structurally_equal(&expanded, other, depth + 1)
                }
            }
            (other, Type::Named(name)) => {
                let expanded = self.table.resolve(&Type::Named(name.clone()));
                if &expanded == b {
                    false
                } else {
                    self.structurally_equal(other, &expanded, depth + 1)
                }
            }
            _ => false,
        }
    }

    fn same_type(&self, a: &Type, b: &Type) -> bool {
        let ra = self.table.resolve(a);
        let rb = self.table.resolve(b);
        if matches!(ra, Type::Unknown) || matches!(rb, Type::Unknown) {
            return true;
        }
        ra == rb || self.structurally_equal(&ra, &rb, 0)
    }

    fn expected_kind(&self, expected: Option<&Type>) -> Option<Type> {
        expected.map(|t| self.table.resolve(t))
    }

    /// `owner` 是否实现了接口 `iface`：方法名、接收者模式、参数与返回类型全部匹配。
    fn implements(&self, owner: &str, iface: &str) -> bool {
        let (Some(required), Some(available)) =
            (self.interfaces.get(iface), self.methods.get(owner))
        else {
            return false;
        };
        required.iter().all(|method| {
            let Some(full) = available.get(&method.name) else {
                return false;
            };
            let Some(sig) = self.funcs.get(full) else {
                return false;
            };
            sig.params.len() == method.params.len() + 1
                && sig.params[0].mode == method.mode
                && self.same_type(&sig.ret, &method.ret)
                && sig.params[1..]
                    .iter()
                    .zip(&method.params)
                    .all(|(param, expected)| self.same_type(&param.ty, expected))
        })
    }

    fn validate_type(&mut self, ty: &Type, line: u32) {
        self.validate_type_depth(ty, line, 0);
    }

    fn validate_type_depth(&mut self, ty: &Type, line: u32, depth: usize) {
        if depth > 32 {
            return;
        }
        match ty {
            Type::Named(name) => {
                // `pkg.type` 形式也算使用了该 import。
                if let Some((prefix, _)) = name.split_once('.') {
                    let prefix = prefix.to_string();
                    if self.imports.contains_key(&prefix) {
                        self.used_imports.insert(prefix);
                    }
                }
                if !self.table.is_known_named(name)
                    && !self.interfaces.contains_key(name)
                    && !self.enums.contains_key(name)
                    && !self.current_type_params.iter().any(|param| param == name)
                {
                    self.error(format!("unknown type '{}'", name), line);
                }
            }
            Type::Ary(fields) => {
                for (_, t) in fields {
                    self.validate_type_depth(t, line, depth + 1);
                }
            }
            Type::Vec(t) | Type::Option(t) | Type::Share(t) | Type::Task(t) => {
                self.validate_type_depth(t, line, depth + 1);
            }
            Type::VecHetero(ts) => {
                for t in ts {
                    self.validate_type_depth(t, line, depth + 1);
                }
            }
            Type::Map(k, v) | Type::Result(k, v) => {
                self.validate_type_depth(k, line, depth + 1);
                self.validate_type_depth(v, line, depth + 1);
            }
            Type::Func(args, ret) => {
                for t in args {
                    self.validate_type_depth(t, line, depth + 1);
                }
                self.validate_type_depth(ret, line, depth + 1);
            }
            _ => {}
        }
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn declare(&mut self, name: &str, ty: Type, mutable: bool, line: u32) {
        self.declare_with(name, ty, mutable, line, false);
    }

    /// `let` 局部变量：参与「未使用变量」告警。
    fn declare_let(&mut self, name: &str, ty: Type, mutable: bool, line: u32) {
        self.declare_with(name, ty, mutable, line, true);
    }

    fn declare_with(&mut self, name: &str, ty: Type, mutable: bool, line: u32, check_unused: bool) {
        let duplicate = match self.scopes.last() {
            Some(scope) => scope.contains_key(name),
            None => return,
        };
        if duplicate {
            self.error(
                format!("variable '{}' is already defined in this scope", name),
                line,
            );
            return;
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                VarInfo {
                    ty,
                    mutable,
                    line,
                    used: false,
                    check_unused,
                },
            );
        }
    }

    /// 标记变量已被读取（赋值目标不算读取）。
    fn mark_used(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.get_mut(name) {
                info.used = true;
                return;
            }
        }
    }

    /// 结束作用域并报告未使用的局部变量。
    fn end_scope(&mut self) {
        let scope = match self.scopes.pop() {
            Some(scope) => scope,
            None => return,
        };
        let mut unused: Vec<(u32, String)> = scope
            .into_iter()
            .filter(|(name, info)| info.check_unused && !info.used && !name.starts_with('_'))
            .map(|(name, info)| (info.line, name))
            .collect();
        unused.sort_by(|a, b| a.0.cmp(&b.0));
        for (line, name) in unused {
            self.warning(format!("unused variable '{}'", name), line);
        }
    }

    fn warn_name(&mut self, name: &str, kind: NameKind, line: u32) {
        let ok = match kind {
            NameKind::Const => is_screaming_snake(name),
            NameKind::Package => is_snake_case(name),
            _ => is_snake_case(name),
        };
        if !ok {
            let expected = match kind {
                NameKind::Const => "SCREAMING_SNAKE_CASE",
                _ => "snake_case",
            };
            self.warning(
                format!("name '{}' should be {}", name, expected),
                line,
            );
        }
    }

    fn error(&mut self, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic {
            level: Level::Error,
            message: message.into(),
            line,
        });
    }

    fn warning(&mut self, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic {
            level: Level::Warning,
            message: message.into(),
            line,
        });
    }
}

#[derive(Clone, Copy)]
enum NameKind {
    Variable,
    Function,
    Type,
    Const,
    Package,
}

fn always_returns(stmts: &[Stmt]) -> bool {
    let last = match stmts.last() {
        Some(stmt) => stmt,
        None => return false,
    };
    match &last.kind {
        StmtKind::Return(_) => true,
        StmtKind::If {
            then_body,
            else_body,
            ..
        } => match else_body {
            Some(else_body) => always_returns(then_body) && always_returns(else_body),
            None => false,
        },
        // match 的穷尽性由类型检查单独保证，这里只要所有分支都返回值即可。
        StmtKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|arm| always_returns(&arm.body))
        }
        _ => false,
    }
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_screaming_snake(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_numeric(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_digit())
}

fn root_ident(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(name.clone()),
        ExprKind::Field { base, .. } => root_ident(base),
        _ => None,
    }
}

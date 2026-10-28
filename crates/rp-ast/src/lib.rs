//! Rip 语言 AST 定义。

pub type NodeId = u32;

#[derive(Debug, Clone)]
pub struct Program {
    pub package: String,
    pub items: Vec<Item>,
    pub file: String,
    pub package_line: u32,
}

#[derive(Debug, Clone)]
pub enum Item {
    Import(Import),
    Type(TypeDecl),
    Interface(InterfaceDecl),
    Enum(EnumDecl),
    Const(VarDecl),
    Static(VarDecl),
    Func(FuncDecl),
}

/// 枚举：`type 名称 = enum { 变体, 变体(字段: 类型), ... }`。
#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<EnumVariant>,
    pub line: u32,
    pub public: bool,
}

/// 枚举变体：`name` 或 `name(field: type, ...)`。
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<EnumField>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct EnumField {
    pub name: String,
    pub ty: TypeExpr,
}

/// 接口：一组方法签名，由具体类型隐式实现（拥有同名同签名方法即视为实现）。
#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: String,
    pub methods: Vec<InterfaceMethod>,
    pub line: u32,
    pub public: bool,
}

/// 接口方法签名：接收者固定是接口自身，因此只记录接收者模式与其余参数。
#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: String,
    /// 接收者模式，只允许 `read` 或 `write`（缺省 `read`）。
    pub mode: ParamMode,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub alias: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,
    pub ty: TypeExpr,
    pub line: u32,
    pub public: bool,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    /// 完整函数名；方法写成 `类型.方法`（如 `user.display`）。
    pub name: String,
    /// 方法所属类型名（`func user.display` → `Some("user")`）；`None` 表示普通函数。
    pub owner: Option<String>,
    /// 泛型类型参数，如 `func first<T>(items: vec<T>) -> T` 里的 `T`。
    /// 空表示普通函数；运行时类型擦除，只在类型检查阶段使用。
    pub type_params: Vec<String>,
    /// 类型参数约束：`T: shape`（T → shape）。约束只能是接口名。
    pub type_bounds: std::collections::HashMap<String, String>,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub body: Vec<Stmt>,
    pub attributes: Vec<Attribute>,
    /// `async func`：调用返回异步任务，必须配合 `await` 使用。
    pub is_async: bool,
    pub line: u32,
    pub public: bool,
    pub self_only: bool,
    pub package: String,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<String>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub mode: ParamMode,
    pub ty: TypeExpr,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    Value,
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    pub ty: TypeExpr,
    pub init: Expr,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExprKind {
    Named(Vec<String>),
    Ary(Vec<FieldType>),
    Vec(Vec<TypeExpr>),
    Map(Box<TypeExpr>, Box<TypeExpr>),
    Option(Box<TypeExpr>),
    Result(Box<TypeExpr>, Box<TypeExpr>),
    Share(Box<TypeExpr>),
    /// `task<T>`：`async` 调用的结果，只能通过 `await` 取值。
    Task(Box<TypeExpr>),
    Func(Vec<TypeExpr>, Box<TypeExpr>),
    Read(Box<TypeExpr>),
    Void,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldType {
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let {
        name: String,
        mutable: bool,
        ty: TypeExpr,
        init: Expr,
    },
    Expr(Expr),
    Assign {
        target: Expr,
        value: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },
    Loop {
        body: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// `while let 模式 = 表达式 => { ... }`：模式匹配成立才执行循环体。
    WhileLet {
        pattern: Pattern,
        scrutinee: Expr,
        body: Vec<Stmt>,
    },
    ForIn {
        name: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    /// `defer 调用;`：参数在 defer 处立即求值，函数返回时按后进先出执行。
    Defer {
        call: Expr,
    },
    Break,
    Continue,
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
    },
    /// 并发块：`branches` 里每条顶层语句是一条分支，各分支在自己的线程里执行。
    ///
    /// 分支完全隔离：看不到块外的局部变量，结果只能通过数据库、文件、网络等外部资源产生。
    Parallel {
        branches: Vec<Stmt>,
    },
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Some(String),
    None,
    Ok(String),
    Err(String),
    Ary(Vec<AryPatField>),
    /// 枚举变体：`status.active` / `status.banned[reason: string]`。
    Enum {
        name: String,
        variant: String,
        fields: Vec<AryPatField>,
    },
}

#[derive(Debug, Clone)]
pub struct AryPatField {
    pub name: String,
    pub ty: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntBase {
    Dec,
    Bin,
    Oct,
    Hex,
}

/// 整数字面量的类型后缀（`42i32`、`0xFFu8`）。
///
/// 前缀只表示进制，不决定类型；类型完全由后缀给出。没有后缀的整数字面量是 `int`（= `i64`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSuffix {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntSuffix {
    pub fn name(self) -> &'static str {
        match self {
            IntSuffix::I8 => "i8",
            IntSuffix::I16 => "i16",
            IntSuffix::I32 => "i32",
            IntSuffix::I64 => "i64",
            IntSuffix::U8 => "u8",
            IntSuffix::U16 => "u16",
            IntSuffix::U32 => "u32",
            IntSuffix::U64 => "u64",
        }
    }

    pub fn from_name(name: &str) -> Option<IntSuffix> {
        match name {
            "i8" => Some(IntSuffix::I8),
            "i16" => Some(IntSuffix::I16),
            "i32" => Some(IntSuffix::I32),
            "i64" => Some(IntSuffix::I64),
            "u8" => Some(IntSuffix::U8),
            "u16" => Some(IntSuffix::U16),
            "u32" => Some(IntSuffix::U32),
            "u64" => Some(IntSuffix::U64),
            _ => None,
        }
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntSuffix::I8 | IntSuffix::I16 | IntSuffix::I32 | IntSuffix::I64
        )
    }

    pub fn bits(self) -> u32 {
        match self {
            IntSuffix::I8 | IntSuffix::U8 => 8,
            IntSuffix::I16 | IntSuffix::U16 => 16,
            IntSuffix::I32 | IntSuffix::U32 => 32,
            IntSuffix::I64 | IntSuffix::U64 => 64,
        }
    }

    /// 该类型的取值范围。`u64` 的上界超出 `i64`，因此统一用 `i128` 表示。
    pub fn range(self) -> (i128, i128) {
        if self.is_signed() {
            let half = 1i128 << (self.bits() - 1);
            (-half, half - 1)
        } else {
            (0, (1i128 << self.bits()) - 1)
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    /// `value` 是字面量的精确值（可能超出 `i64`，例如 `u64` 上界）；真实类型由 `suffix` 或上下文决定。
    Int { value: i128, base: IntBase, suffix: Option<IntSuffix> },
    Float(f64),
    Str(String),
    Char(String),
    Bool(bool),
    Null,
    Ident(String),
    Field { base: Box<Expr>, name: String },
    Call { callee: Box<Expr>, args: Vec<Expr>, type_args: Vec<TypeExpr> },
    Method { receiver: Box<Expr>, name: String, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    List(Vec<ListItem>),
    Struct { path: Vec<String>, fields: Vec<(String, Expr)> },
    Some(Box<Expr>),
    Ok(Box<Expr>),
    Err(Box<Expr>),
    None,
    Closure { params: Vec<Param>, ret: TypeExpr, body: Vec<Stmt> },
    Try(Box<Expr>),
    /// `await f()`：等待异步调用完成并取出返回值。
    Await(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum ListItem {
    Value(Expr),
    Named(String, Expr),
    Key(Expr, Expr),
    Typed(TypeExpr, Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

impl Program {
    pub fn func(&self, name: &str) -> Option<&FuncDecl> {
        self.items.iter().find_map(|item| match item {
            Item::Func(f) if f.name == name => Some(f),
            _ => None,
        })
    }
}

//! Rip 递归下降解析器。

use rp_ast::*;
use rp_lexer::{lex, Keyword, LexError, Token, TokenKind};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
}

impl ParseError {
    fn new(message: impl Into<String>, line: u32) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}

impl From<LexError> for ParseError {
    fn from(value: LexError) -> Self {
        Self {
            message: value.message,
            line: value.line,
        }
    }
}

/// 递归下降解析的最大嵌套深度，防止恶意深嵌套源码耗尽栈空间。
/// 每层括号会经过 expr 与 primary 两个入口，debug 构建中一条优先级链
/// 的栈帧可达几十 KB，配合 parse_program 的大栈线程双重兜底。
const MAX_DEPTH: usize = 64;

pub fn parse_program(source: &str, file: &str) -> Result<Program, ParseError> {
    // 深嵌套解析在递归下降中每层可能消耗几十 KB 栈（debug 构建尤甚），
    // 统一放到 32MB 大栈线程里执行，保证任何调用方（如 rp-lsp 主线程）
    // 都不会被恶意源码拖垮；深度上限（MAX_DEPTH）仍负责拒绝病态输入。
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, || {
                let tokens = lex(source)?;
                let mut parser = Parser {
                    tokens,
                    pos: 0,
                    next_id: 1,
                    depth: 0,
                };
                parser.program(file)
            })
            .map_err(|_| ParseError::new("cannot spawn parser thread", 0))?
            .join()
            .unwrap_or_else(|_| Err(ParseError::new("parser thread panicked", 0)))
    })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    next_id: NodeId,
    depth: usize,
}

impl Parser {
    fn program(&mut self, file: &str) -> Result<Program, ParseError> {
        if !matches!(self.current().kind, TokenKind::Kw(Keyword::Mod)) {
            return Err(ParseError::new(
                "every .rp file must start with a `mod` declaration",
                self.current().line,
            ));
        }
        let package_line = self.current().line;
        self.advance();
        let package = self.expect_ident()?;
        self.expect_semi()?;

        let mut items = Vec::new();
        while !matches!(self.current().kind, TokenKind::Eof) {
            items.push(self.item()?);
        }
        Ok(Program {
            package,
            items,
            file: file.to_string(),
            package_line,
        })
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let attributes = self.attributes()?;
        let self_only = self.eat_keyword(Keyword::SelfRef);
        let public = if self.eat_keyword(Keyword::Pub) {
            true
        } else {
            false
        };
        // `pub async func`：async 只允许修饰自由函数。
        let is_async = self.eat_keyword(Keyword::Async);
        if (!attributes.is_empty() || self_only || is_async)
            && !matches!(self.current().kind, TokenKind::Kw(Keyword::Func))
        {
            return Err(ParseError::new(
                "'self', 'async' and attributes are only allowed on a function declaration",
                self.current().line,
            ));
        }
        if self_only && public {
            return Err(ParseError::new(
                "cannot combine 'self' and 'pub'",
                self.current().line,
            ));
        }
        match &self.current().kind {
            TokenKind::Kw(Keyword::Use) => {
                if public {
                    return Err(ParseError::new("use cannot be pub", self.current().line));
                }
                self.use_decl()
            }
            TokenKind::Kw(Keyword::Type)
            | TokenKind::Kw(Keyword::Struct)
            | TokenKind::Kw(Keyword::Obj) => {
                let line = self.current().line;
                self.advance();
                let name = self.expect_ident()?;
                self.expect_assign()?;
                if matches!(self.current().kind, TokenKind::Kw(Keyword::Interface)) {
                    return self.interface_decl(name, line, public);
                }
                if matches!(self.current().kind, TokenKind::Kw(Keyword::Enum)) {
                    return self.enum_decl(name, line, public);
                }
                let ty = self.type_expr()?;
                self.expect_semi()?;
                Ok(Item::Type(TypeDecl {
                    name,
                    ty,
                    line,
                    public,
                }))
            }
            TokenKind::Kw(Keyword::Const) => {
                if public {
                    return Err(ParseError::new(
                        "const cannot be pub; use pub func or pub type",
                        self.current().line,
                    ));
                }
                self.advance();
                let line = self.current().line;
                let name = self.expect_ident()?;
                self.expect_colon()?;
                let ty = self.type_expr()?;
                self.expect_assign()?;
                let init = self.expr()?;
                self.expect_semi()?;
                Ok(Item::Const(VarDecl {
                    name,
                    ty,
                    init,
                    line,
                }))
            }
            TokenKind::Kw(Keyword::Static) => {
                if public {
                    return Err(ParseError::new(
                        "static cannot be pub; use pub func or pub type",
                        self.current().line,
                    ));
                }
                self.advance();
                let line = self.current().line;
                let name = self.expect_ident()?;
                self.expect_colon()?;
                let ty = self.type_expr()?;
                self.expect_assign()?;
                let init = self.expr()?;
                self.expect_semi()?;
                Ok(Item::Static(VarDecl {
                    name,
                    ty,
                    init,
                    line,
                }))
            }
            TokenKind::Kw(Keyword::Func) => {
                let func = self.func_decl(public, self_only, is_async, attributes)?;
                Ok(Item::Func(func))
            }
            _ => Err(ParseError::new(
                "expected use, type, const, static or func declaration",
                self.current().line,
            )),
        }
    }

    /// `use std::http;` / `use std::http as h;` / `use acme::tools;`
    ///
    /// 只支持导入模块（路径最后一段是模块名），不支持逐项 `use a::b::f;`。
    fn use_decl(&mut self) -> Result<Item, ParseError> {
        let line = self.current().line;
        self.advance();
        let mut segments = vec![self.expect_ident()?];
        loop {
            if !matches!(self.current().kind, TokenKind::Colon)
                || !matches!(self.peek_kind(1), TokenKind::Colon)
            {
                break;
            }
            self.advance();
            self.advance();
            segments.push(self.expect_ident()?);
        }
        let alias = if self.eat_keyword(Keyword::As) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        self.expect_semi()?;
        Ok(Item::Import(Import {
            path: segments.join("/"),
            alias,
            line,
        }))
    }

    fn func_decl(
        &mut self,
        public: bool,
        self_only: bool,
        is_async: bool,
        attributes: Vec<Attribute>,
    ) -> Result<FuncDecl, ParseError> {
        let line = self.current().line;
        self.advance();
        // `func user.display(...)`：方法声明，完整名称为 `类型.方法`。
        let first = self.expect_ident()?;
        let (owner, name) = if matches!(self.current().kind, TokenKind::Dot) {
            self.advance();
            let method = self.expect_ident()?;
            (Some(first.clone()), format!("{}.{}", first, method))
        } else {
            (None, first)
        };
        let (type_params, type_bounds) = self.type_params()?;
        self.expect_lparen()?;
        let params = self.params()?;
        self.expect_rparen()?;
        self.expect_arrow()?;
        let ret = self.type_expr()?;
        let body = self.block()?;
        Ok(FuncDecl {
            name,
            owner,
            type_params,
            type_bounds,
            params,
            ret,
            body,
            attributes,
            is_async,
            line,
            public,
            self_only,
            package: String::new(),
        })
    }

    /// `type 名称 = interface { func 方法(参数) -> 返回 ... }`
    ///
    /// 接口只描述方法签名，接收者固定为接口自身，因此成员里不写接收者参数。
    fn interface_decl(
        &mut self,
        name: String,
        line: u32,
        public: bool,
    ) -> Result<Item, ParseError> {
        self.advance();
        if !self.eat_lbrace() {
            return Err(ParseError::new(
                "expected '{' after `interface`",
                self.current().line,
            ));
        }
        let mut methods = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace) {
            if matches!(self.current().kind, TokenKind::Eof) {
                return Err(ParseError::new(
                    "unterminated interface; expected '}'",
                    self.current().line,
                ));
            }
            let method_line = self.current().line;
            let mode = if self.eat_keyword(Keyword::Write) {
                ParamMode::Write
            } else {
                self.eat_keyword(Keyword::Read);
                ParamMode::Read
            };
            if !matches!(self.current().kind, TokenKind::Kw(Keyword::Func)) {
                return Err(ParseError::new(
                    "interface members must be `func name(...) -> type`",
                    self.current().line,
                ));
            }
            self.advance();
            let method_name = self.expect_ident()?;
            self.expect_lparen()?;
            let params = self.params()?;
            self.expect_rparen()?;
            self.expect_arrow()?;
            let ret = self.type_expr()?;
            methods.push(InterfaceMethod {
                name: method_name,
                mode,
                params,
                ret,
                line: method_line,
            });
        }
        self.expect_rbrace()?;
        Ok(Item::Interface(InterfaceDecl {
            name,
            methods,
            line,
            public,
        }))
    }

    /// `type 名称 = enum { 变体, 变体(字段: 类型), ... }`
    ///
    /// 变体之间用逗号分隔，允许尾随逗号；无字段的变体直接写名字。
    fn enum_decl(&mut self, name: String, line: u32, public: bool) -> Result<Item, ParseError> {
        self.advance();
        if !self.eat_lbrace() {
            return Err(ParseError::new(
                "expected '{' after `enum`",
                self.current().line,
            ));
        }
        let mut variants = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace) {
            if matches!(self.current().kind, TokenKind::Eof) {
                return Err(ParseError::new(
                    "unterminated enum; expected '}'",
                    self.current().line,
                ));
            }
            let variant_line = self.current().line;
            let variant = self.expect_ident()?;
            let mut fields = Vec::new();
            if matches!(self.current().kind, TokenKind::LParen) {
                self.advance();
                if !matches!(self.current().kind, TokenKind::RParen) {
                    loop {
                        let field = self.expect_ident()?;
                        self.expect_colon()?;
                        let ty = self.type_expr()?;
                        fields.push(EnumField { name: field, ty });
                        if self.eat_comma() {
                            continue;
                        }
                        break;
                    }
                }
                self.expect_rparen()?;
            }
            variants.push(EnumVariant {
                name: variant,
                fields,
                line: variant_line,
            });
            if self.eat_comma() {
                continue;
            }
            if !matches!(self.current().kind, TokenKind::RBrace) {
                return Err(ParseError::new(
                    "expected ',' between enum variants",
                    self.current().line,
                ));
            }
        }
        self.expect_rbrace()?;
        Ok(Item::Enum(EnumDecl {
            name,
            variants,
            line,
            public,
        }))
    }

    /// 泛型参数列表：`<T, U: shape>`，缺省为空。
    ///
    /// 返回（按声明顺序的参数名，参数名 → 约束接口）。
    fn type_params(
        &mut self,
    ) -> Result<(Vec<String>, std::collections::HashMap<String, String>), ParseError> {
        let mut bounds = std::collections::HashMap::new();
        if !matches!(self.current().kind, TokenKind::Lt) {
            return Ok((Vec::new(), bounds));
        }
        self.advance();
        let mut names = Vec::new();
        loop {
            let name = self.expect_ident()?;
            if self.eat_colon() {
                let bound = self.expect_ident()?;
                bounds.insert(name.clone(), bound);
            }
            names.push(name);
            if matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        if !matches!(self.current().kind, TokenKind::Gt) {
            return Err(ParseError::new(
                "expected '>' after type parameters",
                self.current().line,
            ));
        }
        self.advance();
        Ok((names, bounds))
    }

    // 属性：`#[name]` 或 `#[name("a", "b")]`，只允许出现在函数声明之前。
    fn attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attributes = Vec::new();
        while matches!(self.current().kind, TokenKind::Hash) {
            let line = self.current().line;
            self.advance();
            if !matches!(self.current().kind, TokenKind::LBracket) {
                return Err(ParseError::new(
                    "expected '[' after '#'",
                    self.current().line,
                ));
            }
            self.advance();
            let name = self.expect_ident()?;
            let mut args = Vec::new();
            if matches!(self.current().kind, TokenKind::LParen) {
                self.advance();
                if !matches!(self.current().kind, TokenKind::RParen) {
                    loop {
                        let arg = match &self.current().kind {
                            TokenKind::Str(text) => text.clone(),
                            _ => {
                                return Err(ParseError::new(
                                    "attribute argument must be a double-quoted string",
                                    self.current().line,
                                ))
                            }
                        };
                        self.advance();
                        args.push(arg);
                        if self.eat_comma() {
                            continue;
                        }
                        break;
                    }
                }
                self.expect_rparen()?;
            }
            if !matches!(self.current().kind, TokenKind::RBracket) {
                return Err(ParseError::new(
                    "expected ']' after attribute",
                    self.current().line,
                ));
            }
            self.advance();
            attributes.push(Attribute { name, args, line });
        }
        Ok(attributes)
    }

    fn params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if matches!(self.current().kind, TokenKind::RParen) {
            return Ok(params);
        }
        loop {
            let line = self.current().line;
            let mode = if self.eat_keyword(Keyword::Read) {
                Some(ParamMode::Read)
            } else if self.eat_keyword(Keyword::Write) {
                Some(ParamMode::Write)
            } else {
                None
            };
            let name = self.expect_ident()?;
            self.expect_colon()?;
            // `&T` / `&mut T` 是 read / write 的另一种写法（Rust 风格）。
            let borrowed = if matches!(self.current().kind, TokenKind::Amp) {
                self.advance();
                Some(if self.eat_keyword(Keyword::Mut) {
                    ParamMode::Write
                } else {
                    ParamMode::Read
                })
            } else {
                None
            };
            let mode = match (mode, borrowed) {
                (Some(_), Some(_)) => {
                    return Err(ParseError::new(
                        "use either 'read'/'write' or '&'/'&mut' on a parameter, not both",
                        line,
                    ))
                }
                (Some(mode), None) | (None, Some(mode)) => mode,
                (None, None) => ParamMode::Value,
            };
            let ty = self.type_expr()?;
            params.push(Param {
                name,
                mode,
                ty,
                line,
            });
            if self.eat_comma() {
                continue;
            }
            break;
        }
        Ok(params)
    }

    fn type_expr(&mut self) -> Result<TypeExpr, ParseError> {
        self.enter_depth("type")?;
        let result = self.type_expr_inner();
        self.leave_depth();
        result
    }

    fn type_expr_inner(&mut self) -> Result<TypeExpr, ParseError> {
        let line = self.current().line;
        // `()` 是 Rust 的单元类型，等价于 `void`。
        if matches!(self.current().kind, TokenKind::LParen) {
            self.advance();
            self.expect_rparen()?;
            return Ok(TypeExpr {
                kind: TypeExprKind::Void,
                line,
            });
        }
        if self.eat_keyword(Keyword::Read) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr {
                kind: TypeExprKind::Read(Box::new(inner)),
                line,
            });
        }
        if self.eat_keyword(Keyword::Share) {
            if !self.eat_lt() {
                return Err(ParseError::new(
                    "share requires a type argument: share<T>",
                    line,
                ));
            }
            let inner = self.type_expr()?;
            if !self.eat_gt() {
                return Err(ParseError::new("expected '>' after share type", self.current().line));
            }
            return Ok(TypeExpr {
                kind: TypeExprKind::Share(Box::new(inner)),
                line,
            });
        }
        if self.eat_keyword(Keyword::Func) {
            self.expect_lparen()?;
            let mut args = Vec::new();
            if !matches!(self.current().kind, TokenKind::RParen) {
                loop {
                    args.push(self.type_expr()?);
                    if self.eat_comma() {
                        continue;
                    }
                    break;
                }
            }
            self.expect_rparen()?;
            self.expect_arrow()?;
            let ret = self.type_expr()?;
            return Ok(TypeExpr {
                kind: TypeExprKind::Func(args, Box::new(ret)),
                line,
            });
        }
        if matches!(self.current().kind, TokenKind::LBracket) {
            self.advance();
            let mut fields = Vec::new();
            if !matches!(self.current().kind, TokenKind::RBracket) {
                loop {
                    let name = self.expect_ident()?;
                    self.expect_colon()?;
                    let ty = self.type_expr()?;
                    fields.push(FieldType { name, ty });
                    if self.eat_comma() {
                        continue;
                    }
                    break;
                }
            }
            self.expect_rbracket()?;
            return Ok(TypeExpr {
                kind: TypeExprKind::Ary(fields),
                line,
            });
        }

        let name = self.expect_ident()?;
        match name.as_str() {
            "void" => Ok(TypeExpr {
                kind: TypeExprKind::Void,
                line,
            }),
            "vec" => {
                if self.eat_lt() {
                    let mut args = Vec::new();
                    if !matches!(self.current().kind, TokenKind::Gt) {
                        loop {
                            args.push(self.type_expr()?);
                            if self.eat_comma() {
                                continue;
                            }
                            break;
                        }
                    }
                    if !self.eat_gt() {
                        return Err(ParseError::new(
                            "expected '>' after vec type arguments",
                            self.current().line,
                        ));
                    }
                    Ok(TypeExpr {
                        kind: TypeExprKind::Vec(args),
                        line,
                    })
                } else {
                    Ok(TypeExpr {
                        kind: TypeExprKind::Vec(Vec::new()),
                        line,
                    })
                }
            }
            "map" => {
                if !self.eat_lt() {
                    return Err(ParseError::new(
                        "map requires two type arguments: map<K, V>",
                        self.current().line,
                    ));
                }
                let k = self.type_expr()?;
                if !self.eat_comma() {
                    return Err(ParseError::new("expected ',' in map type", self.current().line));
                }
                let v = self.type_expr()?;
                if !self.eat_gt() {
                    return Err(ParseError::new(
                        "expected '>' after map type arguments",
                        self.current().line,
                    ));
                }
                Ok(TypeExpr {
                    kind: TypeExprKind::Map(Box::new(k), Box::new(v)),
                    line,
                })
            }
            "option" => {
                if !self.eat_lt() {
                    return Err(ParseError::new(
                        "option requires a type argument: option<T>",
                        self.current().line,
                    ));
                }
                let inner = self.type_expr()?;
                if !self.eat_gt() {
                    return Err(ParseError::new(
                        "expected '>' after option type",
                        self.current().line,
                    ));
                }
                Ok(TypeExpr {
                    kind: TypeExprKind::Option(Box::new(inner)),
                    line,
                })
            }
            "result" => {
                if !self.eat_lt() {
                    return Err(ParseError::new(
                        "result requires two type arguments: result<T, E>",
                        self.current().line,
                    ));
                }
                let ok_ty = self.type_expr()?;
                if !self.eat_comma() {
                    return Err(ParseError::new(
                        "expected ',' in result type",
                        self.current().line,
                    ));
                }
                let err_ty = self.type_expr()?;
                if !self.eat_gt() {
                    return Err(ParseError::new(
                        "expected '>' after result type",
                        self.current().line,
                    ));
                }
                Ok(TypeExpr {
                    kind: TypeExprKind::Result(Box::new(ok_ty), Box::new(err_ty)),
                    line,
                })
            }
            "task" => {
                if !self.eat_lt() {
                    return Err(ParseError::new(
                        "task requires a type argument: task<T>",
                        self.current().line,
                    ));
                }
                let inner = self.type_expr()?;
                if !self.eat_gt() {
                    return Err(ParseError::new(
                        "expected '>' after task type",
                        self.current().line,
                    ));
                }
                Ok(TypeExpr {
                    kind: TypeExprKind::Task(Box::new(inner)),
                    line,
                })
            }
            _ => {
                let mut path = vec![name];
                while matches!(self.current().kind, TokenKind::Dot) {
                    self.advance();
                    path.push(self.expect_ident()?);
                }
                Ok(TypeExpr {
                    kind: TypeExprKind::Named(path),
                    line,
                })
            }
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, ParseError> {
        self.enter_depth("block")?;
        let result = self.block_inner();
        self.leave_depth();
        result
    }

    fn block_inner(&mut self) -> Result<Vec<Stmt>, ParseError> {
        if !self.eat_lbrace() {
            return Err(ParseError::new(
                "expected '{' to start a block",
                self.current().line,
            ));
        }
        let mut stmts = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace) {
            if matches!(self.current().kind, TokenKind::Eof) {
                return Err(ParseError::new(
                    "unterminated block; expected '}'",
                    self.current().line,
                ));
            }
            stmts.push(self.stmt()?);
        }
        self.expect_rbrace()?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        match &self.current().kind {
            TokenKind::Kw(Keyword::Let) => self.let_stmt(),
            TokenKind::Kw(Keyword::Const) | TokenKind::Kw(Keyword::Static) => Err(ParseError::new(
                "const and static must be declared at module level",
                line,
            )),
            TokenKind::Kw(Keyword::If) => self.if_stmt(),
            TokenKind::Kw(Keyword::Loop) => self.loop_stmt(),
            TokenKind::Kw(Keyword::While) => self.while_stmt(),
            TokenKind::Kw(Keyword::For) => self.for_stmt(),
            TokenKind::Kw(Keyword::Match) => self.match_stmt(),
            TokenKind::Kw(Keyword::Parallel) => self.parallel_stmt(),
            TokenKind::Kw(Keyword::Return) => {
                self.advance();
                let value = if matches!(self.current().kind, TokenKind::Semi) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect_semi()?;
                Ok(Stmt {
                    kind: StmtKind::Return(value),
                    line,
                })
            }
            TokenKind::Kw(Keyword::Defer) => self.defer_stmt(),
            TokenKind::Kw(Keyword::Break) => {
                self.advance();
                self.expect_semi()?;
                Ok(Stmt {
                    kind: StmtKind::Break,
                    line,
                })
            }
            TokenKind::Kw(Keyword::Continue) => {
                self.advance();
                self.expect_semi()?;
                Ok(Stmt {
                    kind: StmtKind::Continue,
                    line,
                })
            }
            _ => {
                let target = self.expr()?;
                if matches!(self.current().kind, TokenKind::Assign) {
                    self.advance();
                    let value = self.expr()?;
                    self.expect_semi()?;
                    Ok(Stmt {
                        kind: StmtKind::Assign { target, value },
                        line,
                    })
                } else {
                    self.expect_semi()?;
                    Ok(Stmt {
                        kind: StmtKind::Expr(target),
                        line,
                    })
                }
            }
        }
    }

    /// `defer 调用;`：只接受函数 / 方法 / 模块调用，参数在 defer 处立即求值。
    fn defer_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let call = self.expr()?;
        if !matches!(
            call.kind,
            ExprKind::Call { .. } | ExprKind::Method { .. }
        ) {
            return Err(ParseError::new(
                "defer requires a function call, e.g. `defer conn.close();`",
                call.line,
            ));
        }
        self.expect_semi()?;
        Ok(Stmt {
            kind: StmtKind::Defer { call },
            line,
        })
    }

    fn let_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let mutable = self.eat_keyword(Keyword::Mut);
        let name = self.expect_ident()?;
        self.expect_colon()?;
        let ty = self.type_expr()?;
        self.expect_assign()?;
        let init = self.expr()?;
        self.expect_semi()?;
        Ok(Stmt {
            kind: StmtKind::Let {
                name,
                mutable,
                ty,
                init,
            },
            line,
        })
    }

    fn if_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let cond = self.expr()?;
        let then_body = self.block()?;
        let else_body = self.else_tail()?;
        Ok(Stmt {
            kind: StmtKind::If {
                cond,
                then_body,
                else_body,
            },
            line,
        })
    }

    /// `else` / `else if` / `elif` 的公共尾巴：`elif` 与 `else if` 完全等价，可混用。
    fn else_tail(&mut self) -> Result<Option<Vec<Stmt>>, ParseError> {
        // `elif` 已经不是 `if` 关键字本身，条件要在这里解析。
        if self.eat_keyword(Keyword::Elif) {
            let line = self.current().line;
            let cond = self.expr()?;
            let then_body = self.block()?;
            let else_body = self.else_tail()?;
            return Ok(Some(vec![Stmt {
                kind: StmtKind::If {
                    cond,
                    then_body,
                    else_body,
                },
                line,
            }]));
        }
        if self.eat_keyword(Keyword::Else) {
            if matches!(self.current().kind, TokenKind::Kw(Keyword::If)) {
                return Ok(Some(vec![self.if_stmt()?]));
            }
            return Ok(Some(self.block()?));
        }
        Ok(None)
    }

    /// `loop { ... }`：无条件循环，只能靠 `break` 退出。
    fn loop_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let body = self.block()?;
        Ok(Stmt {
            kind: StmtKind::Loop { body },
            line,
        })
    }

    /// `while 条件 { ... }` 或 `while let 模式 = 表达式 => { ... }`。
    fn while_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        if self.eat_keyword(Keyword::Let) {
            let pattern = self.pattern()?;
            self.expect_assign()?;
            let scrutinee = self.expr()?;
            if matches!(self.current().kind, TokenKind::LBrace) {
                return Err(ParseError::new(
                    "while let needs '=>' between the expression and the block",
                    self.current().line,
                ));
            }
            self.expect_fat_arrow()?;
            let body = self.block()?;
            return Ok(Stmt {
                kind: StmtKind::WhileLet {
                    pattern,
                    scrutinee,
                    body,
                },
                line,
            });
        }
        let cond = self.expr()?;
        let body = self.block()?;
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            line,
        })
    }

    /// `for 变量 in 迭代对象 { ... }`。
    fn for_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let name = self.expect_ident()?;
        if !self.eat_keyword(Keyword::In) {
            return Err(ParseError::new(
                "for requires `for <name> in <vec|map> { ... }`",
                self.current().line,
            ));
        }
        let iter = self.expr()?;
        let body = self.block()?;
        Ok(Stmt {
            kind: StmtKind::ForIn { name, iter, body },
            line,
        })
    }

    fn match_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let subject = self.expr()?;
        if !self.eat_lbrace() {
            return Err(ParseError::new(
                "expected '{' after match subject",
                self.current().line,
            ));
        }
        let mut arms = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace) {
            if matches!(self.current().kind, TokenKind::Eof) {
                return Err(ParseError::new(
                    "unterminated match; expected '}'",
                    self.current().line,
                ));
            }
            let arm_line = self.current().line;
            let pattern = self.pattern()?;
            if matches!(self.current().kind, TokenKind::Assign) {
                return Err(ParseError::new(
                    "match arm uses '=>', not '='",
                    self.current().line,
                ));
            }
            self.expect_fat_arrow()?;
            let body = self.block()?;
            arms.push(MatchArm {
                pattern,
                body,
                line: arm_line,
            });
        }
        self.expect_rbrace()?;
        Ok(Stmt {
            kind: StmtKind::Match { subject, arms },
            line,
        })
    }

    /// `parallel { stmt; stmt; }`：块内每条顶层语句是一条并发分支。
    fn parallel_stmt(&mut self) -> Result<Stmt, ParseError> {
        let line = self.current().line;
        self.advance();
        let branches = self.block()?;
        if branches.is_empty() {
            return Err(ParseError::new(
                "parallel block needs at least one statement",
                line,
            ));
        }
        Ok(Stmt {
            kind: StmtKind::Parallel { branches },
            line,
        })
    }

    fn pattern(&mut self) -> Result<Pattern, ParseError> {
        let line = self.current().line;
        if self.eat_keyword(Keyword::Some) {
            self.expect_lparen()?;
            let name = self.expect_ident()?;
            self.expect_rparen()?;
            return Ok(Pattern::Some(name));
        }
        if self.eat_keyword(Keyword::None) {
            return Ok(Pattern::None);
        }
        if self.eat_keyword(Keyword::Ok) {
            self.expect_lparen()?;
            let name = self.expect_ident()?;
            self.expect_rparen()?;
            return Ok(Pattern::Ok(name));
        }
        if self.eat_keyword(Keyword::Err) {
            self.expect_lparen()?;
            let name = self.expect_ident()?;
            self.expect_rparen()?;
            return Ok(Pattern::Err(name));
        }
        // `枚举.变体` / `枚举.变体[字段: 类型, ...]`
        if let TokenKind::Ident(enum_name) = &self.current().kind {
            if matches!(self.peek_kind(1), TokenKind::Dot) {
                let name = enum_name.clone();
                self.advance();
                self.advance();
                let variant = self.expect_ident()?;
                let fields = if matches!(self.current().kind, TokenKind::LBracket) {
                    self.pattern_fields()?
                } else {
                    Vec::new()
                };
                return Ok(Pattern::Enum {
                    name,
                    variant,
                    fields,
                });
            }
        }
        if matches!(self.current().kind, TokenKind::LBracket) {
            let fields = self.pattern_fields()?;
            return Ok(Pattern::Ary(fields));
        }
        Err(ParseError::new(
            "pattern must be some(x), none, ok(x), err(e), [field: type, ...] or enum.variant[...]",
            line,
        ))
    }

    /// `[字段: 类型, 字段: _, ...]`；进入时 `[` 尚未消费，返回时 `]` 已消费。
    fn pattern_fields(&mut self) -> Result<Vec<AryPatField>, ParseError> {
        if !self.eat_lbracket() {
            return Err(ParseError::new(
                "expected '[' in pattern",
                self.current().line,
            ));
        }
        let mut fields = Vec::new();
        if !matches!(self.current().kind, TokenKind::RBracket) {
            loop {
                let name = self.expect_ident()?;
                self.expect_colon()?;
                let ty = if matches!(&self.current().kind, TokenKind::Ident(n) if n == "_") {
                    self.advance();
                    None
                } else {
                    Some(self.type_expr()?)
                };
                fields.push(AryPatField { name, ty });
                if self.eat_comma() {
                    continue;
                }
                break;
            }
        }
        self.expect_rbracket()?;
        Ok(fields)
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.enter_depth("expression")?;
        let result = self.or_expr();
        self.leave_depth();
        result
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and_expr()?;
        while matches!(self.current().kind, TokenKind::OrOr) {
            let line = self.current().line;
            self.advance();
            let right = self.and_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op: BinOp::Or,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.eq_expr()?;
        while matches!(self.current().kind, TokenKind::AndAnd) {
            let line = self.current().line;
            self.advance();
            let right = self.eq_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op: BinOp::And,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn eq_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.cmp_expr()?;
        loop {
            let op = match self.current().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::Ne,
                _ => break,
            };
            let line = self.current().line;
            self.advance();
            let right = self.cmp_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn cmp_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.add_expr()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            let line = self.current().line;
            self.advance();
            let right = self.add_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn add_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.mul_expr()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            let line = self.current().line;
            self.advance();
            let right = self.mul_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn mul_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary_expr()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            let line = self.current().line;
            self.advance();
            let right = self.unary_expr()?;
            left = self.new_expr(
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                line,
            );
        }
        Ok(left)
    }

    fn unary_expr(&mut self) -> Result<Expr, ParseError> {
        let line = self.current().line;
        // `await f()`：只允许等待一次异步调用，避免任务值在类型系统外流动。
        if self.eat_keyword(Keyword::Await) {
            let inner = self.unary_expr()?;
            return Ok(self.new_expr(ExprKind::Await(Box::new(inner)), line));
        }
        let op = match self.current().kind {
            TokenKind::Bang => Some(UnOp::Not),
            TokenKind::Minus => Some(UnOp::Neg),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let expr = self.unary_expr()?;
            return Ok(self.new_expr(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
                line,
            ));
        }
        self.postfix_expr()
    }

    fn postfix_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.primary_expr()?;
        // `f<int>(...)`：显式类型实参，只允许直接跟在函数名后且后面必须是调用。
        // 用回溯判断，`a < b` 这类比较不会被误吃。
        let mut pending_type_args: Vec<TypeExpr> = Vec::new();
        if matches!(expr.kind, ExprKind::Ident(_)) && matches!(self.current().kind, TokenKind::Lt) {
            let saved = self.pos;
            match self.try_parse_type_args() {
                Some(args) => pending_type_args = args,
                None => self.pos = saved,
            }
        }
        loop {
            match self.current().kind.clone() {
                TokenKind::Dot => {
                    self.advance();
                    let line = self.current().line;
                    let name = match self.current().kind.clone() {
                        TokenKind::Ident(n) => {
                            self.advance();
                            n
                        }
                        TokenKind::Int {
                            value,
                            base: IntBase::Dec,
                            ..
                        } => {
                            self.advance();
                            value.to_string()
                        }
                        // `static` 是关键字，但 `http.static(...)` 是标准库成员名
                        TokenKind::Kw(Keyword::Static) => {
                            self.advance();
                            "static".to_string()
                        }
                        _ => {
                            return Err(ParseError::new(
                                "expected field or method name after '.'",
                                line,
                            ))
                        }
                    };
                    if matches!(self.current().kind, TokenKind::LParen) {
                        self.advance();
                        let args = self.args()?;
                        self.expect_rparen()?;
                        expr = self.new_expr(
                            ExprKind::Method {
                                receiver: Box::new(expr),
                                name,
                                args,
                            },
                            line,
                        );
                    } else {
                        expr = self.new_expr(
                            ExprKind::Field {
                                base: Box::new(expr),
                                name,
                            },
                            line,
                        );
                    }
                }
                TokenKind::LParen => {
                    let line = self.current().line;
                    self.advance();
                    let args = self.args()?;
                    self.expect_rparen()?;
                    expr = self.new_expr(
                        ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                            type_args: std::mem::take(&mut pending_type_args),
                        },
                        line,
                    );
                }
                TokenKind::LBracket => {
                    let line = self.current().line;
                    let path = expr_to_path(&expr).ok_or_else(|| {
                        ParseError::new(
                            "indexing is not supported; only named struct literals use '['",
                            line,
                        )
                    })?;
                    self.advance();
                    let mut fields = Vec::new();
                    if !matches!(self.current().kind, TokenKind::RBracket) {
                        loop {
                            let name = self.expect_ident()?;
                            self.expect_assign()?;
                            let value = self.expr()?;
                            fields.push((name, value));
                            if self.eat_comma() {
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect_rbracket()?;
                    expr = self.new_expr(ExprKind::Struct { path, fields }, line);
                }
                TokenKind::Question => {
                    let line = self.current().line;
                    self.advance();
                    expr = self.new_expr(ExprKind::Try(Box::new(expr)), line);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if matches!(self.current().kind, TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat_comma() {
                continue;
            }
            break;
        }
        Ok(args)
    }

    fn primary_expr(&mut self) -> Result<Expr, ParseError> {
        self.enter_depth("expression")?;
        let result = self.primary_expr_inner();
        self.leave_depth();
        result
    }

    fn primary_expr_inner(&mut self) -> Result<Expr, ParseError> {
        let line = self.current().line;
        match self.current().kind.clone() {
            TokenKind::Int {
                value,
                base,
                suffix,
            } => {
                self.advance();
                Ok(self.new_expr(ExprKind::Int { value, base, suffix }, line))
            }
            TokenKind::Float(value) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Float(value), line))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Str(s), line))
            }
            TokenKind::Template(raw) => {
                self.advance();
                self.template_expr(&raw, line)
            }
            TokenKind::Char(s) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Char(s), line))
            }
            TokenKind::Kw(Keyword::True) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Bool(true), line))
            }
            TokenKind::Kw(Keyword::False) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Bool(false), line))
            }
            TokenKind::Kw(Keyword::Null) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Null, line))
            }
            TokenKind::Kw(Keyword::None) => {
                self.advance();
                Ok(self.new_expr(ExprKind::None, line))
            }
            TokenKind::Kw(Keyword::Some) => {
                self.advance();
                self.expect_lparen()?;
                let value = self.expr()?;
                self.expect_rparen()?;
                Ok(self.new_expr(ExprKind::Some(Box::new(value)), line))
            }
            TokenKind::Kw(Keyword::Ok) => {
                self.advance();
                self.expect_lparen()?;
                let value = self.expr()?;
                self.expect_rparen()?;
                Ok(self.new_expr(ExprKind::Ok(Box::new(value)), line))
            }
            TokenKind::Kw(Keyword::Err) => {
                self.advance();
                self.expect_lparen()?;
                let value = self.expr()?;
                self.expect_rparen()?;
                Ok(self.new_expr(ExprKind::Err(Box::new(value)), line))
            }
            TokenKind::Kw(Keyword::Func) => {
                self.advance();
                self.expect_lparen()?;
                let params = self.params()?;
                self.expect_rparen()?;
                self.expect_arrow()?;
                let ret = self.type_expr()?;
                let body = self.block()?;
                Ok(self.new_expr(
                    ExprKind::Closure { params, ret, body },
                    line,
                ))
            }
            TokenKind::Ident(name) => {
                self.advance();
                Ok(self.new_expr(ExprKind::Ident(name), line))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.expr()?;
                self.expect_rparen()?;
                Ok(expr)
            }
            TokenKind::LBracket => {
                self.advance();
                let mut items = Vec::new();
                if !matches!(self.current().kind, TokenKind::RBracket) {
                    loop {
                        if matches!(self.current().kind, TokenKind::Colon) {
                            self.advance();
                            let ty = self.type_expr()?;
                            self.expect_assign()?;
                            let value = self.expr()?;
                            items.push(ListItem::Typed(ty, value));
                        } else if matches!(&self.current().kind, TokenKind::Ident(_))
                            && matches!(self.peek_kind(1), TokenKind::Assign)
                        {
                            let name = self.expect_ident()?;
                            self.expect_assign()?;
                            let value = self.expr()?;
                            items.push(ListItem::Named(name, value));
                        } else {
                            let first = self.expr()?;
                            if matches!(self.current().kind, TokenKind::Assign) {
                                self.advance();
                                let value = self.expr()?;
                                items.push(ListItem::Key(first, value));
                            } else {
                                items.push(ListItem::Value(first));
                            }
                        }
                        if self.eat_comma() {
                            continue;
                        }
                        break;
                    }
                }
                self.expect_rbracket()?;
                Ok(self.new_expr(ExprKind::List(items), line))
            }
            _ => Err(ParseError::new(
                format!("unexpected token in expression: {:?}", self.current().kind),
                line,
            )),
        }
    }

    /// `"""…"""` 模板：按 `${expr}` 拆成「文本 + to_string(expr)」的加法链。
    ///
    /// 结果就是普通的字符串拼接表达式，三种执行方式（解释器 / VM / AOT）都无需改动。
    fn template_expr(&mut self, raw: &str, line: u32) -> Result<Expr, ParseError> {
        let chars: Vec<char> = raw.chars().collect();
        let mut pieces: Vec<Expr> = Vec::new();
        let mut text = String::new();
        let mut text_line = line;
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '\\' {
                match chars.get(i + 1) {
                    Some('$') => {
                        text.push('$');
                        i += 2;
                        continue;
                    }
                    Some('\\') => {
                        text.push('\\');
                        i += 2;
                        continue;
                    }
                    _ => {
                        text.push(ch);
                        i += 1;
                        continue;
                    }
                }
            }
            if ch == '$' && chars.get(i + 1) == Some(&'{') {
                let (inner, end) = match scan_interpolation(&chars, i + 2) {
                    Some(found) => found,
                    None => {
                        return Err(ParseError::new(
                            "unterminated '${' in template literal",
                            text_line,
                        ))
                    }
                };
                if inner.trim().is_empty() {
                    return Err(ParseError::new(
                        "empty '${}' in template literal",
                        text_line,
                    ));
                }
                if !text.is_empty() {
                    let piece = self.new_expr(ExprKind::Str(std::mem::take(&mut text)), text_line);
                    pieces.push(piece);
                }
                // 用换行补齐，让插值内部的错误行号指向源码真实位置。
                let pad = "\n".repeat(text_line.saturating_sub(1) as usize);
                let tokens = lex(&format!("{}{}", pad, inner))?;
                let mut sub = Parser {
                    tokens,
                    pos: 0,
                    next_id: self.next_id,
                    // 继承父级深度，避免用嵌套模板绕过递归上限。
                    depth: self.depth,
                };
                let expr = sub.expr()?;
                if !matches!(sub.current().kind, TokenKind::Eof) {
                    return Err(ParseError::new(
                        "unexpected token in '${…}'",
                        sub.current().line,
                    ));
                }
                self.next_id = sub.next_id;
                let piece = self.new_expr(
                    ExprKind::Method {
                        receiver: Box::new(expr),
                        name: "to_string".to_string(),
                        args: Vec::new(),
                    },
                    text_line,
                );
                pieces.push(piece);
                text_line += inner.matches('\n').count() as u32;
                i = end;
                continue;
            }
            if ch == '\n' {
                text_line += 1;
            }
            text.push(ch);
            i += 1;
        }
        if !text.is_empty() {
            let piece = self.new_expr(ExprKind::Str(text), text_line);
            pieces.push(piece);
        }
        let mut iter = pieces.into_iter();
        let mut expr = match iter.next() {
            Some(first) => first,
            None => self.new_expr(ExprKind::Str(String::new()), line),
        };
        for piece in iter {
            expr = self.new_expr(
                ExprKind::Binary {
                    op: BinOp::Add,
                    left: Box::new(expr),
                    right: Box::new(piece),
                },
                line,
            );
        }
        Ok(expr)
    }

    fn new_expr(&mut self, kind: ExprKind, line: u32) -> Expr {
        let id = self.next_id;
        self.next_id += 1;
        Expr { id, kind, line }
    }

    /// 进入一个递归下降入口；超过 MAX_DEPTH 时返回普通解析错误而不是继续递归。
    fn enter_depth(&mut self, what: &str) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError::new(
                format!("{} nesting is too deep (limit {})", what, MAX_DEPTH),
                self.current().line,
            ));
        }
        Ok(())
    }

    fn leave_depth(&mut self) {
        self.depth -= 1;
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self, ahead: usize) -> TokenKind {
        self.tokens
            .get(self.pos + ahead)
            .map(|t| t.kind.clone())
            .unwrap_or(TokenKind::Eof)
    }

    /// 尝试解析 `<类型, ...>` 且后面紧跟 `(`；不匹配时返回 `None`，由调用方回退位置。
    fn try_parse_type_args(&mut self) -> Option<Vec<TypeExpr>> {
        if !matches!(self.current().kind, TokenKind::Lt) {
            return None;
        }
        self.advance();
        let mut args = Vec::new();
        loop {
            match self.type_expr() {
                Ok(ty) => args.push(ty),
                Err(_) => return None,
            }
            if self.eat_comma() {
                continue;
            }
            break;
        }
        if !matches!(self.current().kind, TokenKind::Gt) {
            return None;
        }
        self.advance();
        if !matches!(self.current().kind, TokenKind::LParen) {
            return None;
        }
        Some(args)
    }

    fn advance(&mut self) {
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.current().kind.clone() {
            TokenKind::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::new(
                format!("expected identifier, found {:?}", self.current().kind),
                self.current().line,
            )),
        }
    }

    fn eat_keyword(&mut self, kw: Keyword) -> bool {
        if matches!(self.current().kind, TokenKind::Kw(k) if k == kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_colon(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Colon) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_comma(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_lt(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Lt) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_gt(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Gt) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_lbrace(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::LBrace) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_lbracket(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::LBracket) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_semi(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::Semi, "';'")
    }

    fn expect_colon(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::Colon, "':'")
    }

    fn expect_assign(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::Assign, "'='")
    }

    fn expect_arrow(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::Arrow, "'->'")
    }

    fn expect_fat_arrow(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::FatArrow, "'=>'")
    }

    fn expect_lparen(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::LParen, "'('")
    }

    fn expect_rparen(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::RParen, "')'")
    }

    fn expect_rbrace(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::RBrace, "'}'")
    }

    fn expect_rbracket(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::RBracket, "']'")
    }

    fn expect(&mut self, kind: TokenKind, label: &str) -> Result<(), ParseError> {
        if self.current().kind == kind {
            self.advance();
            Ok(())
        } else {
            Err(ParseError::new(
                format!("expected {}, found {:?}", label, self.current().kind),
                self.current().line,
            ))
        }
    }
}

/// 从 `${` 之后扫描到匹配的 `}`，跳过字符串字面量与嵌套花括号。
/// 返回内部表达式文本与 `}` 之后的位置。
fn scan_interpolation(chars: &[char], mut i: usize) -> Option<(String, usize)> {
    let start = i;
    let mut depth = 1usize;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(mark) = quote {
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == mark {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[start..i].iter().collect(), i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取出唯一函数的 return 表达式。
    fn returned(source: &str) -> Expr {
        let program = parse_program(source, "test.rp").expect("parse");
        for item in program.items {
            if let Item::Func(decl) = item {
                if let Some(Stmt {
                    kind: StmtKind::Return(Some(expr)),
                    ..
                }) = decl.body.first()
                {
                    return expr.clone();
                }
            }
        }
        panic!("no return expression");
    }

    #[test]
    fn template_becomes_concatenation() {
        let expr = returned("mod t;\nfunc f() -> string {\n    return \"\"\"a${x}b\"\"\";\n}\n");
        assert!(matches!(
            expr.kind,
            ExprKind::Binary { op: BinOp::Add, .. }
        ));
    }

    #[test]
    fn plain_template_is_a_string() {
        let expr = returned("mod t;\nfunc f() -> string {\n    return \"\"\"text\"\"\";\n}\n");
        match expr.kind {
            ExprKind::Str(text) => assert_eq!(text, "text"),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn escaped_dollar_stays_literal() {
        let expr = returned("mod t;\nfunc f() -> string {\n    return \"\"\"a \\${x}\"\"\";\n}\n");
        match expr.kind {
            ExprKind::Str(text) => assert_eq!(text, "a ${x}"),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn unterminated_interpolation_is_rejected() {
        let result = parse_program(
            "mod t;\nfunc f() -> string {\n    return \"\"\"a${x\"\"\";\n}\n",
            "test.rp",
        );
        assert!(result.is_err());
    }

    #[test]
    fn deeply_nested_parens_return_error_instead_of_overflow() {
        let open = "(".repeat(10_000);
        let close = ")".repeat(10_000);
        let source = format!("mod t;\nfunc f() -> int {{\n    return {}1{};\n}}\n", open, close);
        let result = parse_program(&source, "test.rp");
        match result {
            Err(err) => assert!(err.message.contains("nesting is too deep")),
            Ok(_) => panic!("expected parse error for 10000-level nesting"),
        }
    }

    #[test]
    fn moderately_nested_parens_still_parse() {
        let open = "(".repeat(20);
        let close = ")".repeat(20);
        let source = format!("mod t;\nfunc f() -> int {{\n    return {}1{};\n}}\n", open, close);
        assert!(parse_program(&source, "test.rp").is_ok());
    }

    #[test]
    fn deeply_nested_types_return_error() {
        let deep = "share<".repeat(10_000) + "int" + &">".repeat(10_000);
        let source = format!("mod t;\ntype x = {};\n", deep);
        let result = parse_program(&source, "test.rp");
        match result {
            Err(err) => assert!(err.message.contains("nesting is too deep")),
            Ok(_) => panic!("expected parse error for 10000-level type nesting"),
        }
    }
}

fn expr_to_path(expr: &Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(vec![name.clone()]),
        ExprKind::Field { base, name } => {
            if name.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let mut path = expr_to_path(base)?;
            path.push(name.clone());
            Some(path)
        }
        _ => None,
    }
}

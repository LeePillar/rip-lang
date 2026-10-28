//! Rip 词法分析器。

use rp_ast::{IntBase, IntSuffix};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Mod,
    Use,
    As,
    Pub,
    Type,
    Struct,
    Obj,
    Interface,
    Enum,
    Func,
    Async,
    Await,
    Let,
    Mut,
    Const,
    Static,
    SelfRef,
    Return,
    Defer,
    If,
    Elif,
    Else,
    For,
    Loop,
    While,
    In,
    Match,
    Parallel,
    Break,
    Continue,
    Read,
    Write,
    Share,
    True,
    False,
    Null,
    Ok,
    Err,
    Some,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    /// 整数字面量：值、进制前缀、可选类型后缀（`42i32` / `0xFFu8`）。
    Int {
        value: i128,
        base: IntBase,
        suffix: Option<IntSuffix>,
    },
    Float(f64),
    Str(String),
    /// `"""` 多行模板：原样保留换行与缩进，由 parser 处理 `${…}` 插值。
    Template(String),
    Char(String),
    Kw(Keyword),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semi,
    Dot,
    Arrow,
    FatArrow,
    Question,
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    /// 单个 `&`：只用于参数类型 `&T` / `&mut T`。
    Amp,
    OrOr,
    Bang,
    Hash,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub line: u32,
}

impl LexError {
    fn new(message: impl Into<String>, line: u32) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    // Windows 编辑器常写入 UTF-8 BOM；它不属于源码内容，先去掉。
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut tokens = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            ' ' | '\t' | '\r' => {
                i += 1;
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                i += 2;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                i += 2;
                let mut closed = false;
                while i + 1 < chars.len() {
                    if chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(LexError::new("unterminated block comment", line));
                }
            }
            '"' => {
                if chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"') {
                    let (s, next, new_line) = lex_template(&chars, i + 3, line)?;
                    tokens.push(Token {
                        kind: TokenKind::Template(s),
                        line,
                    });
                    line = new_line;
                    i = next;
                } else {
                    let (s, next, _) = lex_string(&chars, i + 1, line, '"')?;
                    tokens.push(Token {
                        kind: TokenKind::Str(s),
                        line,
                    });
                    i = next;
                }
            }
            '\'' => {
                let (s, next, new_line) = lex_string(&chars, i + 1, line, '\'')?;
                let mut scalars = s.chars();
                let first = scalars.next();
                if first.is_none() || scalars.next().is_some() {
                    return Err(LexError::new(
                        "character literal must contain exactly one Unicode scalar",
                        line,
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::Char(s),
                    line,
                });
                line = new_line;
                i = next;
            }
            '0'..='9' => {
                let (tok, next) = lex_number(&chars, i, line)?;
                tokens.push(Token { kind: tok, line });
                i = next;
            }
            c if is_ident_start(c) => {
                let start = i;
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let kind = match keyword(&word) {
                    Some(kw) => TokenKind::Kw(kw),
                    None => TokenKind::Ident(word),
                };
                tokens.push(Token { kind, line });
            }
            '(' => single(&mut tokens, TokenKind::LParen, &mut i, line),
            ')' => single(&mut tokens, TokenKind::RParen, &mut i, line),
            '{' => single(&mut tokens, TokenKind::LBrace, &mut i, line),
            '}' => single(&mut tokens, TokenKind::RBrace, &mut i, line),
            '[' => single(&mut tokens, TokenKind::LBracket, &mut i, line),
            ']' => single(&mut tokens, TokenKind::RBracket, &mut i, line),
            ',' => single(&mut tokens, TokenKind::Comma, &mut i, line),
            ';' => single(&mut tokens, TokenKind::Semi, &mut i, line),
            '.' => single(&mut tokens, TokenKind::Dot, &mut i, line),
            '#' => single(&mut tokens, TokenKind::Hash, &mut i, line),
            '?' => single(&mut tokens, TokenKind::Question, &mut i, line),
            ':' => single(&mut tokens, TokenKind::Colon, &mut i, line),
            '+' => single(&mut tokens, TokenKind::Plus, &mut i, line),
            '*' => single(&mut tokens, TokenKind::Star, &mut i, line),
            '%' => single(&mut tokens, TokenKind::Percent, &mut i, line),
            '-' => {
                if peek(&chars, i + 1) == Some('>') {
                    tokens.push(Token {
                        kind: TokenKind::Arrow,
                        line,
                    });
                    i += 2;
                } else {
                    single(&mut tokens, TokenKind::Minus, &mut i, line);
                }
            }
            '=' => {
                if peek(&chars, i + 1) == Some('=') {
                    tokens.push(Token {
                        kind: TokenKind::EqEq,
                        line,
                    });
                    i += 2;
                } else if peek(&chars, i + 1) == Some('>') {
                    tokens.push(Token {
                        kind: TokenKind::FatArrow,
                        line,
                    });
                    i += 2;
                } else {
                    single(&mut tokens, TokenKind::Assign, &mut i, line);
                }
            }
            '!' => {
                if peek(&chars, i + 1) == Some('=') {
                    tokens.push(Token {
                        kind: TokenKind::NotEq,
                        line,
                    });
                    i += 2;
                } else {
                    single(&mut tokens, TokenKind::Bang, &mut i, line);
                }
            }
            '<' => {
                if peek(&chars, i + 1) == Some('=') {
                    tokens.push(Token {
                        kind: TokenKind::Le,
                        line,
                    });
                    i += 2;
                } else {
                    single(&mut tokens, TokenKind::Lt, &mut i, line);
                }
            }
            '>' => {
                if peek(&chars, i + 1) == Some('=') {
                    tokens.push(Token {
                        kind: TokenKind::Ge,
                        line,
                    });
                    i += 2;
                } else {
                    single(&mut tokens, TokenKind::Gt, &mut i, line);
                }
            }
            '&' => {
                if peek(&chars, i + 1) == Some('&') {
                    tokens.push(Token {
                        kind: TokenKind::AndAnd,
                        line,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Amp,
                        line,
                    });
                    i += 1;
                }
            }
            '|' => {
                if peek(&chars, i + 1) == Some('|') {
                    tokens.push(Token {
                        kind: TokenKind::OrOr,
                        line,
                    });
                    i += 2;
                } else {
                    return Err(LexError::new("unexpected '|'", line));
                }
            }
            '/' => single(&mut tokens, TokenKind::Slash, &mut i, line),
            _ => {
                return Err(LexError::new(
                    format!("unexpected character '{}'", c),
                    line,
                ))
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        line,
    });
    Ok(tokens)
}

fn single(tokens: &mut Vec<Token>, kind: TokenKind, i: &mut usize, line: u32) {
    tokens.push(Token { kind, line });
    *i += 1;
}

fn peek(chars: &[char], i: usize) -> Option<char> {
    chars.get(i).copied()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn keyword(word: &str) -> Option<Keyword> {
    Some(match word {
        "mod" => Keyword::Mod,
        "use" => Keyword::Use,
        "as" => Keyword::As,
        "pub" => Keyword::Pub,
        "type" => Keyword::Type,
        "struct" => Keyword::Struct,
        "obj" => Keyword::Obj,
        "interface" => Keyword::Interface,
        "enum" => Keyword::Enum,
        "func" => Keyword::Func,
        "async" => Keyword::Async,
        "await" => Keyword::Await,
        "let" => Keyword::Let,
        "mut" => Keyword::Mut,
        "const" => Keyword::Const,
        "static" => Keyword::Static,
        "self" => Keyword::SelfRef,
        "return" => Keyword::Return,
        "defer" => Keyword::Defer,
        "if" => Keyword::If,
        "elif" => Keyword::Elif,
        "else" => Keyword::Else,
        "for" => Keyword::For,
        "loop" => Keyword::Loop,
        "while" => Keyword::While,
        "in" => Keyword::In,
        "match" => Keyword::Match,
        "parallel" => Keyword::Parallel,
        "break" => Keyword::Break,
        "continue" => Keyword::Continue,
        "read" => Keyword::Read,
        "write" => Keyword::Write,
        "share" => Keyword::Share,
        "true" => Keyword::True,
        "false" => Keyword::False,
        "null" => Keyword::Null,
        "ok" => Keyword::Ok,
        "err" => Keyword::Err,
        "some" => Keyword::Some,
        "none" => Keyword::None,
        _ => return None,
    })
}

fn lex_string(
    chars: &[char],
    mut i: usize,
    line: u32,
    quote: char,
) -> Result<(String, usize, u32), LexError> {
    let mut out = String::new();
    let mut current_line = line;
    while i < chars.len() {
        let c = chars[i];
        if c == quote {
            return Ok((out, i + 1, current_line));
        }
        if c == '\n' {
            if quote == '\'' {
                return Err(LexError::new("unterminated character literal", line));
            }
            current_line += 1;
            out.push('\n');
            i += 1;
            continue;
        }
        if c == '\\' {
            i += 1;
            if i >= chars.len() {
                return Err(LexError::new("unterminated escape sequence", current_line));
            }
            let esc = chars[i];
            match esc {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\\' => out.push('\\'),
                _ => {
                    return Err(LexError::new(
                        format!("unsupported escape '\\{}'", esc),
                        current_line,
                    ))
                }
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Err(LexError::new("unterminated string literal", line))
}

/// `"""` 多行模板：内容原样保留（含换行与缩进），只把 `\${` 与 `\\` 交给 parser 处理。
fn lex_template(
    chars: &[char],
    mut i: usize,
    line: u32,
) -> Result<(String, usize, u32), LexError> {
    let mut out = String::new();
    let mut current_line = line;
    while i < chars.len() {
        if chars[i] == '"'
            && chars.get(i + 1) == Some(&'"')
            && chars.get(i + 2) == Some(&'"')
        {
            return Ok((out, i + 3, current_line));
        }
        if chars[i] == '\n' {
            current_line += 1;
        }
        out.push(chars[i]);
        i += 1;
    }
    Err(LexError::new("unterminated template literal", line))
}

fn lex_number(
    chars: &[char],
    mut i: usize,
    line: u32,
) -> Result<(TokenKind, usize), LexError> {
    let mut base = IntBase::Dec;
    if chars[i] == '0' {
        if let Some(mark) = peek(chars, i + 1) {
            let b = match mark {
                'b' | 'B' => Some((IntBase::Bin, 2u32)),
                'o' | 'O' => Some((IntBase::Oct, 8u32)),
                'x' | 'X' => Some((IntBase::Hex, 16u32)),
                _ => None,
            };
            if let Some((kind, radix)) = b {
                base = kind;
                i += 2;
                let digits_start = i;
                while i < chars.len() && (chars[i].is_digit(radix) || chars[i] == '_') {
                    i += 1;
                }
                let digits: String = chars[digits_start..i]
                    .iter()
                    .filter(|c| **c != '_')
                    .collect();
                if digits.is_empty() {
                    return Err(LexError::new("integer literal has no digits", line));
                }
                let value = u128::from_str_radix(&digits, radix).map_err(|_| {
                    LexError::new("integer literal is too large", line)
                })?;
                let (suffix, next) = int_suffix(chars, i, line)?;
                check_literal_fits(value, suffix, line)?;
                return Ok((
                    TokenKind::Int {
                        value: value as i128,
                        base,
                        suffix,
                    },
                    next,
                ));
            }
        }
    }

    let digits_start = i;
    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
        i += 1;
    }
    let mut is_float = false;
    if i < chars.len() && chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()
    {
        is_float = true;
        i += 1;
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
            i += 1;
        }
    }
    // 指数部分：`1e9` / `2.5e-3`。只有后面紧跟数字时才当作指数。
    if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            is_float = true;
            i = j;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    let text: String = chars[digits_start..i]
        .iter()
        .filter(|c| **c != '_')
        .collect();

    if is_float {
        let value: f64 = text
            .parse()
            .map_err(|_| LexError::new("invalid float literal", line))?;
        // float 只有 f64 一种宽度，后缀写成 f64 仅为可读性。
        let (suffix, next) = float_suffix(chars, i, line)?;
        let _ = suffix;
        return Ok((TokenKind::Float(value), next));
    }

    let value: u128 = text
        .parse()
        .map_err(|_| LexError::new("integer literal is too large", line))?;
    let (suffix, next) = int_suffix(chars, i, line)?;
    check_literal_fits(value, suffix, line)?;
    Ok((
        TokenKind::Int {
            value: value as i128,
            base,
            suffix,
        },
        next,
    ))
}

/// 读取整数字面量的类型后缀。没有后缀返回 `None`；未知后缀是词法错误。
fn int_suffix(
    chars: &[char],
    i: usize,
    line: u32,
) -> Result<(Option<IntSuffix>, usize), LexError> {
    let (word, next) = ident_at(chars, i);
    if word.is_empty() {
        return Ok((None, i));
    }
    match IntSuffix::from_name(&word) {
        Some(suffix) => Ok((Some(suffix), next)),
        None => Err(LexError::new(
            format!(
                "invalid integer literal suffix '{}'; expected i8/i16/i32/i64/u8/u16/u32/u64",
                word
            ),
            line,
        )),
    }
}

/// 读取浮点字面量的类型后缀：只接受 `f64`（float 只有 64 位）。
fn float_suffix(chars: &[char], i: usize, line: u32) -> Result<(Option<&'static str>, usize), LexError> {
    let (word, next) = ident_at(chars, i);
    if word.is_empty() {
        return Ok((None, i));
    }
    if word == "f64" {
        return Ok((Some("f64"), next));
    }
    Err(LexError::new(
        format!("invalid float literal suffix '{}'; only f64 is supported", word),
        line,
    ))
}

fn ident_at(chars: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    while i < chars.len() && is_ident_continue(chars[i]) {
        i += 1;
    }
    (chars[start..i].iter().collect(), i)
}

/// 后缀存在时字面量必须能装进该类型；无后缀的字面量按 `int` 检查。
///
/// 有符号类型额外允许到 `|min|`（例如 `-128i8`），因为负号是单独的一元运算，
/// 真正的范围检查在类型检查阶段按最终（可能已取负）的值进行。
fn check_literal_fits(value: u128, suffix: Option<IntSuffix>, line: u32) -> Result<(), LexError> {
    let (min, max) = match suffix {
        Some(suffix) if suffix.is_signed() => (0i128, 1i128 << (suffix.bits() - 1)),
        Some(suffix) => suffix.range(),
        None => (0i128, i64::MAX as i128 + 1),
    };
    let value = value as i128;
    if value < min || value > max {
        let name = match suffix {
            Some(suffix) => suffix.name().to_string(),
            None => "int".to_string(),
        };
        return Err(LexError::new(
            format!("integer literal {} does not fit {}", value, name),
            line,
        ));
    }
    Ok(())
}

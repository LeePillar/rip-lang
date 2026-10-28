//! 原生包接口声明（`.rp.decl`）。
//!
//! 声明文件是 Rust 侧实现的包在 Rip 侧的**唯一签名来源**：类型检查、
//! AOT 代码生成与插件加载都读同一份内容，保证三种执行方式语义一致。
//!
//! 语法：
//!
//! ```text
//! abi = 2
//! prefix = "acme_tools"              // abi 2 必填：四个符号带该前缀
//! platform = "windows-x86_64"
//! sha256 = "…"                       // 可选：插件文件摘要
//!
//! pub func to_string(v: any) -> result<string, string>
//! pub func from_str(s: string) -> result<any, string>
//! ```
//!
//! `abi = 1` 是旧契约：符号名固定为 `rp_native_abi/call/free`，同一产物只能
//! 静态链接一个这样的包。`abi = 2` 要求声明 `prefix`，符号变成
//! `rp_native_<prefix>_abi/call/free`，任意多个包可以共存于同一进程或产物。

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::builtins::BuiltinSig;
use crate::Type;

/// 当前插件 ABI 版本。加载器接受 `MIN_ABI_VERSION..=ABI_VERSION`。
pub const ABI_VERSION: u32 = 2;

/// 仍可加载的最低 ABI 版本（`abi = 1` 使用固定符号名，仅支持单插件）。
pub const MIN_ABI_VERSION: u32 = 1;

/// 声明中的一条函数签名。
#[derive(Debug, Clone)]
pub struct NativeFunc {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
    /// 声明文件中的行号，用于诊断。
    pub line: u32,
}

/// 一个原生包的完整声明。
#[derive(Debug, Clone, Default)]
pub struct NativeDecl {
    pub abi: u32,
    pub platform: Option<String>,
    pub sha256: Option<String>,
    /// 符号前缀：设置后四个 ABI 符号带 `<prefix>_` 前缀，
    /// 多个原生包可以同时静态链接进同一个产物。
    pub prefix: Option<String>,
    pub funcs: Vec<NativeFunc>,
}

impl NativeDecl {
    /// 转成类型检查使用的签名表。
    pub fn signatures(&self) -> Vec<(String, BuiltinSig)> {
        self.funcs
            .iter()
            .map(|func| {
                (
                    func.name.clone(),
                    BuiltinSig {
                        params: func.params.clone(),
                        ret: func.ret.clone(),
                        variadic_any: false,
                    },
                )
            })
            .collect()
    }

    pub fn func(&self, name: &str) -> Option<&NativeFunc> {
        self.funcs.iter().find(|func| func.name == name)
    }
}

/// 解析 `.rp.decl` 文本。
pub fn parse_decl(text: &str) -> Result<NativeDecl, String> {
    let mut decl = NativeDecl {
        abi: MIN_ABI_VERSION,
        ..Default::default()
    };
    let mut saw_abi = false;
    for (index, raw) in text.lines().enumerate() {
        let line_no = index as u32 + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("abi") {
            let value = value.trim();
            let value = match value.strip_prefix('=') {
                Some(rest) => rest.trim(),
                None => return Err(format!("line {}: expected 'abi = <n>'", line_no)),
            };
            let abi: u32 = value
                .parse()
                .map_err(|_| format!("line {}: invalid abi '{}'", line_no, value))?;
            if abi < MIN_ABI_VERSION || abi > ABI_VERSION {
                return Err(format!(
                    "line {}: unsupported abi {} (this build supports {}..={})",
                    line_no, abi, MIN_ABI_VERSION, ABI_VERSION
                ));
            }
            saw_abi = true;
            decl.abi = abi;
            continue;
        }
        if let Some(value) = line.strip_prefix("platform") {
            if let Some(rest) = value.trim().strip_prefix('=') {
                decl.platform = Some(unquote(rest.trim()).to_string());
                continue;
            }
            return Err(format!("line {}: expected 'platform = \"…\"'", line_no));
        }
        if let Some(value) = line.strip_prefix("sha256") {
            if let Some(rest) = value.trim().strip_prefix('=') {
                decl.sha256 = Some(unquote(rest.trim()).to_string());
                continue;
            }
            return Err(format!("line {}: expected 'sha256 = \"…\"'", line_no));
        }
        if let Some(value) = line.strip_prefix("prefix") {
            if let Some(rest) = value.trim().strip_prefix('=') {
                let prefix = unquote(rest.trim()).to_string();
                if prefix.is_empty()
                    || !prefix
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    return Err(format!(
                        "line {}: prefix '{}' must contain only ASCII letters, digits and '_'",
                        line_no, prefix
                    ));
                }
                decl.prefix = Some(prefix);
                continue;
            }
            return Err(format!("line {}: expected 'prefix = \"…\"'", line_no));
        }
        let func_line = line.strip_prefix("pub ").unwrap_or(line);
        if let Some(rest) = func_line.strip_prefix("func ") {
            decl.funcs.push(parse_func(rest, line_no)?);
            continue;
        }
        return Err(format!("line {}: unknown declaration '{}'", line_no, line));
    }
    if !saw_abi {
        // 未写 `abi` 的旧声明按 v1 处理，避免已有插件因升级工具链而全部失效。
        decl.abi = MIN_ABI_VERSION;
    }
    if decl.abi >= 2 && decl.prefix.is_none() {
        return Err(
            "abi 2 requires 'prefix': each plugin needs its own rp_native_<prefix>_* symbols so several plugins can be linked into one build"
                .to_string(),
        );
    }
    let mut seen = std::collections::HashSet::new();
    for func in &decl.funcs {
        if !seen.insert(func.name.clone()) {
            return Err(format!(
                "line {}: function '{}' is declared twice",
                func.line, func.name
            ));
        }
    }
    Ok(decl)
}

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(index) => &line[..index],
        None => line,
    }
}

fn unquote(text: &str) -> &str {
    text.trim().trim_matches('"')
}

fn parse_func(text: &str, line: u32) -> Result<NativeFunc, String> {
    let open = text
        .find('(')
        .ok_or_else(|| format!("line {}: expected 'func name(params)'", line))?;
    let name = text[..open].trim();
    if name.is_empty() {
        return Err(format!("line {}: function name is empty", line));
    }
    let close = match_paren(text, open)
        .ok_or_else(|| format!("line {}: missing ')' in function '{}'", line, name))?;
    let params_text = &text[open + 1..close];
    let rest = text[close + 1..].trim();
    let ret_text = rest
        .strip_prefix("->")
        .ok_or_else(|| format!("line {}: function '{}' needs '-> return_type'", line, name))?
        .trim();
    if ret_text.is_empty() {
        return Err(format!("line {}: function '{}' has no return type", line, name));
    }
    let mut params = Vec::new();
    for part in split_top_level(params_text) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (param_name, param_ty) = part
            .split_once(':')
            .ok_or_else(|| format!("line {}: parameter '{}' needs ': type'", line, part))?;
        if param_name.trim().is_empty() {
            return Err(format!("line {}: parameter name is empty", line));
        }
        params.push(parse_type_text(param_ty.trim(), line)?);
    }
    Ok(NativeFunc {
        name: name.to_string(),
        params,
        ret: parse_type_text(ret_text, line)?,
        line,
    })
}

/// 找到与 `open` 处 '(' 匹配的 ')' 下标。
fn match_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层逗号切分（忽略 `<>` 内部的逗号）。
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn parse_type_text(text: &str, line: u32) -> Result<Type, String> {
    let mut parser = TypeParser {
        src: text.as_bytes(),
        pos: 0,
    };
    let ty = parser.parse_type(0, line)?;
    parser.skip_ws();
    if parser.pos != parser.src.len() {
        return Err(format!(
            "line {}: unexpected trailing text '{}' in type",
            line,
            &text[parser.pos..]
        ));
    }
    Ok(ty)
}

struct TypeParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> TypeParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, ch: u8) -> bool {
        self.skip_ws();
        if self.pos < self.src.len() && self.src[self.pos] == ch {
            self.pos += 1;
            return true;
        }
        false
    }

    fn ident(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.src.len() {
            let ch = self.src[self.pos];
            if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return None;
        }
        Some(String::from_utf8_lossy(&self.src[start..self.pos]).to_string())
    }

    fn parse_type(&mut self, depth: usize, line: u32) -> Result<Type, String> {
        if depth > 16 {
            return Err(format!("line {}: type nesting is too deep", line));
        }
        let name = self
            .ident()
            .ok_or_else(|| format!("line {}: expected a type name", line))?;
        let ty = match name.as_str() {
            "void" => Type::Void,
            "int" | "i64" => Type::Int,
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "float" | "f64" => Type::Float,
            "string" | "char" => Type::Str,
            "bool" => Type::Bool,
            "any" => Type::Any,
            "vec" => {
                let inner = self.parse_args(1, depth, line)?;
                Type::Vec(Box::new(inner.into_iter().next().unwrap_or(Type::Any)))
            }
            "option" => {
                let inner = self.parse_args(1, depth, line)?;
                Type::Option(Box::new(inner.into_iter().next().unwrap_or(Type::Any)))
            }
            "share" => {
                let inner = self.parse_args(1, depth, line)?;
                Type::Share(Box::new(inner.into_iter().next().unwrap_or(Type::Any)))
            }
            "map" => {
                let mut inner = self.parse_args(2, depth, line)?;
                let value = inner.pop().unwrap_or(Type::Any);
                let key = inner.pop().unwrap_or(Type::Any);
                Type::Map(Box::new(key), Box::new(value))
            }
            "result" => {
                let mut inner = self.parse_args(2, depth, line)?;
                let err = inner.pop().unwrap_or(Type::Str);
                let ok = inner.pop().unwrap_or(Type::Any);
                Type::Result(Box::new(ok), Box::new(err))
            }
            other => Type::Named(other.to_string()),
        };
        Ok(ty)
    }

    fn parse_args(&mut self, count: usize, depth: usize, line: u32) -> Result<Vec<Type>, String> {
        if !self.eat(b'<') {
            return Err(format!(
                "line {}: generic type needs '<…>' arguments",
                line
            ));
        }
        let mut args = Vec::new();
        for index in 0..count {
            if index > 0 && !self.eat(b',') {
                return Err(format!("line {}: expected ',' in type arguments", line));
            }
            args.push(self.parse_type(depth + 1, line)?);
        }
        if !self.eat(b'>') {
            return Err(format!("line {}: expected '>' in type arguments", line));
        }
        Ok(args)
    }
}

fn registry() -> &'static RwLock<HashMap<String, NativeDecl>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, NativeDecl>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 注册（或覆盖）一个原生包的声明。重复注册以最后一次为准。
pub fn register_native_package(module: &str, decl: NativeDecl) {
    if let Ok(mut map) = registry().write() {
        map.insert(module.to_string(), decl);
    }
}

/// 查询已注册的原生包声明。
pub fn native_decl(module: &str) -> Option<NativeDecl> {
    registry().read().ok()?.get(module).cloned()
}

/// 按模块名 + 函数名查询签名，供类型检查使用。
pub fn native_package_function(module: &str, name: &str) -> Option<BuiltinSig> {
    let decl = native_decl(module)?;
    let func = decl.func(name)?;
    Some(BuiltinSig {
        params: func.params.clone(),
        ret: func.ret.clone(),
        variadic_any: false,
    })
}

/// 清空注册表（测试用）。
pub fn clear_native_packages() {
    if let Ok(mut map) = registry().write() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_decl() {
        let decl = parse_decl(
            "abi = 1\nplatform = \"windows-x86_64\"\n\npub func to_string(v: any) -> result<string, string>\n",
        )
        .unwrap();
        assert_eq!(decl.abi, 1);
        assert_eq!(decl.platform.as_deref(), Some("windows-x86_64"));
        assert_eq!(decl.funcs.len(), 1);
        assert_eq!(decl.funcs[0].name, "to_string");
        assert_eq!(decl.funcs[0].params, vec![Type::Any]);
    }

    #[test]
    fn parses_container_types() {
        let decl = parse_decl("pub func f(a: vec<map<string, int>>, b: option<result<int, string>>) -> void\n")
            .unwrap();
        assert_eq!(decl.funcs[0].params.len(), 2);
        assert!(matches!(decl.funcs[0].params[0], Type::Vec(_)));
        assert!(matches!(decl.funcs[0].ret, Type::Void));
    }

    #[test]
    fn rejects_bad_abi() {
        assert!(parse_decl("abi = 9\n").is_err());
    }

    #[test]
    fn ignores_comments() {
        let decl = parse_decl("// 注释\nabi = 1\n\npub func f() -> void // 尾注释\n").unwrap();
        assert_eq!(decl.funcs.len(), 1);
    }

    #[test]
    fn detects_duplicate_functions() {
        assert!(parse_decl("pub func f() -> void\npub func f() -> int\n").is_err());
    }

    #[test]
    fn parses_symbol_prefix() {
        let decl = parse_decl("abi = 1\nprefix = \"acme_tools\"\n\npub func f() -> void\n").unwrap();
        assert_eq!(decl.prefix.as_deref(), Some("acme_tools"));
        // 前缀会进入生成的 C 符号名，必须是合法标识符片段。
        assert!(parse_decl("prefix = \"acme-tools\"\n").is_err());
        assert!(parse_decl("prefix = \"\"\n").is_err());
    }

    #[test]
    fn abi_two_requires_prefix() {
        let error = parse_decl("abi = 2\n\npub func f() -> void\n").expect_err("needs prefix");
        assert!(error.contains("requires 'prefix'"));
        let decl = parse_decl("abi = 2\nprefix = \"acme_tools\"\n\npub func f() -> void\n").unwrap();
        assert_eq!(decl.abi, 2);
    }

    #[test]
    fn legacy_decl_without_abi_is_version_one() {
        // 未写 `abi` 的旧声明继续按 v1 工作，不需要补 prefix。
        let decl = parse_decl("pub func f() -> void\n").unwrap();
        assert_eq!(decl.abi, MIN_ABI_VERSION);
        assert!(decl.prefix.is_none());
    }

    #[test]
    fn registers_and_queries() {
        clear_native_packages();
        let decl = parse_decl("pub func f(a: int) -> string\n").unwrap();
        register_native_package("demo/native", decl);
        let sig = native_package_function("demo/native", "f").unwrap();
        assert_eq!(sig.params, vec![Type::Int]);
        assert_eq!(sig.ret, Type::Str);
        assert!(native_package_function("demo/native", "missing").is_none());
        clear_native_packages();
    }
}

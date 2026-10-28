//! Rip 语言服务器（LSP）。
//!
//! 通过标准输入输出实现最小可用的 JSON-RPC 服务：
//! `initialize` / `shutdown` / `textDocument/didOpen` / `textDocument/didChange` /
//! `textDocument/hover` / `textDocument/definition`。
//! 文档变更后立即运行「词法 → 语法 → 类型 → 所有权」完整前端，并推送
//! `textDocument/publishDiagnostics`，与 CLI 的检查结果保持一致。

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Mutex, OnceLock};

use rp_ast::{Item, Stmt, StmtKind, TypeExpr, TypeExprKind};
use serde_json::{json, Value};

/// 已打开的文档全文：跳转与悬浮都基于最后一次同步的内容。
fn documents() -> &'static Mutex<HashMap<String, String>> {
    static DOCUMENTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    DOCUMENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 阻塞运行语言服务器，返回进程退出码。
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        let message = match read_message(&mut reader) {
            Ok(Some(value)) => value,
            Ok(None) => return 0,
            Err(_) => return 1,
        };

        let method = message
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        for reply in handle(&method, &params, id) {
            if write_message(&mut writer, &reply).is_err() {
                return 1;
            }
        }

        if method == "exit" {
            return 0;
        }
    }
}

fn handle(method: &str, params: &Value, id: Option<Value>) -> Vec<Value> {
    match method {
        "initialize" => vec![response(
            id,
            json!({
                "capabilities": {
                    "textDocumentSync": 1,
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "completionProvider": {
                        "triggerCharacters": ["."],
                        "resolveProvider": false
                    },
                    "referencesProvider": true
                },
                "serverInfo": { "name": "rip-lsp", "version": env!("CARGO_PKG_VERSION") }
            }),
        )],
        "initialized" => Vec::new(),
        "shutdown" => vec![response(id, Value::Null)],
        "textDocument/didOpen" => {
            let doc = &params["textDocument"];
            let uri = doc.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            let text = doc.get("text").and_then(|v| v.as_str()).unwrap_or("");
            remember(uri, text);
            publish_diagnostics(uri, text, None)
        }
        "textDocument/didChange" => {
            let uri = params["textDocument"]
                .get("uri")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = params["contentChanges"]
                .as_array()
                .and_then(|changes| changes.last())
                .and_then(|change| change.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            remember(uri, text);
            publish_diagnostics(uri, text, None)
        }
        "textDocument/hover" => vec![response(id, hover(params))],
        "textDocument/definition" => vec![response(id, definition(params))],
        "textDocument/completion" => vec![response(id, completion(params))],
        "textDocument/references" => vec![response(id, references(params))],
        _ => {
            if id.is_some() {
                vec![response(id, Value::Null)]
            } else {
                Vec::new()
            }
        }
    }
}

fn remember(uri: &str, text: &str) {
    if let Ok(mut documents) = documents().lock() {
        documents.insert(uri.to_string(), text.to_string());
    }
}

fn remembered(uri: &str) -> Option<String> {
    documents().lock().ok()?.get(uri).cloned()
}

/// 请求的文档、行、列，以及该位置上的标识符。
fn request_target(params: &Value) -> Option<(String, String, u32, u32)> {
    let uri = params["textDocument"]
        .get("uri")
        .and_then(|value| value.as_str())?
        .to_string();
    let position = &params["position"];
    let line = position.get("line").and_then(|value| value.as_u64())? as u32;
    let character = position.get("character").and_then(|value| value.as_u64())? as u32;
    let text = remembered(&uri)?;
    identifier_at(&text, line, character)?;
    Some((uri, text, line, character))
}

/// 取出 `line` 行 `character` 列所在的标识符。
fn identifier_at(text: &str, line: u32, character: u32) -> Option<String> {
    let line_text = text.lines().nth(line as usize)?;
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut at = (character as usize).min(chars.len() - 1);
    if !is_word(chars[at]) {
        if at > 0 && is_word(chars[at - 1]) {
            at -= 1;
        } else {
            return None;
        }
    }
    let mut start = at;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < chars.len() && is_word(chars[end + 1]) {
        end += 1;
    }
    Some(chars[start..=end].iter().collect())
}

/// 光标处是否是 `别名.成员` 形式，返回（别名, 成员）。
fn member_at(text: &str, line: u32, character: u32) -> Option<(String, String)> {
    let line_text = text.lines().nth(line as usize)?;
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut at = (character as usize).min(chars.len() - 1);
    if !is_word(chars[at]) {
        if at > 0 && is_word(chars[at - 1]) {
            at -= 1;
        } else {
            return None;
        }
    }
    let mut start = at;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < chars.len() && is_word(chars[end + 1]) {
        end += 1;
    }
    let member: String = chars[start..=end].iter().collect();
    if start == 0 || chars[start - 1] != '.' {
        return None;
    }
    let dot = start - 1;
    let mut alias_start = dot;
    while alias_start > 0 && is_word(chars[alias_start - 1]) {
        alias_start -= 1;
    }
    if alias_start == dot {
        return None;
    }
    let alias: String = chars[alias_start..dot].iter().collect();
    Some((alias, member))
}

fn definition(params: &Value) -> Value {
    let Some((uri, text, line, character)) = request_target(params) else {
        return Value::Null;
    };
    let Some(identifier) = identifier_at(&text, line, character) else {
        return Value::Null;
    };
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return Value::Null;
    };
    for item in &program.items {
        if let Item::Func(func) = item {
            if func.name == identifier {
                return location(&uri, &text, func.line);
            }
        }
    }
    match find_declaration(&program, &identifier) {
        Some(declared) => location(&uri, &text, declared),
        None => Value::Null,
    }
}

fn hover(params: &Value) -> Value {
    let Some((_, text, line, character)) = request_target(params) else {
        return Value::Null;
    };
    let Some(identifier) = identifier_at(&text, line, character) else {
        return Value::Null;
    };
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return Value::Null;
    };
    for item in &program.items {
        if let Item::Func(func) = item {
            if func.name == identifier {
                return markdown(format!("```rip\n{}\n```", signature(func)));
            }
        }
    }
    if let Some((_, ty)) = find_declaration_typed(&program, &identifier) {
        return markdown(format!("```rip\nlet {}: {}\n```", identifier, ty));
    }
    // `别名.成员`：内建标准库函数给出签名。
    if let Some((alias, member)) = member_at(&text, line, character) {
        for item in &program.items {
            if let Item::Import(import) = item {
                let expected = import
                    .alias
                    .clone()
                    .unwrap_or_else(|| {
                        import
                            .path
                            .rsplit('/')
                            .next()
                            .unwrap_or(&import.path)
                            .to_string()
                    });
                if expected != alias {
                    continue;
                }
                if let Some(sig) = rp_types::builtins::builtin_function(&import.path, &member) {
                    let params: Vec<String> =
                        sig.params.iter().map(type_text_builtin).collect();
                    return markdown(format!(
                        "```rip\n{}.{}({}) -> {}\n```",
                        alias,
                        member,
                        params.join(", "),
                        type_text_builtin(&sig.ret)
                    ));
                }
            }
        }
    }
    Value::Null
}

/// 补全候选里附带的关键字与内建类型名。
const KEYWORDS: &[&str] = &[
    "mod", "use", "as", "pub", "type", "interface", "enum", "func", "let", "mut", "const", "static", "self",
    "return", "defer", "if", "else", "for", "in", "loop", "while", "match", "parallel", "break", "continue", "read",
    "write", "share", "true", "false", "null", "ok", "err", "some", "none",
];

const BUILTIN_TYPES: &[&str] = &[
    "int", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "float", "string", "bool",
    "char", "void", "any", "vec", "map", "option", "result", "share", "func",
];

/// 光标左侧连续标识符字符。
fn trailing_word(chars: &[char], at: usize) -> String {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut start = at.min(chars.len());
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    chars[start..at.min(chars.len())].iter().collect()
}

/// 光标处于 `别名.` 或 `别名.前缀` 形式时，返回别名。
fn member_alias(chars: &[char], at: usize) -> Option<String> {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut start = at.min(chars.len());
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    if start == 0 || chars[start - 1] != '.' {
        return None;
    }
    let dot = start - 1;
    let mut alias_start = dot;
    while alias_start > 0 && is_word(chars[alias_start - 1]) {
        alias_start -= 1;
    }
    if alias_start == dot {
        return None;
    }
    Some(chars[alias_start..dot].iter().collect())
}

/// 导入别名 → 包路径。
fn import_path(program: &rp_ast::Program, alias: &str) -> Option<String> {
    for item in &program.items {
        if let Item::Import(import) = item {
            let expected = import.alias.clone().unwrap_or_else(|| {
                import
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&import.path)
                    .to_string()
            });
            if expected == alias {
                return Some(import.path.clone());
            }
        }
    }
    None
}

fn builtin_params(params: &[rp_types::Type]) -> String {
    params.iter().map(type_text_builtin).collect::<Vec<_>>().join(", ")
}

/// `textDocument/completion`：标识符补全与 `别名.成员` 补全。
fn completion(params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return json!([]);
    };
    let Some(text) = remembered(uri) else {
        return json!([]);
    };
    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;
    let Some(line_text) = text.lines().nth(line as usize) else {
        return json!([]);
    };
    let chars: Vec<char> = line_text.chars().collect();
    let at = (character as usize).min(chars.len());
    let prefix = trailing_word(&chars, at);
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return json!([]);
    };

    // `别名.`：列出该内建模块的函数与签名。
    if let Some(alias) = member_alias(&chars, at) {
        let Some(path) = import_path(&program, &alias) else {
            return json!([]);
        };
        let mut items = Vec::new();
        for name in rp_types::builtins::builtin_module_names(&path) {
            if !prefix.is_empty() && !name.starts_with(&prefix) {
                continue;
            }
            let detail = match rp_types::builtins::builtin_function(&path, name) {
                Some(sig) => format!(
                    "{}.{}({}) -> {}",
                    alias,
                    name,
                    builtin_params(&sig.params),
                    type_text_builtin(&sig.ret)
                ),
                None => format!("{}.{}", alias, name),
            };
            items.push(json!({ "label": name, "kind": 3, "detail": detail, "sortText": "1" }));
        }
        return json!(items);
    }

    // (label, kind, detail, sortText)：局部绑定优先，其次文档内条目，最后关键字与类型。
    let mut entries: Vec<(String, u8, String, &'static str)> = Vec::new();
    for (name, detail) in visible_bindings(&program, line) {
        if name.starts_with(&prefix) {
            entries.push((name, 6, detail, "0"));
        }
    }
    for item in &program.items {
        match item {
            Item::Func(func) => {
                if func.name.starts_with(&prefix) {
                    entries.push((func.name.clone(), 3, signature(func), "1"));
                }
            }
            Item::Type(decl) => {
                if decl.name.starts_with(&prefix) {
                    entries.push((
                        decl.name.clone(),
                        7,
                        format!("type {} = {}", decl.name, type_text(&decl.ty)),
                        "2",
                    ));
                }
            }
            Item::Interface(decl) => {
                if decl.name.starts_with(&prefix) {
                    entries.push((
                        decl.name.clone(),
                        7,
                        format!("interface {} ({} methods)", decl.name, decl.methods.len()),
                        "2",
                    ));
                }
            }
            Item::Enum(decl) => {
                if decl.name.starts_with(&prefix) {
                    entries.push((
                        decl.name.clone(),
                        7,
                        format!("enum {} ({} variants)", decl.name, decl.variants.len()),
                        "2",
                    ));
                }
            }
            Item::Const(decl) => {
                if decl.name.starts_with(&prefix) {
                    entries.push((
                        decl.name.clone(),
                        21,
                        format!("const {}: {}", decl.name, type_text(&decl.ty)),
                        "2",
                    ));
                }
            }
            Item::Static(decl) => {
                if decl.name.starts_with(&prefix) {
                    entries.push((
                        decl.name.clone(),
                        21,
                        format!("static {}: {}", decl.name, type_text(&decl.ty)),
                        "2",
                    ));
                }
            }
            Item::Import(import) => {
                let alias = import.alias.clone().unwrap_or_else(|| {
                    import
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&import.path)
                        .to_string()
                });
                if alias.starts_with(&prefix) {
                    entries.push((alias, 9, import.path.clone(), "3"));
                }
            }
        }
    }
    for keyword in KEYWORDS {
        if keyword.starts_with(&prefix) {
            entries.push(((*keyword).to_string(), 14, "keyword".to_string(), "4"));
        }
    }
    for name in BUILTIN_TYPES {
        if name.starts_with(&prefix) {
            entries.push(((*name).to_string(), 7, "type".to_string(), "4"));
        }
    }
    entries.sort_by(|a, b| a.3.cmp(b.3).then_with(|| a.0.cmp(&b.0)));
    // 同一个名字只留优先级最高的一项。
    let mut seen = std::collections::HashSet::new();
    entries.retain(|(label, ..)| seen.insert(label.clone()));
    json!(entries
        .into_iter()
        .map(|(label, kind, detail, _)| json!({
            "label": label,
            "kind": kind,
            "detail": detail,
        }))
        .collect::<Vec<_>>())
}

/// 光标所在函数里、声明行不晚于光标行的全部绑定（参数、let、for、match 解构）。
fn visible_bindings(program: &rp_ast::Program, line: u32) -> Vec<(String, String)> {
    let mut target: Option<&rp_ast::FuncDecl> = None;
    for item in &program.items {
        if let Item::Func(func) = item {
            if func.line <= line + 1 {
                target = Some(func);
            }
        }
    }
    let Some(func) = target else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for param in &func.params {
        out.push((
            param.name.clone(),
            format!("{}: {}", param.name, type_text(&param.ty)),
        ));
    }
    collect_bindings(&func.body, line + 1, &mut out);
    out
}

fn collect_bindings(stmts: &[Stmt], limit: u32, out: &mut Vec<(String, String)>) {
    use rp_ast::Pattern;
    for stmt in stmts {
        if stmt.line > limit {
            continue;
        }
        match &stmt.kind {
            StmtKind::Let { name, ty, .. } => {
                out.push((name.clone(), format!("{}: {}", name, type_text(ty))));
            }
            StmtKind::If {
                then_body,
                else_body,
                ..
            } => {
                collect_bindings(then_body, limit, out);
                if let Some(body) = else_body {
                    collect_bindings(body, limit, out);
                }
            }
            StmtKind::Loop { body } | StmtKind::While { body, .. } => {
                collect_bindings(body, limit, out)
            }
            StmtKind::ForIn { name, body, .. } => {
                out.push((name.clone(), format!("{}: any", name)));
                collect_bindings(body, limit, out);
            }
            StmtKind::WhileLet {
                pattern, body, ..
            } => {
                match pattern {
                    Pattern::Some(name) | Pattern::Ok(name) | Pattern::Err(name) => {
                        out.push((name.clone(), format!("{}: any", name)));
                    }
                    Pattern::Ary(fields) => {
                        for field in fields {
                            if field.name != "_" {
                                out.push((field.name.clone(), format!("{}: any", field.name)));
                            }
                        }
                    }
                    Pattern::Enum { fields, .. } => {
                        for field in fields {
                            if field.name != "_" {
                                out.push((field.name.clone(), format!("{}: any", field.name)));
                            }
                        }
                    }
                    Pattern::None => {}
                }
                collect_bindings(body, limit, out);
            }
            StmtKind::Match { arms, .. } => {
                for arm in arms {
                    match &arm.pattern {
                        Pattern::Some(name) | Pattern::Ok(name) | Pattern::Err(name) => {
                            out.push((name.clone(), format!("{}: any", name)));
                        }
                        Pattern::Ary(fields) => {
                            for field in fields {
                                let ty = field
                                    .ty
                                    .as_ref()
                                    .map(type_text)
                                    .unwrap_or_else(|| "any".to_string());
                                out.push((field.name.clone(), format!("{}: {}", field.name, ty)));
                            }
                        }
                        Pattern::Enum { fields, .. } => {
                            for field in fields {
                                if field.name == "_" {
                                    continue;
                                }
                                let ty = field
                                    .ty
                                    .as_ref()
                                    .map(type_text)
                                    .unwrap_or_else(|| "any".to_string());
                                out.push((field.name.clone(), format!("{}: {}", field.name, ty)));
                            }
                        }
                        Pattern::None => {}
                    }
                    collect_bindings(&arm.body, limit, out);
                }
            }
            StmtKind::Parallel { branches } => collect_bindings(branches, limit, out),
            _ => {}
        }
    }
}

/// `textDocument/references`：在所有已打开文档里查找标识符出现的位置。
fn references(params: &Value) -> Value {
    let Some((uri, text, line, character)) = request_target(params) else {
        return json!([]);
    };
    let Some(name) = identifier_at(&text, line, character) else {
        return json!([]);
    };
    let include_declaration = params["context"]["includeDeclaration"]
        .as_bool()
        .unwrap_or(true);
    let declaration = rp_parser::parse_program(&text, "<lsp>")
        .ok()
        .and_then(|program| declaration_position(&program, &text, &uri, &name));
    let snapshot: Vec<(String, String)> = match documents().lock() {
        Ok(docs) => docs.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
        Err(_) => return json!([]),
    };
    let mut locations = Vec::new();
    for (doc_uri, doc_text) in snapshot {
        for (at_line, start, end) in identifier_locations(&doc_text, &name) {
            if !include_declaration {
                if let Some((decl_uri, decl_line, decl_start)) = &declaration {
                    if &doc_uri == decl_uri && at_line == *decl_line && start == *decl_start {
                        continue;
                    }
                }
            }
            locations.push((
                doc_uri.clone(),
                json!({
                    "uri": doc_uri,
                    "range": {
                        "start": { "line": at_line, "character": start },
                        "end": { "line": at_line, "character": end }
                    }
                }),
            ));
        }
    }
    // 文档哈希表顺序不稳定；按 URI + 位置排序保证输出可复现。
    locations.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| {
                a.1["range"]["start"]["line"]
                    .as_u64()
                    .cmp(&b.1["range"]["start"]["line"].as_u64())
            })
            .then_with(|| {
                a.1["range"]["start"]["character"]
                    .as_u64()
                    .cmp(&b.1["range"]["start"]["character"].as_u64())
            })
    });
    json!(locations
        .into_iter()
        .map(|(_, location)| location)
        .collect::<Vec<_>>())
}

/// 标识符在每个文档里的全部代码位置（跳过注释与字符串，由词法器保证）。
fn identifier_locations(text: &str, name: &str) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::new();
    let Ok(tokens) = rp_lexer::lex(text) else {
        return out;
    };
    let lines: Vec<&str> = text.lines().collect();
    let width = name.chars().count() as u32;
    let mut cursors: HashMap<u32, usize> = HashMap::new();
    for token in tokens {
        let rp_lexer::TokenKind::Ident(word) = &token.kind else {
            continue;
        };
        if word != name {
            continue;
        }
        let Some(line_text) = lines.get(token.line.saturating_sub(1) as usize) else {
            continue;
        };
        let chars: Vec<char> = line_text.chars().collect();
        let from = cursors.get(&token.line).copied().unwrap_or(0);
        let Some(start) = find_word(&chars, name, from) else {
            continue;
        };
        cursors.insert(token.line, start + width as usize);
        out.push((token.line - 1, start as u32, start as u32 + width));
    }
    out
}

fn find_word(chars: &[char], name: &str, from: usize) -> Option<usize> {
    let target: Vec<char> = name.chars().collect();
    if target.is_empty() {
        return None;
    }
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut index = from;
    while index + target.len() <= chars.len() {
        let matches = chars[index..index + target.len()] == target[..]
            && (index == 0 || !is_word(chars[index - 1]))
            && (index + target.len() == chars.len() || !is_word(chars[index + target.len()]));
        if matches {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// 声明位置：先找顶层函数 / 类型 / 常量，再回落到函数体内的局部声明。
fn declaration_position(
    program: &rp_ast::Program,
    text: &str,
    uri: &str,
    name: &str,
) -> Option<(String, u32, u32)> {
    let line = declaration_line(program, name)?;
    let line_text = text.lines().nth(line.saturating_sub(1) as usize)?;
    let chars: Vec<char> = line_text.chars().collect();
    let start = find_word(&chars, name, 0).unwrap_or(0) as u32;
    Some((uri.to_string(), line.saturating_sub(1), start))
}

fn declaration_line(program: &rp_ast::Program, name: &str) -> Option<u32> {
    for item in &program.items {
        match item {
            Item::Func(func) if func.name == name => return Some(func.line),
            Item::Type(decl) if decl.name == name => return Some(decl.line),
            Item::Const(decl) | Item::Static(decl) if decl.name == name => return Some(decl.line),
            _ => {}
        }
    }
    find_declaration(program, name)
}

fn markdown(value: String) -> Value {
    json!({ "contents": { "kind": "markdown", "value": value } })
}

fn location(uri: &str, text: &str, line: u32) -> Value {
    let line = line.saturating_sub(1);
    let width = text
        .lines()
        .nth(line as usize)
        .map(|line| line.chars().count() as u64)
        .unwrap_or(1)
        .max(1);
    json!({
        "uri": uri,
        "range": {
            "start": { "line": line, "character": 0 },
            "end": { "line": line, "character": width }
        }
    })
}

fn signature(func: &rp_ast::FuncDecl) -> String {
    let params: Vec<String> = func
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, type_text(&param.ty)))
        .collect();
    format!(
        "func {}({}) -> {}",
        func.name,
        params.join(", "),
        type_text(&func.ret)
    )
}

fn type_text(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(parts) => parts.join("."),
        TypeExprKind::Ary(fields) => format!(
            "[{}]",
            fields
                .iter()
                .map(|field| format!("{}: {}", field.name, type_text(&field.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExprKind::Vec(items) if items.is_empty() => "vec".to_string(),
        TypeExprKind::Vec(items) => format!(
            "vec<{}>",
            items.iter().map(type_text).collect::<Vec<_>>().join(", ")
        ),
        TypeExprKind::Map(key, value) => {
            format!("map<{}, {}>", type_text(key), type_text(value))
        }
        TypeExprKind::Option(inner) => format!("option<{}>", type_text(inner)),
        TypeExprKind::Result(ok, err) => {
            format!("result<{}, {}>", type_text(ok), type_text(err))
        }
        TypeExprKind::Share(inner) => format!("share<{}>", type_text(inner)),
        TypeExprKind::Task(inner) => format!("task<{}>", type_text(inner)),
        TypeExprKind::Func(args, ret) => format!(
            "func({}) -> {}",
            args.iter().map(type_text).collect::<Vec<_>>().join(", "),
            type_text(ret)
        ),
        TypeExprKind::Read(inner) => format!("read {}", type_text(inner)),
        TypeExprKind::Void => "void".to_string(),
    }
}

/// 在函数体内递归查找 `let` 声明，返回所在行号。
fn find_declaration(program: &rp_ast::Program, name: &str) -> Option<u32> {
    for item in &program.items {
        if let Item::Func(func) = item {
            for param in &func.params {
                if param.name == name {
                    return Some(param.line);
                }
            }
            if let Some(line) = declaration_in(&func.body, name) {
                return Some(line);
            }
        }
    }
    None
}

/// 与 `find_declaration` 相同，同时返回声明类型。
fn find_declaration_typed(program: &rp_ast::Program, name: &str) -> Option<(u32, String)> {
    for item in &program.items {
        if let Item::Func(func) = item {
            for param in &func.params {
                if param.name == name {
                    return Some((param.line, type_text(&param.ty)));
                }
            }
            if let Some(found) = declaration_typed_in(&func.body, name) {
                return Some(found);
            }
        }
    }
    None
}

fn declaration_in(stmts: &[Stmt], name: &str) -> Option<u32> {
    declaration_typed_in(stmts, name).map(|(line, _)| line)
}

fn declaration_typed_in(stmts: &[Stmt], name: &str) -> Option<(u32, String)> {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Let { name: declared, ty, .. } if declared == name => {
                return Some((stmt.line, type_text(ty)));
            }
            StmtKind::If {
                then_body,
                else_body,
                ..
            } => {
                if let Some(found) = declaration_typed_in(then_body, name) {
                    return Some(found);
                }
                if let Some(body) = else_body {
                    if let Some(found) = declaration_typed_in(body, name) {
                        return Some(found);
                    }
                }
            }
            StmtKind::Loop { body } | StmtKind::While { body, .. } | StmtKind::WhileLet { body, .. } => {
                if let Some(found) = declaration_typed_in(body, name) {
                    return Some(found);
                }
            }
            StmtKind::ForIn { name: item, body, .. } => {
                if item == name {
                    return Some((stmt.line, "any".to_string()));
                }
                if let Some(found) = declaration_typed_in(body, name) {
                    return Some(found);
                }
            }
            StmtKind::Match { arms, .. } => {
                for arm in arms {
                    if let Some(found) = declaration_typed_in(&arm.body, name) {
                        return Some(found);
                    }
                }
            }
            StmtKind::Parallel { branches } => {
                if let Some(found) = declaration_typed_in(branches, name) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn type_text_builtin(ty: &rp_types::Type) -> String {
    use rp_types::Type;
    match ty {
        Type::Unknown => "unknown".to_string(),
        Type::Void => "void".to_string(),
        Type::Int => "int".to_string(),
        Type::I8 => "i8".to_string(),
        Type::I16 => "i16".to_string(),
        Type::I32 => "i32".to_string(),
        Type::U8 => "u8".to_string(),
        Type::U16 => "u16".to_string(),
        Type::U32 => "u32".to_string(),
        Type::U64 => "u64".to_string(),
        Type::Float => "float".to_string(),
        Type::Str => "string".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Any => "any".to_string(),
        Type::Named(name) => name.clone(),
        Type::Ary(fields) => format!(
            "[{}]",
            fields
                .iter()
                .map(|(name, ty)| format!("{}: {}", name, type_text_builtin(ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Vec(inner) => format!("vec<{}>", type_text_builtin(inner)),
        Type::VecHetero(items) => format!(
            "vec<{}>",
            items
                .iter()
                .map(type_text_builtin)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::VecDynamic => "vec".to_string(),
        Type::Map(key, value) => format!(
            "map<{}, {}>",
            type_text_builtin(key),
            type_text_builtin(value)
        ),
        Type::Option(inner) => format!("option<{}>", type_text_builtin(inner)),
        Type::Result(ok, err) => {
            format!("result<{}, {}>", type_text_builtin(ok), type_text_builtin(err))
        }
        Type::Share(inner) => format!("share<{}>", type_text_builtin(inner)),
        Type::Task(inner) => format!("task<{}>", type_text_builtin(inner)),
        Type::Func(args, ret) => format!(
            "func({}) -> {}",
            args.iter()
                .map(type_text_builtin)
                .collect::<Vec<_>>()
                .join(", "),
            type_text_builtin(ret)
        ),
    }
}

fn publish_diagnostics(uri: &str, source: &str, version: Option<i64>) -> Vec<Value> {
    let diagnostics = analyze(source);
    let mut params = json!({ "uri": uri, "diagnostics": diagnostics });
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    vec![json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params })]
}

fn analyze(source: &str) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    match rp_parser::parse_program(source, "<lsp>") {
        Ok(program) => {
            let checked = rp_types::check_program(&program);
            for diagnostic in &checked.diagnostics {
                diagnostics.push(to_lsp_diagnostic(diagnostic));
            }
            for diagnostic in rp_ownership::check_ownership(&program, &checked) {
                diagnostics.push(to_lsp_diagnostic(&diagnostic));
            }
        }
        Err(error) => {
            let line = error.line.saturating_sub(1);
            diagnostics.push(json!({
                "range": {
                    "start": { "line": line, "character": 0 },
                    "end": { "line": line, "character": 1 }
                },
                "severity": 1,
                "source": "rip",
                "message": error.message
            }));
        }
    }
    diagnostics
}

fn to_lsp_diagnostic(diagnostic: &rp_types::Diagnostic) -> Value {
    let line = diagnostic.line.saturating_sub(1);
    json!({
        "range": {
            "start": { "line": line, "character": 0 },
            "end": { "line": line, "character": 1 }
        },
        "severity": if diagnostic.level == rp_types::Level::Error { 1 } else { 2 },
        "source": "rip",
        "message": diagnostic.message
    })
}

fn response(id: Option<Value>, result: Value) -> Value {
    match id {
        Some(id) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        None => json!({ "jsonrpc": "2.0", "result": result }),
    }
}

/// 单条消息体的大小上限（64 MiB），防止恶意 Content-Length 造成无界分配。
const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }

    let length = match content_length {
        Some(length) => length,
        None => return Ok(None),
    };
    if length > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Content-Length {} exceeds the 64 MiB limit", length),
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(value))
}

fn write_message<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "mod app;\n\nuse std::fs;\n\nfunc greet(name: string) -> string {\n    let count: int = 1;\n    return name;\n}\n\nfunc main() -> void {\n    let text: string = greet(\"x\");\n    fs.write_file(\"a\", text);\n}\n";

    fn position(line: u64, character: u64) -> Value {
        json!({ "textDocument": { "uri": "file:///app.rp" }, "position": { "line": line, "character": character } })
    }

    #[test]
    fn extracts_identifier_under_cursor() {
        assert_eq!(identifier_at(SOURCE, 10, 23).as_deref(), Some("greet"));
        // 光标停在标识符右侧时取左侧的词。
        assert_eq!(identifier_at(SOURCE, 10, 28).as_deref(), Some("greet"));
        assert_eq!(identifier_at(SOURCE, 10, 4).as_deref(), Some("let"));
    }

    #[test]
    fn detects_member_access() {
        assert_eq!(
            member_at(SOURCE, 11, 8),
            Some(("fs".to_string(), "write_file".to_string()))
        );
    }

    #[test]
    fn resolves_function_definition() {
        remember("file:///app.rp", SOURCE);
        let result = definition(&position(10, 23));
        assert_eq!(result["uri"], "file:///app.rp");
        // `func greet` 在第 5 行（0 基为 4）。
        assert_eq!(result["range"]["start"]["line"], 4);
    }

    #[test]
    fn hovers_function_signature() {
        remember("file:///app.rp", SOURCE);
        let result = hover(&position(10, 23));
        let text = result["contents"]["value"].as_str().expect("markdown");
        assert!(text.contains("func greet(name: string) -> string"), "{}", text);
    }

    #[test]
    fn hovers_local_variable_type() {
        remember("file:///app.rp", SOURCE);
        let result = hover(&position(10, 8));
        let text = result["contents"]["value"].as_str().expect("markdown");
        assert!(text.contains("let text: string"), "{}", text);
    }

    #[test]
    fn hovers_builtin_member_signature() {
        remember("file:///app.rp", SOURCE);
        let result = hover(&position(11, 8));
        let text = result["contents"]["value"].as_str().expect("markdown");
        assert!(text.contains("fs.write_file"), "{}", text);
    }

    #[test]
    fn unknown_position_has_no_result() {
        remember("file:///app.rp", SOURCE);
        assert_eq!(hover(&position(0, 0)), Value::Null);
    }

    #[test]
    fn completes_functions_locals_imports_and_types() {
        remember("file:///app.rp", SOURCE);
        let items = completion(&position(10, 4));
        let labels: Vec<&str> = items
            .as_array()
            .expect("completion array")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        for expected in ["greet", "text", "fs", "string", "let"] {
            assert!(
                labels.contains(&expected),
                "missing {} in {:?}",
                expected,
                labels
            );
        }
    }

    #[test]
    fn completes_member_signatures_after_dot() {
        remember("file:///app.rp", SOURCE);
        // 光标在 `fs.wri` 之后。
        let items = completion(&position(11, 10));
        let first = items.as_array().and_then(|items| items.first()).expect("member completion");
        assert_eq!(first["label"], "write_file");
        let detail = first["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("fs.write_file"), "{}", detail);

        // 光标紧跟点号：列出整个模块。
        let items = completion(&position(11, 7));
        let labels: Vec<&str> = items
            .as_array()
            .expect("member completion")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(labels.contains(&"read_file"), "{:?}", labels);
        assert!(labels.contains(&"write_file"), "{:?}", labels);
        assert!(labels.len() > 5, "{:?}", labels);
    }

    #[test]
    fn references_skip_comments_and_strings() {
        const REFS: &str = "mod refs;\n\nuse std::dev;\n\nfunc helper() -> void {\n    // helper in comment\n    let note: string = \"helper\";\n    dev.log(note);\n}\n\nfunc main() -> void {\n    helper();\n}\n";
        remember("file:///refs.rp", REFS);
        let request = json!({
            "textDocument": { "uri": "file:///refs.rp" },
            "position": { "line": 11, "character": 6 },
            "context": { "includeDeclaration": true }
        });
        let locations = references(&request);
        let locations: Vec<&Value> = locations
            .as_array()
            .expect("locations")
            .iter()
            .filter(|location| location["uri"] == "file:///refs.rp")
            .collect();
        assert_eq!(locations.len(), 2, "{:?}", locations);
        assert_eq!(locations[0]["range"]["start"]["line"], 4);
        assert_eq!(locations[1]["range"]["start"]["line"], 11);
    }

    #[test]
    fn references_can_exclude_declaration() {
        remember("file:///refs-only.rp", SOURCE);
        let request = json!({
            "textDocument": { "uri": "file:///refs-only.rp" },
            "position": { "line": 10, "character": 23 },
            "context": { "includeDeclaration": false }
        });
        let locations = references(&request);
        let locations: Vec<&Value> = locations
            .as_array()
            .expect("locations")
            .iter()
            .filter(|location| location["uri"] == "file:///refs-only.rp")
            .collect();
        // SOURCE 里 greet 出现在声明与调用各一次，排除声明后只剩调用。
        assert_eq!(locations.len(), 1, "{:?}", locations);
        assert_eq!(locations[0]["range"]["start"]["line"], 10);
    }

    #[test]
    fn read_message_rejects_oversized_content_length() {
        // 4 GiB 的 Content-Length 必须报错断开，而不是分配等长缓冲。
        let raw = b"Content-Length: 4294967296\r\n\r\n{}";
        let mut reader = &raw[..];
        let result = read_message(&mut reader);
        assert!(result.is_err());
    }

    #[test]
    fn read_message_accepts_normal_content_length() {
        let body = b"{\"jsonrpc\":\"2.0\",\"id\":1}";
        let raw = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        let mut cursor = std::io::Cursor::new([raw.as_slice(), &body[..]].concat());
        let message = read_message(&mut cursor).expect("read").expect("message");
        assert_eq!(message["id"], 1);
    }
}

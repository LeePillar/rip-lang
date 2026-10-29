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

use rp_ast::{Expr, ExprKind, Item, Stmt, StmtKind, TypeExpr, TypeExprKind};
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
        "initialize" => {
            remember_root(params);
            vec![response(
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
                        "referencesProvider": true,
                        "signatureHelpProvider": {
                            "triggerCharacters": ["(", ","],
                            "retriggerCharacters": [","]
                        },
                        "documentHighlightProvider": true,
                        "workspaceSymbolProvider": true,
                        "inlayHintProvider": true,
                        "documentSymbolProvider": true,
                        "foldingRangeProvider": true
                    },
                    "serverInfo": { "name": "rip-lsp", "version": env!("CARGO_PKG_VERSION") }
                }),
            )]
        }
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
        "textDocument/signatureHelp" => vec![response(id, signature_help(params))],
        "textDocument/documentHighlight" => vec![response(id, document_highlight(params))],
        "workspace/symbol" => vec![response(id, workspace_symbol(params))],
        "textDocument/inlayHint" => vec![response(id, inlay_hint(params))],
        "textDocument/documentSymbol" => vec![response(id, document_symbols(params))],
        "textDocument/foldingRange" => vec![response(id, folding_ranges(params))],
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

/// `textDocument/documentSymbol`：返回扁平的 SymbolInformation 列表。
fn document_symbols(params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return json!([]);
    };
    let Some(text) = remembered(uri) else {
        return json!([]);
    };
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return json!([]);
    };
    let mut symbols = vec![json!({
        "name": program.package,
        "kind": 2,
        "location": location(uri, &text, program.package_line)
    })];
    for item in &program.items {
        let (name, kind, line) = match item {
            Item::Func(func) => (func.name.clone(), 12, func.line),
            Item::Type(decl) => (decl.name.clone(), 23, decl.line),
            Item::Interface(decl) => (decl.name.clone(), 11, decl.line),
            Item::Enum(decl) => (decl.name.clone(), 10, decl.line),
            Item::Const(decl) | Item::Static(decl) => (decl.name.clone(), 14, decl.line),
            Item::Import(_) => continue,
        };
        symbols.push(json!({
            "name": name,
            "kind": kind,
            "location": location(uri, &text, line)
        }));
    }
    json!(symbols)
}

/// `textDocument/foldingRange`：成对花括号块 + 多行块注释。
fn folding_ranges(params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return json!([]);
    };
    let Some(text) = remembered(uri) else {
        return json!([]);
    };
    let (lines, comments) = masked_source(&text);
    let mut ranges: Vec<(u32, u32, &'static str)> = comments
        .into_iter()
        .map(|(start, end)| (start, end, "comment"))
        .collect();
    // 字符串与注释已被屏蔽，剩下的花括号都是真实语法块。
    let mut open = Vec::new();
    for (index, chars) in lines.iter().enumerate() {
        for ch in chars {
            match ch {
                '{' => open.push(index as u32),
                '}' => {
                    if let Some(start) = open.pop() {
                        if index as u32 > start {
                            ranges.push((start, index as u32, "region"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    json!(ranges
        .into_iter()
        .map(|(start, end, kind)| json!({
            "startLine": start,
            "endLine": end,
            "kind": kind
        }))
        .collect::<Vec<_>>())
}

/// 工作区根路径：`workspace/symbol` 从该目录递归收集符号。
fn workspace_root() -> &'static Mutex<Option<String>> {
    static ROOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    ROOT.get_or_init(|| Mutex::new(None))
}

fn remember_root(params: &Value) {
    let from_uri = |value: &Value| value.as_str().map(uri_to_path);
    let root = params
        .get("rootPath")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| from_uri(&params["rootUri"]))
        .or_else(|| from_uri(&params["workspaceFolders"][0]["uri"]));
    if let (Some(root), Ok(mut slot)) = (root, workspace_root().lock()) {
        *slot = Some(root);
    }
}

/// `file:///e:/Code/Rip` → `e:/Code/Rip`。
fn uri_to_path(uri: &str) -> String {
    let decoded = percent_decode(uri);
    let rest = decoded.strip_prefix("file://").unwrap_or(&decoded);
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some('/'), Some(drive)) if drive.is_ascii_alphabetic() && chars.next() == Some(':') => {
            rest[1..].to_string()
        }
        _ => rest.to_string(),
    }
}

fn path_to_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.strip_prefix('/').unwrap_or(&normalized);
    let mut out = String::from("file:///");
    for ch in trimmed.chars() {
        match ch {
            '/' => out.push('/'),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            _ => out.push(ch),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// `textDocument/documentHighlight`：高亮当前文档内同名的全部出现位置。
fn document_highlight(params: &Value) -> Value {
    let Some((_, text, line, character)) = request_target(params) else {
        return json!([]);
    };
    let Some(name) = identifier_at(&text, line, character) else {
        return json!([]);
    };
    json!(identifier_locations(&text, &name)
        .into_iter()
        .map(|(line, start, end)| json!({
            "range": {
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end }
            },
            "kind": 1
        }))
        .collect::<Vec<_>>())
}

/// `textDocument/signatureHelp`：光标最近未闭合括号对应的函数签名。
fn signature_help(params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return Value::Null;
    };
    let Some(text) = remembered(uri) else {
        return Value::Null;
    };
    let line = params["position"]["line"].as_u64().unwrap_or(0) as usize;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as usize;
    let Some(line_text) = text.lines().nth(line) else {
        return Value::Null;
    };
    let chars: Vec<char> = line_text.chars().collect();
    let at = character.min(chars.len());
    let Some(open) = unclosed_paren(&chars, at) else {
        return Value::Null;
    };
    let Some((alias, callee)) = callee_before(&chars, open) else {
        return Value::Null;
    };
    let active = top_level_commas(&chars[open + 1..at]);
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return Value::Null;
    };
    let (label, parameters) = match alias {
        Some(alias) => {
            let Some(path) = import_path(&program, &alias) else {
                return Value::Null;
            };
            let Some(sig) = rp_types::builtins::builtin_function(&path, &callee) else {
                return Value::Null;
            };
            (
                format!(
                    "{}.{}({}) -> {}",
                    alias,
                    callee,
                    builtin_params(&sig.params),
                    type_text_builtin(&sig.ret)
                ),
                sig.params.iter().map(type_text_builtin).collect::<Vec<_>>(),
            )
        }
        None => {
            let Some(func) = program.items.iter().find_map(|item| match item {
                Item::Func(func) if func.name == callee => Some(func),
                _ => None,
            }) else {
                return Value::Null;
            };
            (
                signature(func),
                func.params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect::<Vec<_>>(),
            )
        }
    };
    json!({
        "signatures": [{
            "label": label,
            "parameters": parameters
                .iter()
                .map(|name| json!({ "label": name }))
                .collect::<Vec<_>>()
        }],
        "activeSignature": 0,
        "activeParameter": active
    })
}

/// 光标左侧最近的未闭合 `(`（忽略已闭合的括号对）。
fn unclosed_paren(chars: &[char], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for index in (0..at.min(chars.len())).rev() {
        match chars[index] {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' => {
                if depth == 0 {
                    return (chars[index] == '(').then_some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// `(` 之前的调用目标：`(None, f)` 或 `(Some(别名), 成员)`。
fn callee_before(chars: &[char], open: usize) -> Option<(Option<String>, String)> {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut end = open;
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let callee: String = chars[start..end].iter().collect();
    if start >= 2 && chars[start - 1] == '.' {
        let mut alias_start = start - 1;
        while alias_start > 0 && is_word(chars[alias_start - 1]) {
            alias_start -= 1;
        }
        if alias_start < start - 1 {
            return Some((Some(chars[alias_start..start - 1].iter().collect()), callee));
        }
    }
    Some((None, callee))
}

fn top_level_commas(chars: &[char]) -> u32 {
    let mut depth = 0i32;
    let mut commas = 0;
    for ch in chars {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas
}

/// `workspace/symbol`：递归扫描工作区 `.rp` 文件里的函数、类型与常量。
fn workspace_symbol(params: &Value) -> Value {
    let query = params["query"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();
    let Some(root) = workspace_root().lock().ok().and_then(|slot| slot.clone()) else {
        return json!([]);
    };
    let mut files = Vec::new();
    let mut pending = vec![std::path::PathBuf::from(&root)];
    while let Some(dir) = pending.pop() {
        if files.len() >= 400 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if path.is_dir() {
                if matches!(
                    name.as_str(),
                    "target" | "node_modules" | ".git" | ".trae" | ".skills" | "dist"
                ) {
                    continue;
                }
                pending.push(path);
            } else if name.ends_with(".rp") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut symbols = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
            continue;
        };
        let uri = path_to_uri(&path.to_string_lossy());
        for item in &program.items {
            let (name, kind, line) = match item {
                Item::Func(func) => (func.name.clone(), 12, func.line),
                Item::Type(decl) => (decl.name.clone(), 23, decl.line),
                Item::Interface(decl) => (decl.name.clone(), 11, decl.line),
                Item::Enum(decl) => (decl.name.clone(), 10, decl.line),
                Item::Const(decl) | Item::Static(decl) => (decl.name.clone(), 14, decl.line),
                Item::Import(_) => continue,
            };
            if !query.is_empty() && !name.to_lowercase().contains(&query) {
                continue;
            }
            symbols.push(json!({
                "name": name,
                "kind": kind,
                "location": location(&uri, &text, line)
            }));
            if symbols.len() >= 300 {
                return json!(symbols);
            }
        }
    }
    json!(symbols)
}

/// 扫描状态：字符串与注释里的内容不参与结构分析。
#[derive(Clone, Copy)]
enum ScanState {
    Code,
    LineComment,
    BlockComment,
    Quoted(char),
    Template,
}

/// 把字符串与注释内容替换为空格（保留行列结构），并汇总多行块注释的行区间。
fn masked_source(text: &str) -> (Vec<Vec<char>>, Vec<(u32, u32)>) {
    let chars: Vec<char> = text.chars().collect();
    let mut lines: Vec<Vec<char>> = vec![Vec::new()];
    let mut comments = Vec::new();
    let mut line = 0usize;
    let mut block_start: Option<u32> = None;
    let mut state = ScanState::Code;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            lines.push(Vec::new());
            line += 1;
            if let ScanState::LineComment = state {
                state = ScanState::Code;
            }
            i += 1;
            continue;
        }
        match state {
            ScanState::Code => {
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    lines[line].push(' ');
                    lines[line].push(' ');
                    state = ScanState::LineComment;
                    i += 2;
                } else if c == '/' && chars.get(i + 1) == Some(&'*') {
                    block_start = Some(line as u32);
                    lines[line].push(' ');
                    lines[line].push(' ');
                    state = ScanState::BlockComment;
                    i += 2;
                } else if c == '"'
                    && chars.get(i + 1) == Some(&'"')
                    && chars.get(i + 2) == Some(&'"')
                {
                    lines[line].extend([' ', ' ', ' ']);
                    state = ScanState::Template;
                    i += 3;
                } else if c == '"' || c == '\'' {
                    lines[line].push(' ');
                    state = ScanState::Quoted(c);
                    i += 1;
                } else {
                    lines[line].push(c);
                    i += 1;
                }
            }
            ScanState::LineComment => {
                lines[line].push(' ');
                i += 1;
            }
            ScanState::BlockComment => {
                if c == '*' && chars.get(i + 1) == Some(&'/') {
                    lines[line].push(' ');
                    lines[line].push(' ');
                    if let Some(start) = block_start.take() {
                        if line as u32 > start {
                            comments.push((start, line as u32));
                        }
                    }
                    state = ScanState::Code;
                    i += 2;
                } else {
                    lines[line].push(' ');
                    i += 1;
                }
            }
            ScanState::Quoted(quote) => {
                lines[line].push(' ');
                if c == '\\' && chars.get(i + 1).is_some_and(|next| *next != '\n') {
                    // 转义字符连同下一个字符一起屏蔽。
                    lines[line].push(' ');
                    i += 2;
                } else if c == quote {
                    state = ScanState::Code;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            ScanState::Template => {
                if c == '"' && chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"') {
                    lines[line].extend([' ', ' ', ' ']);
                    state = ScanState::Code;
                    i += 3;
                } else {
                    lines[line].push(' ');
                    i += 1;
                }
            }
        }
    }
    (lines, comments)
}

/// `textDocument/inlayHint`：在普通函数调用的实参前插入参数名提示。
fn inlay_hint(params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return json!([]);
    };
    let Some(text) = remembered(uri) else {
        return json!([]);
    };
    let Ok(program) = rp_parser::parse_program(&text, "<lsp>") else {
        return json!([]);
    };
    let mut functions: HashMap<String, Vec<String>> = HashMap::new();
    for item in &program.items {
        if let Item::Func(func) = item {
            functions.insert(
                func.name.clone(),
                func.params.iter().map(|param| param.name.clone()).collect(),
            );
        }
    }
    let mut calls = Vec::new();
    for item in &program.items {
        match item {
            Item::Func(func) => collect_calls(&func.body, &mut calls),
            Item::Const(decl) | Item::Static(decl) => collect_expr_calls(&decl.init, &mut calls),
            _ => {}
        }
    }

    let (lines, _) = masked_source(&text);
    let mut scanned: HashMap<String, Vec<(CallSite, bool)>> = HashMap::new();
    let mut hints = Vec::new();
    for call in calls {
        let Some(params) = functions.get(&call.name) else {
            continue;
        };
        if params.is_empty() || params.len() != call.args.len() {
            continue;
        }
        let sites = scanned.entry(call.name.clone()).or_insert_with(|| {
            scan_call_sites(&lines, &call.name)
                .into_iter()
                .map(|site| (site, false))
                .collect()
        });
        let Some((site, used)) = sites
            .iter_mut()
            .find(|(site, used)| !*used && site.line == call.line.saturating_sub(1))
        else {
            continue;
        };
        *used = true;
        // 扫描出的实参位置必须与 AST 的实参行一致，否则整次调用跳过。
        if site.args.len() != params.len()
            || site
                .args
                .iter()
                .zip(&call.args)
                .any(|((line, _), arg)| *line + 1 != *arg)
        {
            continue;
        }
        for (param, (line, character)) in params.iter().zip(&site.args) {
            hints.push(json!({
                "position": { "line": line, "character": character },
                "label": format!("{}:", param),
                "kind": 2,
                "paddingRight": true
            }));
        }
    }
    json!(hints)
}

/// 一次调用在源码中的位置：`名称(` 所在行（0 基）与各实参起始位置。
struct CallSite {
    line: u32,
    args: Vec<(u32, u32)>,
}

/// 调用点的 callee 名称、所在行（1 基）与各实参起始行（1 基）。
struct Call {
    name: String,
    line: u32,
    args: Vec<u32>,
}

/// AST 中的普通函数调用（callee 为简单标识符）。
fn collect_calls(stmts: &[Stmt], out: &mut Vec<Call>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Let { init, .. } | StmtKind::Expr(init) => collect_expr_calls(init, out),
            StmtKind::Assign { target, value } => {
                collect_expr_calls(target, out);
                collect_expr_calls(value, out);
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                collect_expr_calls(cond, out);
                collect_calls(then_body, out);
                if let Some(body) = else_body {
                    collect_calls(body, out);
                }
            }
            StmtKind::Loop { body } => collect_calls(body, out),
            StmtKind::While { cond, body } => {
                collect_expr_calls(cond, out);
                collect_calls(body, out);
            }
            StmtKind::WhileLet {
                scrutinee, body, ..
            } => {
                collect_expr_calls(scrutinee, out);
                collect_calls(body, out);
            }
            StmtKind::ForIn { iter, body, .. } => {
                collect_expr_calls(iter, out);
                collect_calls(body, out);
            }
            StmtKind::Return(Some(expr)) => collect_expr_calls(expr, out),
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Defer { call } => collect_expr_calls(call, out),
            StmtKind::Match { subject, arms } => {
                collect_expr_calls(subject, out);
                for arm in arms {
                    collect_calls(&arm.body, out);
                }
            }
            StmtKind::Parallel { branches } => collect_calls(branches, out),
        }
    }
}

fn collect_expr_calls(expr: &Expr, out: &mut Vec<Call>) {
    match &expr.kind {
        ExprKind::Call { callee, args, .. } => {
            if let ExprKind::Ident(name) = &callee.kind {
                out.push(Call {
                    name: name.clone(),
                    line: expr.line,
                    args: args.iter().map(|arg| arg.line).collect(),
                });
            }
            collect_expr_calls(callee, out);
            for arg in args {
                collect_expr_calls(arg, out);
            }
        }
        ExprKind::Method { receiver, args, .. } => {
            collect_expr_calls(receiver, out);
            for arg in args {
                collect_expr_calls(arg, out);
            }
        }
        ExprKind::Field { base, .. } => collect_expr_calls(base, out),
        ExprKind::Unary { expr, .. } => collect_expr_calls(expr, out),
        ExprKind::Binary { left, right, .. } => {
            collect_expr_calls(left, out);
            collect_expr_calls(right, out);
        }
        ExprKind::List(items) => {
            for item in items {
                match item {
                    rp_ast::ListItem::Value(item)
                    | rp_ast::ListItem::Named(_, item)
                    | rp_ast::ListItem::Key(_, item)
                    | rp_ast::ListItem::Typed(_, item) => collect_expr_calls(item, out),
                }
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, value) in fields {
                collect_expr_calls(value, out);
            }
        }
        ExprKind::Some(inner)
        | ExprKind::Ok(inner)
        | ExprKind::Err(inner)
        | ExprKind::Try(inner)
        | ExprKind::Await(inner) => collect_expr_calls(inner, out),
        ExprKind::Closure { body, .. } => collect_calls(body, out),
        _ => {}
    }
}

/// 扫描文档里所有 `名称(` 调用，记录实参起始位置；跳过声明与成员访问。
fn scan_call_sites(lines: &[Vec<char>], name: &str) -> Vec<CallSite> {
    let target: Vec<char> = name.chars().collect();
    let is_word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let mut sites = Vec::new();
    for (index, chars) in lines.iter().enumerate() {
        let mut at = 0;
        while at + target.len() <= chars.len() {
            let matches = chars[at..at + target.len()] == target[..]
                && chars.get(at + target.len()) == Some(&'(')
                && (at == 0 || !is_word(chars[at - 1]))
                && !preceded_by_dot_or_func(chars, at);
            if matches {
                sites.push(CallSite {
                    line: index as u32,
                    args: argument_starts(lines, index, at + target.len() + 1),
                });
                at += target.len() + 1;
            } else {
                at += 1;
            }
        }
    }
    sites
}

/// 名称前（跳过空白）是否是 `.` 或 `func`：方法访问与函数声明都不算调用点。
fn preceded_by_dot_or_func(chars: &[char], at: usize) -> bool {
    let is_word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
    let mut index = at;
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    let Some(previous) = index.checked_sub(1) else {
        return false;
    };
    if chars[previous] == '.' {
        return true;
    }
    if !is_word(chars[previous]) {
        return false;
    }
    let mut start = previous + 1;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    chars[start..=previous].iter().collect::<String>() == "func"
}

/// 从 `(` 之后开始切分实参，返回每个实参的起始位置（0 基行列）。
fn argument_starts(lines: &[Vec<char>], mut line: usize, mut character: usize) -> Vec<(u32, u32)> {
    let mut starts = Vec::new();
    let mut depth = 0usize;
    loop {
        skip_blanks(lines, &mut line, &mut character);
        match peek_char(lines, &mut line, &mut character) {
            None => return starts,
            Some(')') if depth == 0 => return starts,
            Some(',') if depth == 0 => {
                // 空实参（如 `f(a, )`）直接跳过，不生成提示。
                character += 1;
                continue;
            }
            Some(_) => {}
        }
        starts.push((line as u32, character as u32));
        // 扫描到本实参结束：顶层逗号或调用的右括号。
        loop {
            match peek_char(lines, &mut line, &mut character) {
                None => return starts,
                Some('(' | '[' | '{') => {
                    depth += 1;
                    character += 1;
                }
                Some(')' | ']' | '}') if depth == 0 => return starts,
                Some(')' | ']' | '}') => {
                    depth -= 1;
                    character += 1;
                }
                Some(',') if depth == 0 => {
                    character += 1;
                    break;
                }
                Some(_) => character += 1,
            }
        }
    }
}

/// 读取当前行列的字符；到达行尾时自动换到下一行。
fn peek_char(lines: &[Vec<char>], line: &mut usize, character: &mut usize) -> Option<char> {
    loop {
        let current = lines.get(*line)?;
        if let Some(ch) = current.get(*character) {
            return Some(*ch);
        }
        *line += 1;
        *character = 0;
    }
}

fn skip_blanks(lines: &[Vec<char>], line: &mut usize, character: &mut usize) {
    while let Some(ch) = peek_char(lines, line, character) {
        if !ch.is_whitespace() {
            break;
        }
        *character += 1;
    }
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
    fn signature_help_for_local_function() {
        remember("file:///app.rp", SOURCE);
        let result = signature_help(&position(10, 29));
        let label = result["signatures"][0]["label"].as_str().expect("label");
        assert!(label.contains("greet(name: string) -> string"), "{}", label);
        assert_eq!(result["activeParameter"], 0);
    }

    #[test]
    fn signature_help_for_builtin_member() {
        remember("file:///app.rp", SOURCE);
        let line = "    fs.write_file(\"a\", text);";
        let at = line.find("text").expect("text") as u64;
        let result = signature_help(&position(11, at));
        let label = result["signatures"][0]["label"].as_str().expect("label");
        assert!(label.starts_with("fs.write_file("), "{}", label);
        assert_eq!(result["activeParameter"], 1);
    }

    #[test]
    fn highlights_identifier_occurrences() {
        remember("file:///app.rp", SOURCE);
        let result = document_highlight(&position(10, 25));
        assert!(result.as_array().expect("array").len() >= 2);
    }

    #[test]
    fn converts_between_file_uri_and_path() {
        assert_eq!(uri_to_path("file:///e:/Code/Rip"), "e:/Code/Rip");
        assert_eq!(uri_to_path("file:///home/rip"), "/home/rip");
        assert_eq!(path_to_uri("e:\\Code\\Rip"), "file:///e:/Code/Rip");
        assert_eq!(path_to_uri("/home/rip"), "file:///home/rip");
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

    #[test]
    fn hints_parameter_names_only_at_call_sites() {
        const HINTS: &str = "mod hints;\n\nfunc add(a: int, b: int) -> int {\n    return a + b;\n}\n\nfunc main() -> void {\n    let sum: int = add(1, 2);\n}\n";
        remember("file:///hints.rp", HINTS);
        let hints = inlay_hint(&json!({
            "textDocument": { "uri": "file:///hints.rp" },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 8, "character": 0 } }
        }));
        let hints = hints.as_array().expect("hints");
        // `func add(` 是声明，不应产生提示；调用点 `add(1, 2)` 两个实参各一条。
        assert_eq!(hints.len(), 2, "{:?}", hints);
        assert_eq!(hints[0]["label"], "a:");
        assert_eq!(hints[0]["kind"], 2);
        assert_eq!(hints[0]["paddingRight"], true);
        assert_eq!(hints[0]["position"]["line"], 7);
        assert_eq!(hints[0]["position"]["character"], 23);
        assert_eq!(hints[1]["label"], "b:");
        assert_eq!(hints[1]["position"]["character"], 26);
    }

    #[test]
    fn lists_document_symbols_flat() {
        const SYMBOLS: &str = "mod syms;\n\ntype point = [x: int, y: int];\n\ntype status = enum {\n    active,\n    banned(reason: string),\n}\n\ntype shape = interface {\n    func area() -> float\n}\n\nconst limit: int = 3;\n\nfunc main() -> void {\n    let local: int = limit;\n}\n";
        remember("file:///syms.rp", SYMBOLS);
        let symbols = document_symbols(&json!({ "textDocument": { "uri": "file:///syms.rp" } }));
        let symbols = symbols.as_array().expect("symbols");
        let pairs: Vec<(&str, u64)> = symbols
            .iter()
            .map(|symbol| {
                (
                    symbol["name"].as_str().unwrap_or_default(),
                    symbol["kind"].as_u64().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("syms", 2),
                ("point", 23),
                ("status", 10),
                ("shape", 11),
                ("limit", 14),
                ("main", 12),
            ]
        );
    }

    #[test]
    fn folds_brace_blocks_and_block_comments() {
        const FOLDS: &str = "mod folds;\n\n/* 多行\n   块注释 */\nfunc main() -> void {\n    if true {\n        return;\n    }\n}\n";
        remember("file:///folds.rp", FOLDS);
        let ranges = folding_ranges(&json!({ "textDocument": { "uri": "file:///folds.rp" } }));
        let ranges = ranges.as_array().expect("ranges");
        let ranges: Vec<(u64, u64, &str)> = ranges
            .iter()
            .map(|range| {
                (
                    range["startLine"].as_u64().unwrap_or_default(),
                    range["endLine"].as_u64().unwrap_or_default(),
                    range["kind"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            ranges,
            vec![(2, 3, "comment"), (4, 8, "region"), (5, 7, "region")]
        );
    }

    #[test]
    fn new_requests_tolerate_missing_params() {
        assert_eq!(inlay_hint(&Value::Null), json!([]));
        assert_eq!(document_symbols(&json!({})), json!([]));
        assert_eq!(
            folding_ranges(&json!({ "textDocument": { "uri": "file:///missing.rp" } })),
            json!([])
        );
    }
}

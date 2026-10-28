//! 极简 HTML 模板引擎：`{{ 变量 }}` 默认自动转义，支持 `if` / `unless` / `each`。
//!
//! 设计目标是「服务端渲染的常见 90%」而不是通用模板语言：
//! - `{{ name }}`：HTML 转义输出；`{{{ name }}}`：原样输出（仅用于可信内容）。
//! - `{{#if user}}…{{else}}…{{/if}}`、`{{#unless user}}…{{/unless}}`。
//! - `{{#each items}}…{{ . }}…{{ @index }}…{{/each}}`，内层可读外层变量。
//! - `{{! 注释 }}`。
//!
//! 编译结果按模板源码缓存在线程本地（每个工作线程一份），
//! 页面渲染只需一次哈希查找 + 遍历已编译节点。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::{format_value, Value};

/// 编译后的模板。
pub struct Template {
    nodes: Vec<Node>,
}

enum Node {
    Text(String),
    /// 变量路径：`user.name`、`.`、`@index`。
    Var { path: String, raw: bool },
    If {
        path: String,
        negate: bool,
        yes: Vec<Node>,
        no: Vec<Node>,
    },
    Each { path: String, body: Vec<Node> },
}

enum Stop {
    End,
    Else,
}

/// 渲染模板；编译结果按源码缓存。
pub fn render(source: &str, data: &Value) -> Result<String, String> {
    let template = compiled(source)?;
    let scope = Scope {
        value: Some(data.clone()),
        index: None,
        parent: None,
    };
    let mut out = String::with_capacity(source.len() + 64);
    render_nodes(&template.nodes, &scope, &mut out);
    Ok(out)
}

/// HTML 转义：文本节点与属性值都适用。
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

thread_local! {
    /// 每个解释器线程一份：模板在多次渲染间复用，避免重复解析。
    static CACHE: RefCell<HashMap<String, Arc<Template>>> = RefCell::new(HashMap::new());
}

/// 清空当前线程的模板缓存（`template.clear_cache`）。
pub fn clear_cache() {
    CACHE.with(|cache| cache.borrow_mut().clear());
}

fn compiled(source: &str) -> Result<Arc<Template>, String> {
    CACHE.with(|cache| {
        if let Some(template) = cache.borrow().get(source) {
            return Ok(Arc::clone(template));
        }
        let template = Arc::new(parse(source)?);
        let mut cache = cache.borrow_mut();
        // 模板数量上限：超出后整体清空，避免被动态拼出的模板撑爆内存。
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(source.to_string(), Arc::clone(&template));
        Ok(template)
    })
}

fn parse(source: &str) -> Result<Template, String> {
    let mut parser = Parser {
        src: source,
        pos: 0,
    };
    let (nodes, _) = parser.parse_until(None)?;
    Ok(Template { nodes })
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_until(&mut self, stop: Option<&str>) -> Result<(Vec<Node>, Option<Stop>), String> {
        let mut nodes = Vec::new();
        loop {
            let rest = &self.src[self.pos..];
            if rest.is_empty() {
                if let Some(name) = stop {
                    return Err(format!("模板缺少 {{{{/{}}}}}", name));
                }
                return Ok((nodes, None));
            }
            let Some(open) = rest.find("{{") else {
                nodes.push(Node::Text(rest.to_string()));
                self.pos = self.src.len();
                continue;
            };
            if open > 0 {
                nodes.push(Node::Text(rest[..open].to_string()));
                self.pos += open;
            }
            let rest = &self.src[self.pos..];
            // 三花括号：原样输出。
            if rest.starts_with("{{{") {
                let end = rest
                    .find("}}}")
                    .ok_or_else(|| "模板里的 {{{ 没有闭合".to_string())?;
                let path = rest[3..end].trim().to_string();
                if path.is_empty() {
                    return Err("模板变量名为空".to_string());
                }
                nodes.push(Node::Var { path, raw: true });
                self.pos += end + 3;
                continue;
            }
            let end = rest
                .find("}}")
                .ok_or_else(|| "模板里的 {{ 没有闭合".to_string())?;
            let inner = rest[2..end].trim().to_string();
            self.pos += end + 2;

            if let Some(comment) = inner.strip_prefix('!') {
                let _ = comment;
                continue;
            }
            if let Some(name) = inner.strip_prefix('/') {
                let name = name.trim();
                return match stop {
                    Some(expected) if expected == name => Ok((nodes, Some(Stop::End))),
                    _ => Err(format!("模板出现未预期的 {{{{/{}}}}}", name)),
                };
            }
            if inner == "else" {
                return if stop.is_some() {
                    Ok((nodes, Some(Stop::Else)))
                } else {
                    Err("模板里的 {{else}} 不在 if 内".to_string())
                };
            }
            if let Some(spec) = inner.strip_prefix('#') {
                let mut parts = spec.splitn(2, char::is_whitespace);
                let keyword = parts.next().unwrap_or_default().trim();
                let path = parts.next().unwrap_or_default().trim().to_string();
                parts_check(keyword, &path)?;
                let (body, stop_token) = self.parse_until(Some(keyword))?;
                match keyword {
                    "if" | "unless" => {
                        let mut otherwise = Vec::new();
                        if matches!(stop_token, Some(Stop::Else)) {
                            let (rest_nodes, _) = self.parse_until(Some(keyword))?;
                            otherwise = rest_nodes;
                        }
                        nodes.push(Node::If {
                            path,
                            negate: keyword == "unless",
                            yes: body,
                            no: otherwise,
                        });
                    }
                    "each" => nodes.push(Node::Each { path, body }),
                    _ => unreachable!("keyword 已校验"),
                }
                continue;
            }
            if inner.is_empty() {
                return Err("模板变量名为空".to_string());
            }
            nodes.push(Node::Var {
                path: inner,
                raw: false,
            });
        }
    }
}

fn parts_check(keyword: &str, path: &str) -> Result<(), String> {
    if !matches!(keyword, "if" | "unless" | "each") {
        return Err(format!("模板不支持的块 {{{{#{}}}}}", keyword));
    }
    if path.is_empty() {
        return Err(format!("模板 {{{{#{}}}}} 缺少表达式", keyword));
    }
    Ok(())
}

/// 渲染作用域：`each` 会压入一层，外层变量仍可访问。
struct Scope<'a> {
    value: Option<Value>,
    index: Option<i64>,
    parent: Option<&'a Scope<'a>>,
}

fn render_nodes(nodes: &[Node], scope: &Scope<'_>, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Var { path, raw } => {
                let Some(value) = lookup(scope, path) else {
                    continue;
                };
                let text = output_text(&value);
                if *raw {
                    out.push_str(&text);
                } else {
                    out.push_str(&escape(&text));
                }
            }
            Node::If {
                path,
                negate,
                yes,
                no,
            } => {
                let truthy = lookup(scope, path).is_some_and(|value| is_truthy(&value));
                let branch = if truthy != *negate { yes } else { no };
                render_nodes(branch, scope, out);
            }
            Node::Each { path, body } => {
                let Some(items) = lookup(scope, path) else {
                    continue;
                };
                match &items {
                    Value::Vec(items) => {
                        for (index, item) in items.borrow().iter().enumerate() {
                            let inner = Scope {
                                value: Some(item.clone()),
                                index: Some(index as i64),
                                parent: Some(scope),
                            };
                            render_nodes(body, &inner, out);
                        }
                    }
                    // 结构体数组按字段顺序遍历，便于渲染 `http.request` 这类值。
                    Value::Ary(ary) => {
                        let fields: Vec<Value> = ary
                            .borrow()
                            .fields
                            .iter()
                            .map(|(_, value)| value.clone())
                            .collect();
                        for (index, item) in fields.into_iter().enumerate() {
                            let inner = Scope {
                                value: Some(item),
                                index: Some(index as i64),
                                parent: Some(scope),
                            };
                            render_nodes(body, &inner, out);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// 路径查找：先查最内层作用域，逐层向外；`a.b` 逐段下钻。
fn lookup(scope: &Scope<'_>, path: &str) -> Option<Value> {
    if path == "." || path == "this" {
        return scope.value.clone();
    }
    if path == "@index" {
        return scope.index.map(Value::Int);
    }
    let mut segments = path.split('.');
    let head = segments.next()?;
    let mut cursor = Some(scope);
    let mut base = None;
    while let Some(current) = cursor {
        if let Some(value) = field(current.value.as_ref(), head) {
            base = Some(value);
            break;
        }
        cursor = current.parent;
    }
    let mut node = base?;
    for segment in segments {
        node = field(Some(&node), segment)?;
    }
    Some(node)
}

fn field(base: Option<&Value>, name: &str) -> Option<Value> {
    match base? {
        Value::Map(map) => map.borrow().get(&Value::string(name)),
        Value::Ary(ary) => ary.borrow().get(name),
        _ => None,
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::None => false,
        Value::Bool(flag) => *flag,
        Value::Int(v) => *v != 0,
        Value::UInt(v) => *v != 0,
        Value::Float(v) => *v != 0.0,
        Value::Str(text) => !text.is_empty(),
        Value::Char(ch) => !ch.is_empty(),
        Value::Vec(items) => !items.borrow().is_empty(),
        Value::Ary(ary) => !ary.borrow().fields.is_empty(),
        Value::Map(map) => !map.borrow().entries.is_empty(),
        Value::Some(inner) => is_truthy(inner),
        Value::Ok(_) | Value::Err(_) | Value::Func(_) | Value::Task(_) => true,
    }
}

/// 输出文本：`null` / `none` 渲染成空串，其余沿用标准格式化。
fn output_text(value: &Value) -> String {
    match value {
        Value::Null | Value::None => String::new(),
        Value::Some(inner) | Value::Ok(inner) => output_text(inner),
        Value::Err(inner) => output_text(inner),
        other => format_value(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::MapData;

    fn data(pairs: Vec<(&str, Value)>) -> Value {
        let mut map = MapData::default();
        for (key, value) in pairs {
            map.set(Value::string(key), value);
        }
        Value::map_data(map)
    }

    #[test]
    fn escapes_by_default() {
        let out = render("<p>{{ name }}</p>", &data(vec![("name", Value::string("<b>"))]))
            .expect("render");
        assert_eq!(out, "<p>&lt;b&gt;</p>");
        let raw = render("<p>{{{ name }}}</p>", &data(vec![("name", Value::string("<b>"))]))
            .expect("render");
        assert_eq!(raw, "<p><b></p>");
    }

    #[test]
    fn if_else_and_each() {
        let source = "{{#if user}}hi {{ user.name }}{{else}}anon{{/if}}\
                      {{#each items}}[{{ @index }}:{{ . }}]{{/each}}";
        let payload = data(vec![
            (
                "user",
                data(vec![("name", Value::string("Rip"))]),
            ),
            (
                "items",
                Value::vec(vec![Value::Int(1), Value::Int(2)]),
            ),
        ]);
        let out = render(source, &payload).expect("render");
        assert_eq!(out, "hi Rip[0:1][1:2]");
    }

    #[test]
    fn rejects_unclosed_block() {
        assert!(render("{{#if a}}x", &data(vec![])).is_err());
    }

    #[test]
    fn lookup_falls_back_to_parent_scope() {
        let payload = data(vec![
            ("title", Value::string("T")),
            ("items", Value::vec(vec![Value::Int(1)])),
        ]);
        let out = render("{{#each items}}{{ title }}-{{ . }}{{/each}}", &payload).expect("render");
        assert_eq!(out, "T-1");
    }
}

//! `rip doc`：从 AST 生成单文件 HTML API 文档。
//!
//! 注释不进入 AST，这里按声明行号回到源文件，收集紧邻声明（允许中间夹
//! 属性行）的 `///` 文档注释。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rp_ast::{FuncDecl, Item, ParamMode, Program, TypeExpr, TypeExprKind};

use super::{locate, ProjectUnit};

pub(crate) struct DocEntry {
    pub name: String,
    pub signature: String,
    pub doc: Option<String>,
    pub public: bool,
    pub attributes: Vec<String>,
}

pub(crate) struct DocType {
    pub name: String,
    pub signature: String,
    pub doc: Option<String>,
    pub public: bool,
    pub methods: Vec<DocEntry>,
}

pub(crate) struct DocEnum {
    pub name: String,
    pub doc: Option<String>,
    pub public: bool,
    pub variants: Vec<(String, Vec<(String, String)>)>,
    pub methods: Vec<DocEntry>,
}

pub(crate) struct DocInterface {
    pub name: String,
    pub doc: Option<String>,
    pub public: bool,
    pub methods: Vec<DocEntry>,
}

pub(crate) struct DocGlobal {
    pub name: String,
    pub signature: String,
    pub doc: Option<String>,
    pub public: bool,
}

#[derive(Default)]
pub(crate) struct DocModel {
    pub functions: Vec<DocEntry>,
    pub types: Vec<DocType>,
    pub enums: Vec<DocEnum>,
    pub interfaces: Vec<DocInterface>,
    pub globals: Vec<DocGlobal>,
}

/// 构建文档模型；`units` 用于把合并后的行号映射回源文件。
pub(crate) fn build(
    program: &Program,
    units: &[ProjectUnit],
    entry: &str,
) -> DocModel {
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut model = DocModel::default();
    let mut methods: HashMap<String, Vec<DocEntry>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Func(func) => {
                let entry_doc = entry_of_func(func, units, entry, &mut cache);
                match &func.owner {
                    Some(owner) => methods.entry(owner.clone()).or_default().push(entry_doc),
                    None => model.functions.push(entry_doc),
                }
            }
            Item::Type(decl) => {
                model.types.push(DocType {
                    name: decl.name.clone(),
                    signature: format!("type {} = {};", decl.name, render_type(&decl.ty)),
                    doc: doc_comment(units, decl.line, entry, &mut cache),
                    public: decl.public,
                    methods: Vec::new(),
                });
            }
            Item::Enum(decl) => {
                model.enums.push(DocEnum {
                    name: decl.name.clone(),
                    doc: doc_comment(units, decl.line, entry, &mut cache),
                    public: decl.public,
                    variants: decl
                        .variants
                        .iter()
                        .map(|variant| {
                            (
                                variant.name.clone(),
                                variant
                                    .fields
                                    .iter()
                                    .map(|field| (field.name.clone(), render_type(&field.ty)))
                                    .collect(),
                            )
                        })
                        .collect(),
                    methods: Vec::new(),
                });
            }
            Item::Interface(decl) => {
                let mut doc_interface = DocInterface {
                    name: decl.name.clone(),
                    doc: doc_comment(units, decl.line, entry, &mut cache),
                    public: decl.public,
                    methods: Vec::new(),
                };
                doc_interface.methods = decl
                    .methods
                    .iter()
                    .map(|method| DocEntry {
                        name: method.name.clone(),
                        signature: format!(
                            "func {}({}) -> {}",
                            method.name,
                            method
                                .params
                                .iter()
                                .map(render_param)
                                .collect::<Vec<_>>()
                                .join(", "),
                            render_type(&method.ret)
                        ),
                        doc: doc_comment(units, method.line, entry, &mut cache),
                        public: true,
                        attributes: Vec::new(),
                    })
                    .collect();
                model.interfaces.push(doc_interface);
            }
            Item::Const(decl) | Item::Static(decl) => {
                model.globals.push(DocGlobal {
                    name: decl.name.clone(),
                    signature: format!("{}: {}", decl.name, render_type(&decl.ty)),
                    doc: doc_comment(units, decl.line, entry, &mut cache),
                    public: matches!(item, Item::Const(_)),
                });
            }
            Item::Import(_) => {}
        }
    }
    // 方法可能在类型声明之后出现：统一在收集完所有条目后再挂到所属类型。
    for (owner, entries) in methods {
        if let Some(doc_type) = model.types.iter_mut().find(|item| item.name == owner) {
            doc_type.methods = entries;
            continue;
        }
        if let Some(doc_enum) = model.enums.iter_mut().find(|item| item.name == owner) {
            doc_enum.methods = entries;
            continue;
        }
        model.types.push(DocType {
            name: owner.clone(),
            signature: format!("type {} = [..];", owner),
            doc: None,
            public: true,
            methods: entries,
        });
    }
    model
        .functions
        .sort_by(|left, right| left.name.cmp(&right.name));
    model.types.sort_by(|left, right| left.name.cmp(&right.name));
    model.enums.sort_by(|left, right| left.name.cmp(&right.name));
    model
        .interfaces
        .sort_by(|left, right| left.name.cmp(&right.name));
    model.globals.sort_by(|left, right| left.name.cmp(&right.name));
    model
}

fn entry_of_func(
    func: &FuncDecl,
    units: &[ProjectUnit],
    entry: &str,
    cache: &mut HashMap<String, Vec<String>>,
) -> DocEntry {
    let mut signature = String::new();
    if func.public {
        signature.push_str("pub ");
    }
    if func.self_only {
        signature.push_str("self ");
    }
    signature.push_str("func ");
    signature.push_str(&func.name);
    if !func.type_params.is_empty() {
        signature.push('<');
        signature.push_str(&func.type_params.join(", "));
        signature.push('>');
    }
    signature.push('(');
    signature.push_str(
        &func
            .params
            .iter()
            .map(render_param)
            .collect::<Vec<_>>()
            .join(", "),
    );
    signature.push_str(") -> ");
    signature.push_str(&render_type(&func.ret));
    DocEntry {
        name: func.name.clone(),
        signature,
        doc: doc_comment(units, func.line, entry, cache),
        public: func.public,
        attributes: func
            .attributes
            .iter()
            .map(|attribute| format!("#[{}({})]", attribute.name, attribute.args.join(", ")))
            .collect(),
    }
}

fn render_param(param: &rp_ast::Param) -> String {
    let mode = match param.mode {
        ParamMode::Value => "",
        ParamMode::Read => "read ",
        ParamMode::Write => "write ",
    };
    format!("{}{}: {}", mode, param.name, render_type(&param.ty))
}

fn render_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeExprKind::Named(path) => path.join("."),
        TypeExprKind::Ary(fields) => format!(
            "[{}]",
            fields
                .iter()
                .map(|field| format!("{}: {}", field.name, render_type(&field.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExprKind::Vec(items) => format!(
            "vec<{}>",
            items
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExprKind::Map(key, value) => {
            format!("map<{}, {}>", render_type(key), render_type(value))
        }
        TypeExprKind::Option(inner) => format!("option<{}>", render_type(inner)),
        TypeExprKind::Result(ok, err) => {
            format!("result<{}, {}>", render_type(ok), render_type(err))
        }
        TypeExprKind::Share(inner) => format!("share<{}>", render_type(inner)),
        TypeExprKind::Task(inner) => format!("task<{}>", render_type(inner)),
        TypeExprKind::Read(inner) => format!("read {}", render_type(inner)),
        TypeExprKind::Func(params, ret) => format!(
            "func({}) -> {}",
            params
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", "),
            render_type(ret)
        ),
        TypeExprKind::Void => "void".to_string(),
    }
}

/// 收集紧邻声明上方的 `///`；中间允许出现属性行与空行。
fn doc_comment(
    units: &[ProjectUnit],
    line: u32,
    entry: &str,
    cache: &mut HashMap<String, Vec<String>>,
) -> Option<String> {
    let (file, original) = locate(units, line, entry);
    let lines = cache.entry(file.clone()).or_insert_with(|| {
        fs::read_to_string(&file)
            .map(|text| text.lines().map(|line| line.to_string()).collect())
            .unwrap_or_default()
    });
    if original == 0 || original as usize > lines.len() {
        return None;
    }
    let mut collected = Vec::new();
    let mut index = original as usize - 1;
    while index > 0 {
        let text = lines[index - 1].trim_start();
        if text.starts_with("#[") {
            index -= 1;
            continue;
        }
        if let Some(rest) = text.strip_prefix("///") {
            collected.push(rest.trim().to_string());
            index -= 1;
            continue;
        }
        break;
    }
    if collected.is_empty() {
        None
    } else {
        collected.reverse();
        Some(collected.join("\n"))
    }
}

pub(crate) fn to_json(name: &str, version: &str, model: &DocModel) -> serde_json::Value {
    let functions: Vec<serde_json::Value> = model.functions.iter().map(entry_json).collect();
    let types: Vec<serde_json::Value> = model
        .types
        .iter()
        .map(|doc_type| {
            serde_json::json!({
                "name": doc_type.name,
                "signature": doc_type.signature,
                "doc": doc_type.doc,
                "public": doc_type.public,
                "methods": doc_type.methods.iter().map(entry_json).collect::<Vec<_>>()
            })
        })
        .collect();
    let enums: Vec<serde_json::Value> = model
        .enums
        .iter()
        .map(|doc_enum| {
            serde_json::json!({
                "name": doc_enum.name,
                "doc": doc_enum.doc,
                "public": doc_enum.public,
                "variants": doc_enum.variants.iter().map(|(name, fields)| {
                    serde_json::json!({
                        "name": name,
                        "fields": fields.iter().map(|(name, ty)| {
                            serde_json::json!({ "name": name, "type": ty })
                        }).collect::<Vec<_>>()
                    })
                }).collect::<Vec<_>>(),
                "methods": doc_enum.methods.iter().map(entry_json).collect::<Vec<_>>()
            })
        })
        .collect();
    let interfaces: Vec<serde_json::Value> = model
        .interfaces
        .iter()
        .map(|interface| {
            serde_json::json!({
                "name": interface.name,
                "doc": interface.doc,
                "public": interface.public,
                "methods": interface.methods.iter().map(entry_json).collect::<Vec<_>>()
            })
        })
        .collect();
    let globals: Vec<serde_json::Value> = model
        .globals
        .iter()
        .map(|global| {
            serde_json::json!({
                "name": global.name,
                "signature": global.signature,
                "doc": global.doc,
                "public": global.public
            })
        })
        .collect();
    serde_json::json!({
        "name": name,
        "version": version,
        "functions": functions,
        "types": types,
        "enums": enums,
        "interfaces": interfaces,
        "globals": globals
    })
}

fn entry_json(entry: &DocEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "signature": entry.signature,
        "doc": entry.doc,
        "public": entry.public,
        "attributes": entry.attributes
    })
}

pub(crate) fn write_html(
    out_dir: &Path,
    name: &str,
    version: &str,
    model: &DocModel,
) -> Result<PathBuf, String> {
    fs::create_dir_all(out_dir)
        .map_err(|error| format!("cannot create {}: {}", out_dir.display(), error))?;
    let path = out_dir.join("index.html");
    fs::write(&path, render_html(name, version, model))
        .map_err(|error| format!("cannot write {}: {}", path.display(), error))?;
    Ok(path)
}

fn render_html(name: &str, version: &str, model: &DocModel) -> String {
    let mut body = String::new();
    let mut nav = String::new();

    body.push_str(&format!(
        "<header><h1>{}</h1><p class=\"version\">v{}</p></header>",
        escape(name),
        escape(version)
    ));

    if !model.functions.is_empty() {
        nav.push_str("<a href=\"#functions\">Functions</a>");
        body.push_str("<section id=\"functions\"><h2>Functions</h2>");
        for entry in &model.functions {
            body.push_str(&render_entry(entry));
        }
        body.push_str("</section>");
    }
    if !model.types.is_empty() {
        nav.push_str("<a href=\"#types\">Types</a>");
        body.push_str("<section id=\"types\"><h2>Types</h2>");
        for doc_type in &model.types {
            body.push_str(&format!(
                "<article class=\"card\"><h3 id=\"type-{}\">{}</h3><pre class=\"signature\">{}</pre>",
                anchor(&doc_type.name),
                escape(&doc_type.name),
                escape(&doc_type.signature)
            ));
            if let Some(doc) = &doc_type.doc {
                body.push_str(&format!("<p class=\"doc\">{}</p>", escape_multiline(doc)));
            }
            if !doc_type.methods.is_empty() {
                body.push_str("<h4>Methods</h4>");
                for entry in &doc_type.methods {
                    body.push_str(&render_entry(entry));
                }
            }
            body.push_str("</article>");
        }
        body.push_str("</section>");
    }
    if !model.enums.is_empty() {
        nav.push_str("<a href=\"#enums\">Enums</a>");
        body.push_str("<section id=\"enums\"><h2>Enums</h2>");
        for doc_enum in &model.enums {
            body.push_str(&format!(
                "<article class=\"card\"><h3 id=\"enum-{}\">{}</h3><pre class=\"signature\">type {} = enum {{ ... }};</pre>",
                anchor(&doc_enum.name),
                escape(&doc_enum.name),
                escape(&doc_enum.name)
            ));
            if let Some(doc) = &doc_enum.doc {
                body.push_str(&format!("<p class=\"doc\">{}</p>", escape_multiline(doc)));
            }
            body.push_str("<ul class=\"variants\">");
            for (variant, fields) in &doc_enum.variants {
                if fields.is_empty() {
                    body.push_str(&format!("<li><code>{}</code></li>", escape(variant)));
                } else {
                    let fields = fields
                        .iter()
                        .map(|(name, ty)| format!("{}: {}", name, ty))
                        .collect::<Vec<_>>()
                        .join(", ");
                    body.push_str(&format!(
                        "<li><code>{}({})</code></li>",
                        escape(variant),
                        escape(&fields)
                    ));
                }
            }
            body.push_str("</ul>");
            if !doc_enum.methods.is_empty() {
                body.push_str("<h4>Methods</h4>");
                for entry in &doc_enum.methods {
                    body.push_str(&render_entry(entry));
                }
            }
            body.push_str("</article>");
        }
        body.push_str("</section>");
    }
    if !model.interfaces.is_empty() {
        nav.push_str("<a href=\"#interfaces\">Interfaces</a>");
        body.push_str("<section id=\"interfaces\"><h2>Interfaces</h2>");
        for interface in &model.interfaces {
            body.push_str(&format!(
                "<article class=\"card\"><h3 id=\"iface-{}\">{}</h3>",
                anchor(&interface.name),
                escape(&interface.name)
            ));
            if let Some(doc) = &interface.doc {
                body.push_str(&format!("<p class=\"doc\">{}</p>", escape_multiline(doc)));
            }
            for entry in &interface.methods {
                body.push_str(&render_entry(entry));
            }
            body.push_str("</article>");
        }
        body.push_str("</section>");
    }
    if !model.globals.is_empty() {
        nav.push_str("<a href=\"#globals\">Globals</a>");
        body.push_str("<section id=\"globals\"><h2>Globals</h2>");
        for global in &model.globals {
            body.push_str(&format!(
                "<article class=\"card\"><pre class=\"signature\">{}</pre>",
                escape(&global.signature)
            ));
            if let Some(doc) = &global.doc {
                body.push_str(&format!("<p class=\"doc\">{}</p>", escape_multiline(doc)));
            }
            body.push_str("</article>");
        }
        body.push_str("</section>");
    }

    format!(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{name} API</title>\n<style>\n:root{{--bg:#fff;--text:#1f2328;--muted:#5b6572;--border:#e3e8ef;--accent:#2563eb;--code:#f6f8fa;--card:#fbfcfe}}\n@media (prefers-color-scheme:dark){{:root{{--bg:#0f141a;--text:#e6edf3;--muted:#9aa7b4;--border:#263140;--accent:#58a6ff;--code:#0b1118;--card:#141b24}}}}\n*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--text);font:15px/1.7 -apple-system,\"Segoe UI\",\"Microsoft YaHei\",sans-serif}}\nnav{{position:fixed;top:0;left:0;bottom:0;width:220px;padding:24px 16px;border-right:1px solid var(--border);overflow:auto}}\nnav a{{display:block;padding:6px 8px;color:var(--muted);text-decoration:none;border-radius:6px}}nav a:hover{{background:var(--code);color:var(--text)}}\nmain{{margin-left:220px;max-width:900px;padding:32px 32px 80px}}\nheader h1{{margin:0 0 4px;font-size:30px}}.version{{color:var(--muted);margin:0 0 24px}}\nsection{{margin-top:40px}}h2{{border-bottom:1px solid var(--border);padding-bottom:8px;font-size:22px}}\n.card{{border:1px solid var(--border);background:var(--card);border-radius:10px;padding:16px 18px;margin:14px 0}}\n.card h3{{margin:0 0 8px;font-size:17px}}.card h4{{margin:16px 0 6px;font-size:14px;color:var(--muted)}}\n.signature{{background:var(--code);border-radius:8px;padding:10px 12px;overflow:auto;margin:8px 0;font:13px/1.6 \"Cascadia Mono\",Consolas,monospace}}\n.doc{{margin:6px 0;white-space:pre-wrap}}.attrs{{color:var(--accent);font-size:12px;margin:2px 0}}\n.variants{{margin:8px 0 0;padding-left:20px}}.variants code{{background:var(--code);padding:1px 6px;border-radius:4px}}\n@media (max-width:760px){{nav{{display:none}}main{{margin-left:0;padding:20px}}}}\n</style>\n</head>\n<body>\n<nav>{nav}</nav>\n<main>{body}</main>\n</body>\n</html>\n",
        name = escape(name),
        nav = nav,
        body = body
    )
}

fn render_entry(entry: &DocEntry) -> String {
    let mut out = String::from("<div class=\"entry\">");
    for attribute in &entry.attributes {
        out.push_str(&format!("<p class=\"attrs\">{}</p>", escape(attribute)));
    }
    out.push_str(&format!(
        "<pre class=\"signature\">{}</pre>",
        escape(&entry.signature)
    ));
    if let Some(doc) = &entry.doc {
        out.push_str(&format!("<p class=\"doc\">{}</p>", escape_multiline(doc)));
    }
    out.push_str("</div>");
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
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

fn escape_multiline(text: &str) -> String {
    escape(text).replace('\n', "<br>")
}

fn anchor(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

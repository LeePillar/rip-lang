//! `std/xml` 与 `std/encode`：XML 解析、URL / HTML / CSV / 查询串编码。

use crate::rt::{FlowResult, Runtime};
use crate::value::{format_value, MapData, Value};

impl Runtime {
    /// `std/xml`：轻量 XML 解析与序列化，节点统一表示为 `xml.node`。
    pub(crate) fn xml_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "parse" => {
                let text = self.string_arg(&args, 0, name, line)?;
                match xml_parse(&text) {
                    Ok(node) => Ok(Value::Ok(Box::new(node))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "stringify" => {
                let node = match args.first() {
                    Some(node) => node.clone(),
                    None => return self.error("xml.stringify expects xml.node", line),
                };
                match xml_node(&node) {
                    Some(_) => Ok(Value::string(xml_stringify(&node, false))),
                    None => self.error("xml.stringify expects xml.node", line),
                }
            }
            "pretty" => {
                let node = match args.first() {
                    Some(node) => node.clone(),
                    None => return self.error("xml.pretty expects xml.node", line),
                };
                match xml_node(&node) {
                    Some(_) => Ok(Value::string(xml_stringify(&node, true))),
                    None => self.error("xml.pretty expects xml.node", line),
                }
            }
            "escape" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(xml_escape(&text)))
            }
            "unescape" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(xml_unescape(&text)))
            }
            "name" => match args.first().and_then(xml_node) {
                Some(node) => Ok(Value::string(node.name.clone())),
                None => self.error("xml.name expects xml.node", line),
            },
            "text" => match args.first().and_then(xml_node) {
                Some(node) => Ok(Value::string(node.text.clone())),
                None => self.error("xml.text expects xml.node", line),
            },
            "attrs" => match args.first().and_then(xml_node) {
                Some(node) => {
                    let mut map = MapData::default();
                    for (key, value) in node.attrs {
                        map.set(Value::string(key), Value::string(value));
                    }
                    Ok(Value::map_data(map))
                }
                None => self.error("xml.attrs expects xml.node", line),
            },
            "children" => match args.first().and_then(xml_node) {
                Some(node) => Ok(Value::vec(node.children)),
                None => self.error("xml.children expects xml.node", line),
            },
            "find" => {
                let node = match args.first().and_then(xml_node) {
                    Some(node) => node,
                    None => return self.error("xml.find expects (xml.node, string)", line),
                };
                let target = self.string_arg(&args, 1, name, line)?;
                Ok(match find_node(&node, &target) {
                    Some(found) => Value::Some(Box::new(found)),
                    None => Value::None,
                })
            }
            "find_all" => {
                let node = match args.first().and_then(xml_node) {
                    Some(node) => node,
                    None => return self.error("xml.find_all expects (xml.node, string)", line),
                };
                let target = self.string_arg(&args, 1, name, line)?;
                let mut found = Vec::new();
                collect_nodes(&node, &target, &mut found);
                Ok(Value::vec(found))
            }
            _ => self.error(format!("std/xml has no function '{}'", name), line),
        }
    }

    /// `std/encode`：URL、HTML、CSV 与查询串编解码。
    pub(crate) fn encode_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "url_encode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(url_encode(&text)))
            }
            "url_decode" => {
                let text = self.string_arg(&args, 0, name, line)?;
                match url_decode(&text) {
                    Ok(decoded) => Ok(Value::Ok(Box::new(Value::string(decoded)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "html_escape" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(html_escape(&text)))
            }
            "html_unescape" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(html_unescape(&text)))
            }
            "csv_encode" => {
                let rows = match args.first() {
                    Some(Value::Vec(rows)) => rows.borrow().clone(),
                    _ => return self.error("csv_encode expects vec<vec<string>>", line),
                };
                let mut out = String::new();
                for (index, row) in rows.iter().enumerate() {
                    let cells = match row {
                        Value::Vec(cells) => cells.borrow().clone(),
                        _ => return self.error("csv_encode expects vec<vec<string>>", line),
                    };
                    if index > 0 {
                        out.push_str("\r\n");
                    }
                    let fields: Vec<String> = cells
                        .iter()
                        .map(|cell| {
                            cell.as_str()
                                .unwrap_or_else(|| format_value(cell))
                        })
                        .collect();
                    out.push_str(&csv_encode_row(&fields));
                }
                Ok(Value::string(out))
            }
            "csv_parse" => {
                let text = self.string_arg(&args, 0, name, line)?;
                Ok(Value::vec(
                    csv_parse(&text)
                        .into_iter()
                        .map(|row| {
                            Value::vec(row.into_iter().map(Value::string).collect())
                        })
                        .collect(),
                ))
            }
            "query_parse" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let mut map = MapData::default();
                for pair in text.split('&') {
                    if pair.is_empty() {
                        continue;
                    }
                    let (key, value) = match pair.split_once('=') {
                        Some((key, value)) => (key, value),
                        None => (pair, ""),
                    };
                    let key = match url_decode(key) {
                        Ok(key) => key,
                        Err(message) => {
                            return Ok(Value::Err(Box::new(Value::string(message))));
                        }
                    };
                    let value = match url_decode(value) {
                        Ok(value) => value,
                        Err(message) => {
                            return Ok(Value::Err(Box::new(Value::string(message))));
                        }
                    };
                    map.set(Value::string(key), Value::string(value));
                }
                Ok(Value::map_data(map))
            }
            "query_stringify" => {
                let entries = match args.first() {
                    Some(Value::Map(map)) => map.borrow().entries.clone(),
                    _ => return self.error("query_stringify expects map<string, string>", line),
                };
                let mut parts = Vec::new();
                for (key, value) in entries {
                    let key = key.as_str().unwrap_or_else(|| format_value(&key));
                    let value = value.as_str().unwrap_or_else(|| format_value(&value));
                    parts.push(format!("{}={}", url_encode(&key), url_encode(&value)));
                }
                Ok(Value::string(parts.join("&")))
            }
            _ => self.error(format!("std/encode has no function '{}'", name), line),
        }
    }
}

struct XmlNode {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Value>,
}

fn xml_node(value: &Value) -> Option<XmlNode> {
    let ary = match value {
        Value::Ary(ary) => ary.borrow(),
        _ => return None,
    };
    if ary.class != "xml.node" {
        return None;
    }
    let name = ary.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let text = ary.get("text").and_then(|v| v.as_str()).unwrap_or_default();
    let mut attrs = Vec::new();
    if let Some(Value::Map(map)) = ary.get("attrs") {
        for (key, value) in &map.borrow().entries {
            attrs.push((
                key.as_str().unwrap_or_default(),
                value.as_str().unwrap_or_default(),
            ));
        }
    }
    let children = match ary.get("children") {
        Some(Value::Vec(items)) => items.borrow().clone(),
        _ => Vec::new(),
    };
    Some(XmlNode {
        name,
        attrs,
        text,
        children,
    })
}

fn make_xml_node(
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Value>,
) -> Value {
    let mut map = MapData::default();
    for (key, value) in attrs {
        map.set(Value::string(key), Value::string(value));
    }
    Value::ary(
        "xml.node",
        vec![
            ("name".to_string(), Value::string(name)),
            ("attrs".to_string(), Value::map_data(map)),
            ("text".to_string(), Value::string(text)),
            ("children".to_string(), Value::vec(children)),
        ],
    )
}

struct XmlParser {
    chars: Vec<char>,
    pos: usize,
    /// 当前嵌套深度：递归下降解析必须有上限，否则深层文档会栈溢出（进程 abort）。
    depth: usize,
}

/// XML 最大嵌套深度。业务文档远低于此值，超限按解析错误处理。
const MAX_XML_DEPTH: usize = 256;

fn xml_parse(text: &str) -> Result<Value, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parser = XmlParser {
        chars: text.chars().collect(),
        pos: 0,
        depth: 0,
    };
    parser.skip_misc();
    if parser.pos >= parser.chars.len() {
        return Err("empty XML document".to_string());
    }
    let root = parser.parse_element()?;
    parser.skip_misc();
    if parser.pos < parser.chars.len() {
        return Err(format!(
            "unexpected trailing content at offset {}",
            parser.pos
        ));
    }
    Ok(root)
}

impl XmlParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        let mut index = self.pos;
        for expected in text.chars() {
            if self.chars.get(index).copied() != Some(expected) {
                return false;
            }
            index += 1;
        }
        true
    }

    fn take_until(&mut self, marker: &str) {
        while self.pos < self.chars.len() {
            if self.starts_with(marker) {
                self.pos += marker.chars().count();
                return;
            }
            self.pos += 1;
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// 跳过声明、注释与 DOCTYPE。
    fn skip_misc(&mut self) {
        loop {
            self.skip_whitespace();
            if self.starts_with("<?") {
                self.take_until("?>");
            } else if self.starts_with("<!--") {
                self.take_until("-->");
            } else if self.starts_with("<!DOCTYPE") || self.starts_with("<!doctype") {
                self.take_until(">");
            } else {
                break;
            }
        }
    }

    fn read_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '/' || c == '>' || c == '=' {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(format!("expected a name at offset {}", self.pos));
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn parse_element(&mut self) -> Result<Value, String> {
        self.depth += 1;
        if self.depth > MAX_XML_DEPTH {
            self.depth -= 1;
            return Err(format!("xml nesting exceeds {} levels", MAX_XML_DEPTH));
        }
        let result = self.parse_element_inner();
        self.depth -= 1;
        result
    }

    fn parse_element_inner(&mut self) -> Result<Value, String> {
        if self.peek() != Some('<') {
            return Err(format!("expected '<' at offset {}", self.pos));
        }
        self.pos += 1;
        let name = self.read_name()?;
        let mut attrs = Vec::new();
        let mut self_closing = false;
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some('/') => {
                    self.pos += 1;
                    if self.peek() == Some('>') {
                        self.pos += 1;
                        self_closing = true;
                        break;
                    }
                    return Err(format!("malformed self-closing tag '{}'", name));
                }
                Some('>') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    let key = self.read_name()?;
                    self.skip_whitespace();
                    if self.peek() != Some('=') {
                        return Err(format!("attribute '{}' has no value", key));
                    }
                    self.pos += 1;
                    self.skip_whitespace();
                    let quote = match self.peek() {
                        Some(c @ ('"' | '\'')) => c,
                        _ => return Err(format!("attribute '{}' value must be quoted", key)),
                    };
                    self.pos += 1;
                    let start = self.pos;
                    while let Some(c) = self.peek() {
                        if c == quote {
                            break;
                        }
                        self.pos += 1;
                    }
                    if self.peek() != Some(quote) {
                        return Err(format!("attribute '{}' value is not terminated", key));
                    }
                    let raw: String = self.chars[start..self.pos].iter().collect();
                    self.pos += 1;
                    attrs.push((key, xml_unescape(&raw)));
                }
                None => return Err(format!("tag '{}' is not closed", name)),
            }
        }
        if self_closing {
            return Ok(make_xml_node(name, attrs, String::new(), Vec::new()));
        }

        let mut text = String::new();
        let mut children = Vec::new();
        loop {
            if self.pos >= self.chars.len() {
                return Err(format!("tag '{}' is not closed", name));
            }
            if self.starts_with("</") {
                self.pos += 2;
                let closing = self.read_name()?;
                self.skip_whitespace();
                if self.peek() != Some('>') {
                    return Err(format!("closing tag '{}' is malformed", closing));
                }
                self.pos += 1;
                if closing != name {
                    return Err(format!(
                        "mismatched closing tag: <{}> closed by </{}>",
                        name, closing
                    ));
                }
                break;
            }
            if self.starts_with("<![CDATA[") {
                self.pos += "<![CDATA[".chars().count();
                let start = self.pos;
                while self.pos < self.chars.len() && !self.starts_with("]]>") {
                    self.pos += 1;
                }
                if self.pos >= self.chars.len() {
                    return Err("CDATA section is not terminated".to_string());
                }
                text.push_str(&self.chars[start..self.pos].iter().collect::<String>());
                self.pos += 3;
                continue;
            }
            if self.starts_with("<!--") {
                self.take_until("-->");
                continue;
            }
            if self.peek() == Some('<') {
                children.push(self.parse_element()?);
                continue;
            }
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == '<' {
                    break;
                }
                self.pos += 1;
            }
            let raw: String = self.chars[start..self.pos].iter().collect();
            text.push_str(&xml_unescape(&raw));
        }
        Ok(make_xml_node(name, attrs, text.trim().to_string(), children))
    }
}

fn find_node(node: &XmlNode, target: &str) -> Option<Value> {
    if node.name == target {
        return Some(make_xml_node(
            node.name.clone(),
            node.attrs.clone(),
            node.text.clone(),
            node.children.clone(),
        ));
    }
    for child in &node.children {
        if let Some(child) = xml_node(child) {
            if let Some(found) = find_node(&child, target) {
                return Some(found);
            }
        }
    }
    None
}

fn collect_nodes(node: &XmlNode, target: &str, out: &mut Vec<Value>) {
    if node.name == target {
        out.push(make_xml_node(
            node.name.clone(),
            node.attrs.clone(),
            node.text.clone(),
            node.children.clone(),
        ));
    }
    for child in &node.children {
        if let Some(child) = xml_node(child) {
            collect_nodes(&child, target, out);
        }
    }
}

fn xml_stringify(value: &Value, pretty: bool) -> String {
    match xml_node(value) {
        Some(node) => stringify_node(&node, pretty, 0),
        None => String::new(),
    }
}

fn stringify_node(node: &XmlNode, pretty: bool, depth: usize) -> String {
    let mut out = String::new();
    if pretty {
        out.push_str(&"  ".repeat(depth));
    }
    out.push('<');
    out.push_str(&node.name);
    for (key, value) in &node.attrs {
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        out.push_str(&xml_escape(value));
        out.push('"');
    }
    if node.children.is_empty() && node.text.is_empty() {
        out.push_str("/>");
        return out;
    }
    out.push('>');
    if node.children.is_empty() {
        out.push_str(&xml_escape(&node.text));
    } else {
        if !node.text.is_empty() {
            if pretty {
                out.push('\n');
                out.push_str(&"  ".repeat(depth + 1));
            }
            out.push_str(&xml_escape(&node.text));
        }
        for (index, child) in node.children.iter().enumerate() {
            if pretty {
                out.push('\n');
            } else if index > 0 || !node.text.is_empty() {
                // 紧凑输出：子节点直接拼接。
            }
            if let Some(child) = xml_node(child) {
                out.push_str(&stringify_node(&child, pretty, depth + 1));
            }
        }
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        }
    }
    out.push_str("</");
    out.push_str(&node.name);
    out.push('>');
    out
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn xml_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '&' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let end = match (index + 1..chars.len()).find(|&i| chars[i] == ';') {
            Some(end) => end,
            None => {
                out.push('&');
                index += 1;
                continue;
            }
        };
        let entity: String = chars[index + 1..end].iter().collect();
        let decoded = match entity.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                index = end + 1;
            }
            None => {
                out.push('&');
                index += 1;
            }
        }
    }
    out
}

fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn url_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err("invalid URL escape: '%' must be followed by two hex digits".to_string());
                }
                let high = hex_value(bytes[index + 1])
                    .ok_or_else(|| "invalid URL escape: not a hex digit".to_string())?;
                let low = hex_value(bytes[index + 2])
                    .ok_or_else(|| "invalid URL escape: not a hex digit".to_string())?;
                out.push(high * 16 + low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Ok(String::from_utf8_lossy(&out).to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn html_escape(text: &str) -> String {
    xml_escape(text)
}

fn html_unescape(text: &str) -> String {
    xml_unescape(text)
}

fn csv_encode_row(fields: &[String]) -> String {
    let mut out = String::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let needs_quote = field.contains(',')
            || field.contains('"')
            || field.contains('\n')
            || field.contains('\r');
        if needs_quote {
            out.push('"');
            out.push_str(&field.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(field);
        }
    }
    out
}

fn csv_parse(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted_field = false;
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_quotes {
            if c == '"' {
                if chars.get(index + 1) == Some(&'"') {
                    field.push('"');
                    index += 2;
                    continue;
                }
                in_quotes = false;
                index += 1;
                continue;
            }
            field.push(c);
            index += 1;
            continue;
        }
        match c {
            '"' if field.is_empty() && !quoted_field => {
                in_quotes = true;
                quoted_field = true;
                index += 1;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                quoted_field = false;
                index += 1;
            }
            '\r' => {
                index += 1;
                if chars.get(index) == Some(&'\n') {
                    index += 1;
                }
                row.push(std::mem::take(&mut field));
                quoted_field = false;
                rows.push(std::mem::take(&mut row));
            }
            '\n' => {
                index += 1;
                row.push(std::mem::take(&mut field));
                quoted_field = false;
                rows.push(std::mem::take(&mut row));
            }
            other => {
                field.push(other);
                index += 1;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

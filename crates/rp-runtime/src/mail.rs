//! `std/mail`：SMTP 邮件发送（TLS 复用 rustls + ring）。
//!
//! 配置用 `mail.config()` 取默认值，再按需给字段赋值；附件用
//! `mail.attachment()` 构造，内容可以来自文件路径或 base64。

use std::time::Duration;

use base64::Engine as _;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};

use crate::rt::{FlowResult, Runtime};
use crate::value::Value;

impl Runtime {
    pub(crate) fn mail_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "config" => Ok(Value::ary(
                "mail.config",
                vec![
                    ("host".to_string(), Value::string("localhost")),
                    ("port".to_string(), Value::Int(25)),
                    ("tls".to_string(), Value::string("none")),
                    ("username".to_string(), Value::string("")),
                    ("password".to_string(), Value::string("")),
                    ("from".to_string(), Value::string("")),
                    ("from_name".to_string(), Value::string("")),
                    ("to".to_string(), Value::vec(Vec::new())),
                    ("cc".to_string(), Value::vec(Vec::new())),
                    ("bcc".to_string(), Value::vec(Vec::new())),
                    ("reply_to".to_string(), Value::string("")),
                    ("subject".to_string(), Value::string("")),
                    ("text".to_string(), Value::string("")),
                    ("html".to_string(), Value::string("")),
                    ("attachments".to_string(), Value::vec(Vec::new())),
                    ("timeout_ms".to_string(), Value::Int(30_000)),
                ],
            )),
            "attachment" => Ok(Value::ary(
                "mail.attachment",
                vec![
                    ("filename".to_string(), Value::string("")),
                    ("path".to_string(), Value::string("")),
                    ("content_base64".to_string(), Value::string("")),
                    (
                        "content_type".to_string(),
                        Value::string("application/octet-stream"),
                    ),
                ],
            )),
            "send" => {
                let config = match args.first() {
                    Some(value) => value.clone(),
                    None => return self.error("mail.send expects a mail.config", line),
                };
                Ok(outcome(send_message(&config)))
            }
            _ => self.error(format!("std/mail has no function '{}'", name), line),
        }
    }
}

fn outcome(result: Result<(), String>) -> Value {
    match result {
        Ok(()) => Value::Ok(Box::new(Value::string("sent"))),
        Err(message) => Value::Err(Box::new(Value::string(message))),
    }
}

fn string_field(config: &Value, name: &str) -> String {
    match config {
        Value::Ary(ary) => ary
            .borrow()
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn int_field(config: &Value, name: &str) -> i64 {
    match config {
        Value::Ary(ary) => ary
            .borrow()
            .get(name)
            .and_then(|value| value.as_int())
            .unwrap_or(0),
        _ => 0,
    }
}

fn strings_field(config: &Value, name: &str) -> Vec<String> {
    match config {
        Value::Ary(ary) => match ary.borrow().get(name) {
            Some(Value::Vec(items)) => items
                .borrow()
                .iter()
                .filter_map(|value| value.as_str())
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn values_field(config: &Value, name: &str) -> Vec<Value> {
    match config {
        Value::Ary(ary) => match ary.borrow().get(name) {
            Some(Value::Vec(items)) => items.borrow().clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn mailbox(address: &str, display: &str) -> Result<Mailbox, String> {
    let raw = if display.is_empty() {
        address.to_string()
    } else {
        format!("{} <{}>", display, address)
    };
    raw.parse::<Mailbox>()
        .map_err(|error| format!("invalid mailbox '{}': {}", address, error))
}

fn attachment_part(value: &Value) -> Result<SinglePart, String> {
    let filename = string_field(value, "filename");
    if filename.is_empty() {
        return Err("attachment filename must not be empty".to_string());
    }
    let encoded = string_field(value, "content_base64");
    let bytes = if !encoded.is_empty() {
        base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|error| format!("attachment '{}': invalid base64: {}", filename, error))?
    } else {
        let path = string_field(value, "path");
        if path.is_empty() {
            return Err(format!(
                "attachment '{}' needs path or content_base64",
                filename
            ));
        }
        std::fs::read(&path).map_err(|error| format!("attachment '{}': {}", path, error))?
    };
    let content_type = ContentType::parse(&string_field(value, "content_type"))
        .map_err(|error| format!("attachment '{}': invalid content type: {}", filename, error))?;
    Ok(Attachment::new(filename).body(bytes, content_type))
}

fn send_message(config: &Value) -> Result<(), String> {
    if !matches!(config, Value::Ary(_)) {
        return Err("mail.send expects a mail.config".to_string());
    }
    let host = string_field(config, "host");
    if host.is_empty() {
        return Err("mail.config.host must not be empty".to_string());
    }
    let from = string_field(config, "from");
    if from.is_empty() {
        return Err("mail.config.from must not be empty".to_string());
    }
    let from_mailbox = mailbox(&from, &string_field(config, "from_name"))?;

    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(string_field(config, "subject"));

    let mut recipients = 0usize;
    for address in strings_field(config, "to") {
        builder = builder.to(mailbox(&address, "")?);
        recipients += 1;
    }
    for address in strings_field(config, "cc") {
        builder = builder.cc(mailbox(&address, "")?);
        recipients += 1;
    }
    for address in strings_field(config, "bcc") {
        builder = builder.bcc(mailbox(&address, "")?);
        recipients += 1;
    }
    if recipients == 0 {
        return Err("mail.config needs at least one recipient".to_string());
    }
    let reply_to = string_field(config, "reply_to");
    if !reply_to.is_empty() {
        builder = builder.reply_to(mailbox(&reply_to, "")?);
    }

    let text = string_field(config, "text");
    let html = string_field(config, "html");
    let mut parts = if !text.is_empty() && !html.is_empty() {
        MultiPart::mixed().multipart(MultiPart::alternative_plain_html(text, html))
    } else if !html.is_empty() {
        MultiPart::mixed().singlepart(SinglePart::html(html))
    } else {
        MultiPart::mixed().singlepart(SinglePart::plain(text))
    };
    let mut attachments = 0usize;
    for attachment in values_field(config, "attachments") {
        parts = parts.singlepart(attachment_part(&attachment)?);
        attachments += 1;
    }
    let message = if attachments == 0 {
        // 没有附件时不包 multipart，让邮件客户端直接看到正文。
        let text = string_field(config, "text");
        let html = string_field(config, "html");
        if !text.is_empty() && !html.is_empty() {
            builder
                .multipart(MultiPart::alternative_plain_html(text, html))
                .map_err(|error| format!("build message: {}", error))?
        } else if !html.is_empty() {
            builder
                .header(ContentType::TEXT_HTML)
                .body(html)
                .map_err(|error| format!("build message: {}", error))?
        } else {
            builder
                .body(text)
                .map_err(|error| format!("build message: {}", error))?
        }
    } else {
        builder
            .multipart(parts)
            .map_err(|error| format!("build message: {}", error))?
    };

    let tls = string_field(config, "tls").to_ascii_lowercase();
    let port = int_field(config, "port").clamp(1, 65_535) as u16;
    let username = string_field(config, "username");
    let has_creds = !username.is_empty();
    let is_local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    let mut transport = SmtpTransport::builder_dangerous(&host).port(port);
    transport = match tls.as_str() {
        "ssl" | "tls" => {
            let params = TlsParameters::new(host.clone())
                .map_err(|error| format!("tls {}: {}", host, error))?;
            transport.tls(Tls::Wrapper(params))
        }
        "starttls" => {
            let params = TlsParameters::new(host.clone())
                .map_err(|error| format!("tls {}: {}", host, error))?;
            transport.tls(Tls::Required(params))
        }
        // 凭据 + tls=none + 非本机：升级为 opportunistic STARTTLS，
        // 避免用户名口令经明文链路被 MITM 窃取（服务端不支持时仍回退明文并告警）。
        _ if has_creds && !is_local => {
            eprintln!(
                "[mail] warning: credentials with tls=none over non-local host '{}'; \
                 enabling opportunistic STARTTLS (set tls=starttls to require it)",
                host
            );
            let params = TlsParameters::new(host.clone())
                .map_err(|error| format!("tls {}: {}", host, error))?;
            transport.tls(Tls::Opportunistic(params))
        }
        _ => transport,
    };
    let timeout = int_field(config, "timeout_ms");
    if timeout > 0 {
        transport = transport.timeout(Some(Duration::from_millis(timeout as u64)));
    }
    if has_creds {
        transport = transport.credentials(Credentials::new(
            username,
            string_field(config, "password"),
        ));
    }
    transport
        .build()
        .send(&message)
        .map(|_| ())
        .map_err(|error| format!("smtp: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        Value::ary(
            "mail.config",
            vec![
                ("host".to_string(), Value::string("localhost")),
                ("port".to_string(), Value::Int(25)),
                ("tls".to_string(), Value::string("none")),
                ("username".to_string(), Value::string("")),
                ("password".to_string(), Value::string("")),
                ("from".to_string(), Value::string("a@example.com")),
                ("from_name".to_string(), Value::string("")),
                (
                    "to".to_string(),
                    Value::vec(vec![Value::string("b@example.com")]),
                ),
                ("cc".to_string(), Value::vec(Vec::new())),
                ("bcc".to_string(), Value::vec(Vec::new())),
                ("reply_to".to_string(), Value::string("")),
                ("subject".to_string(), Value::string("hi")),
                ("text".to_string(), Value::string("hello")),
                ("html".to_string(), Value::string("")),
                ("attachments".to_string(), Value::vec(Vec::new())),
                ("timeout_ms".to_string(), Value::Int(1000)),
            ],
        )
    }

    #[test]
    fn reads_config_fields() {
        let value = config();
        assert_eq!(string_field(&value, "host"), "localhost");
        assert_eq!(int_field(&value, "port"), 25);
        assert_eq!(strings_field(&value, "to"), vec!["b@example.com"]);
    }

    #[test]
    fn rejects_missing_recipient() {
        let value = config();
        if let Value::Ary(ary) = &value {
            ary.borrow_mut()
                .set("to", Value::vec(Vec::new()));
        }
        let error = send_message(&value).unwrap_err();
        assert!(error.contains("recipient"));
    }

    #[test]
    fn rejects_bad_address() {
        let value = Value::ary(
            "mail.attachment",
            vec![
                ("filename".to_string(), Value::string("a.txt")),
                ("path".to_string(), Value::string("")),
                ("content_base64".to_string(), Value::string("")),
                ("content_type".to_string(), Value::string("text/plain")),
            ],
        );
        assert!(attachment_part(&value).is_err());
    }
}

//! Rip 标准库内置类型与函数签名（供类型检查与文档一致性使用）。

use crate::Type;

pub fn int() -> Type {
    Type::Int
}

pub fn string() -> Type {
    Type::Str
}

pub fn boolean() -> Type {
    Type::Bool
}

pub fn float() -> Type {
    Type::Float
}

pub fn void() -> Type {
    Type::Void
}

pub fn any() -> Type {
    Type::Any
}

pub fn vec_of(t: Type) -> Type {
    Type::Vec(Box::new(t))
}

pub fn opt_of(t: Type) -> Type {
    Type::Option(Box::new(t))
}

pub fn result_of(t: Type, e: Type) -> Type {
    Type::Result(Box::new(t), Box::new(e))
}

pub fn map_of(k: Type, v: Type) -> Type {
    Type::Map(Box::new(k), Box::new(v))
}

pub fn named(name: &str) -> Type {
    Type::Named(name.to_string())
}

pub fn http_request() -> Type {
    named("http.request")
}

pub fn http_response() -> Type {
    named("http.response")
}

pub fn http_route() -> Type {
    named("http.route")
}

pub fn http_client_response() -> Type {
    named("http.client_response")
}

pub fn llm_message() -> Type {
    named("llm.message")
}

pub fn llm_options() -> Type {
    named("llm.options")
}

pub fn llm_response() -> Type {
    named("llm.response")
}

pub fn handler() -> Type {
    Type::Func(vec![http_request()], Box::new(http_response()))
}

/// 中间件：接收下游处理函数，返回新的处理函数。
pub fn middleware() -> Type {
    Type::Func(vec![handler()], Box::new(handler()))
}

pub fn db_conn() -> Type {
    named("db.conn")
}

pub fn net_conn() -> Type {
    named("net.conn")
}

pub fn net_listener() -> Type {
    named("net.listener")
}

pub fn net_socket() -> Type {
    named("net.socket")
}

pub fn ws_conn() -> Type {
    named("ws.conn")
}

pub fn xml_node() -> Type {
    named("xml.node")
}

pub fn db_query() -> Type {
    named("db.query")
}

pub fn redis_conn() -> Type {
    named("redis.conn")
}

pub fn sync_mutex() -> Type {
    named("sync.mutex")
}

pub fn sync_atomic() -> Type {
    named("sync.atomic")
}

pub fn sync_channel() -> Type {
    named("sync.channel")
}

pub fn db_rows() -> Type {
    vec_of(map_of(string(), any()))
}

/// 子进程执行结果：退出码与捕获的输出。
pub fn process_output() -> Type {
    named("process.output")
}

/// 文件元数据：大小、是否目录、修改时间（毫秒）。
pub fn fs_metadata() -> Type {
    named("fs.metadata")
}

/// ZIP 条目：名称、原始大小、压缩后大小、CRC32、是否目录。
pub fn zip_entry() -> Type {
    named("zip.entry")
}

/// SMTP 发送配置。
pub fn mail_config() -> Type {
    named("mail.config")
}

/// 邮件附件。
pub fn mail_attachment() -> Type {
    named("mail.attachment")
}

pub fn builtin_type(name: &str) -> Option<Type> {
    let fields = |items: Vec<(&str, Type)>| {
        Type::Ary(
            items
                .into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect(),
        )
    };
    Some(match name {
        "http.request" => fields(vec![
            ("method", string()),
            ("path", string()),
            ("params", map_of(string(), string())),
            ("query", map_of(string(), string())),
            ("headers", map_of(string(), string())),
            ("body", string()),
            ("json", opt_of(any())),
            ("form", map_of(string(), string())),
            ("cookies", map_of(string(), string())),
            ("client_ip", string()),
            ("files", vec_of(named("http.file"))),
            ("request_id", string()),
            ("ctx", map_of(string(), any())),
        ]),
        "http.file" => fields(vec![
            ("field", string()),
            ("filename", string()),
            ("content_type", string()),
            ("size", int()),
            ("data_base64", string()),
        ]),
        "http.response" => fields(vec![
            ("status", int()),
            ("headers", map_of(string(), string())),
            ("body", string()),
            ("json", opt_of(any())),
            (
                "sse",
                opt_of(Type::Func(
                    vec![Type::Func(vec![named("http.sse_event")], Box::new(void()))],
                    Box::new(void()),
                )),
            ),
        ]),
        "http.sse_event" => fields(vec![("event", string()), ("data", string())]),
        "queue.job" => fields(vec![
            ("id", int()),
            ("queue", string()),
            ("payload", string()),
            ("attempts", int()),
        ]),
        "http.client_response" => fields(vec![
            ("status", int()),
            ("headers", map_of(string(), string())),
            ("body", string()),
        ]),
        "llm.message" => fields(vec![("role", string()), ("content", string())]),
        "llm.options" => fields(vec![
            ("model", string()),
            ("temperature", Type::Float),
            ("max_tokens", int()),
            ("top_p", Type::Float),
            ("stream", boolean()),
        ]),
        "llm.response" => fields(vec![
            ("content", string()),
            ("model", string()),
            ("prompt_tokens", int()),
            ("completion_tokens", int()),
            ("finish_reason", string()),
        ]),
        "llm.config" => fields(vec![
            ("provider", string()),
            ("base_url", string()),
            ("api_key", string()),
            ("timeout_ms", int()),
        ]),
        "net.packet" => fields(vec![
            ("data", string()),
            ("host", string()),
            ("port", int()),
        ]),
        "process.output" => fields(vec![
            ("code", int()),
            ("stdout", string()),
            ("stderr", string()),
        ]),
        "fs.metadata" => fields(vec![
            ("size", int()),
            ("is_dir", boolean()),
            ("modified_ms", int()),
        ]),
        "zip.entry" => fields(vec![
            ("name", string()),
            ("size", int()),
            ("compressed_size", int()),
            ("crc32", int()),
            ("is_dir", boolean()),
        ]),
        "mail.config" => fields(vec![
            ("host", string()),
            ("port", int()),
            ("tls", string()),
            ("username", string()),
            ("password", string()),
            ("from", string()),
            ("from_name", string()),
            ("to", vec_of(string())),
            ("cc", vec_of(string())),
            ("bcc", vec_of(string())),
            ("reply_to", string()),
            ("subject", string()),
            ("text", string()),
            ("html", string()),
            ("attachments", vec_of(named("mail.attachment"))),
            ("timeout_ms", int()),
        ]),
        "mail.attachment" => fields(vec![
            ("filename", string()),
            ("path", string()),
            ("content_base64", string()),
            ("content_type", string()),
        ]),
        _ => return None,
    })
}

pub fn is_opaque_named(name: &str) -> bool {
    matches!(
        name,
        "http.route"
            | "llm.client"
            | "time.datetime"
            | "db.conn"
            | "db.query"
            | "redis.conn"
            | "net.conn"
            | "net.listener"
            | "net.socket"
            | "ws.conn"
            | "xml.node"
            | "sync.mutex"
            | "sync.atomic"
            | "sync.channel"
    )
}

pub const TYPE_NAMES: &[&str] = &[
    "http.request",
    "http.response",
    "http.sse_event",
    "http.client_response",
    "http.route",
    "queue.job",
    "llm.message",
    "llm.options",
    "llm.response",
    "llm.config",
    "llm.client",
    "time.datetime",
    "xml.node",
    "net.packet",
    "process.output",
    "fs.metadata",
    "zip.entry",
    "mail.config",
    "mail.attachment",
];

pub struct BuiltinSig {
    pub params: Vec<Type>,
    pub ret: Type,
    pub variadic_any: bool,
}

/// 各内建模块的函数名清单，供编辑器补全使用。
///
/// 与 `builtin_function` 一一对应；`listed_builtins_resolve` 测试保证清单里的
/// 每个名字都能取到签名，改名后不会留下无效的补全项。
pub fn builtin_module_names(module: &str) -> &'static [&'static str] {
    match module {
        "std/http" => &[
            "serve", "fetch", "fetch_file", "fetch_retry", "router", "get", "post", "put",
            "delete", "patch", "options", "head", "static", "download", "metrics", "save_file",
            "chain", "group", "cors", "security_headers", "with_headers", "rate_limit",
            "openapi",
        ],
        "std/crypto" => &[
            "md5", "sha1", "sha256", "sha512", "hmac_sha256", "hmac_sha256_hex", "sha256_file",
            "base64_encode", "base64_decode", "hex_encode", "hex_decode", "random_hex",
            "password_hash", "password_verify", "aes_encrypt", "aes_decrypt", "rsa_generate",
            "rsa_sign", "rsa_verify", "rsa_encrypt", "rsa_decrypt",
        ],
        "std/net" => &[
            "tcp_connect", "tcp_send", "tcp_recv", "tcp_recv_line", "tcp_close", "tcp_listen",
            "tcp_accept", "tcp_listener_close", "udp_bind", "udp_send", "udp_recv", "udp_close",
        ],
        "std/ws" => &["serve", "connect", "send", "recv", "ping", "close"],
        "std/xml" => &[
            "parse", "stringify", "pretty", "escape", "unescape", "name", "text", "attrs",
            "children", "find", "find_all",
        ],
        "std/encode" => &[
            "url_encode", "url_decode", "html_escape", "html_unescape", "csv_encode", "csv_parse",
            "query_parse", "query_stringify",
        ],
        "std/path" => &[
            "join", "file_name", "parent", "extension", "stem", "absolute", "normalize",
        ],
        "std/process" => &["run", "run_shell"],
        "std/regex" => &["is_match", "find", "find_all", "captures", "replace"],
        "std/rand" => &["int", "float", "bool"],
        "std/uuid" => &["v4", "nil"],
        "std/compress" => &["gzip", "gunzip"],
        "std/zip" => &[
            "create", "add", "list", "extract", "read_text", "read_base64", "contains",
        ],
        "std/mail" => &["config", "attachment", "send"],
        "std/session" => &[
            "load", "save", "cookie", "clear", "csrf_token", "csrf_check",
        ],
        "std/validate" => &["required", "int_range", "string_len", "email"],
        "std/template" => &["render", "escape", "register", "clear_cache"],
        "std/jwt" => &["sign", "verify"],
        "std/str" => &[
            "split", "join", "trim", "trim_start", "trim_end", "contains", "starts_with",
            "ends_with", "replace", "lower", "upper", "slice", "repeat", "pad_start",
            "pad_end", "index_of", "len", "is_empty", "lines", "split_once", "normalize",
            "graphemes", "grapheme_len", "width",
        ],
        "std/reflect" => &["type_name", "kind", "fields", "has", "get", "variant", "invoke", "is"],
        "std/vec" => &[
            "len", "is_empty", "contains", "index_of", "first", "last", "sum", "range",
            "range_between",
        ],
        "std/map" => &[
            "keys", "values", "len", "has", "get", "merge", "without",
        ],
        "std/cli" => &["args", "command", "flag", "option", "positionals"],
        "std/cache" => &[
            "set", "get", "delete", "clear", "len", "configure", "stats",
        ],
        "std/queue" => &[
            "enqueue", "reserve", "ack", "fail", "release_expired", "stats",
        ],
        "std/task" => &["spawn", "sleep"],
        "std/sync" => &["mutex", "atomic", "channel", "remove", "exists"],
        "std/dev" => &[
            "log", "log_json", "trace", "breakpoint", "assert", "time_start", "time_end",
        ],
        "std/json" => &["encode", "decode"],
        "std/time" => &[
            "now", "timestamp_ms", "timestamp_s", "format", "format_utc", "parse", "sleep",
            "zone_names", "local_zone", "offset", "format_zone", "parse_zone",
        ],
        "std/sys" => &[
            "env_get", "args", "exit", "platform", "heap_usage", "gc",
        ],
        "std/fs" => &[
            "read_file", "write_file", "exists", "make_dir", "remove_file", "copy_file",
            "file_size", "list_dir", "list_dir_all", "append_file", "remove_dir", "rename",
            "metadata", "temp_file", "temp_dir",
        ],
        "std/db" => &[
            "open", "table", "driver", "has_many", "belongs_to", "belongs_to_by",
        ],
        "std/log" => &[
            "debug", "info", "warn", "error", "set_level", "set_format", "set_file",
            "set_rotate",
        ],
        "std/redis" => &["connect", "lock", "unlock", "rate_limit"],
        "std/llm" => &["new_client"],
        "rust/serde_json" => &["to_string", "to_string_pretty", "from_str"],
        _ => &[],
    }
}

fn sig(params: Vec<Type>, ret: Type) -> BuiltinSig {
    BuiltinSig {
        params,
        ret,
        variadic_any: false,
    }
}

pub fn builtin_function(module: &str, name: &str) -> Option<BuiltinSig> {
    if module == "std/http" {
        let h = handler();
        return Some(match name {
            "serve" => sig(vec![int(), h], void()),
            "fetch" | "fetch_file" => sig(
                vec![
                    string(),
                    string(),
                    map_of(string(), string()),
                    string(),
                ],
                result_of(http_client_response(), string()),
            ),
            "fetch_retry" => sig(
                vec![
                    string(),
                    string(),
                    map_of(string(), string()),
                    string(),
                    int(),
                ],
                result_of(http_client_response(), string()),
            ),
            "router" => sig(vec![vec_of(http_route())], h),
            // 中间件：`func(http.handler) -> http.handler`，可跨线程重建的只有内建工厂与具名函数。
            "chain" => sig(vec![vec_of(middleware()), handler()], handler()),
            "group" => sig(
                vec![string(), vec_of(middleware()), vec_of(http_route())],
                http_route(),
            ),
            "cors" => sig(vec![string()], middleware()),
            "security_headers" => sig(vec![], middleware()),
            "with_headers" => sig(vec![map_of(string(), string())], middleware()),
            "rate_limit" => sig(vec![int(), int()], middleware()),
            "openapi" => sig(vec![handler(), string(), string()], string()),
            "get" | "post" | "put" | "delete" | "patch" | "options" | "head" => {
                sig(vec![string(), h], http_route())
            }
            "static" => sig(vec![string(), string()], http_route()),
            "download" => sig(
                vec![string(), map_of(string(), string()), string()],
                result_of(int(), string()),
            ),
            "metrics" => sig(vec![], string()),
            "save_file" => sig(
                vec![string(), named("http.file")],
                result_of(string(), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/crypto" {
        return Some(match name {
            "md5" | "sha1" | "sha256" | "sha512" => sig(vec![string()], string()),
            "hmac_sha256" => sig(vec![string(), string()], string()),
            "hmac_sha256_hex" => sig(vec![string(), string()], result_of(string(), string())),
            "sha256_file" => sig(vec![string()], result_of(string(), string())),
            "base64_encode" => sig(vec![string()], string()),
            "base64_decode" => sig(vec![string()], result_of(string(), string())),
            "hex_encode" => sig(vec![string()], string()),
            "hex_decode" => sig(vec![string()], result_of(string(), string())),
            "random_hex" => sig(vec![int()], result_of(string(), string())),
            "password_hash" => sig(vec![string()], result_of(string(), string())),
            "password_verify" => sig(vec![string(), string()], boolean()),
            "aes_encrypt" | "aes_decrypt" => {
                sig(vec![string(), string()], result_of(string(), string()))
            }
            "rsa_generate" => sig(vec![int()], result_of(map_of(string(), string()), string())),
            "rsa_sign" => sig(vec![string(), string()], result_of(string(), string())),
            "rsa_verify" => sig(vec![string(), string(), string()], boolean()),
            "rsa_encrypt" => sig(vec![string(), string()], result_of(string(), string())),
            "rsa_decrypt" => sig(vec![string(), string()], result_of(string(), string())),
            _ => return None,
        });
    }
    if module == "std/net" {
        return Some(match name {
            "tcp_connect" => sig(
                vec![string(), int(), int()],
                result_of(net_conn(), string()),
            ),
            "tcp_send" => sig(vec![net_conn(), string()], result_of(int(), string())),
            "tcp_recv" => sig(
                vec![net_conn(), int(), int()],
                result_of(string(), string()),
            ),
            "tcp_recv_line" => sig(
                vec![net_conn(), int(), int()],
                result_of(string(), string()),
            ),
            "tcp_close" => sig(vec![net_conn()], result_of(boolean(), string())),
            "tcp_listen" => sig(vec![string(), int()], result_of(net_listener(), string())),
            "tcp_accept" => sig(
                vec![net_listener(), int()],
                result_of(net_conn(), string()),
            ),
            "tcp_listener_close" => {
                sig(vec![net_listener()], result_of(boolean(), string()))
            }
            "udp_bind" => sig(vec![string(), int()], result_of(net_socket(), string())),
            "udp_send" => sig(
                vec![net_socket(), string(), int(), string()],
                result_of(int(), string()),
            ),
            "udp_recv" => sig(
                vec![net_socket(), int(), int()],
                result_of(named("net.packet"), string()),
            ),
            "udp_close" => sig(vec![net_socket()], result_of(boolean(), string())),
            _ => return None,
        });
    }
    if module == "std/ws" {
        return Some(match name {
            "serve" => sig(
                vec![int(), Type::Func(vec![ws_conn()], Box::new(void()))],
                void(),
            ),
            "connect" => sig(vec![string(), int()], result_of(ws_conn(), string())),
            "send" => sig(vec![ws_conn(), string()], result_of(int(), string())),
            "recv" => sig(vec![ws_conn(), int()], result_of(string(), string())),
            "ping" => sig(vec![ws_conn()], result_of(boolean(), string())),
            "close" => sig(vec![ws_conn()], result_of(boolean(), string())),
            _ => return None,
        });
    }
    if module == "std/xml" {
        return Some(match name {
            "parse" => sig(vec![string()], result_of(xml_node(), string())),
            "stringify" => sig(vec![xml_node()], string()),
            "pretty" => sig(vec![xml_node()], string()),
            "escape" => sig(vec![string()], string()),
            "unescape" => sig(vec![string()], string()),
            "name" | "text" => sig(vec![xml_node()], string()),
            "attrs" => sig(vec![xml_node()], map_of(string(), string())),
            "children" => sig(vec![xml_node()], vec_of(xml_node())),
            "find" => sig(vec![xml_node(), string()], opt_of(xml_node())),
            "find_all" => sig(vec![xml_node(), string()], vec_of(xml_node())),
            _ => return None,
        });
    }
    if module == "std/encode" {
        return Some(match name {
            "url_encode" => sig(vec![string()], string()),
            "url_decode" => sig(vec![string()], result_of(string(), string())),
            "html_escape" | "html_unescape" => sig(vec![string()], string()),
            "csv_encode" => sig(vec![vec_of(vec_of(string()))], string()),
            "csv_parse" => sig(vec![string()], vec_of(vec_of(string()))),
            "query_parse" => sig(vec![string()], map_of(string(), string())),
            "query_stringify" => sig(vec![map_of(string(), string())], string()),
            _ => return None,
        });
    }
    if module == "std/path" {
        return Some(match name {
            "join" => sig(vec![string(), string()], string()),
            "file_name" | "parent" | "extension" | "stem" => sig(vec![string()], opt_of(string())),
            "absolute" => sig(vec![string()], result_of(string(), string())),
            "normalize" => sig(vec![string()], string()),
            _ => return None,
        });
    }
    if module == "std/process" {
        return Some(match name {
            "run" => sig(
                vec![string(), vec_of(string())],
                result_of(process_output(), string()),
            ),
            "run_shell" => sig(vec![string()], result_of(process_output(), string())),
            _ => return None,
        });
    }
    if module == "std/regex" {
        return Some(match name {
            "is_match" => sig(vec![string(), string()], result_of(boolean(), string())),
            "find" => sig(
                vec![string(), string()],
                result_of(opt_of(string()), string()),
            ),
            "find_all" | "captures" => {
                sig(vec![string(), string()], result_of(vec_of(string()), string()))
            }
            "replace" => sig(
                vec![string(), string(), string()],
                result_of(string(), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/rand" {
        return Some(match name {
            "int" => sig(vec![int(), int()], int()),
            "float" => sig(vec![], float()),
            "bool" => sig(vec![], boolean()),
            _ => return None,
        });
    }
    if module == "std/uuid" {
        return Some(match name {
            "v4" | "nil" => sig(vec![], string()),
            _ => return None,
        });
    }
    if module == "std/compress" {
        return Some(match name {
            "gzip" | "gunzip" => sig(vec![string()], result_of(string(), string())),
            _ => return None,
        });
    }
    if module == "std/zip" {
        return Some(match name {
            "create" | "add" => sig(
                vec![string(), map_of(string(), string())],
                result_of(int(), string()),
            ),
            "list" => sig(vec![string()], result_of(vec_of(zip_entry()), string())),
            "extract" => sig(vec![string(), string()], result_of(int(), string())),
            "read_text" | "read_base64" => {
                sig(vec![string(), string()], result_of(string(), string()))
            }
            "contains" => sig(vec![string(), string()], result_of(boolean(), string())),
            _ => return None,
        });
    }
    if module == "std/mail" {
        return Some(match name {
            "config" => sig(vec![], mail_config()),
            "attachment" => sig(vec![], mail_attachment()),
            "send" => sig(vec![mail_config()], result_of(string(), string())),
            _ => return None,
        });
    }
    if module == "std/session" {
        return Some(match name {
            "load" => sig(vec![http_request(), string()], map_of(string(), any())),
            "save" => sig(
                vec![map_of(string(), any()), string(), int()],
                string(),
            ),
            "cookie" => sig(vec![string(), int()], string()),
            "clear" => sig(vec![], string()),
            "csrf_token" => sig(vec![string()], string()),
            "csrf_check" => sig(vec![http_request(), string()], boolean()),
            _ => return None,
        });
    }
    if module == "std/validate" {
        return Some(match name {
            "required" => sig(vec![any()], opt_of(string())),
            "int_range" => sig(vec![any(), int(), int()], result_of(int(), string())),
            "string_len" => sig(
                vec![any(), int(), int()],
                result_of(string(), string()),
            ),
            "email" => sig(vec![any()], result_of(string(), string())),
            _ => return None,
        });
    }
    if module == "std/template" {
        return Some(match name {
            "render" => sig(vec![string(), map_of(string(), any())], string()),
            "escape" => sig(vec![string()], string()),
            "register" => sig(vec![string(), any()], void()),
            "clear_cache" => sig(vec![], void()),
            _ => return None,
        });
    }
    if module == "std/jwt" {
        return Some(match name {
            "sign" => sig(vec![map_of(string(), any()), string(), int()], string()),
            "verify" => sig(
                vec![string(), string()],
                result_of(map_of(string(), any()), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/str" {
        return Some(match name {
            "split" => sig(vec![string(), string()], vec_of(string())),
            "lines" => sig(vec![string()], vec_of(string())),
            "join" => sig(vec![vec_of(string()), string()], string()),
            "trim" | "trim_start" | "trim_end" | "lower" | "upper" => {
                sig(vec![string()], string())
            }
            "contains" | "starts_with" | "ends_with" => {
                sig(vec![string(), string()], boolean())
            }
            "replace" => sig(vec![string(), string(), string()], string()),
            "slice" => sig(vec![string(), int(), int()], string()),
            "repeat" => sig(vec![string(), int()], string()),
            "pad_start" | "pad_end" => sig(vec![string(), int(), string()], string()),
            "index_of" => sig(vec![string(), string()], int()),
            "len" => sig(vec![string()], int()),
            "is_empty" => sig(vec![string()], boolean()),
            "split_once" => sig(vec![string(), string()], opt_of(vec_of(string()))),
            "normalize" => sig(vec![string(), string()], result_of(string(), string())),
            "graphemes" => sig(vec![string()], vec_of(string())),
            "grapheme_len" | "width" => sig(vec![string()], int()),
            _ => return None,
        });
    }
    if module == "std/reflect" {
        return Some(match name {
            "type_name" | "kind" => sig(vec![any()], string()),
            "fields" => sig(vec![any()], vec_of(string())),
            "has" => sig(vec![any(), string()], boolean()),
            "get" => sig(vec![any(), string()], opt_of(any())),
            "variant" => sig(vec![any()], opt_of(string())),
            "invoke" => sig(
                vec![any(), string(), vec_of(any())],
                result_of(any(), string()),
            ),
            "is" => sig(vec![any(), string()], boolean()),
            _ => return None,
        });
    }
    if module == "std/vec" {
        return Some(match name {
            "len" => sig(vec![vec_of(any())], int()),
            "is_empty" => sig(vec![vec_of(any())], boolean()),
            "contains" => sig(vec![vec_of(any()), any()], boolean()),
            "index_of" => sig(vec![vec_of(any()), any()], int()),
            "first" | "last" => sig(vec![vec_of(any())], opt_of(any())),
            "sum" => sig(vec![vec_of(int())], int()),
            "range" => sig(vec![int()], vec_of(int())),
            "range_between" => sig(vec![int(), int()], vec_of(int())),
            _ => return None,
        });
    }
    if module == "std/map" {
        return Some(match name {
            "keys" => sig(vec![map_of(string(), any())], vec_of(string())),
            "values" => sig(vec![map_of(string(), any())], vec_of(any())),
            "len" => sig(vec![map_of(string(), any())], int()),
            "has" => sig(vec![map_of(string(), any()), string()], boolean()),
            "without" => sig(
                vec![map_of(string(), any()), string()],
                map_of(string(), any()),
            ),
            "get" => sig(
                vec![map_of(string(), any()), string()],
                opt_of(any()),
            ),
            "merge" => sig(
                vec![map_of(string(), any()), map_of(string(), any())],
                map_of(string(), any()),
            ),
            _ => return None,
        });
    }
    if module == "std/cli" {
        return Some(match name {
            "args" | "positionals" => sig(vec![], vec_of(string())),
            "command" => sig(vec![], string()),
            "flag" => sig(vec![string()], boolean()),
            "option" => sig(vec![string(), string()], string()),
            _ => return None,
        });
    }
    if module == "std/cache" {
        return Some(match name {
            "set" => sig(vec![string(), string(), int()], void()),
            "get" => sig(vec![string()], opt_of(string())),
            "delete" => sig(vec![string()], boolean()),
            "clear" => sig(vec![], void()),
            "len" => sig(vec![], int()),
            "configure" => sig(vec![int()], int()),
            "stats" => sig(vec![], map_of(string(), int())),
            _ => return None,
        });
    }
    if module == "std/queue" {
        return Some(match name {
            "enqueue" => sig(
                vec![string(), string(), string(), int()],
                result_of(int(), string()),
            ),
            "reserve" => sig(
                vec![string(), string(), int()],
                result_of(opt_of(named("queue.job")), string()),
            ),
            "ack" => sig(vec![string(), int()], result_of(boolean(), string())),
            "fail" => sig(
                vec![string(), int(), string()],
                result_of(string(), string()),
            ),
            "release_expired" => sig(vec![string()], result_of(int(), string())),
            "stats" => sig(
                vec![string(), string()],
                result_of(map_of(string(), int()), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/task" {
        return Some(match name {
            "spawn" => sig(vec![string()], boolean()),
            "sleep" => sig(vec![int()], void()),
            _ => return None,
        });
    }
    if module == "std/sync" {
        return Some(match name {
            "mutex" => sig(vec![string()], named("sync.mutex")),
            "atomic" => sig(vec![string(), int()], named("sync.atomic")),
            "channel" => sig(vec![string(), int()], named("sync.channel")),
            "remove" | "exists" => sig(vec![string()], boolean()),
            _ => return None,
        });
    }
    if module == "std/dev" {
        return Some(match name {
            "log" | "log_json" | "trace" => BuiltinSig {
                params: vec![any()],
                ret: void(),
                variadic_any: true,
            },
            "breakpoint" => sig(vec![], void()),
            "assert" => sig(vec![boolean()], void()),
            "time_start" | "time_end" => sig(vec![string()], void()),
            _ => return None,
        });
    }
    if module == "std/json" {
        return Some(match name {
            "encode" => sig(vec![any()], string()),
            "decode" => sig(vec![string()], result_of(any(), string())),
            _ => return None,
        });
    }
    if module == "std/time" {
        return Some(match name {
            "now" => sig(vec![], named("time.datetime")),
            "timestamp_ms" => sig(vec![], int()),
            "timestamp_s" => sig(vec![], int()),
            "format" => sig(vec![int(), string()], string()),
            "format_utc" => sig(vec![int(), string()], string()),
            "parse" => sig(vec![string(), string()], result_of(int(), string())),
            "zone_names" => sig(vec![], vec_of(string())),
            "local_zone" => sig(vec![], string()),
            "offset" => sig(vec![int(), string()], result_of(int(), string())),
            "format_zone" => sig(
                vec![int(), string(), string()],
                result_of(string(), string()),
            ),
            "parse_zone" => sig(
                vec![string(), string(), string()],
                result_of(int(), string()),
            ),
            "sleep" => sig(vec![int()], void()),
            _ => return None,
        });
    }
    if module == "std/sys" {
        return Some(match name {
            "env_get" => sig(vec![string()], opt_of(string())),
            "args" => sig(vec![], vec_of(string())),
            "exit" => sig(vec![int()], void()),
            "platform" => sig(vec![], string()),
            "heap_usage" | "gc" => sig(vec![], map_of(string(), int())),
            _ => return None,
        });
    }
    if module == "std/fs" {
        return Some(match name {
            "read_file" => sig(vec![string()], result_of(string(), string())),
            "write_file" => sig(vec![string(), string()], result_of(int(), string())),
            "exists" => sig(vec![string()], boolean()),
            "make_dir" => sig(vec![string()], result_of(boolean(), string())),
            "remove_file" => sig(vec![string()], result_of(boolean(), string())),
            "copy_file" => sig(vec![string(), string()], result_of(int(), string())),
            "file_size" => sig(vec![string()], result_of(int(), string())),
            "list_dir" => sig(vec![string()], result_of(vec_of(string()), string())),
            "list_dir_all" => sig(vec![string()], result_of(vec_of(string()), string())),
            "append_file" => sig(vec![string(), string()], result_of(int(), string())),
            "remove_dir" => sig(vec![string()], result_of(boolean(), string())),
            "rename" => sig(vec![string(), string()], result_of(boolean(), string())),
            "metadata" => sig(vec![string()], result_of(fs_metadata(), string())),
            "temp_file" | "temp_dir" => sig(vec![string()], result_of(string(), string())),
            _ => return None,
        });
    }
    if module == "std/db" {
        return Some(match name {
            "open" => sig(vec![string()], result_of(db_conn(), string())),
            "table" => sig(vec![db_conn(), string()], db_query()),
            "driver" => sig(vec![db_conn()], string()),
            "has_many" => sig(
                vec![db_conn(), string(), string(), any()],
                result_of(vec_of(any()), string()),
            ),
            "belongs_to" => sig(
                vec![db_conn(), string(), any()],
                result_of(opt_of(map_of(string(), any())), string()),
            ),
            "belongs_to_by" => sig(
                vec![db_conn(), string(), string(), any()],
                result_of(opt_of(map_of(string(), any())), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/log" {
        return Some(match name {
            "debug" | "info" | "warn" | "error" => sig(vec![string()], void()),
            "set_level" | "set_format" => sig(vec![string()], void()),
            "set_file" => sig(vec![string()], result_of(boolean(), string())),
            "set_rotate" => sig(vec![int(), int()], void()),
            _ => return None,
        });
    }
    if module == "std/redis" {
        return Some(match name {
            "connect" => sig(vec![string()], result_of(redis_conn(), string())),
            "lock" => sig(
                vec![redis_conn(), string(), int()],
                result_of(opt_of(string()), string()),
            ),
            "unlock" => sig(
                vec![redis_conn(), string(), string()],
                result_of(boolean(), string()),
            ),
            "rate_limit" => sig(
                vec![redis_conn(), string(), int(), int()],
                result_of(boolean(), string()),
            ),
            _ => return None,
        });
    }
    if module == "std/llm" {
        return Some(match name {
            "new_client" => sig(vec![named("llm.config")], named("llm.client")),
            _ => return None,
        });
    }
    if module == "rust/serde_json" {
        return Some(match name {
            "to_string" | "to_string_pretty" => sig(vec![any()], result_of(string(), string())),
            "from_str" => sig(vec![string()], result_of(any(), string())),
            _ => return None,
        });
    }
    // 外部原生包：签名来自随包分发的 `.rp.decl`（见 `native` 模块）。
    crate::native::native_package_function(module, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULES: &[&str] = &[
        "std/http",
        "std/crypto",
        "std/net",
        "std/ws",
        "std/xml",
        "std/encode",
        "std/path",
        "std/process",
        "std/regex",
        "std/rand",
        "std/uuid",
        "std/compress",
        "std/zip",
        "std/mail",
        "std/reflect",
        "std/session",
        "std/validate",
        "std/template",
        "std/jwt",
        "std/str",
        "std/vec",
        "std/map",
        "std/cli",
        "std/cache",
        "std/queue",
        "std/task",
        "std/sync",
        "std/dev",
        "std/json",
        "std/time",
        "std/sys",
        "std/fs",
        "std/db",
        "std/log",
        "std/redis",
        "std/llm",
        "rust/serde_json",
    ];

    #[test]
    fn listed_builtins_resolve() {
        for module in MODULES {
            for name in builtin_module_names(module) {
                assert!(
                    builtin_function(module, name).is_some(),
                    "{} has no signature in the completion list",
                    name
                );
            }
        }
    }

    #[test]
    fn every_builtin_module_is_listed() {
        for module in MODULES {
            assert!(
                !builtin_module_names(module).is_empty(),
                "{} is missing from the completion list",
                module
            );
        }
        assert!(builtin_module_names("std/unknown").is_empty());
    }
}

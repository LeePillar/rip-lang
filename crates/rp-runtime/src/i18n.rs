//! 运行时提示语言（english / chinese）。
//!
//! 语言只影响运行时**自身**产生的提示与错误；Rip 程序通过 `dev.log` 等
//! 打印的内容原样输出，`result` 里的错误值也保持原样。

/// 运行时提示语言，由 `rip.toml` 的 `[runtime] log` 配置。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Language {
    /// 英文提示（默认）。
    #[default]
    English,
    /// 中文提示。
    Chinese,
}

impl Language {
    /// 解析配置值：`english` / `en` 或 `chinese` / `zh` / `中文`。
    pub fn parse(name: &str) -> Option<Language> {
        match name.trim().to_ascii_lowercase().as_str() {
            "english" | "en" => Some(Language::English),
            "chinese" | "zh" | "zh-cn" | "中文" => Some(Language::Chinese),
            _ => None,
        }
    }

    /// 配置字符串形式，用于 `rip build` 内嵌配置。
    pub fn name(self) -> &'static str {
        match self {
            Language::English => "english",
            Language::Chinese => "chinese",
        }
    }

    pub fn is_chinese(self) -> bool {
        self == Language::Chinese
    }
}

/// 把运行时英文提示翻成中文；没有对应译文时原样返回。
pub(crate) fn localize(language: Language, message: &str) -> String {
    if language != Language::Chinese {
        return message.to_string();
    }
    if let Some(chinese) = exact(message) {
        return chinese.to_string();
    }
    let mut text = message.to_string();
    for rule in RULES {
        if message.starts_with(rule.prefix)
            && message.ends_with(rule.suffix)
            && message.len() >= rule.prefix.len() + rule.suffix.len()
        {
            let start = rule.prefix.len();
            let end = message.len() - rule.suffix.len();
            text = format!(
                "{}{}{}",
                rule.zh_prefix,
                &message[start..end],
                rule.zh_suffix
            );
            break;
        }
    }
    for (english, chinese) in MIDS {
        if text.contains(english) {
            text = text.replacen(english, chinese, 1);
        }
    }
    text
}

struct Rule {
    prefix: &'static str,
    suffix: &'static str,
    zh_prefix: &'static str,
    zh_suffix: &'static str,
}

const fn rule(prefix: &'static str, suffix: &'static str, zh_prefix: &'static str, zh_suffix: &'static str) -> Rule {
    Rule {
        prefix,
        suffix,
        zh_prefix,
        zh_suffix,
    }
}

/// 带动态值的提示模板：仅替换首尾，中间原样保留。
const RULES: &[Rule] = &[
    rule(
        "failed to initialize module variable '",
        "'",
        "初始化模块变量 '",
        "' 失败",
    ),
    rule(
        "function '",
        " ended without returning a value",
        "函数 '",
        "' 未返回值就结束了",
    ),
    rule("for-in requires vec or map, found ", "", "for-in 需要 vec 或 map，实际为 ", ""),
    rule("condition must be bool, found ", "", "条件必须是 bool，实际为 ", ""),
    rule("! requires bool, found ", "", "! 需要 bool，实际为 ", ""),
    rule(
        "unary - requires number, found ",
        "",
        "一元 - 需要数字，实际为 ",
        "",
    ),
    rule("'?' requires result, found ", "", "'?' 需要 result，实际为 ", ""),
    rule("cannot call value ", "", "无法调用值 ", ""),
    rule("unknown variable '", "'", "未知变量 '", "'"),
    rule("unknown identifier '", "'", "未知标识符 '", "'"),
    rule("no field named '", "'", "没有名为 '", "' 的字段"),
    rule("index ", " out of bounds", "索引 ", " 越界"),
    rule("vec has no field '", "'", "vec 没有字段 '", "'"),
    rule("type ", " has no fields", "类型 ", " 没有字段"),
    rule("operator does not support ", "", "运算符不支持 ", ""),
    rule("cannot compare ", "", "无法比较 ", ""),
    rule("package \"", "\" is not available in this build", "包 \"", "\" 在此版本中不可用"),
    rule("cannot bind ", "", "无法绑定 ", ""),
    rule("cannot open certificate ", "", "无法打开证书 ", ""),
    rule("cannot read certificate ", "", "无法读取证书 ", ""),
    rule(
        "certificate ",
        " has no PEM certificate",
        "证书 ",
        " 中没有 PEM 证书",
    ),
    rule("cannot open private key ", "", "无法打开私钥 ", ""),
    rule("cannot read private key ", "", "无法读取私钥 ", ""),
    rule("private key ", " has no PEM key", "私钥 ", " 中没有 PEM 密钥"),
    rule(
        "invalid TLS certificate or key: ",
        "",
        "TLS 证书或私钥无效：",
        "",
    ),
    rule("cannot read random source: ", "", "无法读取系统随机源：", ""),
    rule("invalid port '", "'", "无效端口 '", "'"),
    rule("unsupported WebSocket opcode ", "", "不支持的 WebSocket opcode ", ""),
    rule("WebSocket handshake failed: ", "", "WebSocket 握手失败：", ""),
];

/// 固定提示的完整译文。
fn exact(message: &str) -> Option<&'static str> {
    Some(match message {
        "assertion failed" => "断言失败",
        "break/continue outside loop" => "break/continue 不在循环内",
        "closure ended without returning a value" => "闭包未返回值就结束了",
        "no match arm matched value" => "没有匹配任何分支的值",
        "no 'main' function found in entry module" => "入口模块中没有 main 函数",
        "unexpected control flow" => "意外的控制流",
        "integer overflow" => "整数溢出",
        "division by zero" => "除数为零",
        "modulo by zero" => "取模除数为零",
        "invalid assignment target" => "无效的赋值目标",
        "cannot assign to a field of this value" => "无法给该值的字段赋值",
        "map literal requires key = value entries" => "map 字面量需要 key = value 条目",
        "ary literal requires named fields: [field = value, ...]" => {
            "ary 字面量需要命名字段：[field = value, ...]"
        }
        "handler is not callable" => "处理器不可调用",
        "serve expects a handler function" => "serve 需要一个处理函数",
        "router expects vec<http.route>" => "router 需要 vec<http.route>",
        "router expects http.route values" => "router 需要 http.route 值",
        "route handler must be a function" => "路由处理器必须是函数",
        "route handler is not callable" => "路由处理器不可调用",
        "static handler expects http.request" => "static 处理器需要 http.request",
        "router handler expects an http.request" => "router 处理器需要 http.request",
        "port must be 0..65535" => "端口必须是 0..65535",
        "invalid response" => "无效的响应",
        "invalid hmac key" => "无效的 HMAC 密钥",
        "cannot initialize AES-256-GCM" => "无法初始化 AES-256-GCM",
        "rsa_generate bits must be 1024..=8192 and a multiple of 8" => {
            "rsa_generate 的位数必须是 1024..=8192 且为 8 的倍数"
        }
        "task.spawn expects one function name" => "task.spawn 需要一个函数名",
        "task.sleep expects one number" => "task.sleep 需要一个数字",
        "task.sleep out of range (0..86400000)" => "task.sleep 超出范围 (0..86400000)",
        "HTTPS requires both --cert and --key" => "HTTPS 需要同时提供 --cert 和 --key",
        "base must be 2..36" => "进制必须在 2..36",
        "exponent must be non-negative" => "指数不能为负数",
        "clamp lower bound exceeds upper bound" => "clamp 下界大于上界",
        "width must be non-negative" => "宽度不能为负数",
        "repeat count must be non-negative" => "重复次数不能为负数",
        "join expects vec<string>" => "join 需要 vec<string>",
        "insert index out of bounds" => "插入索引越界",
        "remove index out of bounds" => "删除索引越界",
        "slice out of bounds" => "切片越界",
        "concat expects vec" => "concat 需要 vec",
        "flatten expects vec<vec<T>>" => "flatten 需要 vec<vec<T>>",
        "chunk size must be positive" => "chunk 大小必须为正数",
        "zip expects vec" => "zip 需要 vec",
        "merge expects map" => "merge 需要 map",
        "called unwrap on none" => "对 none 调用 unwrap",
        "chat expects messages vec" => "chat 需要消息 vec",
        "db.open expects a non-empty DSN" => "db.open 需要非空 DSN",
        "db.driver expects a db.conn as argument 1" => "db.driver 的第 1 个参数需要 db.conn",
        "db.table expects a db.conn as argument 1" => "db.table 的第 1 个参数需要 db.conn",
        "table name cannot be empty" => "表名不能为空",
        "invalid database connection" => "无效的数据库连接",
        "null can only be used with '=' or '!='" => "null 只能与 '=' 或 '!=' 一起使用",
        "where_in expects a vec as argument 2" => "where_in 的第 2 个参数需要 vec",
        "order direction must be 'asc' or 'desc'" => "排序方向必须是 'asc' 或 'desc'",
        "limit must be positive" => "limit 必须为正数",
        "offset cannot be negative" => "offset 不能为负数",
        "join condition cannot be empty" => "join 条件不能为空",
        "insert/update expects a map" => "insert/update 需要 map",
        "connection is closed" => "连接已关闭",
        "connection closed" => "连接已关闭",
        "listener is closed" => "监听器已关闭",
        "socket is closed" => "套接字已关闭",
        "timeout" => "超时",
        "connection closed during handshake" => "握手期间连接已关闭",
        "handshake must be a GET request" => "WebSocket 握手必须是 GET 请求",
        "missing Sec-WebSocket-Key" => "WebSocket 握手缺少 Sec-WebSocket-Key",
        "handshake is too large" => "WebSocket 握手请求过大",
        "handshake contains invalid UTF-8" => "WebSocket 握手请求包含无效 UTF-8",
        "WebSocket frame is too large (max 16 MiB)" => {
            "WebSocket 帧过大（上限 16 MiB）"
        }
        "wss:// is not supported; terminate TLS in front of a ws:// server" => {
            "不支持 wss://；请在前置 TLS 代理后使用 ws://"
        }
        "WebSocket URL must start with ws://" => "WebSocket URL 必须以 ws:// 开头",
        "WebSocket URL has no host" => "WebSocket URL 缺少主机名",
        "invalid IPv6 WebSocket URL" => "无效的 IPv6 WebSocket URL",
        "tcp_connect port must be 0..65535" => "tcp_connect 端口必须是 0..65535",
        "tcp_recv max bytes must be 1..=16777216" => {
            "tcp_recv 最大字节数必须是 1..=16777216"
        }
        "tcp_recv_line max bytes must be 1..=16777216" => {
            "tcp_recv_line 最大字节数必须是 1..=16777216"
        }
        "tcp_listen port must be 0..65535" => "tcp_listen 端口必须是 0..65535",
        "udp_bind port must be 0..65535" => "udp_bind 端口必须是 0..65535",
        "udp_send port must be 0..65535" => "udp_send 端口必须是 0..65535",
        "udp_recv max bytes must be 1..=65535" => {
            "udp_recv 最大字节数必须是 1..=65535"
        }
        "ws.serve port must be 0..65535" => "ws.serve 端口必须是 0..65535",
        "ws.serve expects a handler function" => "ws.serve 需要一个处理函数",
        "ws.serve handler is not callable" => "ws.serve 处理器不可调用",
        "csv_encode expects vec<vec<string>>" => "csv_encode 需要 vec<vec<string>>",
        "query_stringify expects map<string, string>" => {
            "query_stringify 需要 map<string, string>"
        }
        "xml.stringify expects xml.node" => "xml.stringify 需要 xml.node",
        "xml.pretty expects xml.node" => "xml.pretty 需要 xml.node",
        "xml.name expects xml.node" => "xml.name 需要 xml.node",
        "xml.text expects xml.node" => "xml.text 需要 xml.node",
        "xml.attrs expects xml.node" => "xml.attrs 需要 xml.node",
        "xml.children expects xml.node" => "xml.children 需要 xml.node",
        "xml.find expects (xml.node, string)" => "xml.find 需要 (xml.node, string)",
        "xml.find_all expects (xml.node, string)" => "xml.find_all 需要 (xml.node, string)",
        "new_client expects llm.config" => "new_client 需要 llm.config",
        "save_file expects (string, http.file)" => "save_file 需要 (string, http.file)",
        _ => return None,
    })
}

/// 句中固定短语。
const MIDS: &[(&str, &str)] = &[
    (" has no function ", " 没有函数 "),
    (" expects ", " 需要 "),
    (" must be ", " 必须是 "),
    (" and ", " 和 "),
];

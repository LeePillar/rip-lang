//! 出站 HTTP/HTTPS 客户端，供 `http.fetch` 调用第三方服务（云存储、Webhook 等）。
//!
//! 运行时只负责传输，目标地址与目录完全由 Rip 代码决定。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::builtins::{is_header_name, is_header_value};
use crate::tls::install_crypto_provider;

/// 解析后的 URL：仅支持 http / https。
struct Url {
    secure: bool,
    host: String,
    port: u16,
    target: String,
    host_header: String,
}

fn parse_url(url: &str) -> Result<Url, String> {
    // 拒绝 CR/LF/NUL：否则会经请求行或 Host 头注入额外的请求头（请求走私面）。
    if url.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0) {
        return Err("URL contains control characters".to_string());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("invalid URL '{}': missing scheme", url))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        other => return Err(format!("unsupported URL scheme '{}'", other)),
    };
    let split = rest
        .find(|ch| ch == '/' || ch == '?' || ch == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..split];
    let tail = &rest[split..];
    if authority.is_empty() {
        return Err(format!("invalid URL '{}': missing host", url));
    }
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped
            .find(']')
            .ok_or_else(|| format!("invalid IPv6 URL '{}'", url))?;
        let host = &stripped[..end];
        let rest = &stripped[end + 1..];
        let port = match rest.strip_prefix(':') {
            Some(value) => value
                .parse()
                .map_err(|_| format!("invalid port in URL '{}'", url))?,
            None => default_port(secure),
        };
        (host.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, value)) if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) => {
                let port = value
                    .parse()
                    .map_err(|_| format!("invalid port in URL '{}'", url))?;
                (host.to_string(), port)
            }
            _ => (authority.to_string(), default_port(secure)),
        }
    };
    let mut target = if tail.is_empty() {
        "/".to_string()
    } else if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{}", tail)
    };
    if let Some(index) = target.find('#') {
        target.truncate(index);
    }
    let host_header = if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };
    Ok(Url {
        secure,
        host,
        port,
        target,
        host_header,
    })
}

fn default_port(secure: bool) -> u16 {
    if secure {
        443
    } else {
        80
    }
}

/// 执行一次 HTTP 请求，返回 `(status, headers, body)`。
///
/// `timeout` 是调用方剩余的处理预算；None 表示使用缺省超时。
pub(crate) fn fetch(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<(i64, Vec<(String, String)>, String), String> {
    let (status, received, bytes) = fetch_bytes(method, url, headers, body, timeout)?;
    Ok((
        status,
        received,
        String::from_utf8_lossy(&bytes).to_string(),
    ))
}

/// 出站连接：明文或 TLS，二者共用读写接口。
enum OutboundStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl OutboundStream {
    fn socket(&self) -> &TcpStream {
        match self {
            OutboundStream::Plain(stream) => stream,
            OutboundStream::Tls(stream) => &stream.sock,
        }
    }

    fn set_timeouts(&self, timeout: Duration) -> std::io::Result<()> {
        self.socket().set_read_timeout(Some(timeout))?;
        self.socket().set_write_timeout(Some(timeout))
    }
}

impl Read for OutboundStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            OutboundStream::Plain(stream) => stream.read(buffer),
            OutboundStream::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for OutboundStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            OutboundStream::Plain(stream) => stream.write(buffer),
            OutboundStream::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            OutboundStream::Plain(stream) => stream.flush(),
            OutboundStream::Tls(stream) => stream.flush(),
        }
    }
}

/// 每个目标（scheme + host + port）最多保留的空闲连接数。
const MAX_IDLE_PER_HOST: usize = 8;
/// 进程内空闲连接总数上限。
const MAX_IDLE_TOTAL: usize = 64;
/// 空闲连接最长保留时间；超过后下次借出时直接丢弃。
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    secure: bool,
    host: String,
    port: u16,
}

struct Pooled {
    stream: OutboundStream,
    since: Instant,
}

#[derive(Default)]
struct PoolState {
    idle: HashMap<PoolKey, Vec<Pooled>>,
    total: usize,
}

/// 进程级出站连接池：HTTP 工作线程共用，避免每次调用都做 TCP/TLS 握手。
fn pool() -> &'static Mutex<PoolState> {
    static POOL: OnceLock<Mutex<PoolState>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(PoolState::default()))
}

/// `RIP_NO_HTTP_POOL=1` 时退回"每次新建连接"，便于排查连接复用问题。
fn pooling_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RIP_NO_HTTP_POOL").is_none())
}

fn checkout(key: &PoolKey) -> Option<OutboundStream> {
    let mut state = pool().lock().ok()?;
    let mut list = state.idle.remove(key)?;
    while let Some(entry) = list.pop() {
        state.total = state.total.saturating_sub(1);
        if entry.since.elapsed() <= IDLE_TIMEOUT {
            if !list.is_empty() {
                state.idle.insert(key.clone(), list);
            }
            return Some(entry.stream);
        }
        // 过期连接直接丢弃，继续找下一条。
    }
    None
}

fn checkin(key: PoolKey, stream: OutboundStream) {
    if !pooling_enabled() {
        return;
    }
    let Ok(mut state) = pool().lock() else {
        return;
    };
    if state.total >= MAX_IDLE_TOTAL {
        return;
    }
    let list = state.idle.entry(key).or_default();
    if list.len() >= MAX_IDLE_PER_HOST {
        return;
    }
    list.push(Pooled {
        stream,
        since: Instant::now(),
    });
    state.total += 1;
}

fn connect(
    url: &Url,
    connect_budget: Duration,
    io_budget: Duration,
) -> Result<OutboundStream, String> {
    let address = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {}: {}", url.host, error))?
        .next()
        .ok_or_else(|| format!("cannot resolve {}", url.host))?;
    // SSRF 防护（opt-in）：`RIP_HTTP_BLOCK_PRIVATE=1` 时拒绝回环/私网/链路本地（含云元数据
    // 169.254.169.254）等内网目标。默认不限制：出站抓取是语言内置能力，目标由 Rip 代码决定。
    if block_private_enabled() && is_private_addr(&address) {
        return Err(format!(
            "private address blocked by RIP_HTTP_BLOCK_PRIVATE: {}",
            address.ip()
        ));
    }
    let stream = TcpStream::connect_timeout(&address, connect_budget)
        .map_err(|error| format!("cannot connect to {}: {}", url.host_header, error))?;
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(io_budget))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(io_budget))
        .map_err(|error| error.to_string())?;
    if url.secure {
        let config = client_config()?;
        let name = rustls::pki_types::ServerName::try_from(url.host.clone())
            .map_err(|_| format!("invalid TLS server name '{}'", url.host))?;
        let connection =
            rustls::ClientConnection::new(config, name).map_err(|error| error.to_string())?;
        Ok(OutboundStream::Tls(Box::new(rustls::StreamOwned::new(
            connection, stream,
        ))))
    } else {
        Ok(OutboundStream::Plain(stream))
    }
}

/// `RIP_HTTP_BLOCK_PRIVATE` 开关：进程内只读一次。
fn block_private_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RIP_HTTP_BLOCK_PRIVATE").is_some_and(|value| value != "0")
    })
}

/// 是否属于内网/保留地址（IPv4 与 IPv6，含映射到 v4 的 v6 地址）。
fn is_private_addr(address: &std::net::SocketAddr) -> bool {
    use std::net::IpAddr;
    match address.ip() {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_private_addr(&std::net::SocketAddr::new(
                    IpAddr::V4(mapped),
                    address.port(),
                ));
            }
            let first = v6.segments()[0];
            // fc00::/7 唯一本地；fe80::/10 链路本地；ff00::/8 组播。
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || (first & 0xff00) == 0xff00
        }
    }
}

/// 一次请求/响应往返；返回的连接是否可复用由 `read_response` 判断。
fn exchange(
    stream: &mut OutboundStream,
    request: &[u8],
    head_only: bool,
) -> Result<ResponseRead, String> {
    stream
        .write_all(request)
        .map_err(|error| format!("cannot send request: {}", error))?;
    stream.flush().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    read_response(&mut reader, head_only)
}

/// 与 `fetch` 相同，但返回原始字节，供下载二进制对象（图片、音视频）使用。
pub(crate) fn fetch_bytes(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<(i64, Vec<(String, String)>, Vec<u8>), String> {
    install_crypto_provider();
    let url = parse_url(url)?;
    // 连接与读写都不得超过调用方剩余预算，避免一个慢上游拖死整个处理函数。
    let connect_budget = timeout
        .map(|budget| budget.min(Duration::from_secs(30)))
        .unwrap_or(Duration::from_secs(30));
    let io_budget = timeout
        .map(|budget| budget.min(Duration::from_secs(60)))
        .unwrap_or(Duration::from_secs(60));
    if connect_budget.is_zero() || io_budget.is_zero() {
        return Err("request deadline exceeded before sending".to_string());
    }
    let key = PoolKey {
        secure: url.secure,
        host: url.host.clone(),
        port: url.port,
    };
    let keep_alive = pooling_enabled();
    let request = build_request(method, &url, headers, body, keep_alive)?;
    let head_only = method.trim().eq_ignore_ascii_case("HEAD");
    // 只有幂等请求允许在复用连接失效时重试，避免 POST 被重复执行。
    let idempotent = matches!(
        method.trim().to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS"
    );

    let mut retried = false;
    loop {
        let pooled = if keep_alive { checkout(&key) } else { None };
        let reused = pooled.is_some();
        let mut stream = match pooled {
            Some(stream) => stream,
            None => connect(&url, connect_budget, io_budget)?,
        };
        stream
            .set_timeouts(io_budget)
            .map_err(|error| error.to_string())?;
        match exchange(&mut stream, &request, head_only) {
            Ok(response) => {
                if keep_alive && response.reusable {
                    checkin(key.clone(), stream);
                }
                return Ok((response.status, response.headers, response.body));
            }
            Err(error) => {
                // 复用的空闲连接可能已被对端关闭：丢弃后换新连接重试一次。
                drop(stream);
                if reused && idempotent && !retried {
                    retried = true;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// 以文件内容作为请求体发起请求（二进制安全），用于上传到对象存储等。
pub(crate) fn fetch_file(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    path: &str,
    timeout: Option<Duration>,
) -> Result<(i64, Vec<(String, String)>, String), String> {
    let body = std::fs::read(path).map_err(|error| format!("cannot read '{}': {}", path, error))?;
    fetch(method, url, headers, &body, timeout)
}

/// 客户端 TLS 配置只构建一次：共享配置才能复用会话票据（session ticket）。
fn client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(Arc::clone(config));
    }
    let mut roots = rustls::RootCertStore::empty();
    for anchor in webpki_roots::TLS_SERVER_ROOTS.iter() {
        roots.roots.push(anchor.clone());
    }
    let config = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let _ = CONFIG.set(Arc::clone(&config));
    Ok(config)
}

fn build_request(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    keep_alive: bool,
) -> Result<Vec<u8>, String> {
    let method = method.trim().to_ascii_uppercase();
    if !is_header_name(&method) {
        return Err(format!("invalid HTTP method '{}'", method.trim()));
    }
    let mut request = format!("{} {} HTTP/1.1\r\n", method, url.target);
    request.push_str(&format!("Host: {}\r\n", url.host_header));
    request.push_str(if keep_alive {
        "Connection: keep-alive\r\n"
    } else {
        "Connection: close\r\n"
    });
    request.push_str("Accept: */*\r\n");
    let mut has_content_type = false;
    for (name, value) in headers {
        // 拒绝含 CR/LF/NUL 或非法 token 的头，防止用户可控值经请求头注入额外请求行。
        if !is_header_name(name) || !is_header_value(value) {
            return Err(format!("invalid request header '{}'", name));
        }
        let lower = name.to_ascii_lowercase();
        if lower == "host" || lower == "connection" || lower == "content-length" {
            continue;
        }
        if lower == "content-type" {
            has_content_type = true;
        }
        request.push_str(&format!("{}: {}\r\n", name, value));
    }
    let has_body = !body.is_empty() || matches!(method.as_str(), "POST" | "PUT" | "PATCH");
    if has_body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        if !has_content_type {
            request.push_str("Content-Type: application/octet-stream\r\n");
        }
    }
    request.push_str("\r\n");
    let mut bytes = request.into_bytes();
    if has_body {
        bytes.extend_from_slice(body);
    }
    Ok(bytes)
}

/// 单次响应体上限（256 MiB）：防病态/被攻陷上游用超大 Content-Length 或无限 chunk 耗尽内存。
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// 一次响应读取结果；`reusable` 表示这条连接能否放回池里。
struct ResponseRead {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    reusable: bool,
}

fn read_response(
    reader: &mut BufReader<&mut OutboundStream>,
    head_only: bool,
) -> Result<ResponseRead, String> {
    let status_line = read_line(reader)?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default().to_string();
    if !version.starts_with("HTTP/") {
        return Err(format!("invalid HTTP response: {}", status_line.trim()));
    }
    let status = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| format!("invalid HTTP status line: {}", status_line.trim()))?;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid HTTP header: {}", line.trim()))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.to_ascii_lowercase().contains("chunked")
    });
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let connection_close = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection") && value.to_ascii_lowercase().contains("close")
    });
    let connection_keep_alive = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection") && value.to_ascii_lowercase().contains("keep-alive")
    });
    // HEAD 响应没有 body，即使带 Content-Length 也不能读，否则会阻塞到超时。
    let body = if head_only {
        Vec::new()
    } else if chunked {
        read_chunked(reader)?
    } else if let Some(length) = content_length {
        if length > MAX_RESPONSE_BYTES {
            return Err("response body exceeds limit".to_string());
        }
        let mut buffer = vec![0u8; length];
        reader
            .read_exact(&mut buffer)
            .map_err(|error| format!("cannot read response body: {}", error))?;
        buffer
    } else {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut chunk)
                .map_err(|error| format!("cannot read response body: {}", error))?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > MAX_RESPONSE_BYTES {
                return Err("response body exceeds limit".to_string());
            }
        }
        buffer
    };
    // 只有"长度已知 + 读完后缓冲区没有残留 + 对端未要求关闭"才可复用。
    let length_known = head_only || chunked || content_length.is_some();
    let reusable = length_known
        && reader.buffer().is_empty()
        && !connection_close
        && (version.eq_ignore_ascii_case("HTTP/1.1") || connection_keep_alive);
    Ok(ResponseRead {
        status,
        headers,
        body,
        reusable,
    })
}

fn read_line<R: BufRead>(reader: &mut R) -> Result<String, String> {
    let mut line = String::new();
    let mut bytes = Vec::new();
    reader
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("cannot read response: {}", error))?;
    if bytes.is_empty() {
        return Err("connection closed before response".to_string());
    }
    line.push_str(&String::from_utf8_lossy(&bytes));
    Ok(line)
}

fn read_chunked<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader)?;
        let size_text = line.trim().split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("invalid chunk size '{}'", size_text))?;
        if size == 0 {
            // 丢弃 trailer。
            loop {
                let trailer = read_line(reader)?;
                if trailer == "\r\n" || trailer == "\n" || trailer.is_empty() {
                    break;
                }
            }
            break;
        }
        if body.len().saturating_add(size) > MAX_RESPONSE_BYTES {
            return Err("response body exceeds limit".to_string());
        }
        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| format!("cannot read chunk: {}", error))?;
        body.extend_from_slice(&chunk);
        let _ = read_line(reader)?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// 读掉一个请求的头部；返回 false 表示对端已关闭。
    fn read_request_head(reader: &mut BufReader<TcpStream>) -> bool {
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return false;
            }
            if line == "\r\n" || line == "\n" {
                return true;
            }
        }
    }

    #[test]
    fn build_request_controls_connection_header() {
        let url = parse_url("http://example.com/x").expect("url");
        let keep =
            String::from_utf8(build_request("GET", &url, &[], &[], true).expect("build")).expect("utf8");
        assert!(keep.contains("Connection: keep-alive"));
        let close = String::from_utf8(build_request("GET", &url, &[], &[], false).expect("build"))
            .expect("utf8");
        assert!(close.contains("Connection: close"));
    }

    #[test]
    fn build_request_rejects_header_injection() {
        let url = parse_url("http://example.com/x").expect("url");
        let evil = vec![("X-Note".to_string(), "ok\r\nInjected: yes".to_string())];
        assert!(build_request("GET", &url, &evil, &[], false).is_err());
        // URL 含控制字符同样拒绝。
        assert!(parse_url("http://example.com/\r\nX: 1").is_err());
    }

    /// 同一目标连续两次请求应当复用同一条 TCP 连接。
    #[test]
    fn reuses_keep_alive_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            // 只 accept 一次：如果客户端没有复用连接，第二次请求会没人处理而超时。
            let (stream, _) = listener.accept().expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            for _ in 0..2 {
                assert!(read_request_head(&mut reader), "expected two requests on one connection");
                writer
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                    )
                    .expect("write");
                writer.flush().expect("flush");
            }
        });

        let url = format!("http://127.0.0.1:{}/ping", port);
        let first = fetch_bytes("GET", &url, &[], &[], Some(Duration::from_secs(2)))
            .expect("first request");
        assert_eq!(first.0, 200);
        assert_eq!(first.2.as_slice(), b"ok");
        let second = fetch_bytes("GET", &url, &[], &[], Some(Duration::from_secs(2)))
            .expect("second request");
        assert_eq!(second.0, 200);
        assert_eq!(second.2.as_slice(), b"ok");
        server.join().expect("server thread");
    }
}

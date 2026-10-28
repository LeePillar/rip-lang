//! Redis 客户端（RESP2）。
//!
//! 每个解释器（线程）各自持有连接，不做跨线程共享；连接按 id 登记在
//! `RedisState` 中，供 `redis.conn` 的方法调用。实现覆盖会话缓存、分布式锁、
//! 计数限流所需的常用命令。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::rt::Runtime;
use crate::value::{FlowResult, Value};

/// 连接 Redis 的超时；读写超时防止网络故障把请求线程挂死。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum RedisStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for RedisStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            RedisStream::Plain(stream) => stream.read(buf),
            RedisStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for RedisStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            RedisStream::Plain(stream) => stream.write(buf),
            RedisStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            RedisStream::Plain(stream) => stream.flush(),
            RedisStream::Tls(stream) => stream.flush(),
        }
    }
}

pub(crate) struct RedisConn {
    stream: RedisStream,
    buffer: Vec<u8>,
    position: usize,
    pub host: String,
    pub port: u16,
    pub db: i64,
    /// 建立连接时使用的原始 URL（trim 后），同时作为进程级空闲池的 key。
    url: String,
    /// 读写失败后连接不可信（回复可能只读了一半），不再回池。
    broken: bool,
}

#[derive(Default)]
pub struct RedisState {
    pub(crate) conns: HashMap<i64, RedisConn>,
    next_id: i64,
}

impl RedisState {
    pub fn new() -> Self {
        Self {
            conns: HashMap::new(),
            next_id: 1,
        }
    }

    /// 归还本运行时打开的全部 Redis 连接（任务复用前调用）：
    /// 干净的连接回进程级空闲池供后续 `redis.connect` 复用。
    pub(crate) fn reset(&mut self) {
        for (_, conn) in self.conns.drain() {
            pool_give(conn);
        }
    }
}

/// 每个 URL 允许缓存的空闲连接上限。
const POOL_MAX_IDLE: usize = 8;
/// 空闲超过该时长的连接在下次借出时丢弃，避免用到被服务端断开的连接。
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// bulk 回复长度上限（64 MiB）。
const MAX_BULK_BYTES: i64 = 64 * 1024 * 1024;
/// array 回复元素数上限（正常服务端受字节量自然约束，此值只防病态声明）。
const MAX_ARRAY_ITEMS: i64 = 16 * 1024 * 1024;
/// 嵌套深度上限。
const MAX_REPLY_DEPTH: usize = 64;

/// 进程级 Redis 空闲连接池：`redis.connect` 同 URL 时透明复用，
/// 与 `db.rs` 的 SQL 池同思路；借出/归还即时完成，不限制借出总数。
static POOL: OnceLock<Mutex<HashMap<String, Vec<(RedisConn, Instant)>>>> = OnceLock::new();

fn pool_lock() -> std::sync::MutexGuard<'static, HashMap<String, Vec<(RedisConn, Instant)>>> {
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// 从池中借出一条到该 URL 的连接；过期与验活失败的连接直接丢弃。
fn pool_take(url: &str) -> Option<RedisConn> {
    loop {
        let candidate = {
            let mut pool = pool_lock();
            match pool.get_mut(url) {
                Some(bucket) => {
                    let mut found = None;
                    while let Some((conn, returned)) = bucket.pop() {
                        if returned.elapsed() <= POOL_IDLE_TIMEOUT {
                            found = Some(conn);
                            break;
                        }
                    }
                    found
                }
                None => None,
            }
        };
        let Some(mut conn) = candidate else {
            return None;
        };
        // 验活在锁外进行：服务端可能已断开，PING 失败则继续找下一条。
        if matches!(conn.command(&["PING"]), Ok(Reply::Status(_))) {
            return Some(conn);
        }
    }
}

/// 归还连接到池中；脏连接（读写失败或回复未读完）直接丢弃。
fn pool_give(conn: RedisConn) {
    if conn.broken || conn.position < conn.buffer.len() {
        return;
    }
    let mut pool = pool_lock();
    let bucket = pool.entry(conn.url.clone()).or_default();
    if bucket.len() >= POOL_MAX_IDLE {
        return;
    }
    let mut conn = conn;
    conn.buffer.clear();
    conn.position = 0;
    bucket.push((conn, Instant::now()));
}

pub(crate) enum Reply {
    Status(String),
    Error(String),
    Int(i64),
    Bulk(Option<String>),
    Array(Option<Vec<Reply>>),
}

struct RedisUrl {
    tls: bool,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    db: Option<i64>,
}

fn parse_url(url: &str) -> Result<RedisUrl, String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("rediss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("redis://") {
        (false, rest)
    } else {
        return Err(format!(
            "redis url must start with redis:// or rediss://: {}",
            crate::db::redact_dsn(url)
        ));
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((credentials, host_port)) => (Some(credentials), host_port),
        None => (None, authority),
    };
    let (username, password) = match credentials {
        Some(credentials) => match credentials.split_once(':') {
            Some((user, pass)) => (
                if user.is_empty() { None } else { Some(user.to_string()) },
                Some(pass.to_string()),
            ),
            None => (None, Some(credentials.to_string())),
        },
        None => (None, None),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>()
                .map_err(|_| format!("invalid redis port '{}'", port))?,
        ),
        None => (host_port.to_string(), 6379),
    };
    if host.is_empty() {
        return Err("redis url has no host".to_string());
    }
    let db = path
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .and_then(|path| path.parse::<i64>().ok());
    Ok(RedisUrl {
        tls,
        host,
        port,
        username,
        password,
        db,
    })
}

impl RedisConn {
    /// 按 URL 建立连接，供 `#[interval]` 的分布式选主使用（直连，不入池）。
    pub(crate) fn connect_url(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let parsed = parse_url(url)?;
        Self::connect(&parsed, url)
    }

    /// 发送一条命令并读取回复。
    pub(crate) fn run(&mut self, args: &[&str]) -> Result<Reply, String> {
        self.command(args)
    }

    fn connect(url: &RedisUrl, raw: &str) -> Result<Self, String> {
        let address = format!("{}:{}", url.host, url.port);
        let mut addrs = address
            .to_socket_addrs()
            .map_err(|error| format!("cannot resolve {}: {}", address, error))?;
        let socket = addrs
            .next()
            .ok_or_else(|| format!("cannot resolve {}: no address", address))?;
        let stream = TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT)
            .map_err(|error| format!("cannot connect {}: {}", address, error))?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let stream = if url.tls {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let server_name = rustls::pki_types::ServerName::try_from(url.host.clone())
                .map_err(|_| format!("invalid redis host name '{}'", url.host))?;
            let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
                .map_err(|error| error.to_string())?;
            RedisStream::Tls(Box::new(rustls::StreamOwned::new(connection, stream)))
        } else {
            RedisStream::Plain(stream)
        };
        let mut conn = Self {
            stream,
            buffer: Vec::new(),
            position: 0,
            host: url.host.clone(),
            port: url.port,
            db: url.db.unwrap_or(0),
            url: raw.to_string(),
            broken: false,
        };
        if let Some(password) = &url.password {
            let reply = match &url.username {
                Some(username) => conn.command(&["AUTH", username, password]),
                None => conn.command(&["AUTH", password]),
            }?;
            check_status(reply)?;
        }
        if let Some(db) = url.db {
            let db = db.to_string();
            let reply = conn.command(&["SELECT", &db])?;
            check_status(reply)?;
        }
        Ok(conn)
    }

    fn fill(&mut self) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        let read = self
            .stream
            .read(&mut chunk)
            .map_err(|error| format!("redis read failed: {}", error))?;
        if read == 0 {
            return Err("redis connection closed".to_string());
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(())
    }

    fn read_byte(&mut self) -> Result<u8, String> {
        if self.position >= self.buffer.len() {
            self.fill()?;
        }
        let byte = self.buffer[self.position];
        self.position += 1;
        if self.position > 64 * 1024 {
            self.buffer.drain(..self.position);
            self.position = 0;
        }
        Ok(byte)
    }

    fn read_line(&mut self) -> Result<String, String> {
        let mut line = Vec::new();
        loop {
            let byte = self.read_byte()?;
            if byte == b'\r' {
                let next = self.read_byte()?;
                if next == b'\n' {
                    return Ok(String::from_utf8_lossy(&line).to_string());
                }
                line.push(byte);
                line.push(next);
            } else {
                line.push(byte);
            }
            if line.len() > 1024 * 1024 {
                return Err("redis reply line too long".to_string());
            }
        }
    }

    fn read_bytes(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(count.min(1024 * 1024));
        while out.len() < count {
            if self.position >= self.buffer.len() {
                self.fill()?;
            }
            let available = self.buffer.len() - self.position;
            let take = available.min(count - out.len());
            out.extend_from_slice(&self.buffer[self.position..self.position + take]);
            self.position += take;
        }
        Ok(out)
    }

    fn read_reply(&mut self) -> Result<Reply, String> {
        self.read_reply_depth(0)
    }

    /// RESP 解析限额：防止恶意/被劫持的服务端用超大长度或深层嵌套触发 OOM/栈溢出。
    fn read_reply_depth(&mut self, depth: usize) -> Result<Reply, String> {
        if depth > MAX_REPLY_DEPTH {
            return Err("redis reply nesting too deep".to_string());
        }
        let kind = self.read_byte()?;
        match kind {
            b'+' => Ok(Reply::Status(self.read_line()?)),
            b'-' => Ok(Reply::Error(self.read_line()?)),
            b':' => Ok(Reply::Int(self.read_line()?.parse().unwrap_or(0))),
            b'$' => {
                let length: i64 = self.read_line()?.parse().unwrap_or(-1);
                if length < 0 {
                    Ok(Reply::Bulk(None))
                } else if length > MAX_BULK_BYTES {
                    Err("redis bulk reply too large".to_string())
                } else {
                    let data = self.read_bytes(length as usize)?;
                    let _ = self.read_line()?;
                    Ok(Reply::Bulk(Some(String::from_utf8_lossy(&data).to_string())))
                }
            }
            b'*' => {
                let count: i64 = self.read_line()?.parse().unwrap_or(-1);
                if count < 0 {
                    Ok(Reply::Array(None))
                } else if count > MAX_ARRAY_ITEMS {
                    Err("redis array reply too large".to_string())
                } else {
                    let mut items = Vec::with_capacity((count as usize).min(1024));
                    for _ in 0..count {
                        items.push(self.read_reply_depth(depth + 1)?);
                    }
                    Ok(Reply::Array(Some(items)))
                }
            }
            other => Err(format!(
                "unexpected redis reply type '{}'",
                other as char
            )),
        }
    }

    pub(crate) fn command(&mut self, args: &[&str]) -> Result<Reply, String> {
        let result = self.send_command(args);
        if result.is_err() {
            self.broken = true;
        }
        result
    }

    fn send_command(&mut self, args: &[&str]) -> Result<Reply, String> {
        let mut payload = format!("*{}\r\n", args.len()).into_bytes();
        for arg in args {
            payload.extend_from_slice(format!("${}\r\n", arg.as_bytes().len()).as_bytes());
            payload.extend_from_slice(arg.as_bytes());
            payload.extend_from_slice(b"\r\n");
        }
        self.stream
            .write_all(&payload)
            .map_err(|error| format!("redis write failed: {}", error))?;
        self.stream
            .flush()
            .map_err(|error| format!("redis write failed: {}", error))?;
        self.read_reply()
    }
}

fn check_status(reply: Reply) -> Result<Reply, String> {
    match reply {
        Reply::Error(message) => Err(message),
        other => Ok(other),
    }
}

fn reply_to_value(reply: Reply) -> Value {
    match reply {
        Reply::Status(text) => Value::string(text),
        Reply::Error(message) => Value::Err(Box::new(Value::string(message))),
        Reply::Int(value) => Value::Int(value),
        Reply::Bulk(Some(text)) => Value::string(text),
        Reply::Bulk(None) => Value::None,
        Reply::Array(Some(items)) => Value::vec(items.into_iter().map(reply_to_value).collect()),
        Reply::Array(None) => Value::vec(Vec::new()),
    }
}

fn ok(value: Value) -> Value {
    Value::Ok(Box::new(value))
}

fn err(message: String) -> Value {
    Value::Err(Box::new(Value::string(message)))
}

fn conn_id(args: &[Value]) -> Option<i64> {
    match args.first() {
        Some(Value::Ary(ary)) => ary
            .borrow()
            .get("id")
            .and_then(|value| value.as_int()),
        _ => None,
    }
}

/// 分布式锁释放脚本：只删除自己持有的锁，避免误删别人的租约。
pub(crate) const UNLOCK_SCRIPT: &str =
    "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end";

/// 固定窗口限流：首次计数时设置过期时间。
const RATE_LIMIT_SCRIPT: &str =
    "local c = redis.call('incr', KEYS[1]) if c == 1 then redis.call('pexpire', KEYS[1], ARGV[1]) end if c > tonumber(ARGV[2]) then return 0 else return 1 end";

impl Runtime {
    pub(crate) fn redis_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "connect" => {
                let url = self.string_arg(&args, 0, name, line)?;
                let trimmed = url.trim().to_string();
                let parsed = match parse_url(&trimmed) {
                    Ok(parsed) => parsed,
                    Err(message) => return Ok(err(message)),
                };
                // 先尝试从进程级空闲池复用同 URL 连接，池空时新建。
                let fresh = match pool_take(&trimmed) {
                    Some(conn) => Ok(conn),
                    None => RedisConn::connect(&parsed, &trimmed),
                };
                match fresh {
                    Ok(conn) => {
                        let id = self.redis.next_id;
                        self.redis.next_id += 1;
                        let host = conn.host.clone();
                        let port = conn.port;
                        let db = conn.db;
                        self.redis.conns.insert(id, conn);
                        Ok(ok(Value::ary(
                            "redis.conn",
                            vec![
                                ("id".to_string(), Value::Int(id)),
                                ("host".to_string(), Value::string(host)),
                                ("port".to_string(), Value::Int(port as i64)),
                                ("db".to_string(), Value::Int(db)),
                            ],
                        )))
                    }
                    Err(message) => Ok(err(message)),
                }
            }
            "lock" => {
                let id = match conn_id(&args) {
                    Some(id) => id,
                    None => return self.error("redis.lock expects a redis.conn as argument 1", line),
                };
                let key = self.string_arg(&args, 1, name, line)?;
                let ttl = self.int_arg(&args, 2, name, line)?;
                if ttl <= 0 {
                    return self.error("redis.lock expects a positive ttl in milliseconds", line);
                }
                let token = random_token();
                let ttl = ttl.to_string();
                let reply = match self.redis_command(id, &["SET", &key, &token, "NX", "PX", &ttl]) {
                    Ok(reply) => reply,
                    Err(message) => return Ok(err(message)),
                };
                match reply {
                    Reply::Status(status) if status == "OK" => Ok(ok(Value::Some(Box::new(
                        Value::string(token),
                    )))),
                    Reply::Bulk(None) => Ok(ok(Value::None)),
                    Reply::Error(message) => Ok(err(message)),
                    other => Ok(err(format!("unexpected redis reply {:?}", reply_kind(&other)))),
                }
            }
            "unlock" => {
                let id = match conn_id(&args) {
                    Some(id) => id,
                    None => {
                        return self.error(
                            "redis.unlock expects a redis.conn as argument 1",
                            line,
                        )
                    }
                };
                let key = self.string_arg(&args, 1, name, line)?;
                let token = self.string_arg(&args, 2, name, line)?;
                let reply = match self.redis_command(
                    id,
                    &["EVAL", UNLOCK_SCRIPT, "1", &key, &token],
                ) {
                    Ok(reply) => reply,
                    Err(message) => return Ok(err(message)),
                };
                match reply {
                    Reply::Int(count) => Ok(ok(Value::Bool(count > 0))),
                    Reply::Error(message) => Ok(err(message)),
                    other => Ok(err(format!("unexpected redis reply {:?}", reply_kind(&other)))),
                }
            }
            "rate_limit" => {
                let id = match conn_id(&args) {
                    Some(id) => id,
                    None => {
                        return self.error(
                            "redis.rate_limit expects a redis.conn as argument 1",
                            line,
                        )
                    }
                };
                let key = self.string_arg(&args, 1, name, line)?;
                let limit = self.int_arg(&args, 2, name, line)?;
                let window = self.int_arg(&args, 3, name, line)?;
                if limit <= 0 || window <= 0 {
                    return self.error(
                        "redis.rate_limit expects positive limit and window_ms",
                        line,
                    );
                }
                let limit = limit.to_string();
                let window = window.to_string();
                let reply = match self.redis_command(
                    id,
                    &["EVAL", RATE_LIMIT_SCRIPT, "1", &key, &window, &limit],
                ) {
                    Ok(reply) => reply,
                    Err(message) => return Ok(err(message)),
                };
                match reply {
                    Reply::Int(value) => Ok(ok(Value::Bool(value > 0))),
                    Reply::Error(message) => Ok(err(message)),
                    other => Ok(err(format!("unexpected redis reply {:?}", reply_kind(&other)))),
                }
            }
            _ => self.error(format!("std/redis has no function '{}'", name), line),
        }
    }

    pub(crate) fn redis_conn_method(
        &mut self,
        ary: &std::rc::Rc<std::cell::RefCell<crate::value::Ary>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let id = match ary.borrow().get("id").and_then(|value| value.as_int()) {
            Some(id) => id,
            None => return self.error("invalid redis connection", line),
        };
        if name == "close" {
            if let Some(conn) = self.redis.conns.remove(&id) {
                pool_give(conn);
            }
            return Ok(Value::Null);
        }
        let mut command: Vec<String> = Vec::new();
        match name {
            "ping" => command.push("PING".to_string()),
            "get" => {
                command.push("GET".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "set" => {
                command.push("SET".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
            }
            "setex" => {
                let seconds = self.int_arg(&args, 1, name, line)?;
                command.push("SETEX".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(seconds.to_string());
                command.push(self.string_arg(&args, 2, name, line)?);
            }
            "setnx" => {
                command.push("SET".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
                command.push("NX".to_string());
            }
            "del" => {
                command.push("DEL".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "exists" => {
                command.push("EXISTS".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "expire" => {
                let seconds = self.int_arg(&args, 1, name, line)?;
                command.push("EXPIRE".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(seconds.to_string());
            }
            "incr" => {
                command.push("INCR".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "decr" => {
                command.push("DECR".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "hget" => {
                command.push("HGET".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
            }
            "hset" => {
                command.push("HSET".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
                command.push(self.string_arg(&args, 2, name, line)?);
            }
            "hdel" => {
                command.push("HDEL".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
            }
            "lpush" => {
                command.push("LPUSH".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
            }
            "rpush" => {
                command.push("RPUSH".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(self.string_arg(&args, 1, name, line)?);
            }
            "lpop" => {
                command.push("LPOP".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "rpop" => {
                command.push("RPOP".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            "llen" => {
                command.push("LLEN".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
            }
            // 阻塞出队：最长 9 秒（连接读超时为 10 秒），超时返回 none。
            "brpop" => {
                let seconds = self.int_arg(&args, 1, name, line)?.clamp(1, 9);
                command.push("BRPOP".to_string());
                command.push(self.string_arg(&args, 0, name, line)?);
                command.push(seconds.to_string());
            }
            "eval" => {
                let script = self.string_arg(&args, 0, name, line)?;
                let keys = match args.get(1) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => Vec::new(),
                };
                let extra = match args.get(2) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => Vec::new(),
                };
                command.push("EVAL".to_string());
                command.push(script);
                command.push(keys.len().to_string());
                for item in keys.iter().chain(extra.iter()) {
                    command.push(item.as_str().unwrap_or_default());
                }
            }
            _ => return self.error(format!("redis.conn has no method '{}'", name), line),
        }
        let borrowed: Vec<&str> = command.iter().map(|item| item.as_str()).collect();
        let result = self.redis_command(id, &borrowed);
        if name == "brpop" {
            // BRPOP 返回 [key, value]，这里只暴露 value；超时是 nil。
            return Ok(match result {
                Ok(Reply::Array(Some(mut items))) if items.len() == 2 => match items.pop() {
                    Some(Reply::Bulk(Some(text))) => ok(Value::Some(Box::new(Value::string(text)))),
                    _ => ok(Value::None),
                },
                Ok(Reply::Array(_)) => ok(Value::None),
                Ok(Reply::Error(message)) => err(message),
                Ok(other) => err(format!("unexpected redis reply {:?}", reply_kind(&other))),
                Err(message) => err(message),
            });
        }
        match result {
            Ok(reply) => match reply {
                Reply::Error(message) => Ok(err(message)),
                other => Ok(ok(reply_to_value(other))),
            },
            Err(message) => Ok(err(message)),
        }
    }

    fn redis_command(&mut self, id: i64, args: &[&str]) -> Result<Reply, String> {
        match self.redis.conns.get_mut(&id) {
            Some(conn) => conn.command(args),
            None => Err("redis connection is closed".to_string()),
        }
    }
}

fn reply_kind(reply: &Reply) -> &'static str {
    match reply {
        Reply::Status(_) => "status",
        Reply::Error(_) => "error",
        Reply::Int(_) => "integer",
        Reply::Bulk(_) => "bulk",
        Reply::Array(_) => "array",
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::getrandom(&mut bytes);
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

pub(crate) fn new_token() -> String {
    random_token()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// 最小 RESP 服务端：只回应测试用到的命令，验证请求编码与回复解析。
    fn spawn_stub() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => continue,
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut count_line = String::new();
                    if reader.read_line(&mut count_line).unwrap_or(0) == 0 {
                        break;
                    }
                    let count: usize = count_line
                        .trim()
                        .trim_start_matches('*')
                        .parse()
                        .unwrap_or(0);
                    let mut command: Vec<String> = Vec::new();
                    for _ in 0..count {
                        let mut length_line = String::new();
                        if reader.read_line(&mut length_line).unwrap_or(0) == 0 {
                            break;
                        }
                        let length: usize = length_line
                            .trim()
                            .trim_start_matches('$')
                            .parse()
                            .unwrap_or(0);
                        let mut data = vec![0u8; length + 2];
                        let _ = reader.read_exact(&mut data);
                        command.push(String::from_utf8_lossy(&data[..length]).to_string());
                    }
                    let reply: Vec<u8> = match command.first().map(|item| item.as_str()) {
                        Some("GET") => b"$5\r\nhello\r\n".to_vec(),
                        Some("INCR") => b":7\r\n".to_vec(),
                        Some("EVAL") => b":1\r\n".to_vec(),
                        Some("EXISTS") => b":0\r\n".to_vec(),
                        _ => b"+OK\r\n".to_vec(),
                    };
                    if stream.write_all(&reply).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });
        port
    }

    #[test]
    fn runs_commands_against_stub() {
        let port = spawn_stub();
        let url = format!("redis://127.0.0.1:{}/2", port);
        let mut conn = RedisConn::connect_url(&url).expect("connect");
        assert!(matches!(
            conn.run(&["SET", "k", "v"]).unwrap(),
            Reply::Status(text) if text == "OK"
        ));
        match conn.run(&["GET", "k"]).unwrap() {
            Reply::Bulk(Some(text)) => assert_eq!(text, "hello"),
            other => panic!("unexpected reply {:?}", reply_kind(&other)),
        }
        match conn.run(&["INCR", "counter"]).unwrap() {
            Reply::Int(value) => assert_eq!(value, 7),
            other => panic!("unexpected reply {:?}", reply_kind(&other)),
        }
        match conn.run(&["EVAL", UNLOCK_SCRIPT, "1", "k", "t"]).unwrap() {
            Reply::Int(value) => assert_eq!(value, 1),
            other => panic!("unexpected reply {:?}", reply_kind(&other)),
        }
    }

    #[test]
    fn rejects_invalid_url() {
        assert!(RedisConn::connect_url("http://127.0.0.1:6379").is_err());
    }

    #[test]
    fn pool_reuses_returned_connection() {
        let port = spawn_stub();
        let url = format!("redis://127.0.0.1:{}", port);
        let conn = RedisConn::connect_url(&url).expect("connect");
        pool_give(conn);
        let mut reused = pool_take(&url).expect("returned connection should be reusable");
        assert_eq!(reused.port, port);
        assert!(reused.run(&["GET", "k"]).is_ok());
        // 借出后池已空，不会凭空复用。
        assert!(pool_take(&url).is_none());
    }

    #[test]
    fn pool_skips_broken_and_dirty_connections() {
        let port = spawn_stub();
        let url = format!("redis://127.0.0.1:{}", port);
        let mut conn = RedisConn::connect_url(&url).expect("connect");
        conn.broken = true;
        pool_give(conn);
        assert!(pool_take(&url).is_none());
        let mut conn = RedisConn::connect_url(&url).expect("connect");
        conn.broken = false;
        conn.buffer = vec![b'+', b'O'];
        conn.position = 1;
        pool_give(conn);
        assert!(pool_take(&url).is_none());
    }

    #[test]
    fn pool_take_discards_dead_connections() {
        // 一次性服务端：accept 后立即关闭，回池的连接验活（PING）必然失败。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _accepted = listener.accept();
        });
        let url = format!("redis://127.0.0.1:{}", port);
        let conn = RedisConn::connect_url(&url).expect("connect");
        pool_give(conn);
        assert!(pool_take(&url).is_none());
    }
}

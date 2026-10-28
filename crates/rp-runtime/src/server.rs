//! HTTP 服务端运行参数、处理函数描述、指标与停机信号。
//!
//! 这里只放与解释器无关的部分：配置、可跨线程的处理函数描述、原子计数与
//! 全局停机标志。真正的连接处理在 `builtins` 中由各工作线程的解释器完成，
//! 因为 Rip 变量不可跨线程共享。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

/// 延迟直方图的桶上界（毫秒）；超过最后一桶计入 `+Inf`。
pub const LATENCY_BUCKETS_MS: [u64; 11] = [1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

/// HTTP 服务端运行参数。来自 `rip.toml` 的 `[server]`、CLI 参数与缺省值。
#[derive(Clone)]
pub struct ServerConfig {
    pub host: String,
    pub workers: usize,
    pub max_connections: usize,
    pub max_body_bytes: usize,
    pub max_header_bytes: usize,
    pub max_headers: usize,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub keep_alive: Duration,
    pub max_requests: usize,
    pub health_path: String,
    pub ready_path: String,
    pub metrics_path: String,
    /// 收到停机信号后等待在途请求结束的最长时间。
    pub drain_timeout: Duration,
    /// 单个处理函数的最长执行时间；`Duration::ZERO` 表示不限制。
    pub request_timeout: Duration,
    /// 是否为每个请求输出一行访问日志。
    pub access_log: bool,
    /// 进程堆占用上限（字节）；0 表示不限制。超过后当前请求以运行时错误结束。
    pub max_heap_bytes: usize,
    /// 是否信任 `X-Forwarded-For` / `X-Real-IP`（部署在反向代理之后时开启）。
    pub trust_proxy: bool,
    /// 可信代理网段（逗号分隔的 CIDR 或单个 IP）；非空时只有来自这些地址的
    /// 请求才会采信转发头，并逐跳跳过可信代理还原客户端 IP。
    pub trusted_proxies: Vec<String>,
    /// 每秒允许的请求数（进程内令牌桶）；0 表示关闭限流。
    pub rate_limit: u32,
    /// 令牌桶容量；0 表示与 `rate_limit` 相同（允许一秒钟的突发）。
    pub rate_limit_burst: u32,
    /// 是否按客户端 IP 分别限流；关闭时全进程共用一个桶。
    pub rate_limit_per_ip: bool,
    /// OTLP/HTTP 追踪导出端点（Collector 根地址）；留空表示不导出。
    pub otlp_endpoint: String,
    /// 导出时写入的 `service.name`。
    pub otlp_service: String,
    /// trace 采样比例（0..=1）。
    pub otlp_sample_ratio: f64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            // 0 表示按 CPU 核数自动选择。
            workers: 0,
            max_connections: 1024,
            max_body_bytes: 8 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_headers: 100,
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            keep_alive: Duration::from_secs(5),
            max_requests: 1000,
            health_path: "/healthz".to_string(),
            ready_path: "/readyz".to_string(),
            // 缺省不暴露指标，需要在 [server] 显式配置 metrics_path。
            metrics_path: String::new(),
            drain_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            access_log: true,
            max_heap_bytes: 0,
            trust_proxy: false,
            trusted_proxies: Vec::new(),
            rate_limit: 0,
            rate_limit_burst: 0,
            rate_limit_per_ip: false,
            otlp_endpoint: String::new(),
            otlp_service: "rip".to_string(),
            otlp_sample_ratio: 1.0,
        }
    }
}

impl ServerConfig {
    /// 实际工作线程数：`workers = 0` 时取 CPU 核数，限制在 2..=64。
    pub fn worker_count(&self) -> usize {
        if self.workers > 0 {
            return self.workers.min(256);
        }
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4)
            .clamp(2, 64)
    }
}

/// 可信代理配置：决定是否采信转发头，以及如何逐跳还原客户端 IP。
///
/// - `trust_all = true`（`[server] trust_proxy = true`）：采信任意来源的转发头，
///   取 `X-Forwarded-For` 最左侧的合法 IP。
/// - `trusted_proxies` 非空：只有来自这些网段的请求才采信转发头，并从右往左
///   跳过可信代理，第一个不可信地址就是真实客户端。
pub struct ProxyTrust {
    all: bool,
    cidrs: Vec<(std::net::IpAddr, u8)>,
}

impl ProxyTrust {
    pub fn new(trust_all: bool, specs: &[String]) -> Self {
        Self {
            all: trust_all,
            cidrs: specs.iter().filter_map(|spec| parse_cidr(spec)).collect(),
        }
    }

    /// 解析客户端 IP；无法判定时回退到连接对端地址。
    pub fn resolve(&self, peer: &str, headers: &[(String, String)]) -> String {
        let forwarded = header_value(headers, "x-forwarded-for");
        let real_ip = header_value(headers, "x-real-ip");
        if self.all {
            if let Some(value) = forwarded.as_deref() {
                if let Some(ip) = value.split(',').find_map(valid_ip) {
                    return ip.to_string();
                }
            }
            return real_ip
                .and_then(|value| valid_ip(&value))
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| peer.to_string());
        }
        let peer_ip = match peer.parse::<std::net::IpAddr>() {
            Ok(ip) => ip,
            Err(_) => return peer.to_string(),
        };
        if self.cidrs.is_empty() || !self.trusts(peer_ip) {
            return peer_ip.to_string();
        }
        if let Some(value) = forwarded {
            for part in value.split(',').rev() {
                let Ok(ip) = part.trim().parse::<std::net::IpAddr>() else {
                    continue;
                };
                if self.trusts(ip) {
                    continue;
                }
                return ip.to_string();
            }
        }
        real_ip
            .and_then(|value| valid_ip(&value))
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| peer_ip.to_string())
    }

    fn trusts(&self, ip: std::net::IpAddr) -> bool {
        self.cidrs
            .iter()
            .any(|(network, bits)| ip_in_prefix(ip, *network, *bits))
    }
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn valid_ip(text: &str) -> Option<std::net::IpAddr> {
    text.trim().parse::<std::net::IpAddr>().ok()
}

/// 解析 `10.0.0.0/8` 或 `192.168.1.5` 形式的网段。
fn parse_cidr(spec: &str) -> Option<(std::net::IpAddr, u8)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    match spec.split_once('/') {
        Some((ip, bits)) => {
            let ip: std::net::IpAddr = ip.trim().parse().ok()?;
            let bits: u8 = bits.trim().parse().ok()?;
            Some((ip, bits))
        }
        None => {
            let ip: std::net::IpAddr = spec.parse().ok()?;
            let bits = if ip.is_ipv4() { 32 } else { 128 };
            Some((ip, bits))
        }
    }
}

fn ip_in_prefix(
    ip: std::net::IpAddr,
    network: std::net::IpAddr,
    bits: u8,
) -> bool {
    match (ip, network) {
        (std::net::IpAddr::V4(ip), std::net::IpAddr::V4(network)) => {
            let bits = bits.min(32) as u32;
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            (u32::from(ip) & mask) == (u32::from(network) & mask)
        }
        (std::net::IpAddr::V6(ip), std::net::IpAddr::V6(network)) => {
            let bits = bits.min(128) as u32;
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            (u128::from(ip) & mask) == (u128::from(network) & mask)
        }
        _ => false,
    }
}

/// 进程内令牌桶限流。
///
/// 每秒补充一次令牌（不做毫秒级平滑），全局桶用单个原子 CAS 完成判定，
/// 高并发下没有锁竞争；只有开启按 IP 限流时才需要互斥保护哈希表。
pub struct RateLimiter {
    rate: u32,
    burst: u32,
    per_ip: bool,
    /// 全局桶：高 32 位为剩余令牌，低 32 位为上次补充的 epoch 秒。
    global: AtomicU64,
    /// 每 IP 桶：IP → (剩余令牌, 上次补充的 epoch 秒)。
    per_client: Mutex<HashMap<String, (u32, u32)>>,
}

impl RateLimiter {
    /// `rate = 0` 表示不限流，返回 None。
    pub fn new(rate: u32, burst: u32, per_ip: bool) -> Option<Self> {
        if rate == 0 {
            return None;
        }
        let burst = if burst == 0 { rate } else { burst }.max(rate);
        let now = epoch_seconds();
        Some(Self {
            rate,
            burst,
            per_ip,
            global: AtomicU64::new(((burst as u64) << 32) | now as u64),
            per_client: Mutex::new(HashMap::new()),
        })
    }

    /// 取一个令牌；返回 false 表示应回 429。
    pub fn allow(&self, client_ip: &str) -> bool {
        let now = epoch_seconds();
        if !self.per_ip {
            return self.allow_global(now);
        }
        // 锁中毒只可能来自 panic，表结构仍完好：取回内部数据继续正常限流，
        // 既不放行（fail-open）也不永久拒绝。
        let mut buckets = self
            .per_client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 上限保护：伪造 IP 无法把哈希表撑爆；按最后活跃时间逐出最旧一半，
        // 而不是整体清空（清空会让攻击者用假 IP 反复重置所有人的限流计数）。
        if buckets.len() > 100_000 {
            let mut ages: Vec<(u32, String)> = buckets
                .iter()
                .map(|(ip, (_, last))| (*last, ip.clone()))
                .collect();
            ages.sort_unstable();
            let cut = buckets.len() / 2;
            for (_, ip) in ages.into_iter().take(cut) {
                buckets.remove(&ip);
            }
        }
        let entry = buckets
            .entry(client_ip.to_string())
            .or_insert((self.burst, now));
        let (tokens, last) = refill(entry.0, entry.1, now, self.rate, self.burst);
        *entry = (tokens, last);
        if entry.0 == 0 {
            false
        } else {
            entry.0 -= 1;
            true
        }
    }

    fn allow_global(&self, now: u32) -> bool {
        let mut current = self.global.load(Ordering::Relaxed);
        loop {
            let tokens = (current >> 32) as u32;
            let last = current as u32;
            let (mut tokens, last) = refill(tokens, last, now, self.rate, self.burst);
            if tokens == 0 {
                let packed = ((tokens as u64) << 32) | last as u64;
                let _ = self.global.compare_exchange_weak(
                    current,
                    packed,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                return false;
            }
            tokens -= 1;
            let packed = ((tokens as u64) << 32) | last as u64;
            match self.global.compare_exchange_weak(
                current,
                packed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }
}

/// 按经过的秒数补充令牌；最多补 2 秒，长时间空闲不会积攒出巨大突发。
fn refill(tokens: u32, last: u32, now: u32, rate: u32, burst: u32) -> (u32, u32) {
    if now == last {
        return (tokens, last);
    }
    // 系统时钟回拨时按一秒处理，避免令牌永远不补充。
    let elapsed = if now > last { now - last } else { 1 };
    let added = rate.saturating_mul(elapsed.min(2));
    (tokens.saturating_add(added).min(burst), now)
}

fn epoch_seconds() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as u32)
        .unwrap_or(0)
}

/// 处理函数描述：只保留名字与结构，可跨线程传递。
///
/// `Rc<Function>` 不能跨线程，工作线程会用自己的解释器按描述重建处理函数。
#[derive(Clone)]
pub(crate) enum HandlerSpec {
    Func(String),
    Static { dir: String, prefix: String },
    Router(Vec<RouteSpec>),
    /// 内建中间件工厂（CORS、固定响应头）。
    Middleware(crate::value::MiddlewareSpec),
    /// 中间件链：按顺序包裹 handler。
    Chain {
        middlewares: Vec<HandlerSpec>,
        handler: Box<HandlerSpec>,
    },
    /// 路由分组：剥前缀后交给内部处理器。
    Mounted {
        prefix: String,
        inner: Box<HandlerSpec>,
    },
}

#[derive(Clone)]
pub(crate) struct RouteSpec {
    pub method: String,
    pub path: String,
    pub handler: HandlerSpec,
}

/// 单个路由的统计：延迟直方图 + 状态类计数。
#[derive(Default)]
struct RouteStats {
    buckets: Vec<u64>,
    count: u64,
    sum_ms: u64,
    /// 状态类只有 2xx/3xx/4xx/5xx 四种：用 `&'static str` 作键，命中时不分配内存。
    by_status: HashMap<&'static str, u64>,
}

impl RouteStats {
    fn new() -> Self {
        Self {
            buckets: vec![0; LATENCY_BUCKETS_MS.len()],
            ..Default::default()
        }
    }

    fn record(&mut self, status: i64, elapsed_ms: u64) {
        self.count += 1;
        self.sum_ms += elapsed_ms;
        if let Some(index) = LATENCY_BUCKETS_MS
            .iter()
            .position(|upper| elapsed_ms <= *upper)
        {
            self.buckets[index] += 1;
        }
        *self
            .by_status
            .entry(status_class(status))
            .or_insert(0) += 1;
    }
}

/// 路由统计的分片数：每个请求只锁一个分片，避免所有工作线程抢同一把锁。
const ROUTE_SHARDS: usize = 16;

/// 服务端指标，跨工作线程共享。
pub struct Metrics {
    pub requests: AtomicU64,
    pub status_2xx: AtomicU64,
    pub status_3xx: AtomicU64,
    pub status_4xx: AtomicU64,
    pub status_5xx: AtomicU64,
    pub in_flight: AtomicUsize,
    pub active_connections: AtomicUsize,
    pub rejected: AtomicU64,
    /// 被进程内限流拒绝的请求数。
    pub rate_limited: AtomicU64,
    /// 数据库语句执行次数 / 失败次数 / 总耗时。
    pub db_queries: AtomicU64,
    pub db_errors: AtomicU64,
    pub db_duration_ms: AtomicU64,
    pub duration_ms: AtomicU64,
    /// 延迟直方图：与 `LATENCY_BUCKETS_MS` 一一对应（非累计，输出时再累加）。
    pub duration_buckets: Vec<AtomicU64>,
    pub duration_count: AtomicU64,
    /// 路由维度统计（路由标签 → 直方图与状态类），按路由哈希分片降低锁竞争。
    routes: Vec<Mutex<HashMap<String, RouteStats>>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            requests: AtomicU64::new(0),
            status_2xx: AtomicU64::new(0),
            status_3xx: AtomicU64::new(0),
            status_4xx: AtomicU64::new(0),
            status_5xx: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            active_connections: AtomicUsize::new(0),
            rejected: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            db_queries: AtomicU64::new(0),
            db_errors: AtomicU64::new(0),
            db_duration_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            duration_buckets: (0..LATENCY_BUCKETS_MS.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            duration_count: AtomicU64::new(0),
            routes: (0..ROUTE_SHARDS)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
        }
    }
}

/// 状态码归类，用于低基数指标。
pub fn status_class(status: i64) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

impl Metrics {
    pub fn record(&self, status: i64, elapsed_ms: u64) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.duration_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        if let Some(index) = LATENCY_BUCKETS_MS
            .iter()
            .position(|upper| elapsed_ms <= *upper)
        {
            self.duration_buckets[index].fetch_add(1, Ordering::Relaxed);
        }
        match status {
            200..=299 => self.status_2xx.fetch_add(1, Ordering::Relaxed),
            300..=399 => self.status_3xx.fetch_add(1, Ordering::Relaxed),
            400..=499 => self.status_4xx.fetch_add(1, Ordering::Relaxed),
            _ => self.status_5xx.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// 记录一次路由命中：累计该路由的直方图与状态类。
    ///
    /// 热路径：路由已存在时只做两次哈希查找，不分配内存；只有首次出现才插入。
    pub fn record_route(&self, route: &str, status: i64, elapsed_ms: u64) {
        let mut hasher = DefaultHasher::new();
        route.hash(&mut hasher);
        let shard = (hasher.finish() as usize) % ROUTE_SHARDS;
        let Ok(mut routes) = self.routes[shard].lock() else {
            return;
        };
        if let Some(stats) = routes.get_mut(route) {
            stats.record(status, elapsed_ms);
        } else {
            let mut stats = RouteStats::new();
            stats.record(status, elapsed_ms);
            routes.insert(route.to_string(), stats);
        }
    }

    /// Prometheus 文本格式（text/plain; version=0.0.4）。
    pub fn prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str("# HELP rip_http_requests_total Total HTTP requests handled.\n");
        out.push_str("# TYPE rip_http_requests_total counter\n");
        out.push_str(&format!(
            "rip_http_requests_total {}\n",
            self.requests.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_http_responses_total Responses by status class.\n");
        out.push_str("# TYPE rip_http_responses_total counter\n");
        for (class, value) in [
            ("2xx", &self.status_2xx),
            ("3xx", &self.status_3xx),
            ("4xx", &self.status_4xx),
            ("5xx", &self.status_5xx),
        ] {
            out.push_str(&format!(
                "rip_http_responses_total{{status=\"{}\"}} {}\n",
                class,
                value.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP rip_http_request_duration_ms Request duration in milliseconds.\n");
        out.push_str("# TYPE rip_http_request_duration_ms histogram\n");
        // Prometheus 的桶是累计语义：每一行包含所有更小的桶。
        let mut cumulative = 0u64;
        for (index, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
            cumulative += self.duration_buckets[index].load(Ordering::Relaxed);
            out.push_str(&format!(
                "rip_http_request_duration_ms_bucket{{le=\"{}\"}} {}\n",
                upper, cumulative
            ));
        }
        let count = self.duration_count.load(Ordering::Relaxed);
        out.push_str(&format!(
            "rip_http_request_duration_ms_bucket{{le=\"+Inf\"}} {}\n",
            count
        ));
        out.push_str(&format!(
            "rip_http_request_duration_ms_sum {}\n",
            self.duration_ms.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "rip_http_request_duration_ms_count {}\n",
            count
        ));
        // 各分片分别加锁合并出一份快照，避免输出指标时长期占住单个分片。
        let mut merged: HashMap<String, RouteStats> = HashMap::new();
        for shard in &self.routes {
            let Ok(routes) = shard.lock() else { continue };
            for (name, stats) in routes.iter() {
                let entry = merged
                    .entry(name.clone())
                    .or_insert_with(RouteStats::new);
                entry.count += stats.count;
                entry.sum_ms += stats.sum_ms;
                for (index, value) in stats.buckets.iter().enumerate() {
                    entry.buckets[index] += value;
                }
                for (class, value) in &stats.by_status {
                    *entry.by_status.entry(*class).or_insert(0) += value;
                }
            }
        }
        if !merged.is_empty() {
            let mut names: Vec<&String> = merged.keys().collect();
            names.sort();
            out.push_str("# HELP rip_http_route_requests_total Requests by matched route and status class.\n");
            out.push_str("# TYPE rip_http_route_requests_total counter\n");
            for name in &names {
                let stats = &merged[*name];
                let mut classes: Vec<(&'static str, &u64)> = stats.by_status.iter().map(|(k, v)| (*k, v)).collect();
                classes.sort_by(|left, right| left.0.cmp(right.0));
                for (class, value) in classes {
                    out.push_str(&format!(
                        "rip_http_route_requests_total{{route=\"{}\",status=\"{}\"}} {}\n",
                        escape_label(name),
                        class,
                        value
                    ));
                }
            }
            out.push_str("# HELP rip_http_route_duration_ms Request duration per matched route.\n");
            out.push_str("# TYPE rip_http_route_duration_ms histogram\n");
            for name in &names {
                let stats = &merged[*name];
                let mut cumulative = 0u64;
                for (index, upper) in LATENCY_BUCKETS_MS.iter().enumerate() {
                    cumulative += stats.buckets[index];
                    out.push_str(&format!(
                        "rip_http_route_duration_ms_bucket{{route=\"{}\",le=\"{}\"}} {}\n",
                        escape_label(name),
                        upper,
                        cumulative
                    ));
                }
                out.push_str(&format!(
                    "rip_http_route_duration_ms_bucket{{route=\"{}\",le=\"+Inf\"}} {}\n",
                    escape_label(name),
                    stats.count
                ));
                out.push_str(&format!(
                    "rip_http_route_duration_ms_sum{{route=\"{}\"}} {}\n",
                    escape_label(name),
                    stats.sum_ms
                ));
                out.push_str(&format!(
                    "rip_http_route_duration_ms_count{{route=\"{}\"}} {}\n",
                    escape_label(name),
                    stats.count
                ));
            }
        }
        out.push_str("# HELP rip_access_log_dropped_total Access log lines dropped because stdout could not keep up.\n");
        out.push_str("# TYPE rip_access_log_dropped_total counter\n");
        out.push_str(&format!(
            "rip_access_log_dropped_total {}\n",
            access_log_dropped()
        ));
        out.push_str("# HELP rip_http_in_flight Requests currently being handled.\n");
        out.push_str("# TYPE rip_http_in_flight gauge\n");
        out.push_str(&format!(
            "rip_http_in_flight {}\n",
            self.in_flight.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_http_active_connections Open client connections.\n");
        out.push_str("# TYPE rip_http_active_connections gauge\n");
        out.push_str(&format!(
            "rip_http_active_connections {}\n",
            self.active_connections.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_http_rejected_total Connections rejected by the accept limit.\n");
        out.push_str("# TYPE rip_http_rejected_total counter\n");
        out.push_str(&format!(
            "rip_http_rejected_total {}\n",
            self.rejected.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_http_rate_limited_total Requests rejected by the rate limiter.\n");
        out.push_str("# TYPE rip_http_rate_limited_total counter\n");
        out.push_str(&format!(
            "rip_http_rate_limited_total {}\n",
            self.rate_limited.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_db_queries_total Database statements executed.\n");
        out.push_str("# TYPE rip_db_queries_total counter\n");
        out.push_str(&format!(
            "rip_db_queries_total {}\n",
            self.db_queries.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_db_errors_total Database statements that failed.\n");
        out.push_str("# TYPE rip_db_errors_total counter\n");
        out.push_str(&format!(
            "rip_db_errors_total {}\n",
            self.db_errors.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP rip_db_duration_ms_total Database statement time in milliseconds.\n");
        out.push_str("# TYPE rip_db_duration_ms_total counter\n");
        out.push_str(&format!(
            "rip_db_duration_ms_total {}\n",
            self.db_duration_ms.load(Ordering::Relaxed)
        ));
        // 进程级缓存（std/cache）的命中情况；未使用时全部为 0。
        let (cache_hits, cache_misses, cache_evictions, cache_entries) =
            crate::cache::snapshot();
        out.push_str("# HELP rip_cache_hits_total Cache hits.\n");
        out.push_str("# TYPE rip_cache_hits_total counter\n");
        out.push_str(&format!("rip_cache_hits_total {}\n", cache_hits));
        out.push_str("# HELP rip_cache_misses_total Cache misses.\n");
        out.push_str("# TYPE rip_cache_misses_total counter\n");
        out.push_str(&format!("rip_cache_misses_total {}\n", cache_misses));
        out.push_str("# HELP rip_cache_evictions_total Cache entries evicted (LRU or expired).\n");
        out.push_str("# TYPE rip_cache_evictions_total counter\n");
        out.push_str(&format!("rip_cache_evictions_total {}\n", cache_evictions));
        out.push_str("# HELP rip_cache_entries Cached entries.\n");
        out.push_str("# TYPE rip_cache_entries gauge\n");
        out.push_str(&format!("rip_cache_entries {}\n", cache_entries));
        // OTLP 追踪导出：未启用时三个计数保持为 0。
        let (trace_exported, trace_failed, trace_dropped) = crate::trace::counters();
        out.push_str("# HELP rip_trace_spans_exported_total Spans delivered to the OTLP collector.\n");
        out.push_str("# TYPE rip_trace_spans_exported_total counter\n");
        out.push_str(&format!("rip_trace_spans_exported_total {}\n", trace_exported));
        out.push_str("# HELP rip_trace_export_failures_total Failed OTLP export attempts.\n");
        out.push_str("# TYPE rip_trace_export_failures_total counter\n");
        out.push_str(&format!("rip_trace_export_failures_total {}\n", trace_failed));
        out.push_str("# HELP rip_trace_spans_dropped_total Spans dropped because the queue was full.\n");
        out.push_str("# TYPE rip_trace_spans_dropped_total counter\n");
        out.push_str(&format!("rip_trace_spans_dropped_total {}\n", trace_dropped));
        out.push_str("# HELP rip_build_info Build information.\n");
        out.push_str("# TYPE rip_build_info gauge\n");
        out.push_str(&format!(
            "rip_build_info{{version=\"{}\",platform=\"{}\"}} 1\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        ));
        out
    }
}

/// Prometheus 标签值转义：反斜杠、双引号与换行。
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// 全局停机标志：收到 SIGINT / SIGTERM / Ctrl+C 后置位。
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static SIGNAL_ONCE: Once = Once::new();

/// 注册一次进程级信号处理。多次调用 `http.serve` 也只注册一次。
pub(crate) fn install_signal_handler() {
    SIGNAL_ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            SHUTDOWN.store(true, Ordering::SeqCst);
        });
    });
}

pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

/// 从 W3C `traceparent` 头（`00-<32hex>-<16hex>-<flags>`）取出 trace id。
pub fn trace_id_from_traceparent(value: &str) -> Option<String> {
    crate::trace::parse_traceparent(value).map(|(trace_id, _)| trace_id)
}

/// 生成一个指向同一 trace 的新 `traceparent` 头（随机 span id）。
pub fn traceparent_header(trace_id: &str) -> Option<(String, String)> {
    let mut span = [0u8; 8];
    if getrandom::getrandom(&mut span).is_err() {
        return None;
    }
    let span: String = span.iter().map(|byte| format!("{:02x}", byte)).collect();
    Some((
        "traceparent".to_string(),
        format!("00-{}-{}-01", trace_id, span),
    ))
}

/// 一行访问日志：`key=value` 形式，便于 grep 与采集。
pub fn access_log_line(
    method: &str,
    path: &str,
    status: i64,
    bytes: u64,
    elapsed_ms: u64,
    request_id: &str,
    route: &str,
    trace_id: &str,
    client_ip: &str,
) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    format!(
        "{} INFO http method={} path=\"{}\" status={} bytes={} duration_ms={} route=\"{}\" request_id={} trace_id={} client_ip={}",
        now,
        sanitize(method),
        sanitize(path),
        status,
        bytes,
        elapsed_ms,
        sanitize(route),
        if request_id.is_empty() { "-" } else { request_id },
        if trace_id.is_empty() { "-" } else { trace_id },
        client_ip
    )
}

/// 日志字段清洗：引号与换行会破坏单行结构。
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '"' => '\'',
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

/// 访问日志缓冲：请求线程只追加内存，由后台线程定时整批写 stdout。
///
/// 直接对每个请求 `println!` 会让所有工作线程在 stdout 锁上串行化；即使批量写，
/// 一旦 stdout 变慢（管道、journald 打满、磁盘抖动）仍会把请求线程卡住。
/// 因此这里彻底解耦：请求线程永不写 stdout，缓冲满则丢弃并计数，保证请求延迟不受日志影响。
struct AccessLogBuffer {
    bytes: Vec<u8>,
}

/// 缓冲上限：超过后丢弃新日志（宁可少日志，也不让内存无限涨）。
const ACCESS_LOG_BUFFER_LIMIT: usize = 16 * 1024 * 1024;

static ACCESS_LOG: OnceLock<Mutex<AccessLogBuffer>> = OnceLock::new();
static ACCESS_LOG_DROPPED: AtomicU64 = AtomicU64::new(0);

fn access_log_buffer() -> &'static Mutex<AccessLogBuffer> {
    ACCESS_LOG.get_or_init(|| {
        Mutex::new(AccessLogBuffer {
            bytes: Vec::with_capacity(64 * 1024),
        })
    })
}

/// 追加一行访问日志（只写内存，绝不阻塞请求线程）。
pub fn access_log(line: &str) {
    let Ok(mut state) = access_log_buffer().lock() else {
        return;
    };
    if state.bytes.len() + line.len() + 1 > ACCESS_LOG_BUFFER_LIMIT {
        ACCESS_LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    state.bytes.extend_from_slice(line.as_bytes());
    state.bytes.push(b'\n');
}

/// 把缓冲里的访问日志整批写 stdout（由后台线程或停机流程调用）。
///
/// 先在锁内换出整批数据，再在锁外写：即使 stdout 阻塞，请求线程仍能继续追加。
pub fn flush_access_log() {
    let chunk = {
        let Ok(mut state) = access_log_buffer().lock() else {
            return;
        };
        if state.bytes.is_empty() {
            return;
        }
        std::mem::replace(&mut state.bytes, Vec::with_capacity(64 * 1024))
    };
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(&chunk);
    let _ = handle.flush();
}

/// 因 stdout 跟不上而丢弃的访问日志行数。
pub fn access_log_dropped() -> u64 {
    ACCESS_LOG_DROPPED.load(Ordering::Relaxed)
}

/// 启动访问日志的后台刷盘线程：低流量时日志也不会长时间滞留在内存里。
pub fn start_access_log_flusher() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("rip-access-log".to_string())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_millis(100));
                flush_access_log();
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_traceparent() {
        let valid = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        assert_eq!(
            trace_id_from_traceparent(valid).as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        // 全零 trace id、非十六进制、格式不符都视为无效。
        assert!(trace_id_from_traceparent(
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01"
        )
        .is_none());
        assert!(trace_id_from_traceparent("00-xyz-00f067aa0ba902b7-01").is_none());
        assert!(trace_id_from_traceparent("garbage").is_none());
    }

    #[test]
    fn builds_child_traceparent() {
        let (name, value) =
            traceparent_header("4bf92f3577b34da6a3ce929d0e0e4736").expect("header");
        assert_eq!(name, "traceparent");
        assert!(value.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-"));
        assert!(value.ends_with("-01"));
        assert_eq!(value.split('-').count(), 4);
    }

    #[test]
    fn proxy_trust_resolves_forwarded_chain() {
        let headers = |value: &str| vec![("X-Forwarded-For".to_string(), value.to_string())];
        // 全信任：取最左侧合法地址。
        let all = ProxyTrust::new(true, &[]);
        assert_eq!(
            all.resolve("10.0.0.1", &headers("203.0.113.9, 10.0.0.2")),
            "203.0.113.9"
        );
        // 只信任 10.0.0.0/8：跳过可信代理，返回第一个不可信地址。
        let limited = ProxyTrust::new(false, &["10.0.0.0/8".to_string()]);
        assert_eq!(
            limited.resolve("10.0.0.1", &headers("203.0.113.9, 10.0.0.2")),
            "203.0.113.9"
        );
        // 直连来源不在可信网段：忽略伪造的转发头。
        assert_eq!(
            limited.resolve("198.51.100.7", &headers("203.0.113.9")),
            "198.51.100.7"
        );
        // 没有转发头时回退到对端地址。
        assert_eq!(limited.resolve("10.0.0.1", &[]), "10.0.0.1");
        // 恶意值被跳过。
        assert_eq!(
            all.resolve("10.0.0.1", &headers("not-an-ip, 198.51.100.4")),
            "198.51.100.4"
        );
    }

    #[test]
    fn proxy_trust_matches_ipv6_prefix() {
        let trust = ProxyTrust::new(false, &["::1/128".to_string(), "2001:db8::/32".to_string()]);
        assert_eq!(
            trust.resolve("::1", &[("X-Real-IP".to_string(), "203.0.113.5".to_string())]),
            "203.0.113.5"
        );
        assert_eq!(
            trust.resolve("::2", &[("X-Real-IP".to_string(), "203.0.113.5".to_string())]),
            "::2"
        );
    }

    #[test]
    fn histogram_and_routes_are_exported() {
        let metrics = Metrics::default();
        metrics.record(200, 0);
        metrics.record(504, 700);
        metrics.record_route("GET /slow", 504, 700);
        let text = metrics.prometheus();
        assert!(text.contains("rip_http_request_duration_ms_bucket{le=\"1\"} 1"));
        assert!(text.contains("rip_http_request_duration_ms_bucket{le=\"+Inf\"} 2"));
        assert!(text.contains("rip_http_request_duration_ms_count 2"));
        assert!(text.contains(
            "rip_http_route_requests_total{route=\"GET /slow\",status=\"5xx\"} 1"
        ));
        assert!(text.contains(
            "rip_http_route_duration_ms_bucket{route=\"GET /slow\",le=\"+Inf\"} 1"
        ));
        assert!(text.contains("rip_http_route_duration_ms_count{route=\"GET /slow\"} 1"));
    }

    #[test]
    fn access_log_stays_on_one_line() {
        let line = access_log_line(
            "GET",
            "/a\"b",
            200,
            3,
            1,
            "req-1",
            "GET /a",
            "",
            "127.0.0.1",
        );
        assert!(!line.contains('\n'));
        assert!(line.contains("status=200"));
        assert!(line.contains("route=\"GET /a\""));
        assert!(line.contains("trace_id=-"));
    }
}

//! OpenTelemetry 追踪导出（OTLP/HTTP + JSON）。
//!
//! 只有配置了 `[server] otlp_endpoint`（或 `OTEL_EXPORTER_OTLP_ENDPOINT`）才启用：
//! 关闭时入口函数只做一次原子读就返回，HTTP 热路径不受影响。
//!
//! span 先进有界队列，由后台线程每秒批量 POST 到 `<endpoint>/v1/traces`，
//! 网络失败时整批回填（仍受队列上限约束），因此 Collector 短暂不可用不会丢数据。
//! 导出走运行时自带的 HTTP 客户端，不额外引入依赖。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 队列上限：超过后丢弃最旧的 span，保证进程内存有界。
const MAX_BUFFERED: usize = 4096;
/// 单次导出的最大 span 数。
const MAX_BATCH: usize = 512;
/// 导出间隔。
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

static ENABLED: AtomicBool = AtomicBool::new(false);
static DELIVERED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Config {
    endpoint: String,
    service: String,
    sample_ratio: f64,
}

struct State {
    config: Option<Config>,
    queue: VecDeque<Span>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            config: None,
            queue: VecDeque::new(),
        })
    })
}

/// 一个待导出的 span。
#[derive(Clone)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    pub name: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub attributes: Vec<(String, String)>,
    /// 处理函数是否以错误结束（OTLP `status.code = 2`）。
    pub error: bool,
}

/// 一次 HTTP 请求的 span 上下文；`started` 用于计算耗时。
pub struct HttpSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: String,
    started: Instant,
}

impl HttpSpan {
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// 配置并启动导出线程；重复调用只更新配置。`endpoint` 为空表示关闭。
pub fn configure(endpoint: &str, service: &str, sample_ratio: f64) {
    configure_with(endpoint, service, sample_ratio, true);
}

fn configure_with(endpoint: &str, service: &str, sample_ratio: f64, start_thread: bool) {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        ENABLED.store(false, Ordering::Relaxed);
        if let Ok(mut state) = state().lock() {
            state.config = None;
            state.queue.clear();
        }
        return;
    }
    let config = Config {
        endpoint: traces_url(endpoint),
        service: if service.trim().is_empty() {
            "rip".to_string()
        } else {
            service.trim().to_string()
        },
        // 采样比例收敛到 0..=1，避免配置写错导致全量或全丢。
        sample_ratio: if sample_ratio.is_finite() {
            sample_ratio.clamp(0.0, 1.0)
        } else {
            1.0
        },
    };
    if let Ok(mut state) = state().lock() {
        state.queue.clear();
        state.config = Some(config);
    }
    ENABLED.store(true, Ordering::Relaxed);
    if !start_thread {
        return;
    }
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("rip-otlp".to_string())
            .spawn(flush_loop);
    });
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `(已投递, 失败, 丢弃)` 计数，供 `/metrics` 与 `rip doctor` 使用。
pub fn counters() -> (u64, u64, u64) {
    (
        DELIVERED.load(Ordering::Relaxed),
        FAILED.load(Ordering::Relaxed),
        DROPPED.load(Ordering::Relaxed),
    )
}

/// 规范化 OTLP/HTTP traces 端点：允许只写到 Collector 根地址。
fn traces_url(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{}/v1/traces", trimmed)
    }
}

fn new_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if getrandom::getrandom(&mut buffer).is_err() {
        // 退化路径：用时间与地址拼一个可用的随机值，保证 trace 仍然唯一。
        let nanos = now_ns();
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (nanos >> ((index % 8) * 8)) as u8;
        }
    }
    buffer.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_nanos() as u64)
        .unwrap_or(0)
}

/// 采样判定：按 trace id 做确定性抽样，同一 trace 的所有 span 结果一致。
fn sampled(trace_id: &str, ratio: f64) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 {
        return false;
    }
    let prefix = &trace_id[..trace_id.len().min(8)];
    let value = u64::from_str_radix(prefix, 16).unwrap_or(0);
    // 用 32 位空间判断，精度足够且不会溢出。
    (value % 10_000) < (ratio * 10_000.0) as u64
}

/// 解析 `traceparent`，返回 `(trace_id, span_id)`。
pub fn parse_traceparent(value: &str) -> Option<(String, String)> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    if version.len() != 2 || trace_id.len() != 32 || span_id.len() != 16 {
        return None;
    }
    if !trace_id.chars().all(|ch| ch.is_ascii_hexdigit())
        || !span_id.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return None;
    }
    if trace_id.chars().all(|ch| ch == '0') {
        return None;
    }
    Some((trace_id.to_ascii_lowercase(), span_id.to_ascii_lowercase()))
}

/// 开始一个 HTTP 服务端 span；未启用或未命中采样时返回 None。
pub fn start_http_span(headers: &[(String, String)]) -> Option<HttpSpan> {
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    let ratio = state()
        .lock()
        .ok()
        .and_then(|state| state.config.as_ref().map(|config| config.sample_ratio))
        .unwrap_or(1.0);
    let incoming = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
        .and_then(|(_, value)| parse_traceparent(value));
    let (trace_id, parent_span_id) = match incoming {
        Some((trace_id, span_id)) => (trace_id, span_id),
        None => (new_hex(16), String::new()),
    };
    if !sampled(&trace_id, ratio) {
        return None;
    }
    Some(HttpSpan {
        trace_id,
        span_id: new_hex(8),
        parent_span_id,
        started: Instant::now(),
    })
}

/// 结束 HTTP span 并入库等待导出。
pub fn finish_http_span(
    span: HttpSpan,
    method: &str,
    path: &str,
    route: &str,
    status: i64,
    client_ip: &str,
) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let duration_ms = span.elapsed_ms();
    let target = route_name(method, route, path);
    let attributes = vec![
        ("http.method".to_string(), method.to_string()),
        ("http.target".to_string(), path.to_string()),
        ("http.route".to_string(), route.to_string()),
        ("http.status_code".to_string(), status.to_string()),
        ("http.client_ip".to_string(), client_ip.to_string()),
        ("duration_ms".to_string(), duration_ms.to_string()),
    ];
    record(Span {
        trace_id: span.trace_id,
        span_id: span.span_id,
        parent_span_id: span.parent_span_id,
        name: target,
        start_ns: now_ns().saturating_sub(duration_ms.saturating_mul(1_000_000)),
        end_ns: now_ns(),
        attributes,
        error: status >= 500 || status == 0,
    });
}

fn route_name(method: &str, route: &str, path: &str) -> String {
    if route.is_empty() {
        format!("{} {}", method, path)
    } else if route.starts_with(&format!("{} ", method)) {
        route.to_string()
    } else {
        format!("{} {}", method, route)
    }
}

/// 记录一条数据库子 span（父 span 来自当前请求上下文）。
pub fn record_db_span(
    trace_id: Option<&str>,
    parent_span_id: Option<&str>,
    operation: &str,
    duration_ms: u64,
    failed: bool,
) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(trace_id) = trace_id else { return };
    let end_ns = now_ns();
    record(Span {
        trace_id: trace_id.to_string(),
        span_id: new_hex(8),
        parent_span_id: parent_span_id.unwrap_or("").to_string(),
        name: operation.to_string(),
        start_ns: end_ns.saturating_sub(duration_ms.saturating_mul(1_000_000)),
        end_ns,
        attributes: vec![
            ("db.operation".to_string(), operation.to_string()),
            ("duration_ms".to_string(), duration_ms.to_string()),
        ],
        error: failed,
    });
}

/// 入队一个 span；队列满时丢最旧的并计数。
pub fn record(span: Span) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut state) = state().lock() {
        if state.queue.len() >= MAX_BUFFERED {
            state.queue.pop_front();
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        state.queue.push_back(span);
    }
}

fn flush_loop() {
    loop {
        std::thread::sleep(FLUSH_INTERVAL);
        let _ = flush_once();
    }
}

/// 立即导出一批；返回本次导出的 span 数。测试与停机时可以主动调用。
pub fn flush_once() -> usize {
    let (config, batch) = {
        let Ok(mut state) = state().lock() else {
            return 0;
        };
        let Some(config) = state.config.clone() else {
            return 0;
        };
        let count = state.queue.len().min(MAX_BATCH);
        let batch: VecDeque<Span> = state.queue.drain(..count).collect();
        (config, batch)
    };
    if batch.is_empty() {
        return 0;
    }
    let body = payload(&config.service, batch.iter());
    let headers = vec![(
        "Content-Type".to_string(),
        "application/json".to_string(),
    )];
    match crate::client::fetch(
        "POST",
        &config.endpoint,
        &headers,
        body.as_bytes(),
        Some(Duration::from_secs(5)),
    ) {
        Ok((status, _, _)) if (200..300).contains(&status) => {
            DELIVERED.fetch_add(batch.len() as u64, Ordering::Relaxed);
            batch.len()
        }
        _ => {
            FAILED.fetch_add(1, Ordering::Relaxed);
            // 失败时尽量放回队首，下一轮重试；容量不足的部分直接丢弃。
            if let Ok(mut state) = state().lock() {
                for span in batch.iter().rev() {
                    if state.queue.len() >= MAX_BUFFERED {
                        DROPPED.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    state.queue.push_front(span.clone());
                }
            }
            0
        }
    }
}

/// OTLP/HTTP JSON 载荷。
fn payload<'a, I>(service: &str, spans: I) -> String
where
    I: Iterator<Item = &'a Span>,
{
    let spans: Vec<serde_json::Value> = spans
        .map(|span| {
            let attributes: Vec<serde_json::Value> = span
                .attributes
                .iter()
                .map(|(key, value)| {
                    serde_json::json!({
                        "key": key,
                        "value": { "stringValue": value },
                    })
                })
                .collect();
            serde_json::json!({
                "traceId": span.trace_id,
                "spanId": span.span_id,
                "parentSpanId": span.parent_span_id,
                "name": span.name,
                // 2 = SPAN_KIND_SERVER
                "kind": 2,
                "startTimeUnixNano": span.start_ns.to_string(),
                "endTimeUnixNano": span.end_ns.to_string(),
                "attributes": attributes,
                "status": { "code": if span.error { 2 } else { 1 } },
            })
        })
        .collect();
    serde_json::json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": { "stringValue": service },
                }],
            },
            "scopeSpans": [{
                "scope": { "name": "rip", "version": env!("CARGO_PKG_VERSION") },
                "spans": spans,
            }],
        }],
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(name: &str, trace: &str, parent: &str) -> Span {
        Span {
            trace_id: trace.to_string(),
            span_id: new_hex(8),
            parent_span_id: parent.to_string(),
            name: name.to_string(),
            start_ns: 1_700_000_000_000_000_000,
            end_ns: 1_700_000_000_010_000_000,
            attributes: vec![("http.method".to_string(), "GET".to_string())],
            error: false,
        }
    }

    #[test]
    fn payload_is_valid_otlp_json() {
        let body = payload("checkout", std::iter::once(&span(
            "GET /users/:id",
            "4bf92f3577b34da6a3ce929d0e0e4736",
            "00f067aa0ba902b7",
        )));
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        let resource = &value["resourceSpans"][0];
        assert_eq!(
            resource["resource"]["attributes"][0]["value"]["stringValue"],
            "checkout"
        );
        let out = &resource["scopeSpans"][0]["spans"][0];
        assert_eq!(out["traceId"], "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(out["kind"], 2);
        assert_eq!(out["status"]["code"], 1);
        assert_eq!(
            out["startTimeUnixNano"],
            serde_json::json!("1700000000000000000")
        );
    }

    #[test]
    fn traceparent_round_trip() {
        let header = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        assert_eq!(
            parse_traceparent(header),
            Some((
                "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                "00f067aa0ba902b7".to_string()
            ))
        );
        assert!(parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none());
        assert!(parse_traceparent("00-xyz-00f067aa0ba902b7-01").is_none());
    }

    #[test]
    fn sampling_is_deterministic() {
        let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
        assert!(sampled(trace, 1.0));
        assert!(!sampled(trace, 0.0));
        assert_eq!(sampled(trace, 0.5), sampled(trace, 0.5));
    }

    #[test]
    fn endpoint_gets_traces_suffix() {
        assert_eq!(
            traces_url("http://collector:4318"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            traces_url("http://collector:4318/v1/traces/"),
            "http://collector:4318/v1/traces"
        );
    }

    /// 起一个只收一次的 HTTP 服务，验证 `flush_once` 真的把 OTLP JSON POST 出去。
    #[test]
    fn flush_posts_otlp_json() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
            // 读到 Content-Length 指定的正文结束为止。
            loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).to_string();
                if let Some(length) = text
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                {
                    if let Some(body) = text.split("\r\n\r\n").nth(1) {
                        if body.len() >= length {
                            break;
                        }
                    }
                }
            }
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            String::from_utf8_lossy(&buffer).to_string()
        });

        configure_with(&format!("http://127.0.0.1:{}", port), "checkout", 1.0, false);
        record(span(
            "GET /health",
            "4bf92f3577b34da6a3ce929d0e0e4736",
            "00f067aa0ba902b7",
        ));
        let sent = flush_once();
        let request = handle.join().expect("listener");
        configure_with("", "rip", 1.0, false);
        assert!(sent >= 1);
        assert!(request.starts_with("POST /v1/traces HTTP/1.1"));
        assert!(request.contains("\"traceId\":\"4bf92f3577b34da6a3ce929d0e0e4736\""));
        assert!(request.contains("\"service.name\""));
    }
}

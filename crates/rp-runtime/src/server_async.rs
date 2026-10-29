//! 事件驱动 HTTP 服务端（明文 HTTP），thread-per-core 架构。
//!
//! 每条 worker 线程自建一个 current_thread tokio runtime + LocalSet：
//! acceptor（主线程）把连接分配到负载最低的 worker，此后该连接的
//! 读取、处理函数执行、响应写出全程在本线程内联完成，
//! 每请求零跨线程交接（旧架构的 Job 投递 + oneshot 回约 66µs 归零）。
//!
//! 取舍：处理函数（含同步 DB 调用）阻塞当前线程的事件循环，
//! 并发度等于线程数；换来的是每请求 CPU 与内存占用的显著下降。
//!
//! TLS 仍走同步实现（`builtins::serve_http` 的旧路径），避免引入新的 TLS 依赖。

use std::cell::{Cell, RefCell};
use std::net::TcpListener as StdTcpListener;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};

use crate::builtins::{
    build_request, is_header_name, is_header_value, is_reserved_header, next_request_id,
    parse_content_length, parse_transfer_encoding, response_parts, status_reason,
    HttpRequestHead, ResponseParts,
};
use crate::server::{HandlerSpec, Metrics, ServerConfig};
use crate::value::{Flow, Value};

/// 调试开关：设置 RIP_HTTP_DEBUG 后输出连接与任务轨迹。
fn debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RIP_HTTP_DEBUG").is_some())
}

/// 惰性调试输出：未开启时不求值 `format!`（避免每请求的无谓分配）。
macro_rules! debug {
    ($($arg:tt)*) => {
        if debug_enabled() {
            eprintln!("[http-debug] {}", format_args!($($arg)*));
        }
    };
}

/// 阶段耗时诊断：`RIP_STAGE_TIMING=1` 时按阶段累计纳秒，每 20000 请求打印一次。
/// 关闭时每次只多一个原子读，不影响生产路径。
/// thread-per-core 后不再有 dispatch/reply：请求全程内联。
static STAGE_READ_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_BUILD_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_HANDLER_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_PARTS_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_WRITE_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_COUNT: AtomicU64 = AtomicU64::new(0);

/// CPU 时间归因：`RIP_STAGE_CPU=1` 时按阶段累计「当前线程 CPU 纳秒」（内核+用户）。
/// 与 wall 计时互补：wall 看排队，CPU 看线程真正烧在哪里。
static STAGE_CPU_READ_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_CPU_SYNC_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_CPU_WRITE_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_CPU_CYCLE_NS: AtomicU64 = AtomicU64::new(0);

fn cpu_timing_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RIP_STAGE_CPU").as_deref() == Ok("1"))
}

extern "system" {
    fn GetCurrentThread() -> *mut core::ffi::c_void;
    fn GetThreadTimes(
        thread: *mut core::ffi::c_void,
        creation: *mut u64,
        exit: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
}

/// 当前线程已消耗的 CPU 纳秒（FILETIME 为 100ns 单位，小端 u64 即高低位合并值）。
fn thread_cpu_ns() -> u64 {
    let mut creation = 0u64;
    let mut exit = 0u64;
    let mut kernel = 0u64;
    let mut user = 0u64;
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return 0;
    }
    (kernel + user) * 100
}

fn cpu_now() -> u64 {
    if cpu_timing_enabled() {
        thread_cpu_ns()
    } else {
        0
    }
}

fn cpu_add(slot: &AtomicU64, started: u64) {
    if cpu_timing_enabled() {
        slot.fetch_add(thread_cpu_ns().saturating_sub(started), Ordering::Relaxed);
    }
}

/// 当前线程 CPU 的内核/用户拆分（纳秒）。
fn thread_cpu_split_ns() -> (u64, u64) {
    let mut creation = 0u64;
    let mut exit = 0u64;
    let mut kernel = 0u64;
    let mut user = 0u64;
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return (0, 0);
    }
    (kernel * 100, user * 100)
}

/// 真实 CPU 总量（无窗口重叠双计）：每个 worker 只累加自己线程自上次
/// 请求以来的 CPU 增量，跨 worker 求和 = 全部 worker 线程的真实 CPU。
static STAGE_CPU_KERNEL_NS: AtomicU64 = AtomicU64::new(0);
static STAGE_CPU_USER_NS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static LAST_THREAD_CPU: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

fn account_thread_cpu() {
    let (k, u) = thread_cpu_split_ns();
    LAST_THREAD_CPU.with(|slot| {
        let (pk, pu) = slot.get();
        if pk != 0 {
            STAGE_CPU_KERNEL_NS.fetch_add(k.saturating_sub(pk), Ordering::Relaxed);
            STAGE_CPU_USER_NS.fetch_add(u.saturating_sub(pu), Ordering::Relaxed);
        }
        slot.set((k, u));
    });
}

fn stage_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RIP_STAGE_TIMING").as_deref() == Ok("1"))
}

fn stage_add(slot: &AtomicU64, started: Instant) {
    if stage_enabled() {
        slot.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

fn stage_report() {
    if !stage_enabled() {
        return;
    }
    let n = STAGE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if cpu_timing_enabled() {
        account_thread_cpu();
    }
    if n % 20000 == 0 {
        let us = |slot: &AtomicU64| slot.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0;
        eprintln!(
            "[stage] n={} read={:.1}us build={:.1}us handler={:.1}us parts={:.1}us write={:.1}us",
            n,
            us(&STAGE_READ_NS),
            us(&STAGE_BUILD_NS),
            us(&STAGE_HANDLER_NS),
            us(&STAGE_PARTS_NS),
            us(&STAGE_WRITE_NS),
        );
        if cpu_timing_enabled() {
            eprintln!(
                "[stagecpu] n={} read={:.1}us sync={:.1}us write={:.1}us cycle={:.1}us true={:.1}us kernel={:.1}us user={:.1}us",
                n,
                us(&STAGE_CPU_READ_NS),
                us(&STAGE_CPU_SYNC_NS),
                us(&STAGE_CPU_WRITE_NS),
                us(&STAGE_CPU_CYCLE_NS),
                us(&STAGE_CPU_KERNEL_NS) + us(&STAGE_CPU_USER_NS),
                us(&STAGE_CPU_KERNEL_NS),
                us(&STAGE_CPU_USER_NS),
            );
        }
    }
}

enum ReadOutcome {
    Request(Box<HttpRequestHead>),
    Closed,
    TooLarge,
    BadRequest(String),
}

enum LineRead {
    Line(String),
    Eof,
    TooLong,
}

/// 一条被分配到某个 worker 线程的连接。字段全部 `Send`，交接只发生一次。
struct Conn {
    stream: TcpStream,
    peer: String,
    /// 并发上限许可：随连接一起转移，连接结束时释放。
    permit: tokio::sync::OwnedSemaphorePermit,
    index: usize,
}

/// worker 线程的共享状态：解释器运行时 + 每线程只读的配置。
///
/// `Rc` 而非 `Arc`：所有访问都发生在同一线程的 LocalSet 任务里。
struct WorkerState {
    runtime: RefCell<crate::rt::Runtime>,
    handler: Value,
    handler_label: String,
    metrics: Arc<Metrics>,
    config: Arc<ServerConfig>,
    limiter: Option<Arc<crate::server::RateLimiter>>,
    trust: Arc<crate::server::ProxyTrust>,
    /// 本线程正在服务的连接数（drain 等待用）。
    active: Cell<usize>,
    conn_counts: Arc<Vec<AtomicUsize>>,
    index: usize,
}

/// 连接计数守卫：进入 +1、退出 -1，panic 展开也能正确回退。
struct ConnGuard {
    worker: Rc<WorkerState>,
}

impl ConnGuard {
    fn new(worker: Rc<WorkerState>) -> Self {
        worker.active.set(worker.active.get() + 1);
        // acceptor 发送时 +1 的排队计数：连接真正落到本线程后抵消。
        worker.conn_counts[worker.index].fetch_sub(1, Ordering::Relaxed);
        worker.metrics.active_connections.fetch_add(1, Ordering::Relaxed);
        ConnGuard { worker }
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.worker.active.set(self.worker.active.get().saturating_sub(1));
        self.worker.metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 启动 thread-per-core 服务器；返回时说明服务已停止或启动失败。
pub(crate) fn run(
    listener: StdTcpListener,
    config: ServerConfig,
    spec: HandlerSpec,
    metrics: Arc<Metrics>,
    limiter: Option<Arc<crate::server::RateLimiter>>,
    trust: Arc<crate::server::ProxyTrust>,
    seed: crate::rt::RuntimeSeed,
    language: crate::i18n::Language,
) -> Result<(), String> {
    let workers = config.worker_count();
    crate::server::start_access_log_flusher();
    let conn_counts: Arc<Vec<AtomicUsize>> =
        Arc::new((0..workers).map(|_| AtomicUsize::new(0)).collect());
    let limits = Arc::new(Semaphore::new(config.max_connections.max(16)));
    let config = Arc::new(config);
    let mut senders = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);
    for index in 0..workers {
        let (sender, receiver) = mpsc::channel::<Conn>(256);
        senders.push(sender);
        let worker_seed = seed.clone();
        let worker_spec = spec.clone();
        let worker_metrics = Arc::clone(&metrics);
        let worker_config = Arc::clone(&config);
        let worker_limiter = limiter.clone();
        let worker_trust = Arc::clone(&trust);
        let worker_counts = Arc::clone(&conn_counts);
        let handle = std::thread::Builder::new()
            .name(format!("rip-http-{}", index))
            .spawn(move || {
                worker_loop(
                    receiver,
                    worker_seed,
                    language,
                    worker_spec,
                    worker_metrics,
                    worker_config,
                    worker_limiter,
                    worker_trust,
                    worker_counts,
                    index,
                );
            })
            .map_err(|error| format!("cannot start http worker {}: {}", index, error))?;
        handles.push(handle);
    }

    // acceptor：单线程 runtime 足够，只做 accept 与分发。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start async runtime: {}", error))?;

    runtime.block_on(async move {
        // tokio 要求监听 socket 处于非阻塞模式，否则每条连接都会阻塞一个异步线程。
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("cannot set listener nonblocking: {}", error))?;
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|error| format!("cannot use listener: {}", error))?;
        let mut next = 0usize;
        loop {
            // 先看停机标志再 accept：高连接速率时也不会漏掉停机请求。
            if crate::server::shutdown_requested() {
                break;
            }
            // 100ms 只是停机检查的粒度；有连接时 accept 立即返回，不影响延迟。
            let accepted = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
            let (stream, address) = match accepted {
                Ok(Ok((stream, address))) => (stream, address),
                Ok(Err(error)) => {
                    eprintln!("[http] accept error: {}", error);
                    continue;
                }
                Err(_) => continue,
            };
            let permit = match Arc::clone(&limits).try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    metrics.rejected.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        reject(stream).await;
                    });
                    continue;
                }
            };
            let index = least_loaded(&conn_counts, next);
            next = next.wrapping_add(1);
            let conn = Conn {
                stream,
                peer: address.ip().to_string(),
                permit,
                index,
            };
            conn_counts[index].fetch_add(1, Ordering::Relaxed);
            // worker 初始化失败（receiver 已 drop）或队列满：直接拒绝，不排队。
            if let Err(error) = senders[index].try_send(conn) {
                conn_counts[index].fetch_sub(1, Ordering::Relaxed);
                metrics.rejected.fetch_add(1, Ordering::Relaxed);
                let conn = error.into_inner();
                tokio::spawn(async move {
                    reject(conn.stream).await;
                });
            }
        }
        // 释放发送端：各 worker 收完队列里的连接后进入 drain 并退出。
        drop(senders);
        Ok::<(), String>(())
    })?;

    for handle in handles {
        let _ = handle.join();
    }
    crate::server::flush_access_log();
    Ok(())
}

/// 选排队+在途连接最少的工作线程，起点轮询避免总是压同一个。
fn least_loaded(counts: &[AtomicUsize], start: usize) -> usize {
    let len = counts.len();
    if len <= 1 {
        return 0;
    }
    let start = start % len;
    let mut best = start;
    let mut best_load = counts[start].load(Ordering::Relaxed);
    for offset in 1..len {
        let index = (start + offset) % len;
        let load = counts[index].load(Ordering::Relaxed);
        if load < best_load {
            best = index;
            best_load = load;
        }
    }
    best
}

/// worker 线程：自建 current_thread runtime + LocalSet，连接全程在本线程处理。
#[allow(clippy::too_many_arguments)]
fn worker_loop(
    mut conn_rx: mpsc::Receiver<Conn>,
    seed: crate::rt::RuntimeSeed,
    language: crate::i18n::Language,
    spec: HandlerSpec,
    metrics: Arc<Metrics>,
    config: Arc<ServerConfig>,
    limiter: Option<Arc<crate::server::RateLimiter>>,
    trust: Arc<crate::server::ProxyTrust>,
    conn_counts: Arc<Vec<AtomicUsize>>,
    index: usize,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[http] cannot start worker runtime: {}", error);
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let mut interp = seed.build();
        interp.set_language(language);
        interp.set_server_config((*config).clone());
        interp.metrics = Some(Arc::clone(&metrics));
        if let Err(error) = interp.init_entry() {
            eprintln!("[http] worker init failed: {}", error.message);
            return;
        }
        let handler = match interp.handler_from_spec(&spec) {
            Some(handler) => handler,
            None => {
                eprintln!("[http] worker cannot resolve handler");
                return;
            }
        };
        // 非路由处理函数（普通函数 / 静态目录）用名字作为路由标签。
        let handler_label = match &handler {
            Value::Func(func) => func
                .name
                .trim_matches('<')
                .trim_matches('>')
                .to_string(),
            _ => "handler".to_string(),
        };
        let worker = Rc::new(WorkerState {
            runtime: RefCell::new(interp),
            handler,
            handler_label,
            metrics,
            config,
            limiter,
            trust,
            active: Cell::new(0),
            conn_counts,
            index,
        });
        while let Some(conn) = conn_rx.recv().await {
            let worker = Rc::clone(&worker);
            tokio::task::spawn_local(async move {
                handle_connection(conn, worker).await;
            });
        }
        // drain：等本线程在途连接自然结束（连接循环见停机标志会自行退出）。
        let deadline = Instant::now() + worker.config.drain_timeout;
        while worker.active.get() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

/// 单条连接的处理循环：读取、内联执行处理函数、写出，全程同线程。
async fn handle_connection(conn: Conn, worker: Rc<WorkerState>) {
    let _guard = ConnGuard::new(Rc::clone(&worker));
    let Conn {
        stream,
        peer,
        permit: _permit,
        index: _index,
    } = conn;
    let config = Arc::clone(&worker.config);
    let metrics = Arc::clone(&worker.metrics);
    debug!("conn start {}", peer);
    let _ = stream.set_nodelay(true);
    let mut reader = BufReader::new(stream);
    let mut served = 0usize;
    loop {
        let cycle_cpu0 = cpu_now();
        if crate::server::shutdown_requested() {
            break;
        }
        // 首个请求用 read_timeout，之后用 keep_alive 空闲超时。
        let idle = if served == 0 {
            config.read_timeout
        } else {
            config.keep_alive
        };
        let read_cpu0 = cpu_now();
        let outcome = match tokio::time::timeout(idle, async {
            let read_started = Instant::now();
            let outcome = read_request(&mut reader, &config).await;
            stage_add(&STAGE_READ_NS, read_started);
            outcome
        })
        .await {
            Ok(outcome) => outcome,
            Err(_) => break,
        };
        cpu_add(&STAGE_CPU_READ_NS, read_cpu0);
        let head = match outcome {
            ReadOutcome::Request(head) => head,
            ReadOutcome::Closed => break,
            ReadOutcome::TooLarge => {
                let mut parts = text_parts(413, "payload too large");
                let _ = tokio::time::timeout(
                    config.write_timeout,
                    write_response(reader.get_mut(), &mut parts, false, false, ""),
                )
                .await;
                break;
            }
            ReadOutcome::BadRequest(message) => {
                let mut parts = text_parts(400, &message);
                let _ = tokio::time::timeout(
                    config.write_timeout,
                    write_response(reader.get_mut(), &mut parts, false, false, ""),
                )
                .await;
                break;
            }
        };
        let request_id = head
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("x-request-id"))
            .map(|(_, value)| value.clone())
            // 客户端给的 ID 会原样写进响应头，必须过滤 CR/LF 与超长值。
            .filter(|value| !value.is_empty() && value.len() <= 200 && is_header_value(value))
            .unwrap_or_else(next_request_id);
        let head_only = head.method.eq_ignore_ascii_case("HEAD");
        // 反向代理之后用 X-Forwarded-For / X-Real-IP 还原真实客户端 IP。
        let client_ip = worker.trust.resolve(&peer, &head.headers);
        let keep_alive = head.keep_alive
            && served + 1 < config.max_requests
            && !crate::server::shutdown_requested();

        let mut parts;
        if !config.health_path.is_empty() && head.path == config.health_path {
            parts = text_parts(200, "ok");
        } else if !config.ready_path.is_empty() && head.path == config.ready_path {
            parts = if crate::server::shutdown_requested() {
                text_parts(503, "shutting down")
            } else {
                text_parts(200, "ready")
            };
        } else if !config.metrics_path.is_empty() && head.path == config.metrics_path {
            parts = metrics_parts(&metrics.prometheus());
        } else if worker
            .limiter
            .as_ref()
            .is_some_and(|limiter| !limiter.allow(&client_ip))
        {
            // 超过配额：直接 429，不进入处理函数。
            metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
            parts = rate_limited_parts();
        } else {
            debug!("inline {} {}", head.method, head.path);
            let sync_cpu0 = cpu_now();
            let started = Instant::now();
            let method = head.method.clone();
            let path = head.path.clone();
            let mut span;
            let trace_id;
            let accept_encoding;
            let route;
            let mut sse_producer;
            {
                // 借用解释器：borrow 作用域内绝不 await（会跨任务持有借用）。
                let mut rt = worker.runtime.borrow_mut();
                // 追踪：命中采样时创建服务端 span，并把 trace/span id 注入请求上下文。
                span = crate::trace::start_http_span(&head.headers);
                trace_id = span
                    .as_ref()
                    .map(|span| span.trace_id.clone())
                    .or_else(|| {
                        head.headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
                            .and_then(|(_, value)| {
                                crate::server::trace_id_from_traceparent(value)
                            })
                    });
                accept_encoding = head
                    .headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("accept-encoding"))
                    .map(|(_, value)| value.clone());
                let build_started = Instant::now();
                let request = build_request(*head, client_ip.clone(), &request_id);
                stage_add(&STAGE_BUILD_NS, build_started);
                rt.current_request_id = Some(request_id.clone());
                rt.current_trace_id = trace_id.clone();
                rt.current_span_id = span.as_ref().map(|span| span.span_id.clone());
                rt.current_route = None;
                rt.set_deadline(if config.request_timeout.is_zero() {
                    None
                } else {
                    Some(Instant::now() + config.request_timeout)
                });
                metrics.in_flight.fetch_add(1, Ordering::Relaxed);
                let handler_started = Instant::now();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match &worker.handler {
                        Value::Func(func) => rt.call_value(func, vec![request], 0),
                        _ => Ok(Value::Null),
                    }
                }));
                stage_add(&STAGE_HANDLER_NS, handler_started);
                metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                let timed_out = rt.timed_out();
                route = rt
                    .current_route
                    .take()
                    .unwrap_or_else(|| worker.handler_label.clone());
                rt.clear_deadline();
                rt.current_request_id = None;
                rt.current_trace_id = None;
                rt.current_span_id = None;
                sse_producer = None;
                let parts_started = Instant::now();
                parts = if timed_out {
                    text_parts(504, "gateway timeout")
                } else {
                    match outcome {
                        Ok(Ok(response)) => {
                            sse_producer = crate::builtins::take_sse(&response);
                            response_parts(&response)
                        }
                        // 内部错误只写服务端日志，客户端拿到通用 500。
                        Ok(Err(Flow::Error(error))) => {
                            eprintln!("[http] {} handler error: {}", request_id, error.message);
                            error_parts("internal server error")
                        }
                        Ok(Err(_)) => error_parts("handler failed"),
                        Err(_) => {
                            // panic 展开会跳过正常清理，这里重置调用栈与请求 ID。
                            rt.reset_after_panic();
                            eprintln!("[http] {} handler panicked", request_id);
                            error_parts("handler panicked")
                        }
                    }
                };
                stage_add(&STAGE_PARTS_NS, parts_started);
                cpu_add(&STAGE_CPU_SYNC_NS, sync_cpu0);
            }
            crate::builtins::maybe_compress(&mut parts, accept_encoding.as_deref(), &method);
            let elapsed = started.elapsed().as_millis() as u64;
            metrics.record(parts.status, elapsed);
            metrics.record_route(&route, parts.status, elapsed);
            if config.access_log {
                crate::server::access_log(&crate::server::access_log_line(
                    &method,
                    &path,
                    parts.status,
                    parts.content_length(),
                    elapsed,
                    &request_id,
                    &route,
                    trace_id.as_deref().unwrap_or(""),
                    &client_ip,
                ));
            }
            if let Some(producer) = sse_producer {
                // SSE：只写响应头，随后把 socket 写半部交给 sink，
                // 生产函数在本线程逐帧直写；流结束即断开连接。
                parts = crate::builtins::into_sse_parts(parts);
                let std_stream = match reader.into_inner().into_std() {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                // tokio 的 socket 是非阻塞的：into_std 后切回阻塞写，并设写超时。
                let _ = std_stream.set_nonblocking(false);
                let _ = std_stream.set_write_timeout(Some(config.write_timeout));
                let mut std_stream = std_stream;
                if crate::builtins::write_parts(
                    &mut std_stream,
                    &mut parts,
                    false,
                    head_only,
                    &request_id,
                )
                .is_err()
                {
                    return;
                }
                let sink = Rc::new(crate::value::SseSink::new(std_stream));
                {
                    let mut rt = worker.runtime.borrow_mut();
                    crate::builtins::run_sse_producer(&mut rt, producer, sink, &request_id);
                }
                // 流式响应的 span 覆盖整个推送过程，而不是只到处理函数返回。
                if let Some(span) = span.take() {
                    crate::trace::finish_http_span(span, &method, &path, &route, 200, &client_ip);
                }
                crate::gc::collect_if_due();
                stage_report();
                return;
            }
            if let Some(span) = span.take() {
                crate::trace::finish_http_span(
                    span,
                    &method,
                    &path,
                    &route,
                    parts.status,
                    &client_ip,
                );
            }
            // 写超时：客户端不读响应时不能让连接一直占着并发许可（慢读攻击）。
            let write_cpu0 = cpu_now();
            let write_started = Instant::now();
            let written = tokio::time::timeout(
                config.write_timeout,
                write_response(reader.get_mut(), &mut parts, keep_alive, head_only, &request_id),
            )
            .await;
            match written {
                Ok(Ok(())) => {
                    stage_add(&STAGE_WRITE_NS, write_started);
                    cpu_add(&STAGE_CPU_WRITE_NS, write_cpu0);
                }
                _ => break,
            }
            // 请求结束时按分配间隔回收环：无环时这一步几乎零开销。
            crate::gc::collect_if_due();
            cpu_add(&STAGE_CPU_CYCLE_NS, cycle_cpu0);
            served += 1;
            stage_report();
            if !keep_alive {
                break;
            }
            continue;
        }
        // 内置探针/限流的快速路径：不经过解释器，直接写响应。
        let write_cpu0 = cpu_now();
        let write_started = Instant::now();
        let written = tokio::time::timeout(
            config.write_timeout,
            write_response(reader.get_mut(), &mut parts, keep_alive, head_only, &request_id),
        )
        .await;
        match written {
            Ok(Ok(())) => {
                stage_add(&STAGE_WRITE_NS, write_started);
                cpu_add(&STAGE_CPU_WRITE_NS, write_cpu0);
            }
            _ => break,
        }
        cpu_add(&STAGE_CPU_CYCLE_NS, cycle_cpu0);
        served += 1;
        stage_report();
        if !keep_alive {
            break;
        }
    }
    let _ = reader.get_mut().shutdown().await;
}

/// 超出连接上限时直接回 503。
async fn reject(mut stream: TcpStream) {
    let _ = stream
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 20\r\nConnection: close\r\nRetry-After: 1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nserver is overloaded",
        )
        .await;
    let _ = stream.shutdown().await;
}

fn error_parts(message: &str) -> ResponseParts {
    ResponseParts {
        status: 500,
        headers: vec![(
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body: message.as_bytes().to_vec(),
        file: None,
        streaming: false,
    }
}

fn text_parts(status: i64, body: &str) -> ResponseParts {
    ResponseParts {
        status,
        headers: vec![(
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body: body.as_bytes().to_vec(),
        file: None,
        streaming: false,
    }
}

/// 429 响应：带 `Retry-After`，告诉客户端一秒后重试。
fn rate_limited_parts() -> ResponseParts {
    ResponseParts {
        status: 429,
        headers: vec![
            (
                "Content-Type".to_string(),
                "text/plain; charset=utf-8".to_string(),
            ),
            ("Retry-After".to_string(), "1".to_string()),
        ],
        body: b"too many requests".to_vec(),
        file: None,
        streaming: false,
    }
}

fn metrics_parts(text: &str) -> ResponseParts {
    ResponseParts {
        status: 200,
        headers: vec![(
            "Content-Type".to_string(),
            "text/plain; version=0.0.4; charset=utf-8".to_string(),
        )],
        body: text.as_bytes().to_vec(),
        file: None,
        streaming: false,
    }
}

async fn write_response(
    stream: &mut TcpStream,
    parts: &mut ResponseParts,
    keep_alive: bool,
    head_only: bool,
    request_id: &str,
) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: {}\r\n",
        parts.status,
        status_reason(parts.status),
        parts.content_length(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    if !request_id.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(out, "X-Request-Id: {}\r\n", request_id);
    }
    for (key, value) in &parts.headers {
        // 保留头由服务器统一写入；非法名/值直接丢弃，防止响应头注入与响应拆分。
        if is_reserved_header(key) || !is_header_name(key) || !is_header_value(value) {
            continue;
        }
        use std::fmt::Write as _;
        let _ = write!(out, "{}: {}\r\n", key, value);
    }
    out.push_str("\r\n");
    // 小响应把头与体合并成一次 write：热路径少一次系统调用，P99 更稳。
    if !head_only && parts.file.is_none() && parts.body.len() <= 64 * 1024 {
        let mut buffer = Vec::with_capacity(out.len() + parts.body.len());
        buffer.extend_from_slice(out.as_bytes());
        buffer.extend_from_slice(&parts.body);
        stream.write_all(&buffer).await?;
        return stream.flush().await;
    }
    stream.write_all(out.as_bytes()).await?;
    if head_only {
        return stream.flush().await;
    }
    match &parts.file {
        // 静态文件：按 offset/length 边读边写，64KB 缓冲，内存占用与文件大小无关。
        Some(file) => write_file_part(stream, file).await?,
        None if !parts.body.is_empty() => stream.write_all(&parts.body).await?,
        None => {}
    }
    stream.flush().await
}

/// 从磁盘流式写出文件片段（支持 Range 的偏移与长度）。
async fn write_file_part(stream: &mut TcpStream, part: &crate::builtins::FilePart) -> std::io::Result<()> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt as _};
    let mut file = tokio::fs::File::open(&part.path).await?;
    if part.offset > 0 {
        file.seek(std::io::SeekFrom::Start(part.offset)).await?;
    }
    let mut remaining = part.length;
    let mut buffer = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = (buffer.len() as u64).min(remaining) as usize;
        let read = file.read(&mut buffer[..want]).await?;
        if read == 0 {
            // 文件在发送过程中被截断：只能提前结束，连接随后会被关闭。
            break;
        }
        stream.write_all(&buffer[..read]).await?;
        remaining -= read as u64;
    }
    Ok(())
}

/// 读取一行，带长度上限。
async fn read_line_limited(reader: &mut BufReader<TcpStream>, limit: usize) -> LineRead {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let available = match reader.fill_buf().await {
                Ok(buffer) => buffer,
                Err(_) => return LineRead::Eof,
            };
            if available.is_empty() {
                return LineRead::Eof;
            }
            match available.iter().position(|byte| *byte == b'\n') {
                // 直接从内部缓冲拷贝到行缓冲：不再为每行分配一个中间 Vec。
                Some(index) => {
                    out.extend_from_slice(&available[..=index]);
                    (true, index + 1)
                }
                None => {
                    out.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        reader.consume(used);
        if out.len() > limit {
            return LineRead::TooLong;
        }
        if done {
            return LineRead::Line(
                String::from_utf8_lossy(&out)
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
            );
        }
    }
}

async fn read_chunked(reader: &mut BufReader<TcpStream>, max: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let line = match read_line_limited(reader, 1024).await {
            LineRead::Line(line) => line,
            _ => return None,
        };
        let size_text = line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        if size == 0 {
            loop {
                match read_line_limited(reader, 8 * 1024).await {
                    LineRead::Line(trailer) if trailer.trim().is_empty() => break,
                    LineRead::Line(_) => continue,
                    _ => break,
                }
            }
            return Some(out);
        }
        // 长度用 checked_add：分块长度由客户端给出，直接相加会溢出后绕过上限。
        let total = match out.len().checked_add(size) {
            Some(total) if total <= max => total,
            _ => return None,
        };
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk).await.ok()?;
        out.extend_from_slice(&chunk);
        debug_assert_eq!(out.len(), total);
        let _ = read_line_limited(reader, 8).await;
    }
}

async fn read_request(
    reader: &mut BufReader<TcpStream>,
    config: &ServerConfig,
) -> ReadOutcome {
    let request_line = match read_line_limited(reader, 8 * 1024).await {
        LineRead::Line(line) => line,
        LineRead::Eof => return ReadOutcome::Closed,
        LineRead::TooLong => {
            return ReadOutcome::BadRequest("request line too long".to_string())
        }
    };
    if request_line.trim().is_empty() {
        return ReadOutcome::Closed;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let path = match target.split_once('?') {
        Some((path, _)) => path.to_string(),
        None => target.clone(),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut header_bytes = 0usize;
    loop {
        let line = match read_line_limited(reader, config.max_header_bytes).await {
            LineRead::Line(line) => line,
            LineRead::Eof => {
                return ReadOutcome::BadRequest("unexpected end of headers".to_string())
            }
            LineRead::TooLong => return ReadOutcome::TooLarge,
        };
        header_bytes += line.len();
        if header_bytes > config.max_header_bytes {
            return ReadOutcome::TooLarge;
        }
        if line.is_empty() {
            break;
        }
        if headers.len() >= config.max_headers {
            return ReadOutcome::TooLarge;
        }
        // RFC 7230：头名必须是合法 token（冒号前不允许空白），无冒号的头行直接拒绝，
        // 避免与严格的前置代理对同一请求定界不一致（请求走私面）。
        let Some((key, value)) = line.split_once(':') else {
            return ReadOutcome::BadRequest("malformed header line".to_string());
        };
        if !is_header_name(key) {
            return ReadOutcome::BadRequest("invalid header name".to_string());
        }
        headers.push((key.to_string(), value.trim().to_string()));
    }

    // 请求走私防护：CL 必须唯一且合法；TE 只接受 chunked；两者不允许同时出现。
    let chunked = match parse_transfer_encoding(&headers) {
        Ok(value) => value,
        Err(message) => return ReadOutcome::BadRequest(message),
    };
    if chunked && version.eq_ignore_ascii_case("HTTP/1.0") {
        return ReadOutcome::BadRequest(
            "Transfer-Encoding is not allowed in HTTP/1.0".to_string(),
        );
    }
    let length = match parse_content_length(&headers) {
        Ok(value) => value,
        Err(message) => return ReadOutcome::BadRequest(message),
    };
    if chunked && length.is_some() {
        return ReadOutcome::BadRequest(
            "both Content-Length and Transfer-Encoding are present".to_string(),
        );
    }
    let body = if chunked {
        match read_chunked(reader, config.max_body_bytes).await {
            Some(body) => body,
            None => return ReadOutcome::TooLarge,
        }
    } else {
        let length = length.unwrap_or(0);
        if length > config.max_body_bytes {
            return ReadOutcome::TooLarge;
        }
        let mut body = vec![0u8; length];
        if length > 0 && reader.read_exact(&mut body).await.is_err() {
            return ReadOutcome::BadRequest("incomplete request body".to_string());
        }
        body
    };

    let connection = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("connection"))
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default();
    let keep_alive = if connection.contains("close") {
        false
    } else if version.eq_ignore_ascii_case("HTTP/1.0") {
        connection.contains("keep-alive")
    } else {
        true
    };

    ReadOutcome::Request(Box::new(HttpRequestHead {
        method,
        path,
        target,
        headers,
        body,
        keep_alive,
    }))
}

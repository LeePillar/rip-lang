﻿//! Rip 树遍历解释器。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rp_ast::*;
#[cfg(feature = "interp")]
use rp_types::{builtins as bi, CheckResult, Type, TypeTable};

pub use crate::value::{
    format_value, value_eq, Flow, FlowResult, FuncKind, Function, RuntimeError, Value,
};
#[cfg(feature = "interp")]
pub use crate::value::{int_literal_value, Env};

#[cfg(feature = "interp")]
struct ModuleVar {
    name: String,
    init: Expr,
}

pub struct Runtime {
    /// 类型表：只有解释执行需要（AOT 由 rustc 完成类型检查）。
    #[cfg(feature = "interp")]
    pub(crate) types: TypeTable,
    pub(crate) funcs: Arc<HashMap<String, Arc<FuncDecl>>>,
    /// 模块别名 → 包路径。解释模式下用于 `alias.func()`；AOT 由生成代码直接解析。
    #[allow(dead_code)]
    pub(crate) imports: HashMap<String, String>,
    pub(crate) globals: HashMap<String, Value>,
    #[cfg(feature = "interp")]
    pub(crate) expr_types: HashMap<NodeId, Type>,
    /// 程序里是否出现固定位宽整数（`u8`…`i32`）；没有时跳过运算处的范围检查。
    #[cfg(feature = "interp")]
    pub(crate) has_width_ints: bool,
    #[cfg(feature = "interp")]
    pub(crate) type_names: Vec<(Type, String)>,
    #[cfg(feature = "interp")]
    module_vars: Vec<ModuleVar>,
    /// 模块级 `const` / `static` 是否已初始化；测试隔离模式下每个运行时只初始化一次。
    #[cfg(feature = "interp")]
    module_vars_ready: bool,
    pub(crate) cli_args: Vec<String>,
    pub(crate) log_level: String,
    /// `std/log` 输出格式：`text`（缺省）或 `json`。
    pub(crate) log_format: String,
    /// `std/log` 同时写入的文件；未配置时只输出到标准输出。
    pub(crate) log_file: Option<std::fs::File>,
    /// 日志文件路径（轮转时需要重命名并重新打开）。
    pub(crate) log_path: Option<String>,
    /// 单个日志文件的大小上限（字节）；0 表示不轮转。
    pub(crate) log_max_bytes: u64,
    /// 轮转保留的历史文件数。
    pub(crate) log_backups: usize,
    /// 当前请求 ID，供日志串联使用；不在请求上下文时为 None。
    pub(crate) current_request_id: Option<String>,
    /// 上游传入（或本机生成）的 `traceparent` 中的 trace id。
    pub(crate) current_trace_id: Option<String>,
    /// 当前请求的 span id，供数据库等子 span 挂到同一棵树上。
    pub(crate) current_span_id: Option<String>,
    /// 是否采集语句覆盖率：构建运行时读取一次全局开关，之后只判断这个布尔值。
    pub(crate) coverage: bool,
    /// 当前请求命中的路由标签（`GET /users/:id`），由路由匹配写入。
    pub(crate) current_route: Option<String>,
    /// 处理函数的最迟完成时间；None 表示不限制。
    pub(crate) deadline: Option<Instant>,
    /// 是否因为超过 deadline 被中断；服务端据此返回 504。
    pub(crate) timed_out: bool,
    /// 进程堆占用上限（字节）；0 表示不限制。
    pub(crate) heap_limit: usize,
    /// 调用栈（函数名, 定义行号），出错时快照进 `RuntimeError`。
    /// 解释器由 `call_decl` 维护，AOT 由生成代码的 `enter_frame` / `exit_frame` 维护。
    pub(crate) call_stack: Vec<(String, u32)>,
    /// 调试器；存在时禁用字节码快速路径，逐语句检查断点。
    pub(crate) debugger: Option<std::sync::Arc<crate::debug::Debugger>>,
    /// `defer` 栈：每个函数帧一层，函数退出时按后进先出执行。
    pub(crate) defers: Vec<Vec<(Deferred, u32)>>,
    /// 运行时提示语言，来自 `rip.toml` 的 `[runtime] log`。
    pub(crate) language: crate::i18n::Language,
    pub(crate) timers: HashMap<String, Instant>,
    /// `rip dev --port` / `rip run --port` 指定的 HTTP 端口，优先于程序内字面量。
    pub(crate) http_port: Option<u16>,
    /// `rip run --host` 指定的绑定地址；缺省 127.0.0.1。
    pub(crate) http_host: Option<String>,
    /// `rip run --cert` 指定的 PEM 证书链。
    pub(crate) tls_cert: Option<String>,
    /// `rip run --key` 指定的 PEM 私钥。
    pub(crate) tls_key: Option<String>,
    /// `rip.toml` 的 `[server]` 与 CLI 参数给出的 HTTP 服务端参数。
    pub(crate) server_config: Option<crate::server::ServerConfig>,
    /// HTTP 服务端指标；未启动服务时为 None。
    pub(crate) metrics: Option<std::sync::Arc<crate::server::Metrics>>,
    /// 进程内限流器；未开启限流时为 None。
    pub(crate) rate_limiter: Option<std::sync::Arc<crate::server::RateLimiter>>,
    /// 可信代理配置：解析反向代理转发头，未启动服务时为 None。
    pub(crate) proxy_trust: Option<std::sync::Arc<crate::server::ProxyTrust>>,
    /// `std/net` 与 `std/ws` 打开的连接、监听器与套接字句柄。
    pub(crate) net_state: crate::net::NetState,
    /// `std/db` 打开的 SQLite 连接。
    pub(crate) db: crate::db::DbState,
    /// `std/redis` 打开的连接。
    pub(crate) redis: crate::redis::RedisState,
    /// 编译单元原始 AST，供后台任务在独立线程重建解释器。
    #[cfg(feature = "interp")]
    pub(crate) program: Arc<Program>,
    /// 类型检查结果，供后台任务在独立线程重建解释器。
    #[cfg(feature = "interp")]
    pub(crate) checked: Arc<CheckResult>,
    /// 集群配置：Redis 地址与定时任务选主开关。
    pub(crate) cluster: ClusterConfig,
    /// 字节码快速路径的编译结果；由 `RuntimeSeed` 共享，工作线程不重复编译。
    #[cfg(feature = "interp")]
    pub(crate) vm: Arc<crate::vm::VmCache>,
    /// AOT 二进制附带的原生模块；解释执行时为 None。
    pub(crate) native: Option<NativeModule>,
    /// 已加载的外部原生包（插件）。
    pub(crate) native_packages: HashMap<String, crate::native_pkg::PluginPackage>,
    /// 加载失败的原生包及原因，避免每次调用都重新扫描磁盘。
    pub(crate) native_errors: HashMap<String, String>,
}

/// AOT 生成函数的签名：与解释器入口同形，`line` 用于运行时错误定位。
pub type NativeFn = fn(&mut Runtime, Vec<Value>, u32) -> FlowResult<Value>;

/// `defer` 就地求值后的调用：实参在 defer 语句处捕获，函数退出时按 LIFO 执行。
pub enum Deferred {
    /// 按名字调用：模块函数、原生函数或 `print`。
    Named(String, Vec<Value>),
    /// 已求值的函数值：闭包或函数变量。
    Func(Value, Vec<Value>),
    /// 方法调用：接收者与实参已求值。
    Method(Value, String, Vec<Value>),
    /// 模块调用 `alias.func(...)`：路径与实参已求值。
    Module(String, String, Vec<Value>),
}

/// AOT 生成函数在运行时的登记项。裸函数指针可跨线程共享。
#[derive(Clone, Copy)]
pub struct NativeEntry {
    pub arity: usize,
    pub body: NativeFn,
    /// `async func`：调用时交给调度器，而不是就地执行。
    pub is_async: bool,
}

/// 一条定时任务：函数名、`#[interval]` 参数（None 表示只执行一次）、源码行号。
#[derive(Clone)]
pub struct NativeTask {
    pub name: String,
    pub interval: Option<String>,
    pub line: u32,
}

/// 一条注解路由：`#[get("/path")]` 这类属性在 AOT 下的替代描述。
#[derive(Clone)]
pub struct NativeRoute {
    pub method: String,
    pub path: String,
    pub handler: String,
}

/// AOT 二进制的模块描述：导入表、原生函数表、注解路由、定时任务与模块级初始化。
///
/// 字段全部可跨线程共享；HTTP 工作线程、后台任务据此重建运行时。
#[derive(Clone, Default)]
pub struct NativeModule {
    pub imports: HashMap<String, String>,
    pub funcs: std::sync::Arc<HashMap<String, NativeEntry>>,
    pub routes: std::sync::Arc<Vec<NativeRoute>>,
    pub tasks: std::sync::Arc<Vec<NativeTask>>,
    pub init: Option<fn(&mut Runtime) -> FlowResult<Value>>,
}

/// 共享的字节码缓存句柄：同一编译单元只编译一次，跨线程 / 跨运行时复用。
///
/// 内部结构不对外暴露；`Clone` 只增加引用计数。
#[cfg(feature = "interp")]
#[derive(Clone)]
pub struct SharedVm(pub(crate) Arc<crate::vm::VmCache>);

/// 重建运行时的来源：解释执行用 AST，AOT 用原生模块。
#[derive(Clone)]
pub enum RuntimeSeed {
    #[cfg(feature = "interp")]
    Ast {
        program: Arc<Program>,
        checked: Arc<CheckResult>,
        funcs: Arc<HashMap<String, Arc<FuncDecl>>>,
        /// 字节码编译结果：由第一个运行时编译一次，其余运行时直接共享。
        vm: SharedVm,
    },
    Native(NativeModule),
}

impl RuntimeSeed {
    /// 由编译单元构造：函数表与字节码只建一次，之后每个线程 / 每个测试各自 `build`。
    #[cfg(feature = "interp")]
    pub fn from_program(program: Program, checked: CheckResult) -> RuntimeSeed {
        let mut funcs = HashMap::new();
        for item in &program.items {
            if let Item::Func(func) = item {
                funcs.insert(func.name.clone(), Arc::new(func.clone()));
            }
        }
        let funcs = Arc::new(funcs);
        let vm = SharedVm(Arc::new(crate::vm::compile_all(&funcs, &checked.imports)));
        RuntimeSeed::Ast {
            program: Arc::new(program),
            checked: Arc::new(checked),
            funcs,
            vm,
        }
    }

    pub fn build(&self) -> Runtime {
        match self {
            #[cfg(feature = "interp")]
            RuntimeSeed::Ast {
                program,
                checked,
                funcs,
                vm,
            } => Runtime::from_ast(
                Arc::clone(program),
                Arc::clone(checked),
                Arc::clone(funcs),
                Arc::clone(&vm.0),
                Vec::new(),
            ),
            RuntimeSeed::Native(module) => Runtime::new_native(module.clone()),
        }
    }

    /// 调度线程缓存运行时的身份：同一编译单元的运行时可以复用。
    pub(crate) fn key(&self) -> usize {
        match self {
            #[cfg(feature = "interp")]
            RuntimeSeed::Ast { program, .. } => Arc::as_ptr(program) as *const () as usize,
            RuntimeSeed::Native(module) => Arc::as_ptr(&module.funcs) as *const () as usize,
        }
    }
}

/// 工作线程运行时需要的选项：线程不继承父运行时的语言与日志配置。
#[derive(Clone)]
pub struct ThreadOptions {
    pub language: crate::i18n::Language,
    pub log_level: String,
    pub log_format: String,
}

/// 单个 `#[test]` 的执行结果。
pub struct TestOutcome {
    pub result: Result<(), RuntimeError>,
    /// 是否因超时被中断；此时 `result` 是超时错误。
    pub timed_out: bool,
    pub elapsed: Duration,
}

/// 等待全部并行分支结束；任一分支失败则整个 `parallel` 块失败，报第一个错误。
fn join_parallel(
    runtime: &mut Runtime,
    handles: Vec<std::thread::JoinHandle<Result<(), RuntimeError>>>,
    line: u32,
) -> FlowResult<()> {
    let mut failure: Option<RuntimeError> = None;
    for handle in handles {
        // 分支线程 panic 不影响其他分支，但要让整个块失败而不是假装成功。
        let outcome = handle
            .join()
            .unwrap_or_else(|_| Err(runtime.runtime_error("parallel branch panicked", line)));
        if let Err(error) = outcome {
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    match failure {
        Some(error) => Err(Flow::Error(error)),
        None => Ok(()),
    }
}

/// 集群协作配置，来自 `rip.toml` 的 `[cluster]`。
#[derive(Clone, Default)]
pub struct ClusterConfig {
    /// Redis 连接串，例如 `redis://127.0.0.1:6379/0`。
    pub redis_url: Option<String>,
    /// 多实例部署时，定时任务是否用 Redis 锁选主（同一 tick 只跑一个实例）。
    pub task_lock: bool,
}

/// 执行一次定时任务；有集群锁时先抢租约，抢不到就跳过本轮。
///
/// Redis 不可用时退化为本地执行：宁可重复执行，也不要让任务完全停摆。
fn run_task_tick(
    runtime: &mut Runtime,
    lock: Option<&mut crate::redis::RedisConn>,
    name: &str,
    ttl_ms: u64,
) {
    let Some(conn) = lock else {
        runtime.run_task_once(name);
        return;
    };
    let key = format!("rip:task:{}", name);
    let token = crate::redis::new_token();
    let ttl = ttl_ms.to_string();
    match conn.run(&["SET", &key, &token, "NX", "PX", &ttl]) {
        Ok(crate::redis::Reply::Status(status)) if status == "OK" => {}
        // 其他实例持有租约，本轮跳过。
        Ok(_) => return,
        // 锁服务故障时本地执行，保证任务不中断。
        Err(_) => {
            runtime.run_task_once(name);
            return;
        }
    }
    runtime.run_task_once(name);
    let _ = conn.run(&["EVAL", crate::redis::UNLOCK_SCRIPT, "1", &key, &token]);
}

impl Runtime {
    /// 由 AST 构造解释器。AOT 构建不含解释器，故仅在 `interp` feature 下提供。
    #[cfg(feature = "interp")]
    pub fn new(program: &Program, checked: CheckResult, cli_args: Vec<String>) -> Self {
        // 与测试 / 工作线程共用同一条构造路径，函数表只建一次。
        let mut runtime = RuntimeSeed::from_program(program.clone(), checked).build();
        runtime.cli_args = cli_args;
        runtime
    }

    /// 由共享的编译单元构造运行时。
    ///
    /// 函数表与 AST 通过 `Arc` 复用：`parallel` 分支、HTTP 工作线程和后台任务
    /// 各自新建 `Runtime`，但不必为每个线程深拷贝整份程序。
    #[cfg(feature = "interp")]
    fn from_ast(
        program: Arc<Program>,
        checked: Arc<CheckResult>,
        funcs: Arc<HashMap<String, Arc<FuncDecl>>>,
        vm: Arc<crate::vm::VmCache>,
        cli_args: Vec<String>,
    ) -> Self {
        let mut module_vars = Vec::new();
        for item in &program.items {
            if let Item::Const(decl) | Item::Static(decl) = item {
                module_vars.push(ModuleVar {
                    name: decl.name.clone(),
                    init: decl.init.clone(),
                });
            }
        }
        let mut type_names: Vec<(Type, String)> = checked
            .type_table
            .defs
            .iter()
            .map(|(name, ty)| (checked.type_table.resolve(ty), name.clone()))
            .collect();
        for name in bi::TYPE_NAMES {
            if let Some(ty) = bi::builtin_type(name) {
                type_names.push((checked.type_table.resolve(&ty), (*name).to_string()));
            }
        }
        // 字节码快速路径：纯标量函数编译为栈式指令，其余保持解释执行。
        // 编译结果由 `RuntimeSeed` 持有，多个运行时共享同一份指令。
        let types = checked.type_table.clone();
        let imports = checked.imports.clone();
        let expr_types = checked.expr_types.clone();
        // 只有真正用到固定位宽整数时才需要在运算处做范围检查；纯 int/float 程序零开销。
        let has_width_ints = expr_types
            .values()
            .any(|ty| ty.int_suffix().map_or(false, |suffix| suffix != IntSuffix::I64));
        Self {
            program,
            checked,
            types,
            funcs,
            imports,
            globals: HashMap::new(),
            expr_types,
            has_width_ints,
            type_names,
            module_vars,
            module_vars_ready: false,
            cli_args,
            log_level: "info".to_string(),
            log_format: "text".to_string(),
            log_file: None,
            log_path: None,
            log_max_bytes: 0,
            log_backups: 3,
            current_request_id: None,
            current_trace_id: None,
            current_span_id: None,
            coverage: crate::coverage::enabled(),
            current_route: None,
            deadline: None,
            timed_out: false,
            heap_limit: 0,
            call_stack: Vec::new(),
            debugger: None,
            defers: Vec::new(),
            language: crate::i18n::Language::English,
            timers: HashMap::new(),
            http_port: None,
            http_host: None,
            tls_cert: None,
            tls_key: None,
            server_config: None,
            metrics: None,
            rate_limiter: None,
            proxy_trust: None,
            net_state: crate::net::NetState::default(),
            db: crate::db::DbState::new(),
            redis: crate::redis::RedisState::new(),
            cluster: ClusterConfig::default(),
            vm,
            native: None,
            native_packages: HashMap::new(),
            native_errors: HashMap::new(),
        }
    }

    /// 由 CLI 注入的集群配置，来自 `rip.toml` 的 `[cluster]`。
    pub fn set_cluster(&mut self, cluster: ClusterConfig) {
        self.cluster = cluster;
    }

    /// AOT 生成代码使用的宿主：不加载源码/AST，只承载值、全局变量与标准库状态。
    pub fn new_native(module: NativeModule) -> Self {
        #[cfg(feature = "interp")]
        {
            let program = Program {
                package: String::new(),
                items: Vec::new(),
                file: String::new(),
                package_line: 1,
            };
            let checked = CheckResult {
                diagnostics: Vec::new(),
                expr_types: HashMap::new(),
                funcs: HashMap::new(),
                type_table: rp_types::TypeTable::new(&program),
                imports: module.imports.clone(),
                interfaces: HashMap::new(),
                enums: HashMap::new(),
                type_bounds: HashMap::new(),
            };
            let mut runtime = Self::new(&program, checked, Vec::new());
            runtime.imports = module.imports.clone();
            runtime.native = Some(module);
            runtime
        }
        #[cfg(not(feature = "interp"))]
        {
            Self {
                funcs: Arc::new(HashMap::new()),
                imports: module.imports.clone(),
                globals: HashMap::new(),
                cli_args: Vec::new(),
                log_level: "info".to_string(),
                log_format: "text".to_string(),
                log_file: None,
                log_path: None,
                log_max_bytes: 0,
                log_backups: 3,
                current_request_id: None,
                current_trace_id: None,
                current_span_id: None,
                coverage: crate::coverage::enabled(),
                current_route: None,
                deadline: None,
                timed_out: false,
                heap_limit: 0,
                call_stack: Vec::new(),
                debugger: None,
                defers: Vec::new(),
                language: crate::i18n::Language::English,
                timers: HashMap::new(),
                http_port: None,
                http_host: None,
                tls_cert: None,
                tls_key: None,
                server_config: None,
                metrics: None,
                rate_limiter: None,
                proxy_trust: None,
                net_state: crate::net::NetState::default(),
                db: crate::db::DbState::new(),
                redis: crate::redis::RedisState::new(),
                cluster: ClusterConfig::default(),
                native: Some(module),
                native_packages: HashMap::new(),
                native_errors: HashMap::new(),
            }
        }
    }

    /// 重建运行时的来源：解释模式给出 AST，AOT 模式给出原生模块。
    pub fn seed(&self) -> RuntimeSeed {
        match &self.native {
            Some(module) => RuntimeSeed::Native(module.clone()),
            None => {
                #[cfg(feature = "interp")]
                let seed = RuntimeSeed::Ast {
                    program: Arc::clone(&self.program),
                    checked: Arc::clone(&self.checked),
                    funcs: Arc::clone(&self.funcs),
                    vm: SharedVm(Arc::clone(&self.vm)),
                };
                #[cfg(not(feature = "interp"))]
                let seed = RuntimeSeed::Native(NativeModule::default());
                seed
            }
        }
    }

    /// 工作线程需要的运行时选项：语言与日志配置（线程不继承父运行时状态）。
    pub fn thread_options(&self) -> ThreadOptions {
        ThreadOptions {
            language: self.language,
            log_level: self.log_level.clone(),
            log_format: self.log_format.clone(),
        }
    }

    /// 任务复用运行时：清掉上一次任务留下的局部状态，再重新初始化模块级变量。
    ///
    /// 复用的是「运行时对象」而不是任务状态：globals、借出的数据库 / Redis 连接、
    /// 网络句柄、defer 栈、调用栈、请求上下文都会清空，语义与新建运行时一致。
    pub(crate) fn reset_for_task(&mut self) -> Result<(), RuntimeError> {
        self.call_stack.clear();
        self.defers.clear();
        self.timers.clear();
        self.db.reset();
        self.redis.reset();
        self.net_state.reset();
        self.current_request_id = None;
        self.current_trace_id = None;
        self.current_span_id = None;
        self.current_route = None;
        self.deadline = None;
        self.timed_out = false;
        self.log_file = None;
        self.log_path = None;
        self.log_max_bytes = 0;
        self.log_backups = 0;
        self.globals.clear();
        #[cfg(feature = "interp")]
        {
            self.module_vars_ready = false;
        }
        self.init_entry()
    }

    /// 把父运行时的线程选项套用到工作线程的运行时上。
    pub fn apply_thread_options(&mut self, options: &ThreadOptions) {
        self.language = options.language;
        self.log_level = options.log_level.clone();
        self.log_format = options.log_format.clone();
    }

    /// 设置本次处理的截止时间；`None` 表示不限制。
    pub fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
        self.timed_out = false;
    }

    pub fn clear_deadline(&mut self) {
        self.deadline = None;
    }

    /// 本次处理是否因超时被中断（服务端据此返回 504）。
    pub fn timed_out(&self) -> bool {
        self.timed_out
    }

    /// 距截止时间的剩余预算；没有设置截止时间时为 None。
    pub fn remaining_time(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// 中断检查：解释器在语句、循环迭代与内建调用前调用。
    ///
    /// 阻塞中的单次系统调用（如一次很慢的 SQL）无法被打断，
    /// 但处理函数会在下一个检查点立刻返回超时错误。
    ///
    /// 同时在这里检查堆上限：超过时先尝试环回收，仍然超限才报错——
    /// 避免一次请求把整个进程拖进 OOM。环回收本身在请求 / 测试边界触发。
    pub fn check_deadline(&mut self, line: u32) -> FlowResult<()> {
        if self.heap_limit > 0 && crate::heap::usage() > self.heap_limit {
            crate::gc::collect();
            let used = crate::heap::usage();
            if used > self.heap_limit {
                return self.error(
                    format!(
                        "heap limit exceeded: {} bytes used, limit {} bytes",
                        used, self.heap_limit
                    ),
                    line,
                );
            }
        }
        if let Some(deadline) = self.deadline {
            if Instant::now() >= deadline {
                self.timed_out = true;
                return self.error("request exceeded the handler timeout", line);
            }
        }
        Ok(())
    }

    /// 给一次出站请求补上 W3C `traceparent`，把链路追踪透传到下游服务。
    pub(crate) fn with_traceparent(
        &self,
        headers: Vec<(String, String)>,
    ) -> Vec<(String, String)> {
        let mut headers = headers;
        if headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
        {
            return headers;
        }
        let Some(trace_id) = self.current_trace_id.as_deref() else {
            return headers;
        };
        if let Some(header) = crate::server::traceparent_header(trace_id) {
            headers.push(header);
        }
        headers
    }

    /// AOT 分支入口：`branches` 是生成代码里的分支函数，各在自己的线程里跑。
    pub fn run_parallel_fns(
        &mut self,
        label: &str,
        line: u32,
        branches: Vec<fn(&mut Runtime) -> FlowResult<Value>>,
    ) -> FlowResult<()> {
        let seed = self.seed();
        let options = self.thread_options();
        let mut handles = Vec::new();
        for (index, branch) in branches.into_iter().enumerate() {
            let seed = seed.clone();
            let options = options.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("rip-par-{}-{}", label, index))
                .spawn(move || {
                    let mut runtime = seed.build();
                    runtime.apply_thread_options(&options);
                    runtime.init_entry()?;
                    match branch(&mut runtime) {
                        Ok(_) => Ok(()),
                        Err(Flow::Error(error)) => Err(error),
                        Err(_) => Err(runtime.runtime_error(
                            "'return', 'break' or 'continue' cannot leave a parallel branch",
                            line,
                        )),
                    }
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    return self.error(format!("cannot start parallel branch: {}", error), line)
                }
            }
        }
        join_parallel(self, handles, line)
    }

    /// 函数是否可用（解释模式的 AST 函数或 AOT 的原生函数）。
    pub fn has_func(&self, name: &str) -> bool {
        if self.funcs.contains_key(name) {
            return true;
        }
        self.native
            .as_ref()
            .map(|module| module.funcs.contains_key(name))
            .unwrap_or(false)
    }

    /// 按名字调用函数：解释模式查 AST，AOT 模式查原生函数表。
    ///
    /// `async func` 交给调度器执行，返回任务值；调用方必须用 `await` 消费。
    pub fn call_named(&mut self, name: &str, args: Vec<Value>, line: u32) -> FlowResult<Value> {
        #[cfg(feature = "interp")]
        if let Some(decl) = self.funcs.get(name).cloned() {
            if decl.is_async {
                return self.call_async(&decl.name, args, line);
            }
            return self.call_decl(&decl, args, line);
        }
        let entry = self
            .native
            .as_ref()
            .and_then(|module| module.funcs.get(name).copied());
        if matches!(entry, Some(entry) if entry.is_async) {
            return self.call_async(name, args, line);
        }
        self.call_named_now(name, args, line)
    }

    /// 就地执行函数（async 工作线程与内部流程使用，不再二次派发）。
    pub fn call_named_now(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        #[cfg(feature = "interp")]
        if let Some(decl) = self.funcs.get(name).cloned() {
            return self.call_decl(&decl, args, line);
        }
        let entry = self
            .native
            .as_ref()
            .and_then(|module| module.funcs.get(name).copied());
        match entry {
            Some(entry) => {
                if args.len() != entry.arity {
                    return self.error(
                        format!(
                            "function '{}' expects {} arguments, found {}",
                            name,
                            entry.arity,
                            args.len()
                        ),
                        line,
                    );
                }
                (entry.body)(self, args, line)
            }
            None => self.error(format!("unknown function '{}'", name), line),
        }
    }

    /// 初始化模块级变量：AOT 调用生成的初始化函数，解释模式求值 const/static。
    pub fn init_entry(&mut self) -> Result<(), RuntimeError> {
        if let Some(init) = self.native.as_ref().and_then(|module| module.init) {
            return match init(self) {
                Ok(_) => Ok(()),
                Err(Flow::Error(error)) => Err(error),
                Err(_) => Err(self.runtime_error("module initialization failed", 0)),
            };
        }
        #[cfg(feature = "interp")]
        {
            self.init_module_vars()
        }
        #[cfg(not(feature = "interp"))]
        {
            Ok(())
        }
    }

    /// `print` 的运行时实现：多个值用空格连接，每个值按 Rip 规则格式化。
    pub fn print_values(&mut self, values: Vec<Value>) {
        let text: Vec<String> = values.iter().map(format_value).collect();
        crate::debug::emit_line(text.join(" "));
    }

    /// 读取模块级 const/static（AOT 生成代码调用）。
    pub fn get_global(&self, name: &str) -> Value {
        self.globals.get(name).cloned().unwrap_or(Value::Null)
    }

    /// 写入模块级 static；const 由类型检查器禁止赋值。
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(name.to_string(), value);
    }

    /// 字段访问（AOT 生成代码调用）。
    pub fn value_field(&mut self, base: Value, name: &str, line: u32) -> FlowResult<Value> {
        self.get_field(base, name, line)
    }

    /// for-in 的迭代项：vec 取元素，map 取 key。
    pub fn iter_items(&mut self, value: &Value, line: u32) -> FlowResult<Vec<Value>> {
        match value {
            Value::Vec(items) => Ok(items.borrow().clone()),
            Value::Map(map) => Ok(map
                .borrow()
                .entries
                .iter()
                .map(|(key, _)| key.clone())
                .collect()),
            other => self.error(
                format!("for-in requires vec or map, found {}", format_value(other)),
                line,
            ),
        }
    }

    /// 字段赋值：`root.a.b = value`，返回更新后的根值。
    pub fn assign_path(
        &mut self,
        root: Value,
        path: &[String],
        value: Value,
        line: u32,
    ) -> FlowResult<Value> {
        match set_path(&root, path, value) {
            Ok(()) => Ok(root),
            Err(message) => self.error(message, line),
        }
    }

    /// 生成一个带位置信息的运行时错误（供 AOT 代码 `return Err(host.fail(..))`）。
    pub fn fail(&mut self, message: String, line: u32) -> Flow {
        Flow::Error(self.runtime_error(message, line))
    }

    /// 结构体字面量：补齐内置类型的默认字段（当前为 `http.response`）。
    pub fn make_struct(&self, class: &str, fields: Vec<(String, Value)>) -> Value {
        let mut result: Vec<(String, Value)> = Vec::new();
        if class == "http.response" {
            result.push(("status".to_string(), Value::Int(200)));
            result.push(("headers".to_string(), Value::empty_map()));
            result.push(("body".to_string(), Value::string("")));
            result.push(("json".to_string(), Value::None));
            result.push(("sse".to_string(), Value::None));
        }
        for (name, value) in fields {
            let value = if class == "http.response" && name == "json" {
                match value {
                    Value::None | Value::Some(_) => value,
                    other => Value::Some(Box::new(other)),
                }
            } else {
                value
            };
            if let Some((_, slot)) = result.iter_mut().find(|(field, _)| field == &name) {
                *slot = value;
            } else {
                result.push((name, value));
            }
        }
        Value::ary(class, result)
    }

    /// ary 模式匹配：全部字段都存在时返回按顺序克隆的字段值。
    pub fn match_ary_fields(&self, value: &Value, names: &[&str]) -> Option<Vec<Value>> {
        match value {
            Value::Ary(ary) => {
                let ary = ary.borrow();
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    out.push(ary.get(name)?);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// 构造枚举值：类名是枚举名，内部字段 `#variant` 记录变体。
    pub fn make_enum(&self, enum_name: &str, variant: &str, fields: Vec<(String, Value)>) -> Value {
        let mut result = Vec::with_capacity(fields.len() + 1);
        result.push(("#variant".to_string(), Value::string(variant)));
        result.extend(fields);
        Value::ary(enum_name, result)
    }

    /// 匹配枚举变体并取出指定字段；变体不符返回 None。
    pub fn match_enum(
        &self,
        value: &Value,
        enum_name: &str,
        variant: &str,
        names: &[&str],
    ) -> Option<Vec<Value>> {
        match value {
            Value::Ary(ary) => {
                let ary = ary.borrow();
                if ary.class != enum_name {
                    return None;
                }
                match ary.get("#variant").and_then(|value| value.as_str()) {
                    Some(found) if found == variant => {}
                    _ => return None,
                }
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    out.push(ary.get(name)?);
                }
                Some(out)
            }
            _ => None,
        }
    }


    /// 由 CLI 注入的 HTTP 端口覆盖，供 `rip dev --port` 使用。
    pub fn set_http_port(&mut self, port: Option<u16>) {
        self.http_port = port;
    }

    /// 由 CLI 注入的绑定地址，供 `rip run --host` 使用。
    pub fn set_http_host(&mut self, host: Option<String>) {
        self.http_host = host;
    }

    /// 由 CLI 注入的 TLS 证书与私钥；两者都给定时 `http.serve` 自动走 HTTPS。
    pub fn set_http_tls(&mut self, cert: Option<String>, key: Option<String>) {
        self.tls_cert = cert;
        self.tls_key = key;
    }

    /// 由 CLI 注入的提示语言，来自 `rip.toml` 的 `[runtime] log`。
    pub fn set_language(&mut self, language: crate::i18n::Language) {
        self.language = language;
    }

    /// 注入调试器：之后逐语句检查断点，并停用字节码快速路径。
    pub fn set_debugger(&mut self, debugger: std::sync::Arc<crate::debug::Debugger>) {
        self.debugger = Some(debugger);
    }

    /// 由 CLI 注入的 HTTP 服务端参数，来自 `rip.toml` 的 `[server]` 与命令行。
    pub fn set_server_config(&mut self, config: crate::server::ServerConfig) {
        self.set_heap_limit(config.max_heap_bytes);
        self.server_config = Some(config);
    }

    /// 进程堆占用上限（字节）；0 表示不限制。超过时在语句检查点报错。
    pub fn set_heap_limit(&mut self, bytes: usize) {
        self.heap_limit = bytes;
        if bytes > 0 {
            crate::heap::enable();
        }
    }

    pub fn heap_limit(&self) -> usize {
        self.heap_limit
    }

    /// 由 AOT 生成的 main 注入命令行参数，供 `std/sys.args` 使用。
    pub fn set_cli_args(&mut self, args: Vec<String>) {
        self.cli_args = args;
    }

    /// 初始化模块级 `const` / `static`。
    ///
    /// 工作线程不执行 `main`，需要单独初始化，否则处理函数读不到模块级常量。
    /// 幂等：同一运行时重复调用不会重复求值（测试的 `--no-isolate` 模式会用到）。
    #[cfg(feature = "interp")]
    pub(crate) fn init_module_vars(&mut self) -> Result<(), RuntimeError> {
        if self.module_vars_ready {
            return Ok(());
        }
        let mut env = Env::new();
        for i in 0..self.module_vars.len() {
            let init = self.module_vars[i].init.clone();
            let name = self.module_vars[i].name.clone();
            let value = match self.eval(&init, &mut env) {
                Ok(value) => value,
                Err(Flow::Error(error)) => return Err(error),
                Err(_) => {
                    return Err(self.runtime_error(
                        format!("failed to initialize module variable '{}'", name),
                        init.line,
                    ))
                }
            };
            self.globals.insert(name, value);
        }
        self.module_vars_ready = true;
        Ok(())
    }

    pub fn language(&self) -> crate::i18n::Language {
        self.language
    }

    /// 输出运行时自身提示：英文模式原文，中文模式译文。
    pub(crate) fn say(&self, english: impl Into<String>, chinese: impl Into<String>) {
        if self.language.is_chinese() {
            crate::debug::emit_line(chinese.into());
        } else {
            crate::debug::emit_line(english.into());
        }
    }

    /// 生成带行号的运行时错误，并按当前语言翻译提示。
    pub(crate) fn runtime_error(
        &self,
        message: impl Into<String>,
        line: u32,
    ) -> RuntimeError {
        let message = crate::i18n::localize(self.language, &message.into());
        let stack = self.call_stack.clone();
        RuntimeError::new(message, line).with_stack(stack)
    }

    /// AOT 生成代码的调用帧入口：进入函数时记录 `(函数名, 定义行号)`。
    ///
    /// 解释器由 `call_decl` 做同样的事；这里让两种模式的错误栈一致。
    pub fn enter_frame(&mut self, name: &str, line: u32) {
        self.call_stack.push((name.to_string(), line));
    }

    /// AOT 生成代码的调用帧出口。用 `truncate` 而不是 `pop`，
    /// 与解释器一致：下层调用可能补充过帧。
    pub fn exit_frame(&mut self, depth: usize) {
        self.call_stack.truncate(depth);
    }

    /// 当前调用栈深度；生成代码在进入函数前记录，退出时按此截断。
    pub fn frame_depth(&self) -> usize {
        self.call_stack.len()
    }

    /// panic 被捕获后清理残留状态（调用栈、请求 ID），避免污染后续请求。
    pub fn reset_after_panic(&mut self) {
        self.call_stack.clear();
        self.current_request_id = None;
        self.current_trace_id = None;
        self.current_span_id = None;
    }

    /// 在独立线程启动所有 `#[interval("...")]` 定时任务。
    pub fn start_scheduled_tasks(&mut self) {
        let mut tasks: Vec<(String, String, u32)> = Vec::new();
        if let Some(module) = &self.native {
            // AOT 二进制没有 AST，定时任务由生成的模块描述给出。
            for task in module.tasks.iter() {
                if let Some(spec) = &task.interval {
                    tasks.push((task.name.clone(), spec.clone(), task.line));
                }
            }
        } else {
            #[cfg(feature = "interp")]
            {
                let mut names: Vec<&String> = self.funcs.keys().collect();
                names.sort();
                for name in names {
                    for attribute in &self.funcs[name].attributes {
                        if attribute.name == "interval" && attribute.args.len() == 1 {
                            tasks.push((name.clone(), attribute.args[0].clone(), attribute.line));
                        }
                    }
                }
            }
        }
        for (name, spec, line) in tasks {
            match parse_interval(&spec) {
                Some(duration) => {
                    self.say(
                        format!("[task] {} every {}", name, spec),
                        format!("[任务] {} 每 {}", name, spec),
                    );
                    self.spawn_task(name, Some(duration), line);
                }
                None => self.say(
                    format!(
                        "[task] invalid interval '{}' for '{}' at line {}",
                        spec, name, line
                    ),
                    format!(
                        "[任务] '{}' 的间隔 '{}' 无效（第 {} 行）",
                        name, spec, line
                    ),
                ),
            }
        }
    }

    /// 在独立线程运行一个无参函数。`interval` 为 None 表示只跑一次（后台异步任务）。
    /// 线程拥有独立的运行时状态，不共享 Rip 变量；数据库、文件等外部资源按各自连接访问。
    pub(crate) fn spawn_task(&self, name: String, interval: Option<Duration>, line: u32) -> bool {
        let seed = self.seed();
        let is_native = self.native.is_some();
        let language = self.language;
        let cluster = self.cluster.clone();
        let thread_name = format!("rip-task-{}", name);
        let task_name = name.clone();
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let mut runtime = seed.build();
                runtime.set_language(language);
                // AOT 线程需要执行生成的模块级初始化（const/static）。
                if is_native {
                    if let Err(error) = runtime.init_entry() {
                        runtime.say(
                            format!("[task] {}: init failed: {}", task_name, error.message),
                            format!("[任务] {}：初始化失败：{}", task_name, error.message),
                        );
                        return;
                    }
                }
                // 多实例部署：定时任务用 Redis 租约选主，同一 tick 只有一个实例执行。
                let mut lock = match (&cluster.redis_url, cluster.task_lock) {
                    (Some(url), true) => match crate::redis::RedisConn::connect_url(url) {
                        Ok(conn) => Some(conn),
                        Err(error) => {
                            runtime.say(
                                format!("[task] cannot use cluster lock: {}", error),
                                format!("[任务] 无法使用集群锁：{}", error),
                            );
                            None
                        }
                    },
                    _ => None,
                };
                if let Some(duration) = interval {
                    let tick = duration.as_millis().max(1) as u64;
                    loop {
                        std::thread::sleep(duration);
                        // 单次 tick panic 不应让定时任务线程永久停摆。
                        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run_task_tick(
                                &mut runtime,
                                lock.as_mut(),
                                &task_name,
                                tick.saturating_mul(2).max(10_000),
                            );
                        }));
                        if outcome.is_err() {
                            runtime.say(
                                format!("[task] {} panicked, continuing", task_name),
                                format!("[任务] {} 发生 panic，继续下一轮", task_name),
                            );
                        }
                    }
                } else {
                    runtime.run_task_once(&task_name);
                }
            })
            .map(|_| true)
            .unwrap_or_else(|error| {
                self.say(
                    format!("[task] cannot start '{}': {} (line {})", name, error, line),
                    format!("[任务] 无法启动 '{}'：{}（第 {} 行）", name, error, line),
                );
                false
            })
    }

    fn run_task_once(&mut self, name: &str) -> bool {
        if !self.has_func(name) {
            self.say(
                format!("[task] unknown function '{}'", name),
                format!("[任务] 未知函数 '{}'", name),
            );
            return false;
        }
        let line = self.funcs.get(name).map(|decl| decl.line).unwrap_or(0);
        match self.call_named(name, Vec::new(), line) {
            Ok(_) => true,
            Err(Flow::Error(error)) => {
                self.say(
                    format!("[task] {}:{}: error: {}", name, error.line, error.message),
                    format!("[任务] {}:{}: 错误：{}", name, error.line, error.message),
                );
                false
            }
            Err(_) => false,
        }
    }

    /// 解释执行入口：初始化模块变量并调用 `main`。
    #[cfg(feature = "interp")]
    pub fn run_main(&mut self) -> Result<Value, RuntimeError> {
        self.start_scheduled_tasks();
        let mut env = Env::new();
        for i in 0..self.module_vars.len() {
            let init = self.module_vars[i].init.clone();
            let name = self.module_vars[i].name.clone();
            let value = match self.eval(&init, &mut env) {
                Ok(value) => value,
                Err(Flow::Error(error)) => return Err(error),
                Err(_) => {
                    return Err(self.runtime_error(
                        format!("failed to initialize module variable '{}'", name),
                        init.line,
                    ))
                }
            };
            self.globals.insert(name, value);
        }

        let main = match self.funcs.get("main").cloned() {
            Some(decl) => decl,
            None => {
                return Err(self.runtime_error(
                    "no 'main' function found in entry module",
                    0,
                ))
            }
        };
        match self.call_decl(&main, Vec::new(), 0) {
            Ok(value) => Ok(value),
            Err(Flow::Error(error)) => Err(error),
            Err(Flow::Return(value)) => Ok(value),
            // main 里未捕获的 `?` / `return err(...)` 必须让进程失败，而不是静默退出 0。
            Err(Flow::Propagate(value)) => Err(self.runtime_error(
                format!("uncaught error: {}", format_value(&value)),
                0,
            )),
            Err(Flow::Break) | Err(Flow::Continue) => Err(self.runtime_error(
                "break/continue outside loop",
                0,
            )),
        }
    }

    /// 依次执行 `#[test]` 函数；模块级变量与 static 只初始化一次。
    #[cfg(feature = "interp")]
    pub fn run_tests(
        &mut self,
        names: &[String],
    ) -> Vec<(String, Result<(), RuntimeError>)> {
        let mut results = Vec::with_capacity(names.len());
        if let Err(error) = self.init_module_vars() {
            for name in names {
                results.push((name.clone(), Err(error.clone())));
            }
            return results;
        }
        for name in names {
            results.push((name.clone(), self.run_test(name, None).result));
        }
        results
    }

    /// 执行单个 `#[test]` 函数。
    ///
    /// `timeout` 为 `None` 表示不限制。超时通过运行时已有的截止时间检查点中断：
    /// 语句、循环迭代与内建调用前都会检查，但正在阻塞的单次系统调用无法中途打断。
    /// 同一个运行时可以重复调用；模块级变量只在首次初始化（`--no-isolate` 模式）。
    #[cfg(feature = "interp")]
    pub fn run_test(&mut self, name: &str, timeout: Option<Duration>) -> TestOutcome {
        let started = Instant::now();
        if let Err(error) = self.init_module_vars() {
            return TestOutcome {
                result: Err(error),
                timed_out: false,
                elapsed: started.elapsed(),
            };
        }
        let deadline = timeout.map(|limit| Instant::now() + limit);
        self.set_deadline(deadline);
        let result = match self.funcs.get(name).cloned() {
            None => Err(self.runtime_error(format!("no '{}' function found", name), 0)),
            Some(decl) => match self.call_decl(&decl, Vec::new(), decl.line) {
                Ok(_) => Ok(()),
                Err(Flow::Error(error)) => Err(error),
                Err(Flow::Return(_)) => Ok(()),
                // 测试里未捕获的 `?` 视为失败，而不是静默通过。
                Err(Flow::Propagate(value)) => Err(self.runtime_error(
                    format!("uncaught error: {}", format_value(&value)),
                    decl.line,
                )),
                Err(Flow::Break) | Err(Flow::Continue) => {
                    Err(self.runtime_error("break/continue outside loop", decl.line))
                }
            },
        };
        let timed_out = self.timed_out();
        self.clear_deadline();
        // 每个用例结束按分配间隔回收环，避免测试之间互相累积。
        crate::gc::collect_if_due();
        TestOutcome {
            result,
            timed_out,
            elapsed: started.elapsed(),
        }
    }

    #[cfg(feature = "interp")]
    pub fn run_repl_stmts(
        &mut self,
        stmts: &[Stmt],
        env: &mut Env,
    ) -> Result<Option<Value>, RuntimeError> {
        let mut last = None;
        for stmt in stmts {
            if let StmtKind::Expr(expr) = &stmt.kind {
                match self.eval(expr, env) {
                    Ok(value) => {
                        if !matches!(expr.kind, ExprKind::Call { .. }) {
                            last = Some(value);
                        }
                    }
                    Err(Flow::Error(error)) => return Err(error),
                    Err(Flow::Return(value)) => return Ok(Some(value)),
                    Err(_) => {}
                }
                continue;
            }
            match self.exec_stmt(stmt, env) {
                Ok(()) => {}
                Err(Flow::Error(error)) => return Err(error),
                Err(Flow::Return(value)) => return Ok(Some(value)),
                Err(_) => return Err(self.runtime_error("unexpected control flow", stmt.line)),
            }
        }
        Ok(last)
    }

    pub(crate) fn error<T>(&self, message: impl Into<String>, line: u32) -> FlowResult<T> {
        Err(Flow::Error(self.runtime_error(message, line)))
    }

    #[cfg(feature = "interp")]
    pub(crate) fn type_of(&self, expr: &Expr) -> Type {
        self.expr_types
            .get(&expr.id)
            .cloned()
            .unwrap_or(Type::Unknown)
    }

    #[cfg(feature = "interp")]
    pub(crate) fn ary_class_name(&self, ty: &Type) -> String {
        // 声明了命名类型时直接用名字：结构相同的不同类型不能互相冒充。
        if let Type::Named(name) = ty {
            if self.types.defs.contains_key(name) {
                return name.clone();
            }
        }
        let resolved = self.types.resolve(ty);
        for (known, name) in &self.type_names {
            if known == &resolved {
                return name.clone();
            }
        }
        String::new()
    }

    #[cfg(feature = "interp")]
    pub(crate) fn call_decl(
        &mut self,
        decl: &FuncDecl,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // 命中字节码时由 run_vm 记录自己的帧，避免同一函数在栈里出现两次。
        // 调试会话必须逐语句解释，否则断点与单步无处落脚。
        if self.debugger.is_none() && decl.params.len() == args.len() {
            if let Some(compiled) = self.vm.by_name.get(&decl.name).cloned() {
                let depth = self.call_stack.len();
                let result = self.run_vm(compiled, args, line);
                self.call_stack.truncate(depth);
                return result;
            }
        }
        // 记录调用帧，出错时随错误一起上报（出错信息里的位置更可读）。
        // 用 truncate 而不是 pop：下层调用可能补充帧。
        let depth = self.call_stack.len();
        self.call_stack.push((decl.name.clone(), decl.line));
        let result = self.call_decl_inner(decl, args, line);
        self.call_stack.truncate(depth);
        result
    }

    #[cfg(feature = "interp")]
    fn call_decl_inner(
        &mut self,
        decl: &FuncDecl,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let _profile = crate::profile::enter(&decl.name);
        if decl.params.len() != args.len() {
            return self.error(
                format!(
                    "function '{}' expects {} arguments, found {}",
                    decl.name,
                    decl.params.len(),
                    args.len()
                ),
                line,
            );
        }
        // 已编译为字节码的函数走快速路径；其余保持树遍历解释。
        if self.debugger.is_none() {
            if let Some(compiled) = self.vm.by_name.get(&decl.name).cloned() {
                return self.run_vm(compiled, args, line);
            }
        }
        let mut env = Env::new();
        for (param, arg) in decl.params.iter().zip(args.into_iter()) {
            let arg = self.coerce_int(&param.ty, arg, line)?;
            env.define(&param.name, arg);
        }
        env.push();
        self.defers.push(Vec::new());
        let result = self.exec_stmts(&decl.body, &mut env);
        let deferred = self.defers.pop().unwrap_or_default();
        env.pop();
        let result = self.run_defers(deferred, result);
        match result {
            Ok(()) => {
                let ret = self.types.resolve(&rp_types::type_from_expr(&decl.ret));
                if ret == Type::Void {
                    Ok(Value::Null)
                } else {
                    self.error(
                        format!("function '{}' ended without returning a value", decl.name),
                        decl.line,
                    )
                }
            }
            Err(Flow::Return(value)) => self.coerce_int(&decl.ret, value, decl.line),
            Err(Flow::Propagate(value)) => Ok(value),
            Err(Flow::Break) | Err(Flow::Continue) => {
                self.error("break/continue outside loop", line)
            }
            Err(Flow::Error(error)) => Err(Flow::Error(error)),
        }
    }

    #[cfg(feature = "interp")]
    fn call_closure(
        &mut self,
        params: &[Param],
        ret: &TypeExpr,
        body: &[Stmt],
        captured: &Env,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let _profile = crate::profile::enter("<closure>");
        if params.len() != args.len() {
            return self.error(
                format!(
                    "closure expects {} arguments, found {}",
                    params.len(),
                    args.len()
                ),
                line,
            );
        }
        let mut env = captured.clone();
        env.push();
        for (param, arg) in params.iter().zip(args.into_iter()) {
            let arg = self.coerce_int(&param.ty, arg, line)?;
            env.define(&param.name, arg);
        }
        self.defers.push(Vec::new());
        let result = self.exec_stmts(body, &mut env);
        let deferred = self.defers.pop().unwrap_or_default();
        env.pop();
        let result = self.run_defers(deferred, result);
        match result {
            Ok(()) => {
                let ret = self.types.resolve(&rp_types::type_from_expr(ret));
                if ret == Type::Void {
                    Ok(Value::Null)
                } else {
                    self.error("closure ended without returning a value", line)
                }
            }
            Err(Flow::Return(value)) => self.coerce_int(ret, value, line),
            Err(Flow::Propagate(value)) => Ok(value),
            Err(Flow::Break) | Err(Flow::Continue) => {
                self.error("break/continue outside loop", line)
            }
            Err(Flow::Error(error)) => Err(Flow::Error(error)),
        }
    }

    /// 按后进先出执行当前函数登记的 `defer`，再返回原本的退出流程。
    /// `defer` 自身出错时覆盖原结果（更接近出错现场）。
    fn run_defers(
        &mut self,
        deferred: Vec<(Deferred, u32)>,
        original: FlowResult<()>,
    ) -> FlowResult<()> {
        let mut result = original;
        for (call, line) in deferred.into_iter().rev() {
            if let Err(flow) = self.run_deferred(call, line) {
                result = Err(flow);
            }
        }
        result
    }

    fn run_deferred(&mut self, call: Deferred, line: u32) -> FlowResult<()> {
        match call {
            Deferred::Named(name, args) => {
                if name == "print" {
                    let text: Vec<String> = args.iter().map(format_value).collect();
                    crate::debug::emit_line(text.join(" "));
                    Ok(())
                } else {
                    self.call_named(&name, args, line).map(|_| ())
                }
            }
            Deferred::Func(value, args) => match value {
                Value::Func(func) => self.call_value(&func, args, line).map(|_| ()),
                other => self.error(
                    format!("cannot call value {}", format_value(&other)),
                    line,
                ),
            },
            Deferred::Method(receiver, name, args) => {
                self.call_method(receiver, &name, args, line).map(|_| ())
            }
            Deferred::Module(path, name, args) => {
                self.call_module(&path, &name, args, line).map(|_| ())
            }
        }
    }

    /// 进入一个函数帧：AOT 生成代码在函数入口调用。
    pub fn defer_begin(&mut self) {
        self.defers.push(Vec::new());
    }

    /// 登记一个已就地求值的 `defer` 调用；AOT 生成代码使用。
    pub fn defer_push(&mut self, call: Deferred, line: u32) {
        if let Some(frame) = self.defers.last_mut() {
            frame.push((call, line));
        }
    }

    /// 退出函数帧：倒序执行该帧登记的 `defer`。
    pub fn defer_end(&mut self) -> FlowResult<()> {
        let frame = self.defers.pop().unwrap_or_default();
        self.run_defers(frame, Ok(()))
    }

    /// 在 `defer` 语句处立即求值被调用者与实参（Go 语义），返回值在函数退出时丢弃。
    #[cfg(feature = "interp")]
    fn prepare_defer(&mut self, call: &Expr, env: &mut Env) -> FlowResult<Deferred> {
        match &call.kind {
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Ident(name) = &callee.kind {
                    if name == "print" || self.has_func(name) {
                        let mut values = Vec::with_capacity(args.len());
                        for arg in args {
                            values.push(self.eval(arg, env)?);
                        }
                        return Ok(Deferred::Named(name.clone(), values));
                    }
                }
                if let ExprKind::Field { base, name } = &callee.kind {
                    if let ExprKind::Ident(alias) = &base.kind {
                        if let Some(path) = self.imports.get(alias).cloned() {
                            let mut values = Vec::with_capacity(args.len());
                            for arg in args {
                                values.push(self.eval(arg, env)?);
                            }
                            return Ok(Deferred::Module(path, name.clone(), values));
                        }
                    }
                }
                // 函数值形式：先取调用者再求值实参，与普通调用顺序一致。
                let callee_value = self.eval(callee, env)?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg, env)?);
                }
                match callee_value {
                    Value::Func(_) => Ok(Deferred::Func(callee_value, values)),
                    other => self.error(
                        format!("cannot call value {}", format_value(&other)),
                        call.line,
                    ),
                }
            }
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                if let ExprKind::Ident(alias) = &receiver.kind {
                    if let Some(path) = self.imports.get(alias).cloned() {
                        let mut values = Vec::with_capacity(args.len());
                        for arg in args {
                            values.push(self.eval(arg, env)?);
                        }
                        return Ok(Deferred::Module(path, name.clone(), values));
                    }
                }
                let receiver_value = self.eval(receiver, env)?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval(arg, env)?);
                }
                Ok(Deferred::Method(receiver_value, name.clone(), values))
            }
            _ => self.error(
                "defer requires a function call, e.g. `defer conn.close();`",
                call.line,
            ),
        }
    }

    pub fn call_value(
        &mut self,
        func: &Function,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        // 借用而不是 clone：Router 的克隆会深拷贝整张路由表（每个 HTTP 请求一次）。
        match &func.kind {
            #[cfg(feature = "interp")]
            FuncKind::Decl(decl) => self.call_decl(decl, args, line),
            #[cfg(feature = "interp")]
            FuncKind::Closure {
                params,
                ret,
                body,
                captured,
            } => self.call_closure(params, ret, body, captured, args, line),
            #[cfg(not(feature = "interp"))]
            FuncKind::Decl(_) | FuncKind::Closure { .. } => self.error(
                "AST functions are not available in this AOT build",
                line,
            ),
            FuncKind::Router(routes) => self.invoke_router(routes, args, line),
            FuncKind::Middleware(spec) => {
                let next = match args.first() {
                    Some(value) => value.clone(),
                    None => return self.error("middleware expects a handler", line),
                };
                Ok(Value::func_rc(Rc::new(Function {
                    name: "<middleware>".to_string(),
                    kind: FuncKind::MiddlewareApplied {
                        spec: spec.clone(),
                        next,
                    },
                })))
            }
            FuncKind::MiddlewareApplied { spec, next } => {
                self.invoke_middleware(spec, next, args, line)
            }
            FuncKind::Chain {
                middlewares,
                handler,
                composed,
            } => self.invoke_chain(middlewares, handler, composed, args, line),
            FuncKind::Mounted { prefix, inner } => {
                self.invoke_mounted(prefix, inner, args, line)
            }
            FuncKind::Static { dir, prefix } => self.serve_static(dir, prefix, args, line),
            FuncKind::SseSend(sink) => {
                // 直写连接套接字：客户端断开（写失败）时让生产函数以错误提前结束。
                match sink.send(&encode_sse_frame(args.first())) {
                    Ok(()) => Ok(Value::Null),
                    Err(_) => self.error("sse connection closed", line),
                }
            }
            FuncKind::Native { arity, body } => {
                if args.len() != *arity {
                    return self.error(
                        format!(
                            "native function expects {} arguments, found {}",
                            arity,
                            args.len()
                        ),
                        line,
                    );
                }
                body(self, args, line)
            }
        }
    }

    #[cfg(feature = "interp")]
    fn exec_stmts(&mut self, stmts: &[Stmt], env: &mut Env) -> FlowResult<()> {
        for stmt in stmts {
            self.exec_stmt(stmt, env)?;
        }
        Ok(())
    }

    #[cfg(feature = "interp")]
    fn exec_block(&mut self, stmts: &[Stmt], env: &mut Env) -> FlowResult<()> {
        env.push();
        let result = self.exec_stmts(stmts, env);
        env.pop();
        result
    }

    /// 调试检查点：命中断点 / 单步 / 暂停请求时阻塞，并向适配器发送停止事件。
    /// 未注入调试器或没有需要停下的条件时，只做一次原子读。
    #[cfg(feature = "interp")]
    fn debug_check(&self, line: u32, env: &Env) {
        let debugger = match &self.debugger {
            Some(debugger) if debugger.active() => debugger.clone(),
            _ => return,
        };
        let depth = self.call_stack.len();
        debugger.check(line, depth, |reason| {
            let mut frames = self.call_stack.clone();
            if let Some(last) = frames.last_mut() {
                last.1 = line;
            }
            let mut names: Vec<&String> = self.globals.keys().collect();
            names.sort();
            let globals = names
                .into_iter()
                .map(|name| (name.clone(), format_value(&self.globals[name])))
                .collect();
            crate::debug::StopInfo {
                reason: reason.to_string(),
                line,
                depth,
                frames,
                locals: env.snapshot(),
                globals,
            }
        });
    }

    #[cfg(feature = "interp")]
    fn exec_stmt(&mut self, stmt: &Stmt, env: &mut Env) -> FlowResult<()> {
        // 每个语句都是一次中断检查点：长循环与递归会在下一个语句边界返回超时。
        self.check_deadline(stmt.line)?;
        // 调试器检查点：未注入调试器时无额外开销。
        self.debug_check(stmt.line, env);
        // 当前帧的行号随语句推进：错误栈与调试栈都显示到调用点。
        if let Some(last) = self.call_stack.last_mut() {
            last.1 = stmt.line;
        }
        // 覆盖率采集：未开启时只是一次布尔判断，不影响热路径。
        if self.coverage {
            let function = self
                .call_stack
                .last()
                .map(|(name, _)| name.as_str())
                .unwrap_or("");
            crate::coverage::record(function, stmt.line);
        }
        match &stmt.kind {
            StmtKind::Let {
                name, ty, init, ..
            } => {
                let value = self.eval(init, env)?;
                let value = self.coerce_int(ty, value, stmt.line)?;
                env.define(name, value);
                Ok(())
            }
            StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(())
            }
            StmtKind::Assign { target, value } => {
                let value = self.eval(value, env)?;
                self.assign(target, value, env, stmt.line)
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond = self.eval(cond, env)?;
                if self.truthy(cond, stmt.line)? {
                    self.exec_block(then_body, env)
                } else if let Some(body) = else_body {
                    self.exec_block(body, env)
                } else {
                    Ok(())
                }
            }
            StmtKind::Loop { body } => {
                loop {
                    self.check_deadline(stmt.line)?;
                    match self.exec_block(body, env) {
                        Ok(()) => {}
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                loop {
                    self.check_deadline(stmt.line)?;
                    let value = self.eval(cond, env)?;
                    if !self.truthy(value, stmt.line)? {
                        break;
                    }
                    match self.exec_block(body, env) {
                        Ok(()) => {}
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                loop {
                    self.check_deadline(stmt.line)?;
                    let value = self.eval(scrutinee, env)?;
                    let bindings = match self.match_pattern(pattern, &value)? {
                        Some(bindings) => bindings,
                        None => break,
                    };
                    env.push();
                    for (name, value) in bindings {
                        env.define(&name, value);
                    }
                    let result = self.exec_stmts(body, env);
                    env.pop();
                    match result {
                        Ok(()) => {}
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::ForIn { name, iter, body } => {
                let iterable = self.eval(iter, env)?;
                let items = match &iterable {
                    Value::Vec(items) => items.borrow().clone(),
                    Value::Map(map) => map
                        .borrow()
                        .entries
                        .iter()
                        .map(|(k, _)| k.clone())
                        .collect(),
                    other => {
                        return self.error(
                            format!("for-in requires vec or map, found {}", format_value(other)),
                            stmt.line,
                        )
                    }
                };
                for item in items {
                    self.check_deadline(stmt.line)?;
                    env.push();
                    env.define(name, item);
                    let result = self.exec_stmts(body, env);
                    env.pop();
                    match result {
                        Ok(()) => {}
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            StmtKind::Defer { call } => {
                let prepared = self.prepare_defer(call, env)?;
                match self.defers.last_mut() {
                    Some(frame) => {
                        frame.push((prepared, stmt.line));
                        Ok(())
                    }
                    None => self.error("defer outside of function", stmt.line),
                }
            }
            StmtKind::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval(expr, env)?,
                    None => Value::Null,
                };
                Err(Flow::Return(value))
            }
            StmtKind::Break => Err(Flow::Break),
            StmtKind::Continue => Err(Flow::Continue),
            StmtKind::Match { subject, arms } => {
                let value = self.eval(subject, env)?;
                for arm in arms {
                    if let Some(bindings) = self.match_pattern(&arm.pattern, &value)? {
                        env.push();
                        for (name, value) in bindings {
                            env.define(&name, value);
                        }
                        let result = self.exec_stmts(&arm.body, env);
                        env.pop();
                        return result;
                    }
                }
                self.error("no match arm matched value", stmt.line)
            }
            StmtKind::Parallel { branches } => self.exec_parallel(branches, stmt.line),
        }
    }

    /// 执行 `parallel` 块：每条分支在自己的线程、自己的运行时里跑，全部结束后再返回。
    ///
    /// 分支看不到块外局部变量，因此不需要跨线程传值；数据库连接池、日志等进程级资源共享。
    #[cfg(feature = "interp")]
    fn exec_parallel(&mut self, branches: &[Stmt], line: u32) -> FlowResult<()> {
        let seed = self.seed();
        let options = self.thread_options();
        let mut handles = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let branch = branch.clone();
            let seed = seed.clone();
            let options = options.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("rip-par-{}", index))
                .spawn(move || {
                    let mut runtime = seed.build();
                    runtime.apply_thread_options(&options);
                    runtime.init_entry()?;
                    let mut env = Env::new();
                    match runtime.exec_stmt(&branch, &mut env) {
                        Ok(()) => Ok(()),
                        Err(Flow::Error(error)) => Err(error),
                        Err(_) => Err(runtime.runtime_error(
                            "'return', 'break' or 'continue' cannot leave a parallel branch",
                            branch.line,
                        )),
                    }
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    return self.error(
                        format!("cannot start parallel branch: {}", error),
                        line,
                    )
                }
            }
        }
        join_parallel(self, handles, line)
    }

    #[cfg(feature = "interp")]
    fn match_pattern(
        &self,
        pattern: &Pattern,
        value: &Value,
    ) -> FlowResult<Option<Vec<(String, Value)>>> {
        let bindings = match (pattern, value) {
            (Pattern::Some(name), Value::Some(inner)) => {
                Some(vec![(name.clone(), (**inner).clone())])
            }
            (Pattern::None, Value::None) => Some(Vec::new()),
            (Pattern::Ok(name), Value::Ok(inner)) => {
                Some(vec![(name.clone(), (**inner).clone())])
            }
            (Pattern::Err(name), Value::Err(inner)) => {
                Some(vec![(name.clone(), (**inner).clone())])
            }
            (Pattern::Ary(fields), Value::Ary(ary)) => {
                let ary = ary.borrow();
                let mut bindings = Vec::new();
                for field in fields {
                    if field.name == "_" {
                        continue;
                    }
                    match ary.get(&field.name) {
                        Some(value) => bindings.push((field.name.clone(), value)),
                        None => return Ok(None),
                    }
                }
                Some(bindings)
            }
            (
                Pattern::Enum {
                    name,
                    variant,
                    fields,
                },
                Value::Ary(ary),
            ) => {
                let ary = ary.borrow();
                if ary.class != *name {
                    return Ok(None);
                }
                let found = ary.get("#variant").and_then(|value| value.as_str());
                if found.as_deref() != Some(variant.as_str()) {
                    return Ok(None);
                }
                let mut bindings = Vec::new();
                for field in fields {
                    if field.name == "_" {
                        continue;
                    }
                    match ary.get(&field.name) {
                        Some(value) => bindings.push((field.name.clone(), value)),
                        None => return Ok(None),
                    }
                }
                Some(bindings)
            }
            _ => None,
        };
        Ok(bindings)
    }

    pub fn truthy(&mut self, value: Value, line: u32) -> FlowResult<bool> {
        match value {
            Value::Bool(b) => Ok(b),
            other => self.error(
                format!("condition must be bool, found {}", format_value(&other)),
                line,
            ),
        }
    }

    #[cfg(feature = "interp")]
    fn assign(
        &mut self,
        target: &Expr,
        value: Value,
        env: &mut Env,
        line: u32,
    ) -> FlowResult<()> {
        let (root, path) = target_path(target);
        if path.is_empty() {
            if env.set(&root, value.clone()) {
                return Ok(());
            }
            if self.globals.contains_key(&root) {
                self.globals.insert(root, value);
                return Ok(());
            }
            return self.error(format!("unknown variable '{}'", root), line);
        }
        let root_value = match env.get(&root) {
            Some(value) => value.clone(),
            None => match self.globals.get(&root) {
                Some(value) => value.clone(),
                None => return self.error(format!("unknown variable '{}'", root), line),
            },
        };
        match set_path(&root_value, &path, value) {
            Ok(()) => Ok(()),
            Err(message) => self.error(message, line),
        }
    }

    #[cfg(feature = "interp")]
    fn eval(&mut self, expr: &Expr, env: &mut Env) -> FlowResult<Value> {
        match &expr.kind {
            ExprKind::Int { value, suffix, .. } => Ok(int_literal_value(*value, *suffix)),
            ExprKind::Float(value) => Ok(Value::Float(*value)),
            ExprKind::Str(value) => Ok(Value::string(value.clone())),
            ExprKind::Char(value) => Ok(Value::Char(value.clone())),
            ExprKind::Bool(value) => Ok(Value::Bool(*value)),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::None => Ok(Value::None),
            ExprKind::Ident(name) => {
                if let Some(value) = env.get(name) {
                    return Ok(value.clone());
                }
                if let Some(value) = self.globals.get(name) {
                    return Ok(value.clone());
                }
                if let Some(decl) = self.funcs.get(name) {
                    return Ok(Value::func_rc(Rc::new(Function {
                        name: name.clone(),
                        kind: FuncKind::Decl(decl.clone()),
                    })));
                }
                self.error(format!("unknown identifier '{}'", name), expr.line)
            }
            ExprKind::Field { base, name } => {
                // `枚举.变体`：base 是类型名而不是变量，直接构造单元变体。
                if let ExprKind::Ident(enum_name) = &base.kind {
                    if env.get(enum_name).is_none()
                        && !self.globals.contains_key(enum_name)
                        && self.checked.enums.contains_key(enum_name)
                    {
                        return Ok(self.make_enum(enum_name, name, Vec::new()));
                    }
                }
                let base_value = self.eval(base, env)?;
                self.get_field(base_value, name, expr.line)
            }
            ExprKind::Call { callee, args, .. } => self.eval_call(callee, args, env, expr.line),
            ExprKind::Await(inner) => {
                let value = self.eval(inner, env)?;
                self.await_task(value, expr.line)
            }
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                if let ExprKind::Ident(alias) = &receiver.kind {
                    if let Some(path) = self.imports.get(alias).cloned() {
                        let mut values = Vec::new();
                        for arg in args {
                            values.push(self.eval(arg, env)?);
                        }
                        return self.call_module(&path, name, values, expr.line);
                    }
                }
                let receiver_value = self.eval(receiver, env)?;
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.eval(arg, env)?);
                }
                self.call_method(receiver_value, name, values, expr.line)
            }
            ExprKind::Unary { op, expr: inner } => {
                let value = self.eval(inner, env)?;
                let value = self.unary_value(*op, value, expr.line)?;
                self.check_int_width(expr.id, value, expr.line)
            }
            ExprKind::Binary { op, left, right } => {
                let value = self.eval_binary(*op, left, right, env, expr.line)?;
                self.check_int_width(expr.id, value, expr.line)
            }
            ExprKind::List(items) => self.eval_list(items, expr, env),
            ExprKind::Struct { path, fields } => {
                self.eval_struct(path, fields, expr, env)
            }
            ExprKind::Some(inner) => Ok(Value::Some(Box::new(self.eval(inner, env)?))),
            ExprKind::Ok(inner) => Ok(Value::Ok(Box::new(self.eval(inner, env)?))),
            ExprKind::Err(inner) => Ok(Value::Err(Box::new(self.eval(inner, env)?))),
            ExprKind::Closure { params, ret, body } => Ok(Value::func_rc(Rc::new(Function {
                name: "<closure>".to_string(),
                kind: FuncKind::Closure {
                    params: params.clone(),
                    ret: ret.clone(),
                    body: Rc::new(body.clone()),
                    captured: env.clone(),
                },
            }))),
            ExprKind::Try(inner) => {
                let value = self.eval(inner, env)?;
                match value {
                    Value::Ok(value) => Ok(*value),
                    Value::Err(error) => Err(Flow::Propagate(Value::Err(error))),
                    other => self.error(
                        format!("'?' requires result, found {}", format_value(&other)),
                        expr.line,
                    ),
                }
            }
        }
    }

    #[cfg(feature = "interp")]
    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        env: &mut Env,
        line: u32,
    ) -> FlowResult<Value> {
        if let ExprKind::Ident(name) = &callee.kind {
            if name == "print" {
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.eval(arg, env)?);
                }
                let text: Vec<String> = values.iter().map(format_value).collect();
                crate::debug::emit_line(text.join(" "));
                return Ok(Value::Null);
            }
            if let Some(decl) = self.funcs.get(name).cloned() {
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.eval(arg, env)?);
                }
                if decl.is_async {
                    return self.call_async(&decl.name, values, line);
                }
                return self.call_decl(&decl, values, line);
            }
        }

        if let ExprKind::Field { base, name } = &callee.kind {
            if let ExprKind::Ident(alias) = &base.kind {
                if let Some(path) = self.imports.get(alias).cloned() {
                    let mut values = Vec::new();
                    for arg in args {
                        values.push(self.eval(arg, env)?);
                    }
                    return self.call_module(&path, name, values, line);
                }
            }
        }

        let callee_value = self.eval(callee, env)?;
        let mut values = Vec::new();
        for arg in args {
            values.push(self.eval(arg, env)?);
        }
        match callee_value {
            Value::Func(func) => self.call_value(&func, values, line),
            other => self.error(
                format!("cannot call value {}", format_value(&other)),
                line,
            ),
        }
    }

    fn get_field(&mut self, base: Value, name: &str, line: u32) -> FlowResult<Value> {
        match base {
            Value::Ary(ary) => {
                let value = ary.borrow().get(name);
                match value {
                    Some(value) => Ok(value),
                    None => self.error(format!("no field named '{}'", name), line),
                }
            }
            Value::Vec(items) => {
                if let Ok(index) = name.parse::<usize>() {
                    let items = items.borrow();
                    match items.get(index) {
                        Some(value) => Ok(value.clone()),
                        None => self.error(format!("index {} out of bounds", index), line),
                    }
                } else {
                    self.error(format!("vec has no field '{}'", name), line)
                }
            }
            other => self.error(
                format!("type {} has no fields", format_value(&other)),
                line,
            ),
        }
    }

    #[cfg(feature = "interp")]
    fn eval_list(&mut self, items: &[ListItem], whole: &Expr, env: &mut Env) -> FlowResult<Value> {
        let raw = self.type_of(whole);
        let ty = self.types.resolve(&raw);
        match ty {
            Type::Map(_, _) => {
                let mut entries = Vec::new();
                for item in items {
                    match item {
                        ListItem::Named(name, value) => {
                            entries.push((
                                Value::string(name.clone()),
                                self.eval(value, env)?,
                            ));
                        }
                        ListItem::Key(key, value) => {
                            let key = self.eval(key, env)?;
                            let value = self.eval(value, env)?;
                            entries.push((key, value));
                        }
                        _ => {
                            return self.error(
                                "map literal requires key = value entries",
                                whole.line,
                            )
                        }
                    }
                }
                Ok(Value::map(entries))
            }
            Type::Ary(_) => self.eval_ary_literal(items, &raw, env, whole.line),
            Type::Vec(_) | Type::VecHetero(_) | Type::VecDynamic => {
                let mut values = Vec::new();
                for item in items {
                    match item {
                        ListItem::Value(value)
                        | ListItem::Named(_, value)
                        | ListItem::Key(_, value)
                        | ListItem::Typed(_, value) => values.push(self.eval(value, env)?),
                    }
                }
                Ok(Value::vec(values))
            }
            _ => {
                let all_named = items
                    .iter()
                    .all(|i| matches!(i, ListItem::Named(..) | ListItem::Key(..)));
                if all_named {
                    let mut entries = Vec::new();
                    for item in items {
                        match item {
                            ListItem::Named(name, value) => entries.push((
                                Value::string(name.clone()),
                                self.eval(value, env)?,
                            )),
                            ListItem::Key(key, value) => {
                                let key = self.eval(key, env)?;
                                let value = self.eval(value, env)?;
                                entries.push((key, value));
                            }
                            _ => {}
                        }
                    }
                    Ok(Value::map(entries))
                } else {
                    let mut values = Vec::new();
                    for item in items {
                        match item {
                            ListItem::Value(value)
                            | ListItem::Named(_, value)
                            | ListItem::Key(_, value)
                            | ListItem::Typed(_, value) => {
                                values.push(self.eval(value, env)?)
                            }
                        }
                    }
                    Ok(Value::vec(values))
                }
            }
        }
    }

    #[cfg(feature = "interp")]
    fn eval_ary_literal(
        &mut self,
        items: &[ListItem],
        ty: &Type,
        env: &mut Env,
        line: u32,
    ) -> FlowResult<Value> {
        let class = self.ary_class_name(ty);
        let mut fields: Vec<(String, Value)> = Vec::new();
        for item in items {
            match item {
                ListItem::Named(name, value) => {
                    fields.push((name.clone(), self.eval(value, env)?));
                }
                _ => {
                    return self.error(
                        "ary literal requires named fields: [field = value, ...]",
                        line,
                    )
                }
            }
        }
        Ok(Value::ary(class, fields))
    }

    #[cfg(feature = "interp")]
    fn eval_struct(
        &mut self,
        path: &[String],
        fields: &[(String, Expr)],
        whole: &Expr,
        env: &mut Env,
    ) -> FlowResult<Value> {
        // `枚举.变体[字段 = 值]`：构造枚举值。
        if path.len() == 2 && self.checked.enums.contains_key(&path[0]) {
            let mut values = Vec::with_capacity(fields.len());
            for (name, value) in fields {
                values.push((name.clone(), self.eval(value, env)?));
            }
            return Ok(self.make_enum(&path[0], &path[1], values));
        }
        let class = path.join(".");
        let mut result: Vec<(String, Value)> = Vec::new();
        if class == "http.response" {
            result.push(("status".to_string(), Value::Int(200)));
            result.push(("headers".to_string(), Value::empty_map()));
            result.push(("body".to_string(), Value::string("")));
            result.push(("json".to_string(), Value::None));
            result.push(("sse".to_string(), Value::None));
        }
        for (name, value) in fields {
            let value = self.eval(value, env)?;
            let value = if class == "http.response" && name == "json" {
                match value {
                    Value::None | Value::Some(_) => value,
                    other => Value::Some(Box::new(other)),
                }
            } else {
                value
            };
            if let Some((_, slot)) = result.iter_mut().find(|(n, _)| n == name) {
                *slot = value;
            } else {
                result.push((name.clone(), value));
            }
        }
        let _ = whole;
        Ok(Value::ary(class, result))
    }

    #[cfg(feature = "interp")]
    fn eval_binary(
        &mut self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        env: &mut Env,
        line: u32,
    ) -> FlowResult<Value> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let left_value = self.eval(left, env)?;
            let left_bool = self.truthy(left_value, line)?;
            if op == BinOp::And && !left_bool {
                return Ok(Value::Bool(false));
            }
            if op == BinOp::Or && left_bool {
                return Ok(Value::Bool(true));
            }
            let right_value = self.eval(right, env)?;
            return Ok(Value::Bool(self.truthy(right_value, line)?));
        }

        let left_value = self.eval(left, env)?;
        let right_value = self.eval(right, env)?;
        self.binary_values(op, left_value, right_value, line)
    }

    /// 非短路二元运算；解释器与字节码 VM 共用，保证语义一致。
    pub fn binary_values(
        &mut self,
        op: BinOp,
        left_value: Value,
        right_value: Value,
        line: u32,
    ) -> FlowResult<Value> {
        match op {
            BinOp::Eq => Ok(Value::Bool(value_eq(&left_value, &right_value))),
            BinOp::Ne => Ok(Value::Bool(!value_eq(&left_value, &right_value))),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.compare(op, &left_value, &right_value, line)
            }
            BinOp::Add => {
                if let (Some(a), Some(b)) = (left_value.as_str(), right_value.as_str()) {
                    return Ok(Value::string(format!("{}{}", a, b)));
                }
                self.arithmetic(op, &left_value, &right_value, line)
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                self.arithmetic(op, &left_value, &right_value, line)
            }
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    /// AOT 中对确定是 `int` 的二元运算走这里：先尝试原生整数路径，
    /// 命中就跳过字符串尝试与通用 match；其余情况回退 `binary_values`。
    #[inline]
    pub fn int_binary(
        &mut self,
        op: BinOp,
        left_value: Value,
        right_value: Value,
        line: u32,
    ) -> FlowResult<Value> {
        // 只有算术运算能走 `int_arith`；比较运算仍交给通用路径。
        if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) {
            if let (Value::Int(a), Value::Int(b)) = (&left_value, &right_value) {
                return self.int_arith(op, *a, *b, line, Value::Int);
            }
        }
        self.binary_values(op, left_value, right_value, line)
    }

    /// 一元运算；解释器与字节码 VM 共用。
    pub fn unary_value(&mut self, op: UnOp, value: Value, line: u32) -> FlowResult<Value> {
        match op {
            UnOp::Not => match value {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => self.error(
                    format!("! requires bool, found {}", format_value(&other)),
                    line,
                ),
            },
            UnOp::Neg => match value {
                Value::Int(v) => v
                    .checked_neg()
                    .map(Value::Int)
                    .map(Ok)
                    .unwrap_or_else(|| self.error("integer overflow", line)),
                Value::UInt(v) => Ok(Value::UInt(v.wrapping_neg())),
                Value::Float(v) => Ok(Value::Float(-v)),
                other => self.error(
                    format!("unary - requires number, found {}", format_value(&other)),
                    line,
                ),
            },
        }
    }

    /// 按声明类型把整数归一成正确的表示（`int`/有符号 → `Int`，无符号 → `UInt`），
    /// 并检查范围：`let x: u8 = 300` 这类越界在绑定时直接报错，而不是静默截断。
    #[cfg(feature = "interp")]
    fn coerce_int(&mut self, ty: &TypeExpr, value: Value, line: u32) -> FlowResult<Value> {
        let suffix = match type_int_suffix(ty) {
            Some(suffix) => suffix,
            None => return Ok(value),
        };
        match crate::value::coerce_int_name(value, suffix.name(), line) {
            Ok(value) => Ok(value),
            Err(message) => self.error(message, line),
        }
    }

    /// 运算结果落在固定位宽整数上时按该位宽检查：`let a: u8 = 255; a + 1` 在运算处就 panic，
    /// 而不是等到赋值时才报错（规范：同类型运算按该类型位宽计算，溢出 panic）。
    #[cfg(feature = "interp")]
    fn check_int_width(&mut self, id: NodeId, value: Value, line: u32) -> FlowResult<Value> {
        if !self.has_width_ints {
            return Ok(value);
        }
        let suffix = match self.expr_types.get(&id).and_then(|ty| ty.int_suffix()) {
            Some(suffix) if suffix != IntSuffix::I64 => suffix,
            _ => return Ok(value),
        };
        match crate::value::coerce_int_name(value, suffix.name(), line) {
            Ok(value) => Ok(value),
            Err(message) => self.error(message, line),
        }
    }

    fn arithmetic(
        &mut self,
        op: BinOp,
        left: &Value,
        right: &Value,
        line: u32,
    ) -> FlowResult<Value> {
        match (left, right) {
            (Value::Float(a), Value::Float(b)) => {
                let value = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Mod => a % b,
                    _ => unreachable!(),
                };
                Ok(Value::Float(value))
            }
            (Value::Int(a), Value::Int(b)) => self.int_arith(op, *a, *b, line, Value::Int),
            (Value::UInt(a), Value::UInt(b)) => self.uint_arith(op, *a, *b, line),
            // 无后缀字面量可能以 `Int` 位模式出现，与 `UInt` 运算时按无符号位模式解释。
            (Value::Int(a), Value::UInt(b)) => self.uint_arith(op, *a as u64, *b, line),
            (Value::UInt(a), Value::Int(b)) => self.uint_arith(op, *a, *b as u64, line),
            _ => self.error(
                format!(
                    "operator does not support {} and {}",
                    format_value(left),
                    format_value(right)
                ),
                line,
            ),
        }
    }

    #[inline]
    fn int_arith(
        &mut self,
        op: BinOp,
        a: i64,
        b: i64,
        line: u32,
        wrap: fn(i64) -> Value,
    ) -> FlowResult<Value> {
        let result = match op {
            BinOp::Add => a.checked_add(b),
            BinOp::Sub => a.checked_sub(b),
            BinOp::Mul => a.checked_mul(b),
            BinOp::Div => {
                if b == 0 {
                    return self.error("division by zero", line);
                }
                a.checked_div(b)
            }
            BinOp::Mod => {
                if b == 0 {
                    return self.error("modulo by zero", line);
                }
                a.checked_rem(b)
            }
            _ => None,
        };
        match result {
            Some(value) => Ok(wrap(value)),
            None => self.error("integer overflow", line),
        }
    }

    /// 无符号整数运算：溢出 panic（不回绕），除零同样报错。
    #[inline]
    fn uint_arith(&mut self, op: BinOp, a: u64, b: u64, line: u32) -> FlowResult<Value> {
        let result = match op {
            BinOp::Add => a.checked_add(b),
            BinOp::Sub => a.checked_sub(b),
            BinOp::Mul => a.checked_mul(b),
            BinOp::Div => {
                if b == 0 {
                    return self.error("division by zero", line);
                }
                a.checked_div(b)
            }
            BinOp::Mod => {
                if b == 0 {
                    return self.error("modulo by zero", line);
                }
                a.checked_rem(b)
            }
            _ => None,
        };
        match result {
            Some(value) => Ok(Value::UInt(value)),
            None => self.error("integer overflow", line),
        }
    }

    fn compare(
        &mut self,
        op: BinOp,
        left: &Value,
        right: &Value,
        line: u32,
    ) -> FlowResult<Value> {
        let ordering = match (left, right) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::UInt(a), Value::UInt(b)) => a.partial_cmp(b),
            (Value::Int(a), Value::UInt(b)) => (*a as u64).partial_cmp(b),
            (Value::UInt(a), Value::Int(b)) => a.partial_cmp(&(*b as u64)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Str(a), Value::Str(b)) => a.partial_cmp(b),
            _ => {
                return self.error(
                    format!(
                        "cannot compare {} and {}",
                        format_value(left),
                        format_value(right)
                    ),
                    line,
                )
            }
        };
        use std::cmp::Ordering;
        let result = match ordering {
            Some(ordering) => match op {
                BinOp::Lt => ordering == Ordering::Less,
                BinOp::Gt => ordering == Ordering::Greater,
                BinOp::Le => ordering != Ordering::Greater,
                BinOp::Ge => ordering != Ordering::Less,
                _ => false,
            },
            None => false,
        };
        Ok(Value::Bool(result))
    }
}

/// 从类型表达式读出整数位宽；非整数类型返回 `None`。
#[cfg(feature = "interp")]
fn type_int_suffix(ty: &TypeExpr) -> Option<IntSuffix> {
    let name = match &ty.kind {
        TypeExprKind::Named(path) => path.last()?,
        _ => return None,
    };
    match name.as_str() {
        "int" | "i64" => Some(IntSuffix::I64),
        other => IntSuffix::from_name(other),
    }
}

/// 解析 `#[interval("30s")]` 的时间间隔：支持 ms / s / m / h / d，必须为正整数。
fn parse_interval(spec: &str) -> Option<Duration> {
    let text = spec.trim();
    let (value, multiplier) = if let Some(value) = text.strip_suffix("ms") {
        (value, 1u64)
    } else if let Some(value) = text.strip_suffix('s') {
        (value, 1000)
    } else if let Some(value) = text.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = text.strip_suffix('h') {
        (value, 3_600_000)
    } else if let Some(value) = text.strip_suffix('d') {
        (value, 86_400_000)
    } else {
        return None;
    };
    let amount: u64 = value.trim().parse().ok()?;
    if amount == 0 {
        return None;
    }
    amount
        .checked_mul(multiplier)
        .map(Duration::from_millis)
}

/// 把 `http.sse_event` 编码成一帧 SSE 文本（`event:` + 多行 `data:`）。
fn encode_sse_frame(event: Option<&Value>) -> String {
    let (name, data) = match event {
        Some(Value::Ary(ary)) => {
            let ary = ary.borrow();
            let name = ary
                .get("event")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let data = ary
                .get("data")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .replace("\r\n", "\n");
            (name, data)
        }
        _ => (String::new(), String::new()),
    };
    let mut frame = String::with_capacity(data.len() + 16);
    if !name.is_empty() {
        frame.push_str("event: ");
        frame.push_str(&name);
        frame.push('\n');
    }
    // 多行数据必须逐行加前缀，否则客户端只会收到第一行。
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line);
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

#[cfg(feature = "interp")]
fn target_path(expr: &Expr) -> (String, Vec<String>) {
    match &expr.kind {
        ExprKind::Ident(name) => (name.clone(), Vec::new()),
        ExprKind::Field { base, name } => {
            let (root, mut path) = target_path(base);
            path.push(name.clone());
            (root, path)
        }
        _ => (String::new(), Vec::new()),
    }
}

fn set_path(value: &Value, path: &[String], new_value: Value) -> Result<(), String> {
    if path.is_empty() {
        return Err("invalid assignment target".to_string());
    }
    let name = &path[0];
    if path.len() == 1 {
        match value {
            Value::Ary(ary) => {
                ary.borrow_mut().set(name, new_value);
                Ok(())
            }
            Value::Vec(items) => {
                let index: usize = name
                    .parse()
                    .map_err(|_| format!("vec has no field '{}'", name))?;
                let mut items = items.borrow_mut();
                if index >= items.len() {
                    return Err(format!("index {} out of bounds", index));
                }
                items[index] = new_value;
                Ok(())
            }
            _ => Err("cannot assign to a field of this value".to_string()),
        }
    } else {
        let child = match value {
            Value::Ary(ary) => ary
                .borrow()
                .get(name)
                .ok_or_else(|| format!("no field named '{}'", name))?,
            Value::Vec(items) => {
                let index: usize = name
                    .parse()
                    .map_err(|_| format!("vec has no field '{}'", name))?;
                items
                    .borrow()
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("index {} out of bounds", index))?
            }
            _ => return Err("cannot assign to a field of this value".to_string()),
        };
        set_path(&child, &path[1..], new_value)
    }
}

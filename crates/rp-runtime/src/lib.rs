//! Rip 运行时：树遍历解释器、标准库与 HTTP 服务。

mod archive;
pub mod async_rt;
mod builtins;
mod cache;
mod client;
mod codec;
mod config;
pub mod coverage;
mod db;
pub mod debug;
mod ext;
pub mod gc;
pub mod heap;
mod i18n;
mod jwt;
mod mail;
mod methods;
pub mod migrate;
mod native_pkg;
mod net;
pub mod profile;
mod queue;
mod redis;
mod reflect;
mod rt;
mod server;
mod server_async;
mod sync;
mod template;
mod tls;
pub mod trace;
mod value;
#[cfg(feature = "interp")]
mod vm;

/// 进程级计数分配器：`rip.toml` 配置堆上限或调用 `sys.heap_usage()` 时启用。
#[global_allocator]
static RIP_ALLOCATOR: heap::CountingAllocator = heap::CountingAllocator;

pub use db::{configure_pool, pool_stats, PoolConfig};
pub use i18n::Language;
pub use rp_ast::{BinOp, UnOp};
pub use rt::{
    ClusterConfig, Deferred, NativeEntry, NativeFn, NativeModule, NativeRoute, NativeTask, Runtime,
    RuntimeSeed, TestOutcome, ThreadOptions,
};
#[cfg(feature = "interp")]
pub use rt::SharedVm;
pub use server::{Metrics, ServerConfig};
pub use value::{
    coerce_int_name, deep_clone, format_value, json_to_value, value_eq, value_to_json, Ary, Env,
    Flow, FlowResult, FuncKind, Function, MapData, Route, RouteTable, RuntimeError, Value,
};

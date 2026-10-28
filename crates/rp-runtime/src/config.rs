//! 运行时外部配置：交付后的产物无需重新构建即可调整服务端参数。
//!
//! 优先级（后者被前者覆盖）：内嵌值 < `config/rip.toml` < 环境变量 < 命令行参数。
//!
//! 配置文件按以下顺序查找，命中第一个即用：
//!
//! 1. `RIP_CONFIG` 指定的路径
//! 2. `<可执行文件目录>/config/rip.toml`：随产物分发
//! 3. `./config/rip.toml`：开发或从任意目录启动时
//!
//! 文件里只认 `[server]`、`[cluster]`、`[db]`、`[runtime]` 四段的运行期参数，
//! 其余段（`[build]` 等）属于构建期，运行时不读。

use std::path::{Path, PathBuf};

use crate::db::PoolConfig;
use crate::rt::{ClusterConfig, Runtime};
use crate::server::ServerConfig;

/// 配置文件的相对路径。
const CONFIG_FILE: &str = "config/rip.toml";

/// 定位运行时配置文件。
pub fn config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RIP_CONFIG") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        eprintln!(
            "warning: RIP_CONFIG points to '{}', which is not a file",
            path.display()
        );
        return None;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(CONFIG_FILE);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let local = PathBuf::from(CONFIG_FILE);
    if local.is_file() {
        return Some(local);
    }
    None
}

impl Runtime {
    /// 用外部配置覆盖内嵌值。应在 `set_server_config` / `set_cluster` 之后、
    /// 命令行参数之前调用。
    pub fn apply_external_config(&mut self) {
        let mut server = self.server_config.clone().unwrap_or_default();
        let mut cluster = self.cluster.clone();
        let mut pool = PoolConfig::default();
        if let Some(path) = config_path() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    for (section, key, value) in parse_pairs(&text) {
                        apply_pair(self, &mut server, &mut cluster, &mut pool, &section, &key, &value);
                    }
                }
                Err(error) => eprintln!("warning: cannot read {}: {}", path.display(), error),
            }
        }
        apply_env(self, &mut server, &mut cluster, &mut pool);
        // 经 `set_server_config` 落地，顺带把堆上限同步到运行时。
        self.set_server_config(server);
        self.cluster = cluster;
        // 连接池是进程级共享的，配置写进全局池即可对被所有工作线程生效。
        crate::db::configure_pool(pool);
    }
}

/// 逐行解析 `[section]` 与 `key = value`；本文件只需要这三段，故不引入 TOML 依赖。
fn parse_pairs(text: &str) -> Vec<(String, String, String)> {
    // Windows 记事本保存的 UTF-8 常带 BOM；TOML 规范也允许，先去掉。
    let text = text.trim_start_matches('\u{feff}');
    let mut pairs = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(|ch| ch == '[' || ch == ']').trim().to_string();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            pairs.push((
                section.clone(),
                key.trim().to_string(),
                value.trim().to_string(),
            ));
        }
    }
    pairs
}

/// 去掉行内注释（引号内的 `#` 保留）。
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn text(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

fn number(value: &str) -> Option<usize> {
    text(value).parse::<usize>().ok()
}

fn flag(value: &str) -> bool {
    text(value).eq_ignore_ascii_case("true")
}

fn apply_pair(
    runtime: &mut Runtime,
    server: &mut ServerConfig,
    cluster: &mut ClusterConfig,
    pool: &mut PoolConfig,
    section: &str,
    key: &str,
    value: &str,
) {
    match (section, key) {
        ("server", "host") => server.host = text(value),
        ("server", "workers") => {
            if let Some(value) = number(value) {
                server.workers = value;
            }
        }
        ("server", "max_connections") => {
            if let Some(value) = number(value) {
                server.max_connections = value.max(1);
            }
        }
        ("server", "max_body_bytes") => {
            if let Some(value) = number(value) {
                server.max_body_bytes = value.max(1024);
            }
        }
        ("server", "max_header_bytes") => {
            if let Some(value) = number(value) {
                server.max_header_bytes = value.max(1024);
            }
        }
        ("server", "max_headers") => {
            if let Some(value) = number(value) {
                server.max_headers = value.max(1);
            }
        }
        ("server", "read_timeout_ms") => {
            if let Some(value) = number(value) {
                server.read_timeout = std::time::Duration::from_millis(value as u64);
            }
        }
        ("server", "write_timeout_ms") => {
            if let Some(value) = number(value) {
                server.write_timeout = std::time::Duration::from_millis(value as u64);
            }
        }
        ("server", "keep_alive_ms") => {
            if let Some(value) = number(value) {
                server.keep_alive = std::time::Duration::from_millis(value as u64);
            }
        }
        ("server", "max_requests") => {
            if let Some(value) = number(value) {
                server.max_requests = value.max(1);
            }
        }
        ("server", "health_path") => server.health_path = text(value),
        ("server", "ready_path") => server.ready_path = text(value),
        ("server", "metrics_path") => server.metrics_path = text(value),
        ("server", "drain_timeout_ms") => {
            if let Some(value) = number(value) {
                server.drain_timeout = std::time::Duration::from_millis(value as u64);
            }
        }
        ("server", "request_timeout_ms") => {
            if let Some(value) = number(value) {
                server.request_timeout = std::time::Duration::from_millis(value as u64);
            }
        }
        ("server", "access_log") => server.access_log = flag(value),
        ("server", "max_heap_mb") => {
            if let Some(value) = number(value) {
                server.max_heap_bytes = value * 1024 * 1024;
            }
        }
        ("server", "trust_proxy") => server.trust_proxy = flag(value),
        ("server", "trusted_proxies") => {
            server.trusted_proxies = text(value)
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect();
        }
        ("server", "rate_limit") => {
            if let Some(value) = number(value) {
                server.rate_limit = value.min(u32::MAX as usize) as u32;
            }
        }
        ("server", "rate_limit_burst") => {
            if let Some(value) = number(value) {
                server.rate_limit_burst = value.min(u32::MAX as usize) as u32;
            }
        }
        ("server", "rate_limit_per_ip") => server.rate_limit_per_ip = flag(value),
        // OTLP/HTTP 追踪导出（Collector 根地址，留空关闭）。
        ("server", "otlp_endpoint") => server.otlp_endpoint = text(value),
        ("server", "otlp_service") => {
            let service = text(value);
            if !service.is_empty() {
                server.otlp_service = service;
            }
        }
        ("server", "otlp_sample_ratio") => {
            if let Ok(ratio) = text(value).parse::<f64>() {
                if ratio.is_finite() {
                    server.otlp_sample_ratio = ratio.clamp(0.0, 1.0);
                }
            }
        }
        ("cluster", "redis") => {
            let url = text(value);
            if url.is_empty() {
                cluster.redis_url = None;
            } else {
                cluster.redis_url = Some(url);
            }
        }
        ("cluster", "task_lock") => cluster.task_lock = flag(value),
        // 数据库连接池。
        ("db", "max_open") => {
            if let Some(value) = number(value) {
                pool.max_open = value.max(1);
            }
        }
        ("db", "max_idle") => {
            if let Some(value) = number(value) {
                pool.max_idle = value;
            }
        }
        ("db", "acquire_timeout_ms") => {
            if let Some(value) = number(value) {
                pool.acquire_timeout = std::time::Duration::from_millis(value as u64);
            }
        }
        ("db", "idle_timeout_sec") => {
            if let Some(value) = number(value) {
                pool.idle_timeout = std::time::Duration::from_secs(value as u64);
            }
        }
        // 运行时提示语言。
        ("runtime", "log") => {
            if let Some(language) = crate::i18n::Language::parse(&text(value)) {
                runtime.set_language(language);
            }
        }
        ("runtime", "log_level") => runtime.set_log_level_text(&text(value)),
        ("runtime", "log_format") => runtime.set_log_format_text(&text(value)),
        ("runtime", "log_file") => runtime.set_log_file_path(&text(value)),
        _ => {}
    }
}

/// 环境变量覆盖：容器/编排环境里无需挂配置文件。
fn apply_env(
    runtime: &mut Runtime,
    server: &mut ServerConfig,
    cluster: &mut ClusterConfig,
    pool: &mut PoolConfig,
) {
    if let Some(value) = std::env::var_os("RIP_SERVER_HOST") {
        server.host = value.to_string_lossy().to_string();
    }
    if let Some(value) = env_number("RIP_SERVER_WORKERS") {
        server.workers = value;
    }
    if let Some(value) = env_number("RIP_SERVER_MAX_CONNECTIONS") {
        server.max_connections = value.max(1);
    }
    if let Some(value) = env_number("RIP_SERVER_MAX_BODY_BYTES") {
        server.max_body_bytes = value.max(1024);
    }
    if let Some(value) = env_number("RIP_SERVER_MAX_HEAP_MB") {
        server.max_heap_bytes = value * 1024 * 1024;
    }
    if let Some(value) = std::env::var_os("RIP_CLUSTER_REDIS") {
        let url = value.to_string_lossy().to_string();
        cluster.redis_url = if url.is_empty() { None } else { Some(url) };
    }
    if let Some(value) = std::env::var_os("RIP_CLUSTER_TASK_LOCK") {
        cluster.task_lock = value.to_string_lossy().eq_ignore_ascii_case("true");
    }
    if let Some(value) = env_number("RIP_DB_MAX_OPEN") {
        pool.max_open = value.max(1);
    }
    if let Some(value) = env_number("RIP_DB_MAX_IDLE") {
        pool.max_idle = value;
    }
    if let Some(value) = env_number("RIP_DB_ACQUIRE_TIMEOUT_MS") {
        pool.acquire_timeout = std::time::Duration::from_millis(value as u64);
    }
    if let Some(value) = env_number("RIP_DB_IDLE_TIMEOUT_SEC") {
        pool.idle_timeout = std::time::Duration::from_secs(value as u64);
    }
    if let Some(value) = std::env::var_os("RIP_LOG_LEVEL") {
        runtime.set_log_level_text(&value.to_string_lossy());
    }
    // OTLP 沿用 OpenTelemetry 的标准环境变量，容器里无需挂配置文件。
    if let Some(value) = std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT") {
        server.otlp_endpoint = value.to_string_lossy().trim().to_string();
    }
    if let Some(value) = std::env::var_os("OTEL_SERVICE_NAME") {
        let name = value.to_string_lossy().trim().to_string();
        if !name.is_empty() {
            server.otlp_service = name;
        }
    }
    if let Ok(value) = std::env::var("OTEL_TRACES_SAMPLER_ARG") {
        if let Ok(ratio) = value.trim().parse::<f64>() {
            if ratio.is_finite() {
                server.otlp_sample_ratio = ratio.clamp(0.0, 1.0);
            }
        }
    }
    if let Some(value) = std::env::var_os("RIP_LOG_FORMAT") {
        runtime.set_log_format_text(&value.to_string_lossy());
    }
    if let Some(value) = std::env::var_os("RIP_LOG_FILE") {
        runtime.set_log_file_path(&value.to_string_lossy());
    }
}

fn env_number(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

impl Runtime {
    /// 日志级别：`trace` / `debug` / `info` / `warn` / `error`。
    pub fn set_log_level_text(&mut self, level: &str) {
        let level = level.trim().to_ascii_lowercase();
        if matches!(
            level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            self.log_level = level;
        }
    }

    /// 日志格式：`text` 或 `json`。
    pub fn set_log_format_text(&mut self, format: &str) {
        let format = format.trim().to_ascii_lowercase();
        if format == "text" || format == "json" {
            self.log_format = format;
        }
    }

    /// 日志落盘文件；空值表示只输出到标准输出。
    pub fn set_log_file_path(&mut self, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            self.log_file = None;
            return;
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Path::new(path))
        {
            Ok(file) => self.log_file = Some(file),
            Err(error) => eprintln!("warning: cannot open log file '{}': {}", path, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_ignores_comments() {
        let text = "\u{feff}[server]\nworkers = 4  # 并发\nhost = \"0.0.0.0\"\n\n[runtime]\nlog_level = \"debug\"\n";
        let pairs = parse_pairs(text);
        assert!(pairs.contains(&("server".to_string(), "workers".to_string(), "4".to_string())));
        assert!(pairs.contains(&(
            "server".to_string(),
            "host".to_string(),
            "\"0.0.0.0\"".to_string()
        )));
        assert!(pairs.contains(&(
            "runtime".to_string(),
            "log_level".to_string(),
            "\"debug\"".to_string()
        )));
    }

    #[test]
    fn keeps_hash_inside_strings() {
        let pairs = parse_pairs("[server]\nhost = \"a#b\" # tail\n");
        assert_eq!(pairs[0].2, "\"a#b\"");
    }
}

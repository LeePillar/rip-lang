//! `rip` 命令行工具。

mod aot;
mod doc;
mod native;
mod registry;
mod semver;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use rp_ast::{Item, Program};
use rp_lexer::{lex, Keyword, TokenKind};
use rp_ownership::check_ownership;
use rp_parser::parse_program;
use rp_runtime::{Language, Runtime};
use rp_types::{check_program, CheckResult, Level};

const ARCHIVE_HEADER: &str = "RIP-ARCHIVE 1";
const ARCHIVE_MARKER: &str = "--- source ---\n";

static DENY_WARNINGS: AtomicBool = AtomicBool::new(false);
/// `--offline`：只用本地 vendored 包，不访问包源；此时必须有 `rip.lock`。
static OFFLINE: AtomicBool = AtomicBool::new(false);

fn deny_warnings() -> bool {
    DENY_WARNINGS.load(Ordering::Relaxed)
}

fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

fn main() {
    // 编译器/运行时 panic 时给出统一格式与位置，便于用户直接定位。
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[panic] {}", info);
    }));
    // CLI 即源码运行/构建模式：允许原生包搜索包含当前工作目录；
    // 打包交付的二进制不走这里，默认不搜索 CWD。
    rp_rust_interop::set_search_cwd(true);
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut deny = false;
    let mut offline = false;
    let mut after_separator = false;
    args.retain(|arg| {
        // `--` 之后是程序参数，原样保留。
        if arg == "--" {
            after_separator = true;
            return true;
        }
        if after_separator {
            return true;
        }
        if arg == "--deny-warnings" {
            deny = true;
            false
        } else if arg == "--offline" {
            offline = true;
            false
        } else {
            true
        }
    });
    DENY_WARNINGS.store(deny, Ordering::Relaxed);
    OFFLINE.store(offline, Ordering::Relaxed);

    // 原生包的 `.rp.decl` 必须早于类型检查注册，才能解析 `import "acme/xxx"`。
    rp_rust_interop::install_native_packages();

    if args.is_empty() {
        print_help();
        return;
    }
    let command = args[0].as_str();
    let rest = &args[1..];
    let code = match command {
        "run" => cmd_run(rest),
        "dev" => cmd_dev(rest),
        "build" => cmd_build(rest),
        "pack" => cmd_pack(rest),
        "lock" => cmd_lock(rest),
        // `vendor`：拉取缺失依赖并生成 rip.lock（packages/ 即 vendored 目录）。
        "vendor" => cmd_vendor(rest),
        "repl" => cmd_repl(rest),
        "test" => cmd_test(rest),
        "bench" => cmd_bench(rest),
        "profile" => cmd_profile(rest),
        "doc" => cmd_doc(rest),
        "new" => cmd_new(rest),
        "add" => cmd_add(rest),
        "pub" => cmd_pub(rest),
        "search" => cmd_search(rest),
        "info" => cmd_info(rest),
        "audit" => cmd_audit(rest),
        "check" => cmd_check(rest),
        "doctor" => cmd_doctor(rest),
        "migrate" => cmd_migrate(rest),
        "fmt" => cmd_fmt(rest),
        "lsp" => rp_lsp::run_stdio(),
        "dap" => cmd_dap(rest),
        "version" | "-V" | "--version" => {
            println!(
                "rip {} ({} {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            0
        }
        "help" | "-h" | "--help" => {
            print_help();
            0
        }
        other => {
            eprintln!("error: unknown command '{}'", other);
            eprintln!("run 'rip help' for usage");
            2
        }
    };
    if code != 0 {
        process::exit(code);
    }
}

fn print_help() {
    println!("Rip 语言工具链 (rip {})", env!("CARGO_PKG_VERSION"));
    println!();
    println!("用法:");
    println!("  rip run [<file.rp>] [--host H] [--port N] [--cert P --key P] [-- args...]  运行源码");
    println!("  rip dev <file.rp> [--host H] [--port N] [--cert P --key P]  开发服务：启动并热重载");
    println!("  rip build <file.rp> [-o <dir>] [--name N] [--no-archive] [--native] [--target TRIPLE]   编译为原生二进制并生成 dist.zip；--native 仅标量 int 子集（实验）");
    println!("  rip pack <包目录|包文件>                    编译第三方包为 .rpa");
    println!("  rip lock [--check]                         生成/校验 rip.lock（依赖锁定）");
    println!("  rip vendor [--check]                       等同 rip lock（packages/ 即 vendored 目录）");
    println!("  rip repl                                   交互式解释器");
    println!("  rip test [dir|file] [--filter 名称] [--jobs N] [--timeout MS] [--coverage]  运行 #[test] 函数");
    println!("  rip bench [dir|file] [--filter 名称] [--iterations N] [--warmup N]  运行 #[bench] 函数");
    println!("  rip profile [<file.rp>] [--top N] [--json] [-- args...]  函数级耗时采样报告");
    println!("  rip doc [<file.rp>] [-o <dir>] [--json]    生成 HTML API 文档");
    println!("  rip new <name> [--template http|cli|empty]  创建项目骨架");
    println!("  rip add <package>[@版本约束]               添加包依赖");
    println!("  rip pub [<包目录>] [--registry URL]        发布包到包源");
    println!("  rip search <关键词> [--registry URL]       搜索包源（--json）");
    println!("  rip info <包名> [--registry URL]           查看包的全部版本与依赖");
    println!("  rip audit [--deny LEVEL] [--json] [--licenses]  依赖锁定 / 安全公告 / 许可证审计");
    println!("  rip check <file.rp> [--json]               静态检查");
    println!("  rip doctor [<入口>]                        环境与项目自检");
    println!("  rip migrate <up|down|status> [--dsn DSN]   数据库迁移（版本表 rip_schema_migrations）");
    println!("  rip fmt [file|dir] [--check] [--stdout]    格式化（--check 只检查不写回）");
    println!("  rip lsp                                    启动 LSP（stdio）");
    println!("  rip dap [--port N]                         启动 DAP 调试适配器（stdio 或 TCP）");
    println!("  rip version                                版本信息");
    println!();
    println!("公共选项:");
    println!("  --offline         只使用本地包里已有的依赖（要求 rip.lock 存在）");
    println!("  --deny-warnings   把警告当作错误");
}

struct RunOptions {
    file: Option<String>,
    port: Option<u16>,
    host: Option<String>,
    cert: Option<String>,
    key: Option<String>,
    program_args: Vec<String>,
}

fn parse_run_options(args: &[String]) -> Result<RunOptions, String> {
    let mut options = RunOptions {
        file: None,
        port: None,
        host: None,
        cert: None,
        key: None,
        program_args: Vec::new(),
    };
    let mut after_separator = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if after_separator {
            options.program_args.push(arg.clone());
        } else if arg == "--" {
            after_separator = true;
        } else if arg == "--port" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--port requires a value".to_string())?;
            options.port = Some(
                value
                    .parse()
                    .map_err(|_| format!("invalid port '{}'", value))?,
            );
        } else if let Some(value) = arg.strip_prefix("--port=") {
            options.port = Some(
                value
                    .parse()
                    .map_err(|_| format!("invalid port '{}'", value))?,
            );
        } else if arg == "--host" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--host requires a value".to_string())?;
            options.host = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--host=") {
            options.host = Some(value.to_string());
        } else if arg == "--cert" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--cert requires a PEM file".to_string())?;
            options.cert = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--cert=") {
            options.cert = Some(value.to_string());
        } else if arg == "--key" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--key requires a PEM file".to_string())?;
            options.key = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--key=") {
            options.key = Some(value.to_string());
        } else if options.file.is_none() {
            options.file = Some(arg.clone());
        } else {
            options.program_args.push(arg.clone());
        }
        i += 1;
    }
    Ok(options)
}

fn cmd_run(args: &[String]) -> i32 {
    let options = match parse_run_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {}", error);
            return 2;
        }
    };
    let path = match options.file {
        Some(path) => PathBuf::from(path),
        None => match find_entry(None) {
            Some(path) => path,
            None => {
                eprintln!("error: no entry file found; pass a .rp or .rpp file");
                return 2;
            }
        },
    };
    run_path(
        &path,
        options.program_args,
        options.port,
        options.host,
        options.cert,
        options.key,
    )
}

fn run_path(
    path: &Path,
    program_args: Vec<String>,
    port: Option<u16>,
    host: Option<String>,
    cert: Option<String>,
    key: Option<String>,
) -> i32 {
    let prepared = match prepare(path) {
        Some(prepared) => prepared,
        None => return 1,
    };
    let language = runtime_language(path);
    let config = server_config(path);
    let cluster = cluster_config(path);
    let mut runtime = Runtime::new(&prepared.program, prepared.checked, program_args);
    runtime.set_http_port(port);
    runtime.set_http_host(host);
    runtime.set_http_tls(cert, key);
    runtime.set_server_config(config);
    runtime.set_cluster(cluster);
    runtime.set_language(language);
    // 交付后的产物可在 `config/rip.toml` 或环境变量里覆盖服务端参数。
    runtime.apply_external_config();
    let language = runtime.language();
    match runtime.run_main() {
        Ok(_) => 0,
        Err(error) => {
            let (file, line) = locate(&prepared.units, error.line, &path.display().to_string());
            if language.is_chinese() {
                eprintln!("{}:{}: 运行时错误：{}", file, line, error.message);
            } else {
                eprintln!("{}:{}: runtime error: {}", file, line, error.message);
            }
            for frame in error.stack_lines() {
                eprintln!("{}", frame);
            }
            1
        }
    }
}

/// `rip profile`：运行入口程序并输出函数级耗时报告。
///
/// 采集的是解释器里真实的函数调用（含闭包与内建模块调用），
/// 不改变程序行为；HTTP 服务这类常驻程序在收到中断前不会输出报告。
fn cmd_profile(args: &[String]) -> i32 {
    let mut input = None;
    let mut top = 30usize;
    let mut json = false;
    let mut program_args = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top" => {
                i += 1;
                top = args
                    .get(i)
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(top);
            }
            "--json" => json = true,
            "--" => {
                program_args = args[i + 1..].to_vec();
                break;
            }
            arg if input.is_none() => input = Some(arg.to_string()),
            _ => {}
        }
        i += 1;
    }
    let path = match input {
        Some(path) => PathBuf::from(path),
        None => match find_entry(None) {
            Some(path) => path,
            None => {
                eprintln!("error: no entry file found; pass a .rp or .rpp file");
                return 2;
            }
        },
    };
    let prepared = match prepare(&path) {
        Some(prepared) => prepared,
        None => return 1,
    };
    let language = runtime_language(&path);
    let config = server_config(&path);
    let cluster = cluster_config(&path);
    let mut runtime = Runtime::new(&prepared.program, prepared.checked, program_args);
    runtime.set_server_config(config);
    runtime.set_cluster(cluster);
    runtime.set_language(language);
    runtime.apply_external_config();
    let language = runtime.language();

    rp_runtime::profile::start();
    let started = std::time::Instant::now();
    let outcome = runtime.run_main();
    let elapsed = started.elapsed();
    let stats = rp_runtime::profile::stop();

    if json {
        let functions: Vec<serde_json::Value> = stats
            .iter()
            .map(|(name, stat)| {
                serde_json::json!({
                    "name": name,
                    "calls": stat.calls,
                    "total_ms": stat.total_ns as f64 / 1_000_000.0,
                    "self_ms": stat.self_ns() as f64 / 1_000_000.0,
                    "avg_us": if stat.calls > 0 {
                        stat.total_ns as f64 / stat.calls as f64 / 1_000.0
                    } else {
                        0.0
                    }
                })
            })
            .collect();
        let report = serde_json::json!({
            "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
            "functions": functions
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print_profile_table(&stats, elapsed, top);
    }

    match outcome {
        Ok(_) => 0,
        Err(error) => {
            let (file, line) = locate(&prepared.units, error.line, &path.display().to_string());
            if language.is_chinese() {
                eprintln!("{}:{}: 运行时错误：{}", file, line, error.message);
            } else {
                eprintln!("{}:{}: runtime error: {}", file, line, error.message);
            }
            for frame in error.stack_lines() {
                eprintln!("{}", frame);
            }
            1
        }
    }
}

fn print_profile_table(
    stats: &[(String, rp_runtime::profile::Stat)],
    elapsed: std::time::Duration,
    top: usize,
) {
    let total_wall = elapsed.as_secs_f64() * 1000.0;
    println!(
        "profile: {} function(s), wall {:.2} ms",
        stats.len(),
        total_wall
    );
    if stats.is_empty() {
        println!("  (no Rip function calls recorded)");
        return;
    }
    let shown = if top == 0 { stats.len() } else { stats.len().min(top) };
    println!(
        "{:<48} {:>8} {:>11} {:>11} {:>11} {:>7}",
        "function", "calls", "total ms", "self ms", "avg us", "%"
    );
    for (name, stat) in stats.iter().take(shown) {
        let total_ms = stat.total_ns as f64 / 1_000_000.0;
        let self_ms = stat.self_ns() as f64 / 1_000_000.0;
        let avg_us = if stat.calls > 0 {
            stat.total_ns as f64 / stat.calls as f64 / 1_000.0
        } else {
            0.0
        };
        let percent = if total_wall > 0.0 {
            total_ms / total_wall * 100.0
        } else {
            0.0
        };
        println!(
            "{:<48} {:>8} {:>11.2} {:>11.2} {:>11.1} {:>6.1}%",
            truncate_name(name, 48),
            stat.calls,
            total_ms,
            self_ms,
            avg_us,
            percent
        );
    }
    if shown < stats.len() {
        println!("... {} more (use --top 0 to show all)", stats.len() - shown);
    }
}

fn truncate_name(name: &str, limit: usize) -> String {
    if name.chars().count() <= limit {
        return name.to_string();
    }
    let mut result: String = name.chars().take(limit.saturating_sub(1)).collect();
    result.push('…');
    result
}

/// `rip dap`：启动调试适配器；程序在独立线程里解释执行。
///
/// 缺省走 stdio（编辑器直接拉起 `rip dap`）；`--port N` 时监听一次 TCP
/// 连接（VSCode 的 `debugServer` 模式），适合调试会大量输出的服务。
fn cmd_dap(args: &[String]) -> i32 {
    let mut port = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            i += 1;
            port = args.get(i).and_then(|value| value.parse::<u16>().ok());
        }
        i += 1;
    }
    let launcher: rp_dap::Launcher = Arc::new(|debugger, program, program_args| {
        let path = PathBuf::from(&program);
        let prepared = match prepare(&path) {
            Some(prepared) => prepared,
            None => return Err(String::new()),
        };
        let language = runtime_language(&path);
        let config = server_config(&path);
        let cluster = cluster_config(&path);
        let mut runtime = Runtime::new(&prepared.program, prepared.checked, program_args);
        runtime.set_server_config(config);
        runtime.set_cluster(cluster);
        runtime.set_language(language);
        runtime.apply_external_config();
        let language = runtime.language();
        runtime.set_debugger(debugger);
        match runtime.run_main() {
            Ok(_) => Ok(()),
            Err(error) => {
                let (file, line) =
                    locate(&prepared.units, error.line, &path.display().to_string());
                let mut message = if language.is_chinese() {
                    format!("{}:{}: 运行时错误：{}\n", file, line, error.message)
                } else {
                    format!("{}:{}: runtime error: {}\n", file, line, error.message)
                };
                for frame in error.stack_lines() {
                    message.push_str(&frame);
                    message.push('\n');
                }
                Err(message)
            }
        }
    });
    match port {
        Some(port) => rp_dap::run_tcp(port, launcher),
        None => rp_dap::run_stdio(launcher),
    }
}

/// `rip doc`：生成单文件 HTML API 文档（`--json` 输出结构化模型）。
fn cmd_doc(args: &[String]) -> i32 {
    let mut input = None;
    let mut output = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                output = args.get(i).cloned();
            }
            "--json" => json = true,
            arg if input.is_none() && !arg.starts_with('-') => input = Some(arg.to_string()),
            _ => {}
        }
        i += 1;
    }
    let entry = match input {
        Some(path) => match resolve_entry(&PathBuf::from(path)) {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("error: {}", error);
                return 1;
            }
        },
        None => match find_entry(None) {
            Some(entry) => entry,
            None => {
                eprintln!("error: no entry file found; pass a .rp file");
                return 2;
            }
        },
    };
    let prepared = match prepare(&entry) {
        Some(prepared) => prepared,
        None => return 1,
    };
    let root = project_root(&entry).unwrap_or_else(|| {
        entry
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let manifest = read_manifest(&root);
    let name = manifest
        .name
        .clone()
        .or_else(|| {
            entry
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "app".to_string());
    let version = manifest
        .version
        .clone()
        .unwrap_or_else(|| "0.0.0".to_string());
    let model = doc::build(
        &prepared.program,
        &prepared.units,
        &entry.display().to_string(),
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&doc::to_json(&name, &version, &model))
                .unwrap_or_else(|_| "{}".to_string())
        );
        return 0;
    }
    let out_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("doc"));
    match doc::write_html(&out_dir, &name, &version, &model) {
        Ok(path) => {
            println!("wrote {} ({} functions, {} types, {} enums)", path.display(), model.functions.len(), model.types.len(), model.enums.len());
            0
        }
        Err(error) => {
            eprintln!("error: {}", error);
            1
        }
    }
}

/// 从入口文件所在项目的 rip.toml 读取 `[runtime] log`，缺省为英文。
fn runtime_language(path: &Path) -> Language {
    resolve_entry(path)
        .ok()
        .and_then(|entry| project_root(&entry))
        .map(|root| read_manifest(&root).language)
        .unwrap_or(Language::English)
}

/// 从入口文件所在项目的 rip.toml 读取 `[server]`，缺省为内置值。
fn server_config(path: &Path) -> rp_runtime::ServerConfig {
    resolve_entry(path)
        .ok()
        .and_then(|entry| project_root(&entry))
        .map(|root| read_manifest(&root).server)
        .unwrap_or_default()
}

/// 从入口文件所在项目的 rip.toml 读取 `[cluster]`，缺省不启用集群协作。
fn cluster_config(path: &Path) -> rp_runtime::ClusterConfig {
    resolve_entry(path)
        .ok()
        .and_then(|entry| project_root(&entry))
        .map(|root| read_manifest(&root).cluster)
        .unwrap_or_default()
}

fn cmd_dev(args: &[String]) -> i32 {
    let options = match parse_run_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {}", error);
            return 2;
        }
    };
    let path = match options.file {
        Some(file) => PathBuf::from(file),
        None => {
            eprintln!("error: rip dev requires a .rp file");
            return 2;
        }
    };

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("error: cannot locate rip executable: {}", error);
            return 1;
        }
    };
    let language = runtime_language(&path);

    loop {
        let mut child = Command::new(&exe);
        child.arg("run").arg(&path);
        if let Some(port) = options.port {
            child.arg("--port").arg(port.to_string());
        }
        if let Some(host) = &options.host {
            child.arg("--host").arg(host);
        }
        if let Some(cert) = &options.cert {
            child.arg("--cert").arg(cert);
        }
        if let Some(key) = &options.key {
            child.arg("--key").arg(key);
        }
        if !options.program_args.is_empty() {
            child.arg("--");
            for arg in &options.program_args {
                child.arg(arg);
            }
        }

        let mut child = match child.spawn() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("error: cannot start development server: {}", error);
                return 1;
            }
        };

        let scheme = if options.cert.is_some() && options.key.is_some() {
            "https"
        } else {
            "http"
        };
        match options.port {
            Some(port) => {
                if language.is_chinese() {
                    println!(
                        "[dev] 正在运行 {}，地址 {}://{}:{}（监听更改）",
                        path.display(),
                        scheme,
                        options.host.as_deref().unwrap_or("127.0.0.1"),
                        port
                    );
                } else {
                    println!(
                        "[dev] serving {} on {}://{}:{} (watching for changes)",
                        path.display(),
                        scheme,
                        options.host.as_deref().unwrap_or("127.0.0.1"),
                        port
                    );
                }
            }
            None => {
                if language.is_chinese() {
                    println!("[dev] 正在运行 {}（监听更改）", path.display());
                } else {
                    println!("[dev] running {} (watching for changes)", path.display());
                }
            }
        }

        let mut last = watch_stamp(&path);
        let mut reported_exit = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(350));

            let exited = matches!(child.try_wait(), Ok(Some(_)));
            if exited && !reported_exit {
                if language.is_chinese() {
                    println!("[dev] 程序已退出，等待下一次更改");
                } else {
                    println!("[dev] program exited; waiting for the next change");
                }
                reported_exit = true;
            }

            let current = watch_stamp(&path);
            if current != last {
                last = current;
                if !exited {
                    let _ = child.kill();
                }
                let _ = child.wait();
                if language.is_chinese() {
                    println!("[dev] 检测到更改，正在重启");
                } else {
                    println!("[dev] change detected, restarting");
                }
                break;
            }
        }
    }
}

fn modified(path: &Path) -> Option<u128> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
}

/// 项目模式下把所有参与编译的文件时间戳合并成一个签名，任一文件改动都会触发重启。
fn watch_stamp(path: &Path) -> u128 {
    let entry = resolve_entry(path).unwrap_or_else(|_| path.to_path_buf());
    let mut files = Vec::new();
    match project_root(&entry) {
        Some(root) => {
            let excludes = read_manifest(&root).exclude;
            collect_project_files(&root, &root, &excludes, &mut files);
            files.push(root.join("rip.toml"));
            files.push(entry);
        }
        None => files.push(entry),
    }
    files.sort();
    files.dedup();
    let mut stamp = 0u128;
    for (index, file) in files.iter().enumerate() {
        let value = modified(file).unwrap_or(0);
        stamp = stamp.wrapping_add(value.wrapping_mul(index as u128 + 1));
    }
    stamp
}

struct ManifestInfo {
    name: Option<String>,
    version: Option<String>,
    /// `[module] license`：依赖审计时展示的许可证标识。
    license: Option<String>,
    assets: Vec<String>,
    dir: Option<String>,
    binary: Option<String>,
    language: Language,
    /// `[compile] exclude`：不参与编译的目录（相对项目根，或用目录名）。
    exclude: Vec<String>,
    /// `[build] copy`：打包时随产物复制的运行期数据目录。
    copy: Vec<String>,
    /// `[build] layout`：`flat`（缺省，全部平铺）或 `typed`（bin/lib/data/share）。
    layout: Option<String>,
    /// `[build] target`：目标平台，缺省为当前平台。
    target: Option<String>,
    /// `[server]`：HTTP 服务端并发、超时、限流与内置端点。
    server: rp_runtime::ServerConfig,
    /// `[cluster]`：Redis 地址与定时任务选主开关。
    cluster: rp_runtime::ClusterConfig,
    /// `[registry] source`：包源地址；缺省使用公共包源，可用环境变量 `RIP_REGISTRY` 覆盖。
    registry: Option<String>,
    /// `[registry] offline = true`：等同始终使用 `--offline`。
    registry_offline: bool,
    /// `[registry] token_env`：存放访问令牌的环境变量名；缺省读 `RIP_REGISTRY_TOKEN`。
    registry_token_env: Option<String>,
    /// `[registry] advisories`：安全公告地址；缺省使用 `<registry>/v1/advisories.json`。
    registry_advisories: Option<String>,
    /// `[dependencies]`：包路径 → 版本约束，按声明顺序保存。
    dependencies: Vec<(String, String)>,
    /// `[database] dsn`：`rip migrate` 使用的数据库连接串。
    database: Option<String>,
}

fn read_manifest(root: &Path) -> ManifestInfo {
    let mut info = ManifestInfo {
        name: None,
        version: None,
        license: None,
        assets: Vec::new(),
        dir: None,
        binary: None,
        language: Language::English,
        exclude: Vec::new(),
        copy: Vec::new(),
        layout: None,
        target: None,
        server: rp_runtime::ServerConfig::default(),
        cluster: rp_runtime::ClusterConfig::default(),
        registry: None,
        registry_offline: false,
        registry_token_env: None,
        registry_advisories: None,
        dependencies: Vec::new(),
        database: None,
    };
    let text = match fs::read_to_string(root.join("rip.toml")) {
        Ok(text) => text,
        Err(_) => return info,
    };
    // Windows 编辑器常写入 UTF-8 BOM；带 BOM 时首个 `[section]` 会识别失败。
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        let key = key.trim();
        let value = strip_toml_comment(value).trim();
        match (section.as_str(), key) {
            ("module", "name") => info.name = Some(value.trim_matches('"').to_string()),
            ("module", "version") => info.version = Some(value.trim_matches('"').to_string()),
            ("module", "license") => info.license = Some(value.trim_matches('"').to_string()),
            ("build", "assets") => info.assets = parse_string_array(value),
            ("build", "dir") => info.dir = Some(value.trim_matches('"').to_string()),
            ("build", "name") => info.binary = Some(value.trim_matches('"').to_string()),
            ("build", "layout") => info.layout = Some(value.trim_matches('"').to_string()),
            ("build", "target") => info.target = Some(value.trim_matches('"').to_string()),
            // [build] copy：打包时复制的运行期数据目录，缺省不复制任何目录。
            ("build", "copy") => info.copy = parse_string_array(value),
            // [compile] exclude：编译时跳过的目录，叠加在内置规则之上。
            ("compile", "exclude") => info.exclude = parse_string_array(value),
            // [runtime] log = "english" | "chinese"：运行时提示语言，非法值按默认英文处理。
            ("runtime", "log") => {
                if let Some(language) = Language::parse(value.trim_matches('"')) {
                    info.language = language;
                }
            }
            // [server]：HTTP 并发与稳定性参数。
            ("server", "workers") => {
                if let Some(value) = parse_usize(value) {
                    info.server.workers = value;
                }
            }
            ("server", "max_connections") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_connections = value.max(1);
                }
            }
            ("server", "max_body_bytes") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_body_bytes = value.max(1024);
                }
            }
            ("server", "max_header_bytes") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_header_bytes = value.max(1024);
                }
            }
            ("server", "max_headers") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_headers = value.max(1);
                }
            }
            ("server", "read_timeout_ms") => {
                if let Some(value) = parse_usize(value) {
                    info.server.read_timeout = std::time::Duration::from_millis(value as u64);
                }
            }
            ("server", "write_timeout_ms") => {
                if let Some(value) = parse_usize(value) {
                    info.server.write_timeout = std::time::Duration::from_millis(value as u64);
                }
            }
            ("server", "keep_alive_ms") => {
                if let Some(value) = parse_usize(value) {
                    info.server.keep_alive = std::time::Duration::from_millis(value as u64);
                }
            }
            ("server", "max_requests") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_requests = value.max(1);
                }
            }
            ("server", "health_path") => {
                info.server.health_path = value.trim_matches('"').to_string();
            }
            ("server", "ready_path") => {
                info.server.ready_path = value.trim_matches('"').to_string();
            }
            ("server", "metrics_path") => {
                info.server.metrics_path = value.trim_matches('"').to_string();
            }
            ("server", "drain_timeout_ms") => {
                if let Some(value) = parse_usize(value) {
                    info.server.drain_timeout = std::time::Duration::from_millis(value as u64);
                }
            }
            ("server", "request_timeout_ms") => {
                if let Some(value) = parse_usize(value) {
                    // 0 表示不限制处理函数执行时间。
                    info.server.request_timeout = std::time::Duration::from_millis(value as u64);
                }
            }
            ("server", "access_log") => {
                let flag = value.trim().trim_matches('"');
                info.server.access_log = !matches!(flag, "false" | "off" | "0");
            }
            ("server", "max_heap_mb") => {
                if let Some(value) = parse_usize(value) {
                    info.server.max_heap_bytes = value * 1024 * 1024;
                }
            }
            ("server", "trust_proxy") => {
                let flag = value.trim().trim_matches('"');
                info.server.trust_proxy = !matches!(flag, "false" | "off" | "0");
            }
            ("server", "trusted_proxies") => {
                info.server.trusted_proxies = value
                    .trim()
                    .trim_matches('"')
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect();
            }
            ("server", "rate_limit") => {
                if let Some(value) = parse_usize(value) {
                    info.server.rate_limit = value.min(u32::MAX as usize) as u32;
                }
            }
            ("server", "rate_limit_burst") => {
                if let Some(value) = parse_usize(value) {
                    info.server.rate_limit_burst = value.min(u32::MAX as usize) as u32;
                }
            }
            ("server", "rate_limit_per_ip") => {
                let flag = value.trim().trim_matches('"');
                info.server.rate_limit_per_ip = !matches!(flag, "false" | "off" | "0");
            }
            // OTLP/HTTP 追踪导出。
            ("server", "otlp_endpoint") => {
                info.server.otlp_endpoint = value.trim().trim_matches('"').to_string();
            }
            ("server", "otlp_service") => {
                let service = value.trim().trim_matches('"');
                if !service.is_empty() {
                    info.server.otlp_service = service.to_string();
                }
            }
            ("server", "otlp_sample_ratio") => {
                if let Ok(ratio) = value.trim().trim_matches('"').parse::<f64>() {
                    if ratio.is_finite() {
                        info.server.otlp_sample_ratio = ratio.clamp(0.0, 1.0);
                    }
                }
            }
            // [cluster]：多实例协作。
            ("cluster", "redis") => {
                let value = value.trim_matches('"').to_string();
                if !value.is_empty() {
                    info.cluster.redis_url = Some(value);
                }
            }
            ("cluster", "task_lock") => {
                info.cluster.task_lock = value.trim() == "true";
            }
            // [dependencies]：包路径 → 版本约束；`rip add` 写入，构建时校验。
            ("dependencies", _) => {
                let name = key.trim_matches('"').trim();
                if !name.is_empty() {
                    info.dependencies
                        .push((name.to_string(), value.trim_matches('"').trim().to_string()));
                }
            }
            // [registry]：私有包源与离线开关。
            ("registry", "source") => {
                let value = value.trim_matches('"').to_string();
                if !value.is_empty() {
                    info.registry = Some(value);
                }
            }
            ("registry", "offline") => {
                info.registry_offline = value.trim() == "true";
            }
            ("registry", "token_env") => {
                let value = value.trim_matches('"').trim().to_string();
                if !value.is_empty() {
                    info.registry_token_env = Some(value);
                }
            }
            ("registry", "advisories") => {
                let value = value.trim_matches('"').trim().to_string();
                if !value.is_empty() {
                    info.registry_advisories = Some(value);
                }
            }
            // [database] dsn：rip migrate 的连接串；运行时应用仍可用自己的配置。
            ("database", "dsn") => {
                let value = value.trim_matches('"').to_string();
                if !value.is_empty() {
                    info.database = Some(value);
                }
            }
            _ => {}
        }
    }
    info
}

/// 去掉 TOML 行内注释（引号内的 `#` 保留）。
fn strip_toml_comment(value: &str) -> &str {
    let mut in_string = false;
    for (index, ch) in value.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &value[..index],
            _ => {}
        }
    }
    value
}

fn parse_string_array(value: &str) -> Vec<String> {
    let inner = value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn parse_usize(value: &str) -> Option<usize> {
    let value = value.trim().trim_matches('"').replace('_', "");
    value.parse::<usize>().ok()
}

/// 递归复制文件或目录；目录合并写入，已存在的运行期数据（如 storage）不受影响。
/// 符号链接一律跳过：防止恶意仓库用 `data -> /` 之类的链接把项目外文件
/// 带进交付物，或因链接成环导致无限递归。
fn copy_path(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        let mut entries: Vec<PathBuf> = fs::read_dir(from)?
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            let name = match entry.file_name() {
                Some(name) => name.to_os_string(),
                None => continue,
            };
            copy_path(&entry, &to.join(name))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to).map(|_| ())
    }
}

/// 复制运行期数据：目录递归合并，跳过 `.log` / `.err` / `.tmp`；文件按原路径复制。
fn copy_data_path(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        let mut entries: Vec<PathBuf> = fs::read_dir(from)?
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            let name = match entry.file_name() {
                Some(name) => name.to_os_string(),
                None => continue,
            };
            let label = name.to_string_lossy();
            if label.ends_with(".log") || label.ends_with(".err") || label.ends_with(".tmp") {
                continue;
            }
            // `tests/` 是测试产物目录（如测试专用数据库），不进入交付包。
            if entry.is_dir() && label == "tests" {
                continue;
            }
            copy_data_path(&entry, &to.join(name))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to).map(|_| ())
    }
}

/// 交付目录布局：`flat` 平铺，`typed` 按类型分目录（bin / lib / data / share）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum BuildLayout {
    Flat,
    Typed,
}

impl BuildLayout {
    fn parse(value: Option<&str>) -> BuildLayout {
        match value.map(|value| value.trim().to_ascii_lowercase()) {
            Some(layout) if layout == "typed" => BuildLayout::Typed,
            _ => BuildLayout::Flat,
        }
    }
}

/// 构建目标：`triple = None` 表示本机（直接调用 rustc 的快速路径）。
#[derive(Clone, Debug)]
struct BuildTarget {
    triple: Option<String>,
    windows: bool,
}

impl BuildTarget {
    fn is_cross(&self) -> bool {
        self.triple.is_some()
    }
}

/// 读取编译器自报的 host triple；失败时按常见组合推断。
fn host_triple() -> String {
    if let Some(compiler) = aot_compiler() {
        if let Ok(output) = Command::new(compiler).arg("-vV").output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(host) = text
                    .lines()
                    .find_map(|line| line.strip_prefix("host: ").map(|value| value.trim()))
                {
                    return host.to_string();
                }
            }
        }
    }
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{}-pc-windows-msvc", arch),
        "macos" => format!("{}-apple-darwin", arch),
        "linux" => format!("{}-unknown-linux-gnu", arch),
        other => format!("{}-unknown-{}", arch, other),
    }
}

/// 解析目标：支持 `windows` / `linux` / `macos` 别名与完整 Rust target triple。
/// 与 host 相同的 triple 走本机快速路径。
fn resolve_build_target(requested: Option<&str>, host: &str) -> Result<BuildTarget, String> {
    let raw = match requested.map(str::trim) {
        Some(value) if !value.is_empty() => value.to_ascii_lowercase(),
        _ => {
            return Ok(BuildTarget {
                triple: None,
                windows: cfg!(windows),
            })
        }
    };
    let triple = match raw.as_str() {
        "windows" | "win" => {
            if host.contains("windows") {
                host.to_string()
            } else {
                "x86_64-pc-windows-msvc".to_string()
            }
        }
        "linux" => {
            if host.contains("linux") {
                host.to_string()
            } else {
                "x86_64-unknown-linux-gnu".to_string()
            }
        }
        "macos" | "darwin" => {
            if host.contains("apple-darwin") {
                host.to_string()
            } else {
                "x86_64-apple-darwin".to_string()
            }
        }
        "freebsd" => {
            if host.contains("freebsd") {
                host.to_string()
            } else {
                "x86_64-unknown-freebsd".to_string()
            }
        }
        other => {
            if other.split('-').count() < 3 {
                return Err(format!(
                    "unsupported build target '{}'; pass a Rust target triple such as x86_64-unknown-linux-gnu",
                    requested.unwrap_or(other)
                ));
            }
            other.to_string()
        }
    };
    let windows = triple.contains("windows");
    if triple == host {
        return Ok(BuildTarget {
            triple: None,
            windows,
        });
    }
    Ok(BuildTarget {
        triple: Some(triple),
        windows,
    })
}

fn dist_readme(
    app: &str,
    version: &str,
    entry: &str,
    exe: &str,
    assets: &[String],
    has_migrations: bool,
    copied: &[String],
    layout: BuildLayout,
    windows: bool,
    native: &[String],
    statically_linked: &[String],
) -> String {
    let typed = layout == BuildLayout::Typed;
    let bin_dir = if typed { "bin/" } else { "" };
    let data_dir = if typed { "data/" } else { "" };
    let share_dir = if typed { "share/" } else { "" };
    let exe_file = if windows {
        format!("{}.exe", exe)
    } else {
        exe.to_string()
    };
    let mut text = String::new();
    text.push_str(&format!("# {}\n\n", app));
    text.push_str(&format!("- 应用名称：{}\n", app));
    text.push_str(&format!("- 版本号：{}\n", version));
    text.push_str(&format!(
        "- 平台：{}\n",
        if windows { "Windows" } else { "Linux / Unix / macOS" }
    ));
    text.push_str(&format!("- 入口：{}\n\n", entry));
    text.push_str("## 启动\n\n在本目录内执行：\n\n```\n");
    if windows {
        text.push_str(&format!("{}{} --port 8080\n", bin_dir, exe_file));
    } else {
        text.push_str(&format!("./{}{} --port 8080\n", bin_dir, exe_file));
    }
    text.push_str("```\n\n");
    text.push_str("默认只监听 127.0.0.1，仅本机可访问。需要对外提供服务时指定绑定地址（IP 或本机可解析的域名）：\n\n```\n");
    if windows {
        text.push_str(&format!(
            "{}{} --host 0.0.0.0 --port 8080\n",
            bin_dir, exe_file
        ));
    } else {
        text.push_str(&format!(
            "./{}{} --host 0.0.0.0 --port 8080\n",
            bin_dir, exe_file
        ));
    }
    text.push_str("```\n\n");
    text.push_str("## 目录内容\n\n");
    text.push_str(&format!(
        "- `{}{}`：可执行程序，已编译为原生二进制，不含源码与解释器，无需安装 rip\n",
        bin_dir, exe_file
    ));
    text.push_str("- `README.md`：本文件\n");
    if typed {
        text.push_str("- `lib/`：第三方包（源码或预编译 `.rpa`），已同时内嵌进可执行文件\n");
    }
    for package in native {
        text.push_str(&format!(
            "- `{}lib/native/{}/`：原生包（Rust 实现），运行时自动加载，需与可执行文件一同交付\n",
            bin_dir, package
        ));
    }
    for package in statically_linked {
        text.push_str(&format!(
            "- 原生包 `{}`：已静态链接进可执行文件，无需单独交付\n",
            package
        ));
    }
    for entry in copied {
        text.push_str(&format!(
            "- `{}{}`：随包交付的运行期数据，启动后继续读写，数据不会丢\n",
            data_dir, entry
        ));
    }
    for asset in assets {
        text.push_str(&format!("- `{}{}/`：随程序发布的资源\n", share_dir, asset));
    }
    if has_migrations {
        text.push_str(&format!("- `{}migrations/`：数据库迁移脚本\n", data_dir));
    }
    text
}

fn cmd_build(args: &[String]) -> i32 {
    let mut input = None;
    let mut output = None;
    let mut output_name = None;
    let mut native = false;
    let mut no_archive = false;
    let mut cli_target = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output = args.get(i).cloned();
            }
            "--name" => {
                i += 1;
                output_name = args.get(i).cloned();
            }
            "--target" => {
                i += 1;
                cli_target = args.get(i).cloned();
            }
            "--native" => native = true,
            // `--aot` 现在是缺省行为，保留参数只为兼容已有脚本。
            "--aot" => {}
            "--no-archive" => no_archive = true,
            "--release" | "--debug" | "--deny-warnings" => {}
            arg if input.is_none() => input = Some(arg.to_string()),
            _ => {}
        }
        i += 1;
    }
    let input = match input {
        Some(input) => PathBuf::from(input),
        None => {
            eprintln!("error: rip build requires a .rp file");
            return 2;
        }
    };
    let entry = match resolve_entry(&input) {
        Ok(entry) => entry,
        Err(error) => {
            eprintln!("error: {}", error);
            return 1;
        }
    };
    let prepared = match prepare(&entry) {
        Some(prepared) => prepared,
        None => return 1,
    };

    let root = project_root(&entry).unwrap_or_else(|| {
        entry
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let manifest = read_manifest(&root);
    // 可执行名优先级：命令行 --name > rip.toml [build] name > [module] name > 入口文件名。
    let app = output_name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| manifest.binary.clone().filter(|name| !name.is_empty()))
        .or_else(|| manifest.name.clone().filter(|name| !name.is_empty()))
        .unwrap_or_else(|| {
            entry
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "app".to_string())
        });
    let version = manifest
        .version
        .clone()
        .unwrap_or_else(|| "0.0.0".to_string());
    // 输出目录优先级：命令行 -o > rip.toml [build] dir > 项目根下 dist。
    let dist = output
        .map(PathBuf::from)
        .or_else(|| {
            manifest
                .dir
                .clone()
                .filter(|dir| !dir.is_empty())
                .map(|dir| root.join(dir))
        })
        .unwrap_or_else(|| root.join("dist"));
    if let Err(error) = fs::create_dir_all(&dist) {
        eprintln!("error: cannot create {}: {}", dist.display(), error);
        return 1;
    }
    // 布局与目标平台：typed 按 bin / lib / data / share 分目录，其余平铺。
    let layout = BuildLayout::parse(manifest.layout.as_deref());
    let host = host_triple();
    let target = match resolve_build_target(
        cli_target.as_deref().or(manifest.target.as_deref()),
        &host,
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("error: {}", error);
            return 1;
        }
    };
    let windows = target.windows;
    let (bin_dir, data_dir, share_dir) = match layout {
        BuildLayout::Typed => (dist.join("bin"), dist.join("data"), dist.join("share")),
        BuildLayout::Flat => (dist.clone(), dist.clone(), dist.clone()),
    };
    for dir in [&bin_dir, &data_dir, &share_dir] {
        if dir != &dist {
            if let Err(error) = fs::create_dir_all(dir) {
                eprintln!("error: cannot create {}: {}", dir.display(), error);
                return 1;
            }
        }
    }
    let exe_name = if windows {
        format!("{}.exe", app)
    } else {
        app.clone()
    };
    let exe_path = bin_dir.join(&exe_name);

    // 实验性原生编译：标量 int 子集，与完整 AOT 互斥。
    if native {
        if target.is_cross() {
            eprintln!(
                "error: --native does not support cross-compilation; omit --target or use the full AOT build"
            );
            return 1;
        }
        return match build_native(&prepared.program, &prepared.checked, &exe_path) {
            Ok(()) => {
                println!(
                    "built {} (native, 原生编译) -> {}",
                    exe_path.display(),
                    dist.display()
                );
                0
            }
            Err(error) => {
                eprintln!("error: {}", error);
                eprintln!(
                    "hint: --native only supports the scalar int subset; omit it to build the full language"
                );
                1
            }
        };
    }

    // 完整 AOT：唯一的交付方式。生成 Rust 宿主后由内置编译组件产出原生二进制，
    // 产物不含源码与解释器，客户侧不需要任何 Rip 环境。
    let setup = aot::Setup {
        chinese: manifest.language == Language::Chinese,
        server: manifest.server.clone(),
        cluster: manifest.cluster.clone(),
    };
    // 原生包：有静态库的直接链接进产物（单文件），其余随产物交付运行时加载。
    let plan = native_link_plan(&prepared.checked.imports);
    if let Err(error) = build_aot(
        &prepared.program,
        &prepared.checked,
        &setup,
        &plan.statically_linked,
        &plan.static_dirs,
        &exe_path,
        &target,
    ) {
        eprintln!("error: {}", error);
        return 1;
    }
    let static_names: Vec<String> = plan
        .statically_linked
        .iter()
        .map(|package| package.package.clone())
        .collect();
    if let Err(error) = assemble_delivery(
        &root, &manifest, &dist, &data_dir, &share_dir, layout, &app, &version, &entry, windows,
        &plan.dynamic, &static_names,
    ) {
        eprintln!("error: {}", error);
        return 1;
    }
    match &target.triple {
        Some(triple) => println!(
            "built {} (aot, {}, 交叉编译) -> {}",
            exe_path.display(),
            triple,
            dist.display()
        ),
        None => println!(
            "built {} (aot, 原生编译) -> {}",
            exe_path.display(),
            dist.display()
        ),
    }
    if !no_archive {
        archive_or_warn(&dist);
    }
    0
}

/// 把交付目录打成同级的 `<目录名>.zip`，解压后直接得到完整目录。
fn archive_dist(dist: &Path) -> Result<PathBuf, String> {
    let name = dist
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "dist".to_string());
    let parent = dist.parent().unwrap_or_else(|| Path::new("."));
    let zip_path = parent.join(format!("{}.zip", name));
    let file = fs::File::create(&zip_path)
        .map_err(|error| format!("cannot create {}: {}", zip_path.display(), error))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_tree(&mut writer, dist, &name, options)?;
    writer
        .finish()
        .map_err(|error| format!("cannot finish {}: {}", zip_path.display(), error))?;
    Ok(zip_path)
}

/// 递归写入目录；条目按名字排序，保证同一份源码打出的包可复现。
fn add_tree(
    writer: &mut zip::ZipWriter<fs::File>,
    dir: &Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("cannot read {}: {}", dir.display(), error))?;
    let mut entries: Vec<fs::DirEntry> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_name = format!("{}/{}", prefix, name);
        if path.is_dir() {
            writer
                .add_directory(format!("{}/", entry_name), options)
                .map_err(|error| format!("cannot add {}: {}", entry_name, error))?;
            add_tree(writer, &path, &entry_name, options)?;
        } else if path.is_file() {
            writer
                .start_file(&entry_name, options)
                .map_err(|error| format!("cannot add {}: {}", entry_name, error))?;
            let mut source = fs::File::open(&path)
                .map_err(|error| format!("cannot read {}: {}", path.display(), error))?;
            io::copy(&mut source, writer)
                .map_err(|error| format!("cannot write {}: {}", path.display(), error))?;
        }
    }
    Ok(())
}

/// 归档失败只提示，不影响构建结果。
fn archive_or_warn(dist: &Path) {
    match archive_dist(dist) {
        Ok(path) => println!("archived {}", path.display()),
        Err(error) => eprintln!("warning: cannot archive delivery: {}", error),
    }
}

/// AOT 原生包链接计划。
struct NativeLinkPlan {
    /// 有静态库、会被编译进产物的包。
    statically_linked: Vec<aot::StaticNative>,
    /// 静态库所在目录，供编译器 `-L native=` 查找。
    static_dirs: Vec<PathBuf>,
    /// 没有静态库、需要随产物交付并在运行时加载的包。
    dynamic: Vec<(String, PathBuf)>,
}

/// 拆分原生包：能静态链接的走静态，其余保持动态加载。
///
/// 声明里写了 `prefix` 的包，四个 ABI 符号带前缀，可以任意多个共存；
/// 没有前缀的包共用 `rp_native_abi` 等固定符号，一个产物里只能链接一个。
fn native_link_plan(imports: &HashMap<String, String>) -> NativeLinkPlan {
    let mut plan = NativeLinkPlan {
        statically_linked: Vec::new(),
        static_dirs: Vec::new(),
        dynamic: Vec::new(),
    };
    // (包路径, 包目录, 静态库目录, 链接描述)
    let mut candidates: Vec<(String, PathBuf, PathBuf, aot::StaticNative)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for path in imports.values() {
        if path.starts_with("std/") || seen.iter().any(|name| name == path) {
            continue;
        }
        let Some(dir) = rp_rust_interop::package_dir(path) else {
            continue;
        };
        seen.push(path.clone());
        let static_lib = rp_rust_interop::static_library_file(&dir)
            .and_then(|lib| rp_rust_interop::static_link_name(&lib).map(|name| (lib, name)));
        let decl_path = dir.join("package.decl");
        let decl = match fs::read_to_string(&decl_path) {
            Ok(text) => match rp_types::native::parse_decl(&text) {
                Ok(decl) => Some(decl),
                Err(error) => {
                    // 声明坏了就无法确定符号名，退回动态加载；把原因说出来而不是静默降级。
                    eprintln!("warning: {}: {}", decl_path.display(), error);
                    None
                }
            },
            Err(_) => None,
        };
        match (static_lib, decl) {
            (Some((lib, link_name)), Some(decl)) => {
                let funcs = decl
                    .funcs
                    .iter()
                    .map(|func| {
                        (
                            func.name.clone(),
                            matches!(func.ret, rp_types::Type::Result(_, _)),
                        )
                    })
                    .collect();
                let lib_dir = lib
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| dir.clone());
                candidates.push((
                    path.clone(),
                    dir,
                    lib_dir,
                    aot::StaticNative {
                        package: path.clone(),
                        link_name,
                        symbol_prefix: decl.prefix.clone(),
                        funcs,
                    },
                ));
            }
            _ => plan.dynamic.push((path.clone(), dir)),
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    // 无前缀的旧 ABI 包共用固定符号，一个产物只能链接一个；其余退回动态加载。
    let mut kept_unprefixed = false;
    for (name, dir, lib_dir, package) in candidates {
        if package.symbol_prefix.is_none() {
            if kept_unprefixed {
                eprintln!(
                    "warning: native package '{}' uses abi 1 (fixed rp_native_* symbols); only one such package can be statically linked per build, falling back to dynamic loading. Add `abi = 2` and a unique `prefix` in package.decl to link several plugins together",
                    name
                );
                plan.dynamic.push((name, dir));
                continue;
            }
            kept_unprefixed = true;
        }
        plan.static_dirs.push(lib_dir);
        plan.statically_linked.push(package);
    }
    plan.static_dirs.sort();
    plan.static_dirs.dedup();
    plan.dynamic.sort_by(|a, b| a.0.cmp(&b.0));
    plan
}

/// 组装交付目录：静态资源、迁移脚本、`[build] copy` 数据、第三方包副本与 README。
/// `rip build` 与 `rip build --aot` 共用同一套规则。
#[allow(clippy::too_many_arguments)]
fn assemble_delivery(
    root: &Path,
    manifest: &ManifestInfo,
    dist: &Path,
    data_dir: &Path,
    share_dir: &Path,
    layout: BuildLayout,
    app: &str,
    version: &str,
    entry: &Path,
    windows: bool,
    native: &[(String, PathBuf)],
    statically_linked: &[String],
) -> Result<(), String> {
    let mut assets: Vec<String> = Vec::new();
    for dir in ["public", "assets", "static"] {
        let from = root.join(dir);
        if from.is_dir() && copy_path(&from, &share_dir.join(dir)).is_ok() {
            assets.push(dir.to_string());
        }
    }
    for asset in &manifest.assets {
        let from = root.join(asset);
        if !from.exists() {
            eprintln!("warning: build asset '{}' does not exist", from.display());
            continue;
        }
        let name = from
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| asset.clone());
        if copy_path(&from, &share_dir.join(&name)).is_ok() {
            assets.push(name);
        }
    }
    let has_migrations = root.join("migrations").is_dir()
        && copy_path(&root.join("migrations"), &data_dir.join("migrations")).is_ok();
    // [build] copy：需要随产物交付的运行期数据（数据库、上传文件等）。
    // 由项目自行声明，工具链不预设任何目录；日志与临时文件不复制。
    let mut copied: Vec<String> = Vec::new();
    for item in &manifest.copy {
        let item = item.trim().trim_matches('/');
        if item.is_empty() {
            continue;
        }
        let from = root.join(item);
        if !from.exists() {
            eprintln!("warning: build copy '{}' does not exist", from.display());
            continue;
        }
        match copy_data_path(&from, &data_dir.join(item)) {
            Ok(()) => {
                let label = item.replace('\\', "/");
                copied.push(if from.is_dir() {
                    format!("{}/", label)
                } else {
                    label
                });
            }
            Err(error) => eprintln!("warning: cannot copy {}: {}", from.display(), error),
        }
    }

    // typed 布局把第三方包另存到 lib/packages，便于交付后单独查看/替换；
    // 源码包与预编译包都已内嵌进可执行文件，删除 lib/ 也不影响运行。
    if layout == BuildLayout::Typed {
        if let Some(packages) = packages_root_of(root) {
            let lib = dist.join("lib").join("packages");
            if let Err(error) = copy_path(&packages, &lib) {
                eprintln!("warning: cannot copy packages: {}", error);
            }
        }
    }

    // 原生包（Rust 插件）：按可执行文件所在目录交付，运行时从 `<exe目录>/lib/native` 加载。
    let mut native_names: Vec<String> = Vec::new();
    if !native.is_empty() {
        let exe_dir = if layout == BuildLayout::Typed {
            dist.join("bin")
        } else {
            dist.to_path_buf()
        };
        let native_root = exe_dir.join("lib").join("native");
        for (name, dir) in native {
            let target = native_root.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
            match copy_path(dir, &target) {
                Ok(()) => native_names.push(name.clone()),
                Err(error) => eprintln!("warning: cannot copy native package {}: {}", name, error),
            }
        }
    }

    // 有外置内容却仍在 flat 布局时，提示更适合应用交付的目录结构。
    if layout == BuildLayout::Flat
        && (!assets.is_empty() || has_migrations || !copied.is_empty() || !native.is_empty())
    {
        eprintln!(
            "hint: 项目带有资源或运行期数据；在 rip.toml 设置 layout = \"typed\" 可得到 bin/ data/ share/ 的目录结构"
        );
    }

    let readme = dist_readme(
        app,
        version,
        &entry.display().to_string(),
        app,
        &assets,
        has_migrations,
        &copied,
        layout,
        windows,
        &native_names,
        statically_linked,
    );
    fs::write(dist.join("README.md"), readme)
        .map_err(|error| format!("cannot write {}: {}", dist.join("README.md").display(), error))
}

/// 完整 AOT：生成等价的宿主程序，链接内置运行时库，产出原生二进制。
///
/// 编译组件默认取自 rip 安装目录（`toolchain/` 与 `lib/rip/`），
/// 因此使用者机器上无需安装任何 Rust 工具链。
///
/// 优先链接 `rp-core`（关闭 `interp` feature，产物不含解释器）；
/// 找不到时回退到 `rp-runtime`（含解释器），功能一致、体积略大。
fn build_aot(
    program: &Program,
    checked: &CheckResult,
    setup: &aot::Setup,
    static_natives: &[aot::StaticNative],
    static_dirs: &[PathBuf],
    exe_path: &Path,
    target: &BuildTarget,
) -> Result<(), String> {
    if let Some(triple) = &target.triple {
        return build_aot_cross(
            program,
            checked,
            setup,
            static_natives,
            static_dirs,
            exe_path,
            triple,
        );
    }
    let current = std::env::current_exe()
        .map_err(|error| format!("cannot locate rip executable: {}", error))?;
    let compiler = aot_compiler().ok_or_else(|| {
        "当前安装缺少 AOT 编译组件（内置编译器）。\n      请安装完整版 rip，或设置 RIP_TOOLCHAIN 指向完整安装目录。"
            .to_string()
    })?;
    let deps = aot_deps_dir(&current);
    // 复用 target/<profile>/deps 里的运行时库。源码树可用时自动维护它：缺失或比源码旧
    // 就先构建，编译失败再强制重建并重试一次——外部只需要 `rip build`。
    ensure_host_runtime(&deps, false);
    let mut rebuilt = false;
    loop {
        let (rlib, host_crate) = match newest_rlib(&deps, "librp_core-") {
            Some(rlib) => (rlib, "rp_core"),
            None => {
                let rlib = newest_rlib(&deps, "librp_runtime-").ok_or_else(|| {
                    format!(
                        "当前安装缺少 AOT 运行时组件（{}）。\n      请重新安装完整版 rip。",
                        deps.display()
                    )
                })?;
                eprintln!("note: 未找到纯净运行时库，产物会带上解释器（功能一致，体积略大）。");
                (rlib, "rp_runtime")
            }
        };
        let rust_source = aot::generate(program, checked, setup, host_crate, static_natives)?;
        // 生成的宿主源码只用于本次编译，写到临时目录，交付目录里不保留任何实现细节。
        let source_path = temp_source_path("aot");
        fs::write(&source_path, rust_source)
            .map_err(|error| format!("cannot write {}: {}", source_path.display(), error))?;
        let mut command = Command::new(&compiler);
        command
            .arg("--edition")
            .arg("2021")
            .arg("-O")
            .arg("-A")
            .arg("warnings")
            .arg("--extern")
            .arg(format!("{}={}", host_crate, rlib.display()))
            .arg("-L")
            .arg(format!("dependency={}", deps.display()));
        if let Some(lld) = rust_lld_path(&compiler) {
            command.arg("-C").arg(format!("linker={}", lld.display()));
        }
        // 依赖里的 build script 可能提供原生库搜索路径（如 winapi、sqlite3）与链接参数
        // （如 macOS framework、Linux rpath）；直接调用编译器时要手动补上。
        let directives = native_link_directives(&deps);
        for search in &directives.searches {
            command.arg("-L").arg(format!("native={}", search));
        }
        // 静态链接的原生包：让编译器在包目录里找到 `lib<name>.a` / `<name>.lib`。
        for dir in static_dirs {
            command.arg("-L").arg(format!("native={}", dir.display()));
        }
        for arg in &directives.args {
            command.arg(arg);
        }
        if std::env::var_os("RIP_AOT_DEBUG").is_some() {
            eprintln!("[aot] deps   = {}", deps.display());
            eprintln!("[aot] host   = {}", host_crate);
            eprintln!("[aot] rlib   = {}", rlib.display());
            eprintln!("[aot] command= {:?}", command);
        }
        let status = command
            .arg("-o")
            .arg(exe_path)
            .arg(&source_path)
            .status();
        remove_temp_source(&source_path);
        let status = status.map_err(|error| format!("无法启动内置编译器：{}", error))?;
        if status.success() {
            return Ok(());
        }
        // 运行时库与本次生成的代码不匹配时，编译器只会报「can't find crate」之类的
        // 间接错误。这里自动重建一次再重试，不要求使用者手工执行任何构建命令。
        if !rebuilt && ensure_host_runtime(&deps, true) {
            rebuilt = true;
            eprintln!("note: 运行时库已重建，重试原生编译 ...");
            continue;
        }
        return Err(format!(
            "原生编译失败（退出码 {}）；设置 RIP_AOT_DEBUG=1 可查看完整命令与运行时库路径。\n      运行时库已自动更新到最新，请按上面的编译错误排查。",
            status
        ));
    }
}

/// 交叉编译：用 cargo 在临时工程里编译生成的宿主源码。
///
/// 与 host 路径的区别：
/// - 运行时库（`rp-core`）需要按目标 triple 编译，由 cargo 解析本地 path 依赖完成；
/// - 链接器与 sysroot 由 cargo/rustup 的目标配置决定，支持 `.cargo/config.toml`
///   与环境变量 `RIP_TARGET_LINKER`；
/// - 产物从 `<repo>/target/<triple>/release/` 复制到交付目录。
fn build_aot_cross(
    program: &Program,
    checked: &CheckResult,
    setup: &aot::Setup,
    static_natives: &[aot::StaticNative],
    static_dirs: &[PathBuf],
    exe_path: &Path,
    triple: &str,
) -> Result<(), String> {
    let source_root = aot_source_root().ok_or_else(|| {
        "交叉编译需要 Rip 源码树来构建目标平台的运行时。\n      请设置 RIP_AOT_SOURCE 指向包含 crates/rp-core 的仓库根目录。"
            .to_string()
    })?;
    let cargo = aot_cargo().ok_or_else(|| {
        "交叉编译需要 cargo。\n      请安装 Rust 工具链，或设置 RIP_TOOLCHAIN 指向完整安装目录。"
            .to_string()
    })?;
    let rust_source = aot::generate(program, checked, setup, "rp_core", static_natives)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let project = std::env::temp_dir().join(format!("rip-cross-{}-{}", process::id(), stamp));
    // 若已存在同名符号链接则直接拒绝，防止在多用户机器上被预置链接
    // 把删除/写入引向别处（CWE-377），与 rip-diff 临时目录同一策略。
    match project.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "refusing to use symlinked temp dir {}",
                project.display()
            ));
        }
        _ => {}
    }
    fs::create_dir_all(&project)
        .map_err(|error| format!("cannot create {}: {}", project.display(), error))?;
    fs::write(project.join("main.rs"), rust_source)
        .map_err(|error| format!("cannot write {}: {}", project.display(), error))?;

    let rp_core = source_root.join("crates").join("rp-core");
    let mut manifest = String::new();
    manifest.push_str("[package]\nname = \"rip-cross-app\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n");
    // 临时工程独立于 rip 仓库的 workspace。
    manifest.push_str("[workspace]\n\n");
    manifest.push_str("[[bin]]\nname = \"app\"\npath = \"main.rs\"\n\n[dependencies]\n");
    manifest.push_str(&format!(
        "rp-core = {{ path = \"{}\" }}\n\n[profile.release]\nopt-level = 2\n",
        rp_core.to_string_lossy().replace('\\', "/")
    ));
    fs::write(project.join("Cargo.toml"), manifest)
        .map_err(|error| format!("cannot write Cargo.toml: {}", error))?;

    // 静态链接的原生包：生成 build.rs 提供库搜索路径，其余链接参数由 cargo 处理。
    if !static_dirs.is_empty() {
        let mut build = String::from("fn main() {\n");
        for dir in static_dirs {
            // 用 Debug 形式生成字符串字面量，路径里的引号/反斜杠会被正确转义，
            // 不会破坏生成的 Rust 源码。
            let line = format!(
                "cargo:rustc-link-search=native={}",
                dir.to_string_lossy().replace('\\', "/")
            );
            build.push_str(&format!("    println!({:?});\n", line));
        }
        build.push_str("}\n");
        fs::write(project.join("build.rs"), build)
            .map_err(|error| format!("cannot write build.rs: {}", error))?;
    }

    let target_dir = source_root.join("target");
    eprintln!(
        "cross-compiling for {} (first run builds the target runtime and may take a while)...",
        triple
    );
    let mut command = Command::new(&cargo);
    command
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg(triple)
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--manifest-path")
        .arg(project.join("Cargo.toml"))
        // 从源码树根运行，让 cargo 读到仓库的 `.cargo/config.toml`（host 链接器配置）。
        .current_dir(&source_root);
    if offline() {
        command.arg("--offline");
    }
    if let Some(linker) = std::env::var_os("RIP_TARGET_LINKER") {
        let key = format!(
            "CARGO_TARGET_{}_LINKER",
            triple.to_uppercase().replace('-', "_")
        );
        command.env(key, linker);
    }
    if std::env::var_os("RIP_AOT_DEBUG").is_some() {
        eprintln!("[aot] cross command = {:?}", command);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法启动 cargo：{}", error))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        let _ = fs::remove_dir_all(&project);
        return Err(format!(
            "交叉编译失败（{}）：\n{}\n      提示：交叉编译需要目标标准库（`rustup target add {}`）、目标平台的链接器（.cargo/config.toml 或 RIP_TARGET_LINKER），以及 sqlite/openssl 等 C 依赖的交叉工具链。",
            triple, tail, triple
        ));
    }

    let windows = triple.contains("windows");
    let produced = target_dir
        .join(triple)
        .join("release")
        .join(if windows { "app.exe" } else { "app" });
    let copied = fs::copy(&produced, exe_path)
        .map_err(|error| format!("cannot copy {}: {}", produced.display(), error));
    let _ = fs::remove_dir_all(&project);
    copied.map(|_| ())
}

/// 定位 Rip 源码树：`RIP_AOT_SOURCE` > 可执行文件所在仓库 > 编译期记录的仓库根。
fn aot_source_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("RIP_AOT_SOURCE") {
        let root = PathBuf::from(root);
        if root.join("crates").join("rp-core").join("Cargo.toml").is_file() {
            return Some(root);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        let mut dir = current.parent().map(|parent| parent.to_path_buf());
        while let Some(candidate) = dir {
            if candidate
                .join("crates")
                .join("rp-core")
                .join("Cargo.toml")
                .is_file()
            {
                return Some(candidate);
            }
            dir = candidate.parent().map(|parent| parent.to_path_buf());
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    if root.join("crates").join("rp-core").join("Cargo.toml").is_file() {
        return Some(root.to_path_buf());
    }
    None
}

/// 定位 cargo：`RIP_TOOLCHAIN` > rip 安装目录 `toolchain/` > PATH。
fn aot_cargo() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("RIP_TOOLCHAIN") {
        let candidate = Path::new(&root).join("bin").join(platform_exe("cargo"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let candidate = dir.join("toolchain").join("bin").join(platform_exe("cargo"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    find_in_path(&platform_exe("cargo"))
}

/// 生成的宿主源码临时文件路径：写在当前目录的 `.tmp/` 下，编译结束后删除。
///
/// 不再落到系统临时目录：`rip build` 的中间产物始终留在项目根目录，便于排查，
/// 也不会出现在交付目录里。
fn temp_source_path(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::current_dir()
        .map(|cwd| cwd.join(".tmp"))
        .unwrap_or_else(|_| std::env::temp_dir());
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("rip-{}-{}-{}.rs", tag, process::id(), stamp))
}

/// 编译结束后清理中间源码；目录空了就一并删掉。
fn remove_temp_source(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(dir) = path.parent() {
        if dir.file_name().map(|name| name == ".tmp").unwrap_or(false) {
            let _ = fs::remove_dir(dir);
        }
    }
}

/// 定位 AOT 编译器：`RIP_TOOLCHAIN` > rip 安装目录下的 `toolchain/` > PATH。
fn aot_compiler() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("RIP_TOOLCHAIN") {
        let candidate = Path::new(&root).join("bin").join(platform_exe("rustc"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let candidate = dir.join("toolchain").join("bin").join(platform_exe("rustc"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // 开发机上的常规安装：仍可从 PATH 找到。
    find_in_path(&platform_exe("rustc"))
}

/// AOT 运行时库目录：`RIP_AOT_DEPS` > `<rip>/lib/rip/deps` > 开发布局 `<rip>/deps`。
fn aot_deps_dir(rip_exe: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("RIP_AOT_DEPS") {
        return PathBuf::from(dir);
    }
    let base = rip_exe.parent().unwrap_or_else(|| Path::new("."));
    let bundled = base.join("lib").join("rip").join("deps");
    if bundled.is_dir() {
        return bundled;
    }
    base.join("deps")
}

fn platform_exe(base: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", base)
    } else {
        base.to_string()
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn newest_rlib(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(prefix) || !name.ends_with(".rlib") {
            continue;
        }
        let modified = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map_or(true, |(stamp, _)| modified > *stamp) {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// 确保 AOT 运行时库存在且不比源码旧；`force` 为真时无条件重建。
///
/// `rip build` 直接复用 `target/<profile>/deps` 里的 `librp_core-*.rlib`；一旦改过
/// `rp-runtime` 等源码却没重新构建 `rp-core`，旧 rlib 的依赖已经对不上，rustc 只会
/// 报「can't find crate for `rp_core`」。源码树可用时在这里自动维护它，
/// 使用者只需要执行 `rip build`，不必手工运行任何构建命令。
///
/// 返回是否真的执行了一次构建。
fn ensure_host_runtime(deps: &Path, force: bool) -> bool {
    let (Some(root), Some(cargo)) = (aot_source_root(), aot_cargo()) else {
        return false;
    };
    // 只处理标准的 `target/<profile>/deps` 布局；自定义 RIP_AOT_DEPS 不在其管辖内。
    if !deps.starts_with(root.join("target")) {
        return false;
    }
    let release = deps
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name == "release")
        .unwrap_or(false);
    let stale = force
        || match newest_rlib(deps, "librp_core-") {
            None => true,
            Some(rlib) => match (
                rlib.metadata().and_then(|meta| meta.modified()),
                newest_source_stamp(&root),
            ) {
                (Ok(built), Some(source)) => source > built,
                _ => false,
            },
        };
    if !stale {
        return false;
    }
    eprintln!(
        "note: 构建 AOT 运行时库 rp-core{} ...",
        if release { "（release）" } else { "" }
    );
    let mut command = Command::new(cargo);
    command
        .current_dir(&root)
        .arg("build")
        .arg("-p")
        .arg("rp-core")
        .arg("--no-default-features")
        .arg("--quiet");
    if release {
        command.arg("--release");
    }
    match command.status() {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("note: rp-core 构建失败（{}），继续使用现有运行时库", status);
            false
        }
        Err(error) => {
            eprintln!("note: 无法启动构建组件：{}", error);
            false
        }
    }
}

/// `crates/rp-*` 里 `.rs` / `Cargo.toml` 的最新修改时间。
fn newest_source_stamp(root: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for name in ["rp-core", "rp-runtime", "rp-ast", "rp-types", "rp-rust-interop"] {
        collect_source_stamp(&root.join("crates").join(name), &mut newest);
    }
    newest
}

fn collect_source_stamp(dir: &Path, newest: &mut Option<std::time::SystemTime>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_stamp(&path, newest);
            continue;
        }
        let is_source = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("rs") | Some("toml")
        );
        if !is_source {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
            if newest.map_or(true, |current| modified > current) {
                *newest = Some(modified);
            }
        }
    }
}

/// build script 声明的原生链接信息（直接调用 rustc 时需要手动补）。
#[derive(Default)]
struct LinkDirectives {
    /// `cargo:rustc-link-search=`：原生库搜索路径。
    searches: Vec<String>,
    /// `cargo:rustc-link-arg=`：额外链接参数（macOS framework、Linux rpath 等）。
    args: Vec<String>,
}

/// 收集依赖 build script 的原生库搜索路径与链接参数。
///
/// 普通 `cargo:rustc-link-lib=foo` 会随 rlib 记录的 native 库依赖自动传递，不必重复；
/// `static=` / `framework=` 需要显式补，避免链接器按动态库名去找。
fn native_link_directives(deps: &Path) -> LinkDirectives {
    let mut out = LinkDirectives::default();
    let root = match deps.parent() {
        Some(parent) => parent,
        None => return out,
    };
    let build = root.join("build");
    let entries = match fs::read_dir(&build) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let output = entry.path().join("output");
        let text = match fs::read_to_string(&output) {
            Ok(text) => text,
            Err(_) => continue,
        };
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("cargo:rustc-link-search=") {
                let path = rest.strip_prefix("native=").unwrap_or(rest).trim();
                if path.is_empty() {
                    continue;
                }
                // 构建时记录的是绝对路径；安装包被移动后按目录名映射到随包目录。
                let path = localize_native_search(path, root);
                let path = path.to_string_lossy().to_string();
                if !out.searches.iter().any(|seen| seen == &path) {
                    out.searches.push(path);
                }
                continue;
            }
            // 只收集与二进制链接相关的参数；-bins / -cdylib 之外的变体忽略。
            let arg = line
                .strip_prefix("cargo:rustc-link-arg=")
                .or_else(|| line.strip_prefix("cargo:rustc-link-arg-bins="));
            if let Some(arg) = arg {
                let arg = arg.trim();
                if !arg.is_empty() && !out.args.iter().any(|seen| seen == arg) {
                    out.args.push(arg.to_string());
                }
                continue;
            }
            // macOS 的 framework 必须显式传递。
            if let Some(lib) = line.strip_prefix("cargo:rustc-link-lib=") {
                let lib = lib.trim();
                if lib.starts_with("framework=") || lib.starts_with("static=") {
                    let flag = format!("-l{}", lib);
                    if !out.args.iter().any(|seen| seen == &flag) {
                        out.args.push(flag);
                    }
                }
            }
        }
    }
    out
}

/// 把构建期记录的绝对搜索路径映射到安装包内的对应目录。
///
/// 随包目录有两种布局：`<root>/build/<crate-hash>/out`（依赖自身的产物）与
/// `<root>/native/<crate-hash>/lib`（外部原生库，如 winapi 的导入库）。
/// 原路径存在时保持不变，便于开发机上直接构建。
fn localize_native_search(raw: &str, root: &Path) -> PathBuf {
    let original = Path::new(raw);
    if original.is_dir() {
        return original.to_path_buf();
    }
    let mut parts = original.components().rev();
    let last = match parts.next() {
        Some(part) => part.as_os_str(),
        None => return original.to_path_buf(),
    };
    let parent = match parts.next() {
        Some(part) => part.as_os_str(),
        None => return original.to_path_buf(),
    };
    for kind in ["build", "native"] {
        let candidate = root.join(kind).join(parent).join(last);
        if candidate.is_dir() {
            return candidate;
        }
    }
    original.to_path_buf()
}

/// 实验性原生编译：生成等价的宿主程序并产出原生可执行文件。
fn build_native(program: &Program, checked: &CheckResult, exe_path: &Path) -> Result<(), String> {
    let rust_source = native::generate(program, checked)?;
    let compiler = aot_compiler().ok_or_else(|| {
        "当前安装缺少 AOT 编译组件（内置编译器）。\n      请安装完整版 rip，或设置 RIP_TOOLCHAIN 指向完整安装目录。"
            .to_string()
    })?;
    let source_path = temp_source_path("native");
    fs::write(&source_path, rust_source)
        .map_err(|error| format!("cannot write {}: {}", source_path.display(), error))?;
    let mut command = Command::new(&compiler);
    command
        .arg("--edition")
        .arg("2021")
        .arg("-O")
        .arg("-A")
        .arg("warnings");
    // 本机 MinGW 位于含空格的路径，GNU ld 无法处理；改用工具链自带的链接器。
    if let Some(lld) = rust_lld_path(&compiler) {
        command.arg("-C").arg(format!("linker={}", lld.display()));
    }
    let status = command
        .arg("-o")
        .arg(exe_path)
        .arg(&source_path)
        .status();
    remove_temp_source(&source_path);
    let status = status.map_err(|error| format!("无法启动内置编译器：{}", error))?;
    if !status.success() {
        return Err(format!(
            "原生编译失败（退出码 {}）；设置 RIP_AOT_DEBUG=1 可查看完整编译命令",
            status
        ));
    }
    Ok(())
}

/// Windows 下定位工具链自带的链接器（与仓库 .cargo/config.toml 的选择一致）。
fn rust_lld_path(compiler: &Path) -> Option<PathBuf> {
    // 只有 MinGW（windows-gnu）需要绕开系统 GNU ld；MSVC 目标用默认的 link.exe。
    if !cfg!(all(windows, target_env = "gnu")) {
        return None;
    }
    let output = Command::new(compiler)
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let rustlib = Path::new(&sysroot).join("lib").join("rustlib");
    for entry in fs::read_dir(rustlib).ok()?.flatten() {
        let candidate = entry.path().join("bin").join("rust-lld.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `rip pack`：把一个包（目录或单个 .rp 文件）编译成 `.rpa`。
/// 产物是带头部的源码归档，可被任意项目通过 `packages/包路径/包名.rpa` 引入；
/// 头部记录 rip 版本，版本不一致时 `rip build` 会给出明确报错并拒绝加载。
fn cmd_pack(args: &[String]) -> i32 {
    let input = match args.iter().find(|arg| !arg.starts_with('-')) {
        Some(input) => PathBuf::from(input),
        None => {
            eprintln!("error: rip pack requires a package directory or .rp file");
            return 2;
        }
    };
    if !input.exists() {
        eprintln!("error: {} does not exist", input.display());
        return 1;
    }
    let (files, out) = if input.is_dir() {
        let mut files = Vec::new();
        collect_package_files(&input, &mut files);
        files.retain(|file| package_file_kind(file) == Some("rp"));
        if files.is_empty() {
            eprintln!("error: no .rp files found in {}", input.display());
            return 1;
        }
        let name = input
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "package".to_string());
        (files, input.join(format!("{}.rpa", name)))
    } else {
        if package_file_kind(&input) != Some("rp") {
            eprintln!("error: rip pack expects a .rp source file");
            return 1;
        }
        let out = input.with_extension("rpa");
        (vec![input.clone()], out)
    };

    let mut merged = String::new();
    for (index, file) in files.iter().enumerate() {
        let source = match fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("error: cannot read {}: {}", file.display(), error);
                return 1;
            }
        };
        if !merged.ends_with('\n') && !merged.is_empty() {
            merged.push('\n');
        }
        if index == 0 {
            merged.push_str(&source);
            if !merged.ends_with('\n') {
                merged.push('\n');
            }
        } else {
            merged.push_str(&blank_package_line(&source));
        }
    }
    let name = out
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "package".to_string());
    let payload = format!(
        "rip-package 1\nrip-version: {}\nname: {}\n--- source ---\n{}",
        env!("CARGO_PKG_VERSION"),
        name,
        merged
    );
    if let Err(error) = fs::write(&out, payload) {
        eprintln!("error: cannot write {}: {}", out.display(), error);
        return 1;
    }
    println!("packed {} -> {}", files.len(), out.display());
    0
}

fn cmd_check(args: &[String]) -> i32 {
    let mut file = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if file.is_none() {
            file = Some(arg.clone());
        }
    }
    let path = match file {
        Some(path) => PathBuf::from(path),
        None => match find_entry(None) {
            Some(path) => path,
            None => {
                eprintln!("error: rip check requires a .rp file");
                return 2;
            }
        },
    };
    let prepared = match prepare_result(&path) {
        Ok(prepared) => prepared,
        Err(messages) => {
            if json {
                // CI 门禁需要机器可读的失败结果，不能只往 stderr 打文本。
                let items: Vec<String> = messages
                    .iter()
                    .map(|message| json_diagnostic_from_message(message))
                    .collect();
                println!(
                    "{{\"file\":{},\"ok\":false,\"diagnostics\":[{}]}}",
                    json_escape(&path.display().to_string()),
                    items.join(",")
                );
            } else {
                print_messages(&messages);
            }
            return 1;
        }
    };
    let checked = &prepared.checked;
    let units = &prepared.units;
    if json {
        let items: Vec<String> = checked
            .diagnostics
            .iter()
            .map(|d| {
                let (file, line) = locate(units, d.line, &path.display().to_string());
                format!(
                    "{{\"file\":{},\"level\":\"{}\",\"line\":{},\"message\":{}}}",
                    json_escape(&file),
                    match d.level {
                        Level::Error => "error",
                        Level::Warning => "warning",
                    },
                    line,
                    json_escape(&d.message)
                )
            })
            .collect();
        println!(
            "{{\"file\":{},\"ok\":true,\"diagnostics\":[{}]}}",
            json_escape(&path.display().to_string()),
            items.join(",")
        );
    } else if checked.diagnostics.is_empty() {
        println!("{}: ok", path.display());
    }
    0
}

/// 把 `文件:行: 级别: 消息` 形式的诊断文本转成 JSON 对象（尽力解析）。
fn json_diagnostic_from_message(message: &str) -> String {
    let (level, text) = if let Some(index) = message.find(": error: ") {
        ("error", &message[index + ": error: ".len()..])
    } else if let Some(index) = message.find(": warning: ") {
        ("warning", &message[index + ": warning: ".len()..])
    } else {
        // 读文件失败等非诊断信息，按错误原样输出。
        ("error", message)
    };
    let head = message.split(": ").next().unwrap_or_default();
    let (file, line) = match head.rsplit_once(':') {
        Some((file, line)) if line.trim().parse::<u32>().is_ok() => {
            (file.to_string(), line.trim().parse::<u32>().unwrap_or(0))
        }
        _ => (String::new(), 0),
    };
    format!(
        "{{\"file\":{},\"level\":\"{}\",\"line\":{},\"message\":{}}}",
        json_escape(&file),
        level,
        line,
        json_escape(text)
    )
}

/// `rip doctor` 的自检计数。
#[derive(Default)]
struct DoctorReport {
    failures: usize,
    warnings: usize,
}

impl DoctorReport {
    fn ok(&self, label: &str, detail: impl std::fmt::Display) {
        println!("  [ok]   {:<16} {}", label, detail);
    }

    fn warn(&mut self, label: &str, detail: impl std::fmt::Display) {
        self.warnings += 1;
        println!("  [warn] {:<16} {}", label, detail);
    }

    fn fail(&mut self, label: &str, detail: impl std::fmt::Display) {
        self.failures += 1;
        println!("  [fail] {:<16} {}", label, detail);
    }
}

/// `rip doctor`：环境 / 项目 / 依赖 / AOT 工具链自检，供部署前确认。
fn cmd_doctor(args: &[String]) -> i32 {
    let mut target: Option<String> = None;
    for arg in args {
        if !arg.starts_with('-') && target.is_none() {
            target = Some(arg.clone());
        }
    }
    println!("rip doctor");
    println!(
        "  toolchain        rip {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let mut report = DoctorReport::default();

    let entry = match target {
        Some(path) => Some(PathBuf::from(path)),
        None => find_entry(None),
    };
    let root = entry.as_deref().and_then(project_root);
    match &root {
        Some(root) => report.ok("project", root.display()),
        None => report.warn("project", "no rip.toml found from the current directory"),
    }

    if let Some(root) = &root {
        let manifest = read_manifest(root);
        report.ok(
            "module",
            format!(
                "{} {}",
                manifest.name.clone().unwrap_or_else(|| "<unnamed>".to_string()),
                manifest.version.clone().unwrap_or_else(|| "<no version>".to_string())
            ),
        );

        match read_lock(root) {
            Ok(Some(packages)) => report.ok("lock", format!("{} package(s)", packages.len())),
            Ok(None) => report.warn("lock", "rip.lock is missing; run 'rip vendor'"),
            Err(error) => report.fail("lock", error),
        }
        if read_lock(root).ok().flatten().is_some() {
            match verify_lock(root) {
                Ok(()) => report.ok("lock check", "consistent"),
                Err(messages) => {
                    report.fail("lock check", format!("{} issue(s)", messages.len()));
                    for message in messages {
                        println!("         {}", message);
                    }
                }
            }
        }
        match resolve_dependencies(root) {
            Ok(packages) => report.ok(
                "dependencies",
                format!("{} package(s) resolved", packages.len()),
            ),
            Err(messages) => {
                report.fail("dependencies", format!("{} issue(s)", messages.len()));
                for message in messages {
                    println!("         {}", message);
                }
            }
        }

        match manifest.registry.as_deref() {
            Some(source) if offline() || manifest.registry_offline => {
                report.ok("registry", format!("{} (offline)", source))
            }
            Some(source) => report.ok("registry", source),
            None => report.warn(
                "registry",
                "not configured; missing packages cannot be fetched",
            ),
        }
        if let Some(name) = &manifest.registry_token_env {
            match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => {
                    report.ok("registry token", format!("{} is set", name))
                }
                _ => report.fail("registry token", format!("{} is not set", name)),
            }
        }

        let files = packages_root_of(root)
            .map(|packages| {
                let mut files = Vec::new();
                collect_package_files(&packages, &mut files);
                files.len()
            })
            .unwrap_or(0);
        report.ok("packages", format!("{} file(s) under packages/", files));

        report.ok(
            "server",
            format!(
                "workers={} request_timeout_ms={} drain_timeout_ms={} access_log={}",
                manifest.server.workers,
                manifest.server.request_timeout.as_millis(),
                manifest.server.drain_timeout.as_millis(),
                manifest.server.access_log
            ),
        );
        if let Some(target_platform) = &manifest.target {
            if target_platform != &std::env::consts::OS {
                report.warn(
                    "build target",
                    format!(
                        "'{}' differs from host '{}'; AOT builds must run on the target platform",
                        target_platform,
                        std::env::consts::OS
                    ),
                );
            }
        }
    }

    match aot_compiler() {
        Some(compiler) => report.ok("rustc", compiler.display()),
        None => report.warn("rustc", "not found; 'rip build' needs the bundled toolchain"),
    }
    if let Ok(current) = std::env::current_exe() {
        let deps = aot_deps_dir(&current);
        match newest_rlib(&deps, "librp_core-") {
            Some(rlib) => report.ok("aot host", rlib.display()),
            None => report.warn(
                "aot host",
                format!("librp_core not found under {}", deps.display()),
            ),
        }
    }

    println!();
    if report.failures > 0 {
        println!(
            "doctor: {} failure(s), {} warning(s)",
            report.failures, report.warnings
        );
        1
    } else if report.warnings > 0 {
        println!("doctor: ok with {} warning(s)", report.warnings);
        0
    } else {
        println!("doctor: ok");
        0
    }
}

fn print_migrate_help() {
    println!("用法: rip migrate <up|down|status> [选项]");
    println!();
    println!("子命令:");
    println!("  up                    应用所有未执行的迁移（按版本升序）");
    println!("  down                  回滚最近一条迁移");
    println!("  status                列出已应用 / 未应用的迁移");
    println!();
    println!("选项:");
    println!("  --dsn <DSN>           数据库连接串；缺省依次读 RIP_DATABASE_URL、RIP_DB、rip.toml 的 [database] dsn");
    println!("  --dir <目录>          迁移目录，缺省 migrations");
    println!("  --step <N>            up 最多执行 N 条；down 最多回滚 N 条（down 缺省 1 条）");
    println!("  --to <版本>           up 应用到该版本（含）；down 回滚该版本之后的全部");
    println!("  --dry-run             只打印计划，不写数据库");
    println!("  --allow-modified      忽略已应用脚本的校验和不一致");
    println!();
    println!("命名: <版本>_<名称>.sql（升级）、<版本>_<名称>.down.sql（回滚）；");
    println!("      <版本>_<名称>.<sqlite|mysql|postgres|mssql>.sql 只在该方言下执行。");
}

fn migration_dsn(explicit: Option<String>) -> Result<String, String> {
    if let Some(dsn) = explicit {
        let dsn = dsn.trim().to_string();
        if dsn.is_empty() {
            return Err("--dsn cannot be empty".to_string());
        }
        return Ok(dsn);
    }
    for key in ["RIP_DATABASE_URL", "RIP_DB"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    if let Some(dsn) = read_manifest(Path::new(".")).database {
        return Ok(dsn);
    }
    Err("no database DSN: pass --dsn, export RIP_DATABASE_URL, or set [database] dsn in rip.toml"
        .to_string())
}

fn cmd_migrate(args: &[String]) -> i32 {
    if args.is_empty() {
        print_migrate_help();
        return 2;
    }
    let action = args[0].as_str();
    if matches!(action, "help" | "-h" | "--help") {
        print_migrate_help();
        return 0;
    }
    if !matches!(action, "up" | "down" | "status") {
        eprintln!("error: unknown migrate subcommand '{}'", action);
        print_migrate_help();
        return 2;
    }

    let mut dsn_flag: Option<String> = None;
    let mut directory = PathBuf::from("migrations");
    let mut options = rp_runtime::migrate::Options::default();
    let mut index = 1;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        let mut take_value = |required: &str| -> Result<String, String> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", required))
        };
        let result = match flag {
            "--dsn" => take_value("--dsn").map(|value| dsn_flag = Some(value)),
            "--dir" => take_value("--dir").map(|value| directory = PathBuf::from(value)),
            "--step" => take_value("--step").and_then(|value| {
                match value.parse::<usize>() {
                    Ok(step) if step > 0 => {
                        options.step = Some(step);
                        Ok(())
                    }
                    _ => Err(format!(
                        "--step expects a positive integer, found '{}'",
                        value
                    )),
                }
            }),
            "--to" => take_value("--to").and_then(|value| match value.parse::<i64>() {
                Ok(to) if to >= 0 => {
                    options.to = Some(to);
                    Ok(())
                }
                _ => Err(format!(
                    "--to expects a non-negative version, found '{}'",
                    value
                )),
            }),
            "--dry-run" => {
                options.dry_run = true;
                Ok(())
            }
            "--allow-modified" => {
                options.allow_modified = true;
                Ok(())
            }
            other => Err(format!("unknown migrate option '{}'", other)),
        };
        if let Err(message) = result {
            eprintln!("error: {}", message);
            return 2;
        }
        index += 1;
    }

    if action == "status"
        && (options.step.is_some()
            || options.to.is_some()
            || options.dry_run
            || options.allow_modified)
    {
        eprintln!("error: 'migrate status' accepts only --dsn and --dir");
        return 2;
    }

    let dsn = match migration_dsn(dsn_flag) {
        Ok(dsn) => dsn,
        Err(message) => {
            eprintln!("error: {}", message);
            return 2;
        }
    };

    match action {
        "status" => match rp_runtime::migrate::status(&dsn, &directory) {
            Ok(report) => {
                println!("database: {} ({})", report.dialect, dsn);
                println!("directory: {}", report.directory.display());
                for entry in &report.entries {
                    let mut note = String::new();
                    if entry.missing {
                        note.push_str("  [file missing]");
                    } else if !entry.checksum_ok {
                        note.push_str("  [checksum mismatch]");
                    }
                    if entry.applied_at.is_some() && !entry.has_down {
                        note.push_str("  [no down script]");
                    }
                    match &entry.applied_at {
                        Some(at) => println!(
                            "  {:>8}  applied  {}  {}{}",
                            entry.version, entry.name, at, note
                        ),
                        None => {
                            println!("  {:>8}  pending  {}{}", entry.version, entry.name, note)
                        }
                    }
                }
                println!("{} applied, {} pending", report.applied, report.pending);
                0
            }
            Err(message) => {
                eprintln!("error: {}", message);
                1
            }
        },
        "up" => match rp_runtime::migrate::up(&dsn, &directory, &options) {
            Ok(report) => {
                for step in &report.executed {
                    if report.dry_run {
                        println!("would apply {}  {}", step.version, step.name);
                    } else {
                        println!("applied {}  {} ({} ms)", step.version, step.name, step.duration_ms);
                    }
                }
                if report.executed.is_empty() {
                    println!("database is up to date ({} applied)", report.applied);
                } else if report.dry_run {
                    println!(
                        "dry run: {} migration(s), nothing written; {} applied, {} pending",
                        report.executed.len(),
                        report.applied,
                        report.pending
                    );
                } else {
                    println!(
                        "{} applied, {} pending",
                        report.applied, report.pending
                    );
                }
                0
            }
            Err(message) => {
                eprintln!("error: {}", message);
                1
            }
        },
        _ => match rp_runtime::migrate::down(&dsn, &directory, &options) {
            Ok(report) => {
                for step in &report.executed {
                    if report.dry_run {
                        println!("would roll back {}  {}", step.version, step.name);
                    } else {
                        println!(
                            "rolled back {}  {} ({} ms)",
                            step.version, step.name, step.duration_ms
                        );
                    }
                }
                if report.executed.is_empty() {
                    println!("nothing to roll back ({} applied)", report.applied);
                } else {
                    println!("{} rolled back, {} applied", report.executed.len(), report.applied);
                }
                0
            }
            Err(message) => {
                eprintln!("error: {}", message);
                1
            }
        },
    }
}

fn cmd_fmt(args: &[String]) -> i32 {
    let mut check = false;
    let mut to_stdout = false;
    let mut target: Option<PathBuf> = None;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "--stdout" => to_stdout = true,
            other if other.starts_with('-') => {
                eprintln!("error: unknown fmt option '{}'", other);
                return 2;
            }
            other => target = Some(PathBuf::from(other)),
        }
    }
    let target = match target {
        Some(target) => target,
        None => {
            eprintln!("error: rip fmt requires a .rp file or directory");
            return 2;
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    if target.is_dir() {
        collect_rp_files(&target, &mut files);
        files.sort();
    } else if target.is_file() {
        files.push(target.clone());
    } else {
        eprintln!("error: '{}' does not exist", target.display());
        return 2;
    }
    if files.is_empty() {
        eprintln!("error: no .rp files under '{}'", target.display());
        return 2;
    }
    let mut changed = 0usize;
    for path in &files {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("error: {}: {}", path.display(), error);
                return 1;
            }
        };
        let formatted = fmt_source(&source);
        if formatted == source {
            continue;
        }
        changed += 1;
        if check {
            println!("would reformat {}", path.display());
            continue;
        }
        if to_stdout && files.len() == 1 {
            print!("{}", formatted);
            continue;
        }
        if let Err(error) = fs::write(path, &formatted) {
            eprintln!("error: {}: {}", path.display(), error);
            return 1;
        }
        println!("formatted {}", path.display());
    }
    if changed == 0 {
        if !check && !to_stdout {
            println!("already formatted ({} files)", files.len());
        }
        return 0;
    }
    if check {
        println!("{} file(s) need formatting", changed);
        return 1;
    }
    0
}

/// 格式化：统一 4 空格缩进（按 `{}` / `[]` / `()` 层级）、去行尾空白、
/// 压缩连续空行、排序相邻 import 块。
///
/// 三引号模板字符串与 `/* */` 注释内部原样保留，避免破坏内容。
fn fmt_source(source: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    let mut in_template = false;
    let mut in_block_comment = false;
    for raw in source.lines() {
        let line = raw.trim_end();
        // 模板字符串 / 块注释内部不重排，保持字节级原样。
        if in_template || in_block_comment {
            out.push(line.to_string());
            let (_, _, template, block) = scan_line(line, in_template, in_block_comment);
            in_template = template;
            in_block_comment = block;
            continue;
        }
        if line.trim().is_empty() {
            if out.last().map_or(false, |prev| prev.trim().is_empty()) {
                continue;
            }
            out.push(String::new());
            continue;
        }
        let code = line.trim_start();
        let closers = leading_closers(code);
        let indent = (depth - closers).max(0) as usize;
        out.push(format!("{}{}", "    ".repeat(indent), code));
        let (opens, closes, template, block) = scan_line(code, false, false);
        depth = (depth + opens - closes).max(0);
        in_template = template;
        in_block_comment = block;
    }
    // 去掉文件末尾多余空行，统一以一个换行结束。
    while out.last().map_or(false, |line| line.is_empty()) {
        out.pop();
    }
    let lines = sort_import_blocks(out);
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// 行首连续闭合符号的数量（`}` / `]` / `)`）。
fn leading_closers(code: &str) -> i32 {
    code.chars()
        .take_while(|ch| matches!(ch, '}' | ']' | ')'))
        .count() as i32
}

/// 扫描一行代码：返回 `(开括号数, 闭括号数, 结束时是否在模板字符串内, 是否在块注释内)`。
///
/// 跳过普通字符串、单引号字符、`//` 行注释与 `/* */` 块注释，
/// 因此括号统计只针对真正的代码。
fn scan_line(line: &str, mut template: bool, mut block: bool) -> (i32, i32, bool, bool) {
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0usize;
    let mut opens = 0i32;
    let mut closes = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if block {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                block = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if template {
            if ch == '"'
                && chars.get(index + 1) == Some(&'"')
                && chars.get(index + 2) == Some(&'"')
            {
                template = false;
                index += 3;
                continue;
            }
            index += 1;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' && chars.get(index + 1) == Some(&'"') && chars.get(index + 2) == Some(&'"')
        {
            template = true;
            index += 3;
            continue;
        }
        if ch == '"' {
            in_string = true;
            index += 1;
            continue;
        }
        if ch == '\'' {
            index += 1;
            while index < chars.len() && chars[index] != '\'' {
                if chars[index] == '\\' {
                    index += 1;
                }
                index += 1;
            }
            index += 1;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            break;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            block = true;
            index += 2;
            continue;
        }
        match ch {
            '{' | '[' | '(' => opens += 1,
            '}' | ']' | ')' => closes += 1,
            _ => {}
        }
        index += 1;
    }
    (opens, closes, template, block)
}

/// 排序相邻的 import 块；块之间被其它代码隔开时不跨越。
fn sort_import_blocks(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if is_import_line(&lines[index]) {
            let start = index;
            while index < lines.len() && is_import_line(&lines[index]) {
                index += 1;
            }
            let mut block = lines[start..index].to_vec();
            block.sort_by_key(|line| import_key(line));
            out.extend(block);
            continue;
        }
        out.push(lines[index].clone());
        index += 1;
    }
    out
}

fn is_import_line(line: &str) -> bool {
    line.trim_start().starts_with("use ")
}

fn import_key(line: &str) -> String {
    let rest = line.trim().trim_start_matches("use ").trim();
    let rest = rest.trim_end_matches(';').trim();
    match rest.split_once(" as ") {
        Some((path, _)) => path.trim().to_string(),
        None => rest.to_string(),
    }
}

fn cmd_new(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut template = "empty".to_string();
    let mut force = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--template" | "-t" => {
                index += 1;
                match args.get(index) {
                    Some(value) => template = value.clone(),
                    None => {
                        eprintln!("error: --template requires http, cli or empty");
                        return 2;
                    }
                }
            }
            "--force" => force = true,
            other if other.starts_with('-') => {
                eprintln!("error: unknown new option '{}'", other);
                return 2;
            }
            other => {
                if name.is_some() {
                    eprintln!("error: rip new takes a single project name");
                    return 2;
                }
                name = Some(other.to_string());
            }
        }
        index += 1;
    }
    let name = match name {
        Some(name) => name,
        None => {
            eprintln!("error: usage: rip new <name> [--template http|cli|empty]");
            return 2;
        }
    };
    // 包名必须与目录名一致，因此项目名也必须是合法标识符。
    if !is_project_name(&name) {
        eprintln!(
            "error: '{}' is not a valid project name (use [a-z][a-z0-9_]*)",
            name
        );
        return 2;
    }
    if !matches!(template.as_str(), "http" | "cli" | "empty") {
        eprintln!("error: unknown template '{}' (use http, cli or empty)", template);
        return 2;
    }
    let root = PathBuf::from(&name);
    if root.exists() {
        let empty = fs::read_dir(&root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty && !force {
            eprintln!(
                "error: '{}' already exists; pass --force to write into it anyway",
                root.display()
            );
            return 2;
        }
    }
    if let Err(error) = fs::create_dir_all(&root) {
        eprintln!("error: cannot create '{}': {}", root.display(), error);
        return 1;
    }
    let main_source = match template.as_str() {
        "http" => format!(
            "mod {name};\n\n\
use std::http;\n\n\
// 注解路由在 `http.router` 构建时自动注册。\n\
#[get(\"/\")]\n\
func index(read req: http.request) -> http.response {{\n    \
return http.response[status = 200, json = [message = \"Hello from {name}!\", path = req.path]];\n\
}}\n\n\
func main() -> void {{\n    \
let routes: vec<http.route> = [];\n    \
http.serve(8080, http.router(routes));\n\
}}\n",
            name = name
        ),
        "cli" => format!(
            "mod {name};\n\n\
use std::cli;\n\
use std::dev;\n\n\
func main() -> void {{\n    \
let verbose: bool = cli.flag(\"verbose\");\n    \
let who: string = cli.option(\"name\", \"world\");\n    \
if verbose {{\n        \
dev.log(\"args=\" + cli.args().join(\" \"));\n    \
}}\n    \
print(\"Hello, \" + who + \"!\");\n\
}}\n",
            name = name
        ),
        _ => format!(
            "mod {name};\n\nfunc main() -> void {{\n    print(\"Hello, Rip!\");\n}}\n",
            name = name
        ),
    };
    let manifest = format!(
        "[module]\nname = \"{name}\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n\
[build]\ndir = \"dist\"\n\n\
[runtime]\nlog = \"english\"\n\n\
[server]\nworkers = 0\nmax_connections = 1024\nrequest_timeout_ms = 30000\n\
# 需要 Prometheus 采集时改成 \"/metrics\"\nmetrics_path = \"\"\n",
        name = name
    );
    let gitignore = "dist/\n*.log\nstorage/\n";
    let github_ci = r#"name: ci
on:
  push:
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # 将 rip 加入 PATH：使用官方安装包，或把交付包里的 bin/rip 加进 PATH。
      - name: Install rip
        run: |
          if ! command -v rip >/dev/null 2>&1; then
            echo "install the rip toolchain and put it on PATH" >&2
            exit 1
          fi
      - name: Check
        run: rip check main.rp
      - name: Test
        run: rip test
      # 启用依赖后取消注释：
      # - name: Lock
      #   run: rip lock --check
      # - name: Audit
      #   run: rip audit --deny high
      - name: Build
        run: rip build main.rp --no-archive
"#;
    let gitlab_ci = r#"stages:
  - test

test:
  stage: test
  image: ubuntu:24.04
  script:
    - command -v rip >/dev/null 2>&1 || { echo "install the rip toolchain and put it on PATH" >&2; exit 1; }
    - rip check main.rp
    - rip test
    # 启用依赖后追加：
    # - rip lock --check
    # - rip audit --deny high
    - rip build main.rp --no-archive
"#;
    let files: Vec<(PathBuf, String)> = vec![
        (root.join("rip.toml"), manifest),
        (root.join("main.rp"), main_source),
        (root.join(".gitignore"), gitignore.to_string()),
        (root.join(".github").join("workflows").join("ci.yml"), github_ci.to_string()),
        (root.join(".gitlab-ci.yml"), gitlab_ci.to_string()),
    ];
    for (path, content) in files {
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("error: cannot create '{}': {}", parent.display(), error);
                return 1;
            }
        }
        if let Err(error) = fs::write(&path, content) {
            eprintln!("error: cannot write '{}': {}", path.display(), error);
            return 1;
        }
    }
    println!("created {} ({}) with CI templates", root.display(), template);
    println!();
    println!("next steps:");
    println!("  cd {}", name);
    match template.as_str() {
        "http" => println!("  rip run main.rp          # http://127.0.0.1:8080/"),
        _ => println!("  rip run main.rp"),
    }
    0
}

/// 项目名同时用作包名：小写字母开头，只含小写字母、数字与下划线。
fn is_project_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn cmd_bench(args: &[String]) -> i32 {
    let mut target: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut iterations = 1000usize;
    let mut warmup = 10usize;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        let mut take = |name: &str| -> Option<String> {
            if let Some(value) = &inline {
                return Some(value.clone());
            }
            index += 1;
            args.get(index).cloned().or_else(|| {
                eprintln!("error: {} requires a value", name);
                None
            })
        };
        match flag {
            "--iterations" | "-n" => match take("--iterations").and_then(|value| value.parse().ok())
            {
                Some(value) if value > 0 => iterations = value,
                _ => {
                    eprintln!("error: --iterations expects a positive integer");
                    return 2;
                }
            },
            "--warmup" => match take("--warmup").and_then(|value| value.parse().ok()) {
                Some(value) => warmup = value,
                None => {
                    eprintln!("error: --warmup expects a non-negative integer");
                    return 2;
                }
            },
            "--filter" => match take("--filter") {
                Some(value) => filter = Some(value),
                None => return 2,
            },
            other if other.starts_with('-') => {
                eprintln!("error: unknown bench option '{}'", other);
                return 2;
            }
            other => {
                if target.is_none() {
                    target = Some(other.to_string());
                }
            }
        }
        index += 1;
    }
    let default_target = ".";
    let root = PathBuf::from(target.unwrap_or_else(|| default_target.to_string()));
    if !root.exists() {
        eprintln!("error: bench target '{}' does not exist", root.display());
        return 2;
    }
    // 目标解析：单文件、项目入口或目录下全部 .rp。
    let mut inputs: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        inputs.push(root.clone());
    } else if root.join("rip.toml").is_file() {
        match resolve_entry(&root) {
            Ok(entry) => inputs.push(entry),
            Err(error) => {
                eprintln!("error: {}", error);
                return 2;
            }
        }
    } else {
        collect_rp_files(&root, &mut inputs);
        inputs.sort();
    }
    let mut failures = 0usize;
    let mut executed = 0usize;
    for input in inputs {
        let prepared = match prepare(&input) {
            Some(prepared) => prepared,
            None => {
                failures += 1;
                continue;
            }
        };
        let mut names: Vec<(String, u32)> = Vec::new();
        for item in &prepared.program.items {
            if let Item::Func(func) = item {
                if !func.attributes.iter().any(|attribute| attribute.name == "bench") {
                    continue;
                }
                if let Some(filter) = &filter {
                    if !func.name.contains(filter.as_str()) {
                        continue;
                    }
                }
                names.push((func.name.clone(), func.line));
            }
        }
        if names.is_empty() {
            if filter.is_none() {
                println!("{}: no #[bench] functions", input.display());
            }
            continue;
        }
        let seed = rp_runtime::RuntimeSeed::from_program(prepared.program, prepared.checked);
        for (name, line) in names {
            let mut runtime = seed.build();
            if let Err(error) = runtime.init_entry() {
                eprintln!("bench {} ... FAIL ({})", name, error.message);
                failures += 1;
                continue;
            }
            let mut failed = None;
            for _ in 0..warmup {
                if let Err(message) = bench_call(&mut runtime, &name, line) {
                    failed = Some(message);
                    break;
                }
            }
            if failed.is_some() {
                eprintln!("bench {} ... FAIL", name);
                failures += 1;
                continue;
            }
            let mut samples: Vec<u128> = Vec::with_capacity(iterations);
            for step in 0..iterations {
                let started = std::time::Instant::now();
                if let Err(message) = bench_call(&mut runtime, &name, line) {
                    failed = Some(message);
                    break;
                }
                samples.push(started.elapsed().as_nanos());
                // 每 256 次回收一次环，避免基准函数分配的对象长期堆积。
                if step % 256 == 255 {
                    rp_runtime::gc::collect_if_due();
                }
            }
            if let Some(message) = failed {
                eprintln!("bench {} ... FAIL ({})", name, message);
                failures += 1;
                continue;
            }
            executed += 1;
            print_bench(&name, &samples);
        }
    }
    if executed == 0 && failures == 0 {
        println!("no benches to run");
        return 0;
    }
    if failures > 0 {
        1
    } else {
        0
    }
}

/// 调用一次基准函数，把控制流错误折算成可读信息。
fn bench_call(runtime: &mut Runtime, name: &str, line: u32) -> Result<(), String> {
    match runtime.call_named(name, Vec::new(), line) {
        Ok(_) => Ok(()),
        Err(rp_runtime::Flow::Error(error)) => Err(error.message),
        Err(_) => Err("unexpected control flow (break/continue outside loop)".to_string()),
    }
}

/// 输出一组基准样本：最小 / 平均 / p95 / 最大耗时与每秒调用数。
fn print_bench(name: &str, samples: &[u128]) {
    if samples.is_empty() {
        println!("bench {} ... no samples", name);
        return;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total: u128 = sorted.iter().sum();
    let min = sorted[0];
    let max = sorted[sorted.len() - 1];
    let mean = total / sorted.len() as u128;
    let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
    let ops = if total == 0 {
        f64::INFINITY
    } else {
        sorted.len() as f64 * 1_000_000_000.0 / total as f64
    };
    println!(
        "bench {}: {} iterations, min {}, mean {}, p95 {}, max {}, {}/s",
        name,
        sorted.len(),
        format_ns(min),
        format_ns(mean),
        format_ns(p95),
        format_ns(max),
        format_count(ops)
    );
}

fn format_ns(ns: u128) -> String {
    if ns < 1_000 {
        format!("{}ns", ns)
    } else if ns < 1_000_000 {
        format!("{:.2}µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    }
}

fn format_count(value: f64) -> String {
    if !value.is_finite() {
        return "-".to_string();
    }
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else {
        format!("{:.0}", value)
    }
}

/// `rip add` 接受的包名：单段名（本地 / 简单包源）或 `host/owner/name`（公共包源）。
fn is_valid_package_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn cmd_add(args: &[String]) -> i32 {
    let spec = match args.first() {
        Some(package) => package.as_str(),
        None => {
            eprintln!("error: rip add requires a package name, e.g. acme/tools@^1.2");
            return 2;
        }
    };
    if spec.is_empty() {
        eprintln!("error: rip add requires a package name, e.g. acme/tools@^1.2");
        return 2;
    }
    // `包路径@约束`：约束可写 `^1.2`、`~1.2.3`、`=1.2.3`、`*`。
    let (package, explicit) = match spec.rsplit_once('@') {
        Some((name, requirement)) if !name.is_empty() && !requirement.is_empty() => {
            (name, Some(requirement))
        }
        _ => (spec, None),
    };
    if package.starts_with("std/") {
        println!(
            "{} is a builtin standard module and needs no dependency entry",
            package
        );
        return 0;
    }
    let rust_name = package
        .strip_prefix("rust:")
        .or_else(|| package.strip_prefix("rust/"));
    if rust_name == Some("serde_json") {
        println!(
            "rust/serde_json is already built in (no dependency entry needed)"
        );
        return 0;
    }
    if let Some(name) = rust_name {
        return add_manifest_entry("rust-dependencies", name, "1");
    }
    if !is_valid_package_ref(package) {
        eprintln!(
            "error: package '{}' is not a valid package name (expected name or host/owner/name)",
            package
        );
        return 1;
    }
    let requirement = match explicit {
        Some(requirement) => requirement.to_string(),
        // 未指定约束时：已 vendored 的包按当前版本取兼容约束，否则先放开为任意版本。
        None => suggested_requirement(package),
    };
    if let Err(error) = semver::VersionReq::parse(&requirement) {
        eprintln!(
            "error: invalid version requirement '{}' for '{}': {}",
            requirement, package, error
        );
        return 1;
    }
    let status = add_manifest_entry("dependencies", package, &requirement);
    if status != 0 {
        return status;
    }
    // 写入清单后立即解析并锁定（含传递依赖），避免清单里挂着本地不存在的包。
    // 与 `rip lock` 共用 generate_lock，行为保持一致。
    match generate_lock(Path::new(".")) {
        Ok(count) => {
            println!("wrote ./rip.lock ({} packages)", count);
            0
        }
        Err(messages) => {
            for message in messages {
                eprintln!("{}", message);
            }
            1
        }
    }
}

/// `rip search`：按名字搜索包源。
fn cmd_search(args: &[String]) -> i32 {
    let mut query = String::new();
    let mut registry_url = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--registry" => {
                i += 1;
                registry_url = args.get(i).cloned();
            }
            "--json" => json = true,
            arg if query.is_empty() && !arg.starts_with('-') => query = arg.to_string(),
            _ => {}
        }
        i += 1;
    }
    let root = find_entry(None)
        .and_then(|entry| project_root(&entry))
        .unwrap_or_else(|| PathBuf::from("."));
    match registry::search(&root, &query, registry_url.as_deref()) {
        Ok(packages) => {
            if json {
                let items: Vec<serde_json::Value> = packages.iter().map(package_json).collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "packages": items }))
                        .unwrap_or_else(|_| "{}".to_string())
                );
                return 0;
            }
            if packages.is_empty() {
                println!("no packages matched '{}'", query);
                return 0;
            }
            for package in &packages {
                println!("{}", package.name);
                for version in package.versions.iter().take(5) {
                    print_package_version(version);
                }
                if package.versions.len() > 5 {
                    println!(
                        "  ... {} more version(s) (use 'rip info {}')",
                        package.versions.len() - 5,
                        package.name
                    );
                }
            }
            0
        }
        Err(error) => {
            eprintln!("{}", error);
            1
        }
    }
}

/// `rip info`：查看单个包的全部版本。
fn cmd_info(args: &[String]) -> i32 {
    let mut name = String::new();
    let mut registry_url = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--registry" => {
                i += 1;
                registry_url = args.get(i).cloned();
            }
            "--json" => json = true,
            arg if name.is_empty() && !arg.starts_with('-') => name = arg.to_string(),
            _ => {}
        }
        i += 1;
    }
    if name.is_empty() {
        eprintln!("error: rip info requires a package name");
        return 2;
    }
    let root = find_entry(None)
        .and_then(|entry| project_root(&entry))
        .unwrap_or_else(|| PathBuf::from("."));
    match registry::info(&root, &name, registry_url.as_deref()) {
        Ok(package) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&package_json(&package))
                        .unwrap_or_else(|_| "{}".to_string())
                );
                return 0;
            }
            println!("{} ({} version(s))", package.name, package.versions.len());
            for version in &package.versions {
                print_package_version(version);
            }
            0
        }
        Err(error) => {
            eprintln!("{}", error);
            1
        }
    }
}

fn package_json(package: &registry::PackageSummary) -> serde_json::Value {
    let versions: Vec<serde_json::Value> = package
        .versions
        .iter()
        .map(|version| {
            serde_json::json!({
                "version": version.version,
                "sha256": version.sha256,
                "deps": version.deps.iter().map(|(name, requirement)| {
                    serde_json::json!({ "name": name, "requirement": requirement })
                }).collect::<Vec<_>>()
            })
        })
        .collect();
    serde_json::json!({ "name": package.name, "versions": versions })
}

fn print_package_version(version: &registry::PackageVersion) {
    let short = version
        .sha256
        .as_deref()
        .map(|sha| sha.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "-".to_string());
    let deps = if version.deps.is_empty() {
        String::new()
    } else {
        format!(
            "  deps: {}",
            version
                .deps
                .iter()
                .map(|(name, requirement)| format!("{} {}", name, requirement))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    println!("  {:<12} sha256 {} {}", version.version, short, deps);
}

#[derive(Clone, Debug)]
struct AuditIssue {
    kind: String,
    severity: String,
    package: String,
    version: String,
    id: String,
    title: String,
    url: String,
}

fn severity_rank(value: &str) -> u8 {
    match value.to_ascii_lowercase().as_str() {
        "critical" => 4,
        "high" => 3,
        "moderate" | "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// `rip audit`：锁定文件校验、依赖完整性、安全公告与许可证清单。
fn cmd_audit(args: &[String]) -> i32 {
    let mut json = false;
    let mut deny = "high".to_string();
    let mut show_licenses = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--licenses" => show_licenses = true,
            "--deny" => {
                i += 1;
                deny = args.get(i).cloned().unwrap_or_else(|| "high".to_string());
            }
            _ => {}
        }
        i += 1;
    }
    let deny_rank = if deny.eq_ignore_ascii_case("none") {
        255
    } else {
        severity_rank(&deny)
    };
    let root = match find_entry(None).and_then(|entry| project_root(&entry)) {
        Some(root) => root,
        None => {
            eprintln!("error: rip audit requires a project with rip.toml");
            return 2;
        }
    };
    let manifest = read_manifest(&root);
    let mut issues: Vec<AuditIssue> = Vec::new();
    let locked = match read_lock(&root) {
        Ok(Some(packages)) => packages,
        Ok(None) => {
            issues.push(AuditIssue {
                kind: "lock".to_string(),
                severity: "high".to_string(),
                package: String::new(),
                version: String::new(),
                id: "missing-lock".to_string(),
                title: "rip.lock is missing; run 'rip vendor' or 'rip lock'".to_string(),
                url: String::new(),
            });
            Vec::new()
        }
        Err(error) => {
            issues.push(AuditIssue {
                kind: "lock".to_string(),
                severity: "high".to_string(),
                package: String::new(),
                version: String::new(),
                id: "invalid-lock".to_string(),
                title: error,
                url: String::new(),
            });
            Vec::new()
        }
    };
    if !locked.is_empty() {
        if let Err(messages) = verify_lock(&root) {
            for message in messages {
                issues.push(AuditIssue {
                    kind: "lock".to_string(),
                    severity: "high".to_string(),
                    package: String::new(),
                    version: String::new(),
                    id: "lock-mismatch".to_string(),
                    title: message,
                    url: String::new(),
                });
            }
        }
    }

    // 安全公告：显式配置 > 环境变量 > 包源默认路径。
    let advisory_source = audit_source(&root, &manifest);
    if let Some(source) = &advisory_source {
        match load_advisories(source) {
            Ok(entries) => {
                for entry in entries {
                    let requirement = match semver::VersionReq::parse(&entry.requirements) {
                        Ok(requirement) => requirement,
                        Err(_) => continue,
                    };
                    for package in &locked {
                        let matches = semver::Version::parse(&package.version)
                            .map(|version| requirement.matches(&version))
                            .unwrap_or(false);
                        if package.name == entry.package && matches {
                            issues.push(AuditIssue {
                                kind: "advisory".to_string(),
                                severity: entry.severity.clone(),
                                package: package.name.clone(),
                                version: package.version.clone(),
                                id: entry.id.clone(),
                                title: entry.title.clone(),
                                url: entry.url.clone(),
                            });
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("warning: cannot load advisories from {}: {}", source, error);
            }
        }
    }

    // 许可证：直接读取 vendored 包的 rip.toml。
    let mut licenses: Vec<(String, String)> = Vec::new();
    for package in &locked {
        let dir = root.join(&package.source);
        let license = read_manifest(&dir)
            .license
            .unwrap_or_else(|| "unknown".to_string());
        licenses.push((package.name.clone(), license));
    }
    licenses.sort_by(|left, right| left.0.cmp(&right.0));

    let blocking: Vec<&AuditIssue> = issues
        .iter()
        .filter(|issue| {
            issue.kind != "advisory" || severity_rank(&issue.severity) >= deny_rank
        })
        .collect();

    if json {
        let items: Vec<serde_json::Value> = issues
            .iter()
            .map(|issue| {
                serde_json::json!({
                    "kind": issue.kind,
                    "severity": issue.severity,
                    "package": issue.package,
                    "version": issue.version,
                    "id": issue.id,
                    "title": issue.title,
                    "url": issue.url
                })
            })
            .collect();
        let license_items: Vec<serde_json::Value> = licenses
            .iter()
            .map(|(name, license)| serde_json::json!({ "package": name, "license": license }))
            .collect();
        let report = serde_json::json!({
            "packages": locked.len(),
            "issues": items,
            "licenses": license_items,
            "ok": blocking.is_empty()
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("audit: {} package(s)", locked.len());
        if issues.is_empty() {
            println!("  no known issues");
        }
        for issue in &issues {
            let target = if issue.package.is_empty() {
                String::new()
            } else {
                format!(" {} {}", issue.package, issue.version)
            };
            println!(
                "  {:<8} {}{}  {}  {}",
                issue.severity.to_ascii_uppercase(),
                issue.id,
                target,
                issue.title,
                issue.url
            );
        }
        if show_licenses {
            for (name, license) in &licenses {
                println!("  license  {:<28} {}", name, license);
            }
        }
        if blocking.is_empty() {
            println!("audit: ok (deny >= {})", deny);
        } else {
            println!(
                "audit: {} issue(s) at or above '{}'",
                blocking.len(),
                deny
            );
        }
    }
    if blocking.is_empty() {
        0
    } else {
        1
    }
}

struct AdvisoryEntry {
    id: String,
    package: String,
    requirements: String,
    severity: String,
    title: String,
    url: String,
}

/// 公告来源：`[registry] advisories` > `RIP_ADVISORIES` > 包源下的 `v1/advisories.json`。
///
/// 没有配置包源时跟随公共包源；离线模式或项目没有依赖时不做网络请求。
fn audit_source(root: &Path, manifest: &ManifestInfo) -> Option<String> {
    if let Some(value) = &manifest.registry_advisories {
        return Some(resolve_advisory_path(root, value));
    }
    if let Ok(value) = std::env::var("RIP_ADVISORIES") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(resolve_advisory_path(root, value));
        }
    }
    if let Some(value) = &manifest.registry {
        return Some(resolve_advisory_path(root, value));
    }
    if offline() || manifest.dependencies.is_empty() {
        return None;
    }
    if let Ok(value) = std::env::var(registry::REGISTRY_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(resolve_advisory_path(root, value));
        }
    }
    Some(resolve_advisory_path(root, registry::DEFAULT_REGISTRY))
}

fn resolve_advisory_path(root: &Path, source: &str) -> String {
    let source = source.trim().trim_end_matches('/');
    if source.starts_with("http://") || source.starts_with("https://") {
        if source.ends_with(".json") {
            return source.to_string();
        }
        return format!("{}/v1/advisories.json", source);
    }
    let path = if let Some(rest) = source.strip_prefix("file://") {
        PathBuf::from(rest)
    } else {
        root.join(source)
    };
    if path.extension().map(|ext| ext == "json").unwrap_or(false) {
        path.display().to_string()
    } else {
        path.join("v1").join("advisories.json").display().to_string()
    }
}

fn load_advisories(source: &str) -> Result<Vec<AdvisoryEntry>, String> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        let response = ureq::get(source)
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|error| format!("GET {} failed: {}", source, error))?;
        let mut bytes = Vec::new();
        use std::io::Read;
        response
            .into_reader()
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("advisories file is too large".to_string());
        }
        bytes
    } else {
        fs::read(source).map_err(|error| error.to_string())?
    };
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid advisories JSON: {}", error))?;
    let list = match &value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => map
            .get("advisories")
            .and_then(|items| items.as_array())
            .ok_or("expected an array or an object with an 'advisories' array")?,
        _ => return Err("expected an array or an object with an 'advisories' array".to_string()),
    };
    let mut entries = Vec::new();
    for item in list {
        let text = |key: &str| {
            item.get(key)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string()
        };
        entries.push(AdvisoryEntry {
            id: text("id"),
            package: text("package"),
            requirements: text("requirements"),
            severity: {
                let value = text("severity");
                if value.is_empty() {
                    "moderate".to_string()
                } else {
                    value
                }
            },
            title: text("title"),
            url: text("url"),
        });
    }
    Ok(entries)
}

/// `rip pub`：把当前目录（或指定包目录）发布到包源。
fn cmd_pub(args: &[String]) -> i32 {
    let mut target: Option<String> = None;
    let mut registry_source: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--registry" {
            registry_source = args.get(index + 1).cloned();
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--registry=") {
            registry_source = Some(value.to_string());
            index += 1;
            continue;
        }
        if target.is_none() {
            target = Some(arg.to_string());
        }
        index += 1;
    }
    let dir = PathBuf::from(target.unwrap_or_else(|| ".".to_string()));
    match registry::publish(&dir, registry_source.as_deref()) {
        Ok((name, version, source)) => {
            println!("published {} {} to {}", name, version, source);
            0
        }
        Err(message) => {
            eprintln!("{}", message);
            1
        }
    }
}

/// `rip add` 未指定约束时的建议值：本地已有包则取兼容约束，否则 `*`。
fn suggested_requirement(package: &str) -> String {
    let Some(packages_root) = packages_root_of(Path::new(".")) else {
        return "*".to_string();
    };
    let dir = vendored_package_dir(&packages_root, package);
    let (version, _) = package_manifest(&dir);
    if !version.is_empty() && semver::Version::parse(&version).is_ok() {
        return format!("^{}", version);
    }
    "*".to_string()
}

fn add_manifest_entry(section: &str, key: &str, version: &str) -> i32 {
    let path = Path::new("rip.toml");
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            let name = std::env::current_dir()
                .ok()
                .and_then(|dir| dir.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "app".to_string());
            format!(
                "[module]\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n",
                name
            )
        }
    };

    let quoted = serde_quote(key);
    let entry = format!("{} = \"{}\"", quoted, version);
    let header = format!("[{}]", section);
    let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();

    let section_start = lines.iter().position(|line| line.trim() == header);
    match section_start {
        Some(start) => {
            let end = lines
                .iter()
                .enumerate()
                .skip(start + 1)
                .find(|(_, line)| line.trim().starts_with('['))
                .map(|(index, _)| index)
                .unwrap_or(lines.len());
            let existing = lines[start + 1..end].iter().position(|line| {
                line.split_once('=')
                    .map(|(name, _)| name.trim().trim_matches('"') == key)
                    .unwrap_or(false)
            });
            match existing {
                Some(offset) => {
                    let index = start + 1 + offset;
                    if lines[index].trim() == entry {
                        println!("{} is already listed in rip.toml", key);
                        return 0;
                    }
                    lines[index] = entry.clone();
                }
                None => {
                    lines.insert(end, entry.clone());
                    // 后面紧跟小节标题时补一个空行，保持文件可读。
                    if end + 1 < lines.len() && lines[end + 1].trim_start().starts_with('[') {
                        lines.insert(end + 1, String::new());
                    }
                }
            }
        }
        None => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(entry.clone());
        }
    }

    let mut output = lines.join("\n");
    output.push('\n');
    match fs::write(path, output) {
        Ok(()) => {
            println!("added {} to {}", entry, path.display());
            0
        }
        Err(error) => {
            eprintln!("error: cannot write {}: {}", path.display(), error);
            1
        }
    }
}

fn serde_quote(key: &str) -> String {
    if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_string()
    } else {
        format!("\"{}\"", key)
    }
}

fn cmd_test(args: &[String]) -> i32 {
    let mut filter: Option<String> = None;
    let mut target: Option<String> = None;
    let mut diff = false;
    let mut jobs: Option<usize> = None;
    let mut timeout: Option<Duration> = Some(Duration::from_millis(30_000));
    let mut isolate = true;
    let mut coverage = false;
    let mut coverage_min: Option<f64> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        let mut take_value = |required: &str| -> Result<String, String> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", required))
        };
        match flag {
            "--filter" => match take_value("--filter") {
                Ok(value) => filter = Some(value),
                Err(message) => {
                    eprintln!("error: {}", message);
                    return 2;
                }
            },
            "--jobs" | "-j" => match take_value("--jobs").and_then(|value| {
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|count| *count > 0)
                    .ok_or_else(|| format!("--jobs expects a positive integer, found '{}'", value))
            }) {
                Ok(count) => jobs = Some(count),
                Err(message) => {
                    eprintln!("error: {}", message);
                    return 2;
                }
            },
            "--timeout" => match take_value("--timeout").and_then(|value| {
                let millis = value
                    .parse::<u64>()
                    .map_err(|_| format!("--timeout expects milliseconds, found '{}'", value))?;
                Ok(if millis == 0 {
                    None
                } else {
                    Some(Duration::from_millis(millis))
                })
            }) {
                Ok(value) => timeout = value,
                Err(message) => {
                    eprintln!("error: {}", message);
                    return 2;
                }
            },
            "--no-isolate" => isolate = false,
            "--diff" => diff = true,
            "--coverage" => coverage = true,
            "--coverage-min" => match take_value("--coverage-min").and_then(|value| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|percent| (0.0..=100.0).contains(percent))
                    .ok_or_else(|| format!("--coverage-min expects 0..=100, found '{}'", value))
            }) {
                Ok(value) => {
                    coverage = true;
                    coverage_min = Some(value);
                }
                Err(message) => {
                    eprintln!("error: {}", message);
                    return 2;
                }
            },
            other if other.starts_with('-') => {
                eprintln!("error: unknown test option '{}'", other);
                return 2;
            }
            _ => {
                if target.is_none() {
                    target = Some(arg.to_string());
                }
            }
        }
        index += 1;
    }

    let default_target = if Path::new("rip.toml").is_file() {
        "."
    } else {
        "tests"
    };
    let root = PathBuf::from(target.unwrap_or_else(|| default_target.to_string()));
    if !root.exists() {
        eprintln!("error: test target '{}' does not exist", root.display());
        return 2;
    }
    if diff {
        return cmd_diff(&root);
    }
    if !isolate && jobs.unwrap_or(1) > 1 {
        eprintln!("error: --no-isolate cannot be combined with more than one job");
        return 2;
    }

    let mut inputs: Vec<(PathBuf, bool)> = Vec::new();
    if root.is_file() {
        inputs.push((root.clone(), true));
    } else if root.join("rip.toml").is_file() {
        // 项目模式：合并项目源码与 packages 后统一运行 #[test]。
        // 项目里没有 #[test] 时不执行 main（main 可能是一个常驻服务）。
        let entry = match resolve_entry(&root) {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("error: {}", error);
                return 2;
            }
        };
        inputs.push((entry, false));
    } else {
        let mut files = Vec::new();
        collect_rp_files(&root, &mut files);
        files.sort();
        // 目录扫描只做编译检查 + 执行 #[test]，不执行 main：
        // 示例目录里的 main 可能是常驻 HTTP 服务，执行会挂住 `rip test`。
        inputs.extend(files.into_iter().map(|file| (file, false)));
    }

    let workers = if isolate {
        match jobs {
            Some(count) => count,
            None => std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1),
        }
    } else {
        1
    };
    let options = TestOptions {
        filter,
        jobs: workers.max(1),
        timeout,
        isolate,
    };
    if coverage {
        // 必须在构建 RuntimeSeed 之前开启：字节码编译器据此决定是否插桩。
        rp_runtime::coverage::clear();
        rp_runtime::coverage::enable();
    }
    let mut declared: Vec<(String, Vec<u32>)> = Vec::new();
    let (passed, failed) = run_planned_tests(
        &inputs,
        &options,
        if coverage { Some(&mut declared) } else { None },
    );

    println!();
    println!("{} passed, {} failed", passed, failed);
    let mut code = if failed > 0 { 1 } else { 0 };
    if coverage {
        if report_coverage(&declared, coverage_min) != 0 {
            code = 1;
        }
    }
    code
}

/// 打印语句覆盖率报告；低于 `--coverage-min` 时返回 1。
fn report_coverage(declared: &[(String, Vec<u32>)], min: Option<f64>) -> i32 {
    let report = rp_runtime::coverage::report(declared.iter().cloned());
    println!();
    println!(
        "coverage: {:.1}% ({} of {} executable lines)",
        report.percent(),
        report.covered,
        report.total
    );
    for function in &report.functions {
        if function.missing.is_empty() {
            continue;
        }
        println!(
            "  {}: {}/{} lines, missed {:?}",
            function.name, function.covered, function.total, function.missing
        );
    }
    match min {
        Some(min) if report.percent() + f64::EPSILON < min => {
            eprintln!(
                "error: coverage {:.1}% is below --coverage-min {:.1}%",
                report.percent(),
                min
            );
            1
        }
        _ => 0,
    }
}

/// 收集函数体内所有可执行语句的行号（含嵌套块与 match 分支）。
fn collect_stmt_lines(stmts: &[rp_ast::Stmt], out: &mut Vec<u32>) {
    for stmt in stmts {
        out.push(stmt.line);
        match &stmt.kind {
            rp_ast::StmtKind::If {
                then_body,
                else_body,
                ..
            } => {
                collect_stmt_lines(then_body, out);
                if let Some(body) = else_body {
                    collect_stmt_lines(body, out);
                }
            }
            rp_ast::StmtKind::Loop { body }
            | rp_ast::StmtKind::While { body, .. }
            | rp_ast::StmtKind::WhileLet { body, .. }
            | rp_ast::StmtKind::ForIn { body, .. } => collect_stmt_lines(body, out),
            rp_ast::StmtKind::Match { arms, .. } => {
                for arm in arms {
                    collect_stmt_lines(&arm.body, out);
                }
            }
            rp_ast::StmtKind::Parallel { branches } => collect_stmt_lines(branches, out),
            _ => {}
        }
    }
}

/// 测试执行选项。
struct TestOptions {
    filter: Option<String>,
    /// 并发线程数；1 表示串行。
    jobs: usize,
    /// 单用例超时；`None` 表示不限制。
    timeout: Option<Duration>,
    /// 每个用例使用全新运行时（缺省）；否则同一文件共用一个运行时。
    isolate: bool,
}

struct TestCase {
    name: String,
    line: u32,
}

/// 一个文件（或项目）里的测试批次：共享 AST，按需派生运行时。
struct TestBatch {
    display: String,
    seed: rp_runtime::RuntimeSeed,
    cases: Vec<TestCase>,
    /// 没有 `#[test]` 但需要执行 `main` 的单文件模式。
    run_main: bool,
}

enum Planned {
    /// 不需要运行的状态行（编译通过、无测试函数等）。
    Message(String),
    /// 编译失败；诊断已由 `prepare` 打印。
    PrepareFailed(String),
    Batch(TestBatch),
}

struct TestJob {
    batch: usize,
    /// `run_main` 批次忽略该字段。
    case: usize,
}

enum JobResult {
    Ok,
    Failed {
        error: Option<rp_runtime::RuntimeError>,
        timed_out: bool,
    },
}

/// 准备全部目标，然后按 `--jobs` 并发执行，最后按声明顺序输出结果。
///
/// `coverage` 非空时顺带收集每个函数体内可执行语句的行号，供覆盖率报告使用。
fn run_planned_tests(
    inputs: &[(PathBuf, bool)],
    options: &TestOptions,
    mut coverage: Option<&mut Vec<(String, Vec<u32>)>>,
) -> (usize, usize) {
    let mut planned = Vec::with_capacity(inputs.len());
    for (path, allow_main) in inputs {
        let prepared = match prepare(path) {
            Some(prepared) => prepared,
            None => {
                planned.push(Planned::PrepareFailed(path.display().to_string()));
                continue;
            }
        };
        if let Some(target) = coverage.as_deref_mut() {
            for item in &prepared.program.items {
                if let Item::Func(func) = item {
                    let mut lines = Vec::new();
                    collect_stmt_lines(&func.body, &mut lines);
                    target.push((func.name.clone(), lines));
                }
            }
        }
        let mut cases = Vec::new();
        for item in &prepared.program.items {
            if let Item::Func(func) = item {
                if !func.attributes.iter().any(|attribute| attribute.name == "test") {
                    continue;
                }
                if let Some(filter) = &options.filter {
                    if !func.name.contains(filter) {
                        continue;
                    }
                }
                cases.push(TestCase {
                    name: func.name.clone(),
                    line: func.line,
                });
            }
        }
        let display = path.display().to_string();
        let run_main = cases.is_empty()
            && *allow_main
            && prepared.program.func("main").is_some()
            && options.filter.is_none();
        if cases.is_empty() && !run_main {
            planned.push(Planned::Message(if *allow_main {
                format!("test {} ... ok (compile)", display)
            } else {
                format!("{}: no #[test] functions", display)
            }));
            continue;
        }
        planned.push(Planned::Batch(TestBatch {
            display,
            seed: rp_runtime::RuntimeSeed::from_program(prepared.program, prepared.checked),
            cases,
            run_main,
        }));
    }

    let mut jobs = Vec::new();
    let mut ranges = Vec::with_capacity(planned.len());
    for (index, plan) in planned.iter().enumerate() {
        let start = jobs.len();
        if let Planned::Batch(batch) = plan {
            if batch.run_main {
                jobs.push(TestJob { batch: index, case: 0 });
            } else {
                for case in 0..batch.cases.len() {
                    jobs.push(TestJob { batch: index, case });
                }
            }
        }
        ranges.push((start, jobs.len()));
    }

    let results = execute_jobs(&planned, &jobs, options);

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (index, plan) in planned.iter().enumerate() {
        match plan {
            // 编译检查通过（含没有 #[test] 的文件）按旧行为记一次通过，保持输出与统计不变。
            Planned::Message(text) => {
                println!("{}", text);
                passed += 1;
            }
            Planned::PrepareFailed(display) => {
                println!("test {} ... FAIL", display);
                failed += 1;
            }
            Planned::Batch(batch) => {
                let (start, end) = ranges[index];
                for job_index in start..end {
                    print_job_result(
                        batch,
                        &jobs[job_index],
                        &results[job_index],
                        options.timeout,
                        &mut passed,
                        &mut failed,
                    );
                }
            }
        }
    }
    (passed, failed)
}

/// 执行任务列表：`--jobs 1` 走串行（`--no-isolate` 也走这里），否则用作用域线程。
fn execute_jobs(planned: &[Planned], jobs: &[TestJob], options: &TestOptions) -> Vec<JobResult> {
    if jobs.is_empty() {
        return Vec::new();
    }
    if options.jobs <= 1 || !options.isolate {
        let mut shared: HashMap<usize, Runtime> = HashMap::new();
        return jobs
            .iter()
            .map(|job| {
                let batch = match &planned[job.batch] {
                    Planned::Batch(batch) => batch,
                    _ => unreachable!("job references a non-batch plan"),
                };
                if options.isolate {
                    let mut runtime = batch.seed.build();
                    run_one(&mut runtime, batch, job, options.timeout)
                } else {
                    let runtime = shared
                        .entry(job.batch)
                        .or_insert_with(|| batch.seed.build());
                    run_one(runtime, batch, job, options.timeout)
                }
            })
            .collect();
    }

    let workers = options.jobs.min(jobs.len()).max(1);
    let cursor = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<JobResult>>> =
        Mutex::new((0..jobs.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = cursor.fetch_add(1, Ordering::Relaxed);
                if index >= jobs.len() {
                    break;
                }
                let job = &jobs[index];
                let batch = match &planned[job.batch] {
                    Planned::Batch(batch) => batch,
                    _ => break,
                };
                // 每个用例独立运行时：模块级 const/static 与全局变量互不串扰。
                let mut runtime = batch.seed.build();
                let result = run_one(&mut runtime, batch, job, options.timeout);
                if let Ok(mut slots) = slots.lock() {
                    slots[index] = Some(result);
                }
            });
        }
    });
    slots
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .into_iter()
        .map(|slot| match slot {
            Some(result) => result,
            None => JobResult::Failed {
                error: Some(rp_runtime::RuntimeError::new(
                    "internal: test job produced no result",
                    0,
                )),
                timed_out: false,
            },
        })
        .collect()
}

fn run_one(
    runtime: &mut Runtime,
    batch: &TestBatch,
    job: &TestJob,
    timeout: Option<Duration>,
) -> JobResult {
    if batch.run_main {
        if let Err(error) = runtime.init_entry() {
            return JobResult::Failed {
                error: Some(error),
                timed_out: false,
            };
        }
        return match runtime.run_main() {
            Ok(_) => JobResult::Ok,
            Err(error) => JobResult::Failed {
                error: Some(error),
                timed_out: false,
            },
        };
    }
    let case = &batch.cases[job.case];
    let outcome = runtime.run_test(&case.name, timeout);
    if outcome.result.is_ok() {
        JobResult::Ok
    } else {
        JobResult::Failed {
            error: outcome.result.err(),
            timed_out: outcome.timed_out,
        }
    }
}

fn print_job_result(
    batch: &TestBatch,
    job: &TestJob,
    result: &JobResult,
    timeout: Option<Duration>,
    passed: &mut usize,
    failed: &mut usize,
) {
    let label = if batch.run_main {
        batch.display.clone()
    } else {
        format!("{}.{}", batch.display, batch.cases[job.case].name)
    };
    match result {
        JobResult::Ok => {
            *passed += 1;
            println!("test {} ... ok", label);
        }
        JobResult::Failed { error, timed_out } => {
            *failed += 1;
            println!("test {} ... FAIL", label);
            if *timed_out {
                let millis = timeout.map(|limit| limit.as_millis()).unwrap_or(0);
                eprintln!(
                    "  {}: runtime error: test exceeded the timeout ({} ms)",
                    batch.display, millis
                );
            } else if let Some(error) = error {
                let fallback = if batch.run_main {
                    0
                } else {
                    batch.cases[job.case].line
                };
                let line = if error.line == 0 { fallback } else { error.line };
                eprintln!(
                    "  {}:{}: runtime error: {}",
                    batch.display, line, error.message
                );
            }
            if let Some(error) = error {
                for frame in error.stack_lines() {
                    eprintln!("{}", frame);
                }
            }
        }
    }
}

/// `rip test --diff`：逐个文件比较解释器与 AOT 产物的运行结果。
///
/// 这是三种执行方式语义一致性的回归闸门：任何一条路径漂移都会在这里暴露。
/// 常驻服务（`http.serve` 等）不会在一次执行内结束，直接跳过。
fn cmd_diff(root: &Path) -> i32 {
    let mut files = Vec::new();
    if root.is_file() {
        files.push(root.to_path_buf());
    } else {
        collect_rp_files(root, &mut files);
        files.sort();
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("error: cannot locate rip executable: {}", error);
            return 2;
        }
    };
    // 临时目录名带纳秒后缀避免可预测竞态；若已存在同名符号链接则直接拒绝，
    // 防止在多用户机器上被预置链接把删除/写入引向别处（CWE-377）。
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!("rip-diff-{}-{}", process::id(), nonce));
    match work.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            eprintln!("error: refusing to use symlinked temp dir {}", work.display());
            return 2;
        }
        Ok(_) => {
            let _ = fs::remove_dir_all(&work);
        }
        Err(_) => {}
    }
    if let Err(error) = fs::create_dir_all(&work) {
        eprintln!("error: cannot create {}: {}", work.display(), error);
        return 2;
    }

    let mut compared = 0usize;
    let mut skipped = 0usize;
    let mut mismatched = 0usize;
    for file in &files {
        let display = file.display().to_string();
        // 包源码没有 main，不参与端到端对比。
        if display.contains("packages") {
            continue;
        }
        let source = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(_) => continue,
        };
        if source.contains("http.serve")
            || source.contains("ws.serve")
            || source.contains("net.listen")
            || source.contains("task.spawn")
        {
            println!("skip  {}", display);
            skipped += 1;
            continue;
        }
        let name = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".to_string());
        let dist = work.join(&name);
        let interpreted = match Command::new(&exe).arg("run").arg(file).output() {
            Ok(output) => output,
            Err(error) => {
                println!("fail  {} (cannot run: {})", file.display(), error);
                mismatched += 1;
                continue;
            }
        };
        match Command::new(&exe)
            .arg("build")
            .arg(file)
            .arg("--aot")
            .arg("--no-archive")
            .arg("--name")
            .arg(&name)
            .arg("-o")
            .arg(&dist)
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stderr);
                println!(
                    "fail  {} (aot build failed: {})",
                    file.display(),
                    text.lines().next().unwrap_or("").trim()
                );
                mismatched += 1;
                continue;
            }
            Err(error) => {
                println!("fail  {} (cannot build: {})", file.display(), error);
                mismatched += 1;
                continue;
            }
        }
        let exe_name = platform_exe(&name);
        let native = match Command::new(dist.join(&exe_name)).output() {
            Ok(output) => output,
            Err(error) => {
                println!("fail  {} (cannot run aot binary: {})", file.display(), error);
                mismatched += 1;
                continue;
            }
        };
        compared += 1;
        let interpreted_out = String::from_utf8_lossy(&interpreted.stdout).to_string();
        let native_out = String::from_utf8_lossy(&native.stdout).to_string();
        // 解释器的错误行带 `文件:行号:` 前缀，AOT 不带；这只影响展示，不影响语义。
        let interpreted_err = strip_locations(&String::from_utf8_lossy(&interpreted.stderr));
        let native_err = strip_locations(&String::from_utf8_lossy(&native.stderr));
        let same_status = interpreted.status.code() == native.status.code();
        if same_status && interpreted_out == native_out && interpreted_err == native_err {
            println!("ok    {}", file.display());
        } else {
            mismatched += 1;
            println!("diff  {}", file.display());
            if interpreted_out != native_out {
                report_diff("stdout", &interpreted_out, &native_out);
            }
            if interpreted_err != native_err {
                report_diff("stderr", &interpreted_err, &native_err);
            }
            if !same_status {
                println!(
                    "       exit: interpreted={:?} aot={:?}",
                    interpreted.status.code(),
                    native.status.code()
                );
            }
        }
    }
    let _ = fs::remove_dir_all(&work);
    println!();
    println!(
        "{} compared, {} skipped, {} mismatched",
        compared, skipped, mismatched
    );
    if mismatched > 0 {
        1
    } else {
        0
    }
}

/// 去掉解释器错误行首的 `路径/文件.rp:行号: `，只保留消息本体。
/// 同时丢弃编译期告警（`warning:`）：AOT 由 rustc 编译，不跑 Rip 的告警通道。
fn strip_locations(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        // 告警行可能带 ANSI 颜色，用 contains 判断。
        if line.contains("warning:") {
            continue;
        }
        let mut parts = line.splitn(3, ':');
        let first = parts.next().unwrap_or("");
        let second = parts.next().unwrap_or("");
        let rest = parts.next();
        let is_location = first.ends_with(".rp")
            && !second.is_empty()
            && second.chars().all(|ch| ch.is_ascii_digit());
        match (is_location, rest) {
            (true, Some(rest)) => out.push_str(rest.trim_start()),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

fn report_diff(label: &str, interpreted: &str, native: &str) {
    println!("       --- {} (interpreted) ---", label);
    for line in interpreted.lines().take(8) {
        println!("       {}", line);
    }
    println!("       --- {} (aot) ---", label);
    for line in native.lines().take(8) {
        println!("       {}", line);
    }
}

// ---------- 项目模式：一个入口 + 多目录多文件 ----------

/// 合并后的源码片段：记录该文件在合并源码中的起始行，用于把诊断行号映射回原文件。
struct ProjectUnit {
    display: String,
    start: u32,
    package: String,
}

struct Prepared {
    program: Program,
    checked: CheckResult,
    units: Vec<ProjectUnit>,
}

fn project_root(entry: &Path) -> Option<PathBuf> {
    let parent = entry.parent()?;
    let dir = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    };
    if dir.join("rip.toml").is_file() {
        Some(dir)
    } else {
        None
    }
}

/// 工具链自身产物与测试目录不参与编译；`storage`、`uploads` 等运行期目录
/// 不再内置排除，由 `[compile] exclude` 按项目声明。
fn skip_project_dir(name: &str) -> bool {
    matches!(
        name,
        // packages/（或 package/）是第三方包目录，由专门逻辑收集，不按项目源码处理。
        "packages" | "package" | "tests" | "build" | "dist" | "target" | "node_modules" | ".git"
    )
}

/// 目录是否命中 `[compile] exclude`：支持目录名（如 `storage`）或相对项目根的路径
/// （如 `data/db`），分隔符统一按 `/` 比较。
fn excluded_project_dir(root: &Path, path: &Path, excludes: &[String]) -> bool {
    let relative = path
        .strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    excludes.iter().any(|entry| {
        let entry = entry.trim().trim_matches('/').replace('\\', "/");
        if entry.is_empty() {
            return false;
        }
        entry == relative || entry == name
    })
}

fn collect_project_files(dir: &Path, root: &Path, excludes: &[String], out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if skip_project_dir(name)
                || name.starts_with('.')
                || excluded_project_dir(root, &path, excludes)
            {
                continue;
            }
            collect_project_files(&path, root, excludes, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rp") {
            out.push(path);
        }
    }
}

/// 包的编译产物后缀：源码包为 `.rp`，预编译包为 `.rpa`。
fn package_file_kind(path: &Path) -> Option<&'static str> {
    let extension = path.extension().and_then(|value| value.to_str())?;
    if extension == "rp" {
        Some("rp")
    } else if extension == "rpa" {
        Some("rpa")
    } else {
        None
    }
}

/// 读取预编译包头部的语言版本；没有头部视为未知版本。
fn precompiled_version(bytes: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    head.lines()
        .find_map(|line| line.strip_prefix("rip-version: "))
        .map(|value| value.trim().to_string())
}

/// 第三方包目录：规范写作 `packages/`，同时接受 `package/`（取先存在的）。
fn packages_root_of(root: &Path) -> Option<PathBuf> {
    for name in ["packages", "package"] {
        let candidate = root.join(name);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// 收集 `packages/` 下的第三方包：任意深度，源码 `.rp` 与预编译 `.rpa` 均可。
fn collect_package_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_package_files(&path, out);
        } else if package_file_kind(&path).is_some() {
            out.push(path);
        }
    }
}

/// 包名 = 包目录相对 `packages/` 的路径，与 `import "包路径"` 一致。
///
/// `packages/acme/tools/tools.rp` → `acme/tools`；同目录下的多个 `.rp`
/// 属于同一个包，不会因为文件名不同而产生不同包名。
fn package_name_of(dir: &Path, packages: &Path) -> String {
    let relative = dir.strip_prefix(packages).unwrap_or(dir);
    let name = relative.to_string_lossy().replace('\\', "/");
    if name.is_empty() {
        "package".to_string()
    } else {
        name
    }
}

/// 包之间通过 `use 路径::模块;` 形成的依赖环（仅告警，不阻断编译）。
fn package_cycles(packages: &[(String, String, String)]) -> Vec<String> {
    if packages.len() < 2 {
        return Vec::new();
    }
    let names: Vec<&str> = packages.iter().map(|(_, name, _)| name.as_str()).collect();
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for (_, name, source) in packages {
        let mut deps: Vec<&str> = Vec::new();
        for line in source.lines() {
            let rest = match line.trim().strip_prefix("use ") {
                Some(rest) => rest.trim(),
                None => continue,
            };
            let rest = rest.trim_end_matches(';').trim();
            let rest = match rest.split_once(" as ") {
                Some((path, _)) => path.trim(),
                None => rest,
            };
            let path = rest.replace("::", "/");
            if let Some(dep) = names
                .iter()
                .copied()
                .find(|candidate| *candidate == path || candidate.ends_with(&format!("/{}", path)))
            {
                deps.push(dep);
            }
        }
        graph.insert(name.as_str(), deps);
    }

    fn walk<'a>(
        node: &'a str,
        graph: &HashMap<&'a str, Vec<&'a str>>,
        stack: &mut Vec<&'a str>,
        done: &mut HashSet<&'a str>,
        reported: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if let Some(index) = stack.iter().position(|item| *item == node) {
            let mut cycle: Vec<&str> = stack[index..].to_vec();
            // 以最小包名开头，避免同一个环重复报告。
            if let Some((min, _)) = cycle.iter().enumerate().min_by_key(|(_, name)| **name) {
                cycle.rotate_left(min);
            }
            let key = cycle.join(" -> ");
            if reported.insert(key.clone()) {
                out.push(format!("warning: package cycle: {} -> {}", key, cycle[0]));
            }
            return;
        }
        if done.contains(node) {
            return;
        }
        stack.push(node);
        if let Some(deps) = graph.get(node) {
            for dep in deps {
                walk(*dep, graph, stack, done, reported, out);
            }
        }
        stack.pop();
        done.insert(node);
    }

    let mut out = Vec::new();
    let mut done = HashSet::new();
    let mut reported = HashSet::new();
    let mut stack = Vec::new();
    let mut sorted = names.clone();
    sorted.sort();
    for name in sorted {
        walk(name, &graph, &mut stack, &mut done, &mut reported, &mut out);
    }
    out
}

/// `rip.lock` 中的一条包记录。
struct LockedPackage {
    name: String,
    version: String,
    source: String,
    deps: Vec<String>,
    sha256: String,
}

fn lock_file(root: &Path) -> PathBuf {
    root.join("rip.lock")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// 包目录里的 rip.toml：取版本号与直接依赖（依赖名 → 版本约束）。
fn package_manifest(dir: &Path) -> (String, Vec<(String, String)>) {
    let text = match fs::read_to_string(dir.join("rip.toml")) {
        Ok(text) => text,
        Err(_) => return (String::new(), Vec::new()),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut version = String::new();
    let mut deps = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        let key = key.trim().trim_matches('"');
        let value = strip_toml_comment(value).trim();
        match section.as_str() {
            "module" if key == "version" => version = value.trim_matches('"').to_string(),
            "dependencies" => deps.push((
                key.to_string(),
                value.trim_matches('"').trim().to_string(),
            )),
            _ => {}
        }
    }
    (version, deps)
}

/// 当前 `packages/` 下实际存在的包（按包名排序），作为 rip.lock 的数据源。
///
/// 以包目录为单位聚合：同一目录下的多个 `.rp` 属于同一个包，
/// 摘要覆盖目录内全部文件（含相对路径），保证多文件包的锁校验稳定。
fn current_packages(root: &Path) -> Vec<LockedPackage> {
    let packages_root = match packages_root_of(root) {
        Some(packages) => packages,
        None => return Vec::new(),
    };
    let mut files = Vec::new();
    collect_package_files(&packages_root, &mut files);
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let dir = file.parent().unwrap_or(&packages_root).to_path_buf();
        groups.entry(dir).or_default().push(file);
    }
    let mut packages = Vec::new();
    for (dir, mut files) in groups {
        files.sort();
        let (version, deps) = package_manifest(&dir);
        let mut payload: Vec<u8> = Vec::new();
        for file in &files {
            let relative = file
                .strip_prefix(&packages_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            payload.extend_from_slice(relative.as_bytes());
            payload.push(0);
            if let Ok(bytes) = fs::read(file) {
                payload.extend_from_slice(&bytes);
            }
            payload.push(0);
        }
        let source = dir
            .strip_prefix(root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .replace('\\', "/");
        packages.push(LockedPackage {
            name: package_name_of(&dir, &packages_root),
            version,
            source,
            deps: deps.into_iter().map(|(name, _)| name).collect(),
            sha256: sha256_hex(&payload),
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages
}

/// 解析后的一条依赖：包名与实际版本。
#[derive(Debug, Clone)]
struct ResolvedPackage {
    name: String,
    version: String,
}

/// 按 `packages/包路径` 定位本地 vendored 包目录。
fn vendored_package_dir(packages_root: &Path, name: &str) -> PathBuf {
    let mut dir = packages_root.to_path_buf();
    for segment in name.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            continue;
        }
        dir.push(segment);
    }
    dir
}

struct Resolver<'a> {
    packages_root: &'a Path,
    errors: Vec<String>,
    done: HashMap<String, ResolvedPackage>,
}

impl Resolver<'_> {
    /// 校验一个依赖及其传递依赖；`from` 是声明该依赖的来源（项目或包名），用于报错。
    fn visit(
        &mut self,
        name: &str,
        requirement_text: &str,
        from: &str,
        stack: &mut Vec<String>,
    ) -> Option<ResolvedPackage> {
        let requirement = match semver::VersionReq::parse(requirement_text) {
            Ok(requirement) => requirement,
            Err(error) => {
                self.errors.push(format!(
                    "error: {}: invalid version requirement '{}' for '{}': {}",
                    from, requirement_text, name, error
                ));
                return None;
            }
        };
        if let Some(existing) = self.done.get(name) {
            // 同一个包被多处依赖时，已解析的版本必须同时满足所有约束。
            if let Ok(version) = semver::Version::parse(&existing.version) {
                if !requirement.matches(&version) {
                    self.errors.push(format!(
                        "error: {} requires '{} {}', but the resolved version is {}; a build uses one version per package",
                        from, name, requirement_text, existing.version
                    ));
                }
            }
            return Some(existing.clone());
        }
        // 环：包间循环依赖由 package_cycles 单独告警，这里只防止无限递归。
        if stack.iter().any(|item| item == name) {
            return None;
        }

        let dir = vendored_package_dir(self.packages_root, name);
        if !dir.is_dir() {
            self.errors.push(format!(
                "error: {} depends on '{}', but it is not vendored under '{}/'; run 'rip add {}' or place the package there",
                from,
                name,
                self.packages_root.display(),
                name
            ));
            return None;
        }
        let (version, deps) = package_manifest(&dir);
        if version.is_empty() {
            // 没有版本声明的包只在无约束依赖下可用，保持对旧包的兼容。
            if !requirement.is_any() {
                self.errors.push(format!(
                    "error: {} requires '{} {}', but the package has no [module] version in its rip.toml",
                    from, name, requirement_text
                ));
                return None;
            }
        } else {
            match semver::Version::parse(&version) {
                Ok(parsed) => {
                    if !requirement.matches(&parsed) {
                        self.errors.push(format!(
                            "error: {} requires '{} {}', but the vendored version is {}; update the package or the requirement",
                            from, name, requirement_text, version
                        ));
                        return None;
                    }
                }
                Err(error) => {
                    self.errors.push(format!(
                        "error: package '{}' has an invalid [module] version '{}': {}",
                        name, version, error
                    ));
                    return None;
                }
            }
        }

        let resolved = ResolvedPackage {
            name: name.to_string(),
            version,
        };
        self.done.insert(name.to_string(), resolved.clone());

        stack.push(name.to_string());
        for (dep_name, dep_requirement) in deps {
            self.visit(&dep_name, &dep_requirement, name, stack);
        }
        stack.pop();
        Some(resolved)
    }
}

/// 校验 `[dependencies]`：包必须已 vendored、版本满足约束、传递依赖完整且无版本冲突。
///
/// 没有任何依赖声明时返回空列表，旧项目行为不变。
fn resolve_dependencies(root: &Path) -> Result<Vec<ResolvedPackage>, Vec<String>> {
    let manifest = read_manifest(root);
    if manifest.dependencies.is_empty() {
        return Ok(Vec::new());
    }
    let packages_root = match packages_root_of(root) {
        Some(packages_root) => packages_root,
        None => {
            return Err(vec![format!(
                "error: [dependencies] is declared but no package directory exists; create '{}' and vendor the packages",
                root.join("packages").display()
            )])
        }
    };

    let mut resolver = Resolver {
        packages_root: &packages_root,
        errors: Vec::new(),
        done: HashMap::new(),
    };
    let mut stack = Vec::new();
    for (name, requirement) in &manifest.dependencies {
        resolver.visit(name, requirement, "rip.toml", &mut stack);
    }
    if !resolver.errors.is_empty() {
        return Err(resolver.errors);
    }
    let mut packages: Vec<ResolvedPackage> = resolver.done.into_values().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

fn read_lock(root: &Path) -> Result<Option<Vec<LockedPackage>>, String> {
    let text = match fs::read_to_string(lock_file(root)) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    let mut packages: Vec<LockedPackage> = Vec::new();
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[package]]" {
            packages.push(LockedPackage {
                name: String::new(),
                version: String::new(),
                source: String::new(),
                deps: Vec::new(),
                sha256: String::new(),
            });
            in_package = true;
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        if !in_package {
            continue;
        }
        let value = strip_toml_comment(value).trim();
        let package = match packages.last_mut() {
            Some(package) => package,
            None => continue,
        };
        match key.trim() {
            "name" => package.name = value.trim_matches('"').to_string(),
            "version" => package.version = value.trim_matches('"').to_string(),
            "source" => package.source = value.trim_matches('"').to_string(),
            "sha256" => package.sha256 = value.trim_matches('"').to_string(),
            "deps" => package.deps = parse_string_array(value),
            _ => {}
        }
    }
    Ok(Some(packages))
}

fn write_lock(root: &Path) -> Result<usize, String> {
    let packages = current_packages(root);
    let manifest = read_manifest(root);
    let mut text = String::from("# generated by 'rip lock'; do not edit\nversion = 1\n");
    if let Some(source) = &manifest.registry {
        text.push_str(&format!("registry = \"{}\"\n", source));
    }
    for package in &packages {
        let deps: Vec<String> = package
            .deps
            .iter()
            .map(|dep| format!("\"{}\"", dep))
            .collect();
        text.push_str("\n[[package]]\n");
        text.push_str(&format!("name = \"{}\"\n", package.name));
        text.push_str(&format!("version = \"{}\"\n", package.version));
        text.push_str(&format!("source = \"{}\"\n", package.source));
        text.push_str(&format!("sha256 = \"{}\"\n", package.sha256));
        text.push_str(&format!("deps = [{}]\n", deps.join(", ")));
    }
    fs::write(lock_file(root), text).map_err(|error| format!("cannot write rip.lock: {}", error))?;
    Ok(packages.len())
}

/// 校验 rip.lock 与本地包是否一致；没有 rip.lock 时只在离线模式下报错。
fn verify_lock(root: &Path) -> Result<(), Vec<String>> {
    let locked = match read_lock(root) {
        Ok(Some(packages)) => packages,
        Ok(None) => {
            if offline() || read_manifest(root).registry_offline {
                return Err(vec![
                    "error: --offline requires rip.lock; run 'rip lock' once with packages available"
                        .to_string(),
                ]);
            }
            return Ok(());
        }
        Err(error) => return Err(vec![format!("error: {}", error)]),
    };
    let current = current_packages(root);
    let mut messages = Vec::new();
    for entry in &locked {
        match current.iter().find(|package| package.name == entry.name) {
            Some(package) => {
                if package.sha256 != entry.sha256 {
                    messages.push(format!(
                        "error: package '{}' does not match rip.lock ({}); run 'rip lock' to update",
                        entry.name, entry.source
                    ));
                }
                if package.version != entry.version {
                    messages.push(format!(
                        "error: package '{}' version changed from '{}' to '{}'; run 'rip lock' to update",
                        entry.name, entry.version, package.version
                    ));
                }
                let mut current_deps = package.deps.clone();
                let mut locked_deps = entry.deps.clone();
                current_deps.sort();
                locked_deps.sort();
                if current_deps != locked_deps {
                    messages.push(format!(
                        "error: package '{}' dependency list changed; run 'rip lock' to update",
                        entry.name
                    ));
                }
            }
            None => messages.push(format!(
                "error: package '{}' from rip.lock is missing ({}); restore it or run 'rip lock'",
                entry.name, entry.source
            )),
        }
    }
    for package in &current {
        if !locked.iter().any(|entry| entry.name == package.name) {
            messages.push(format!(
                "error: package '{}' ({}) is not recorded in rip.lock; run 'rip lock'",
                package.name, package.source
            ));
        }
    }
    if messages.is_empty() {
        Ok(())
    } else {
        Err(messages)
    }
}

/// 生成 `rip.lock`：补齐依赖 → 校验约束 → 固化到锁文件。
///
/// `rip lock` 与 `rip vendor` 共用这段逻辑，避免两套解析各写一份。
fn generate_lock(root: &Path) -> Result<usize, Vec<String>> {
    registry::ensure(root)?;
    resolve_dependencies(root)?;
    write_lock(root).map_err(|error| vec![format!("error: {}", error)])
}

/// `rip lock --check` / `rip vendor --check` 共用的校验流程。
fn check_lock(root: &Path) -> i32 {
    // 新克隆的仓库可能还没拉依赖，先补齐再比对锁文件。
    if let Err(messages) = registry::ensure(root) {
        for message in messages {
            eprintln!("{}", message);
        }
        return 1;
    }
    // 校验命令必须要求锁文件存在，否则“通过”没有任何意义。
    if read_lock(root).ok().flatten().is_none() {
        eprintln!(
            "error: {} is missing; run 'rip vendor' to generate it",
            lock_file(root).display()
        );
        return 1;
    }
    match verify_lock(root) {
        Ok(()) => {
            let count = read_lock(root)
                .ok()
                .flatten()
                .map(|packages| packages.len())
                .unwrap_or(0);
            println!("rip.lock: ok ({} packages)", count);
            0
        }
        Err(messages) => {
            for message in messages {
                eprintln!("{}", message);
            }
            1
        }
    }
}

fn cmd_lock(args: &[String]) -> i32 {
    let check = args.iter().any(|arg| arg == "--check");
    let root = match find_entry(None).and_then(|entry| project_root(&entry)) {
        Some(root) => root,
        None => {
            eprintln!("error: rip lock requires a project with rip.toml");
            return 2;
        }
    };
    if check {
        return check_lock(&root);
    }
    match generate_lock(&root) {
        Ok(count) => {
            println!("wrote {} ({} packages)", lock_file(&root).display(), count);
            0
        }
        Err(messages) => {
            for message in messages {
                eprintln!("{}", message);
            }
            1
        }
    }
}

/// `rip vendor`：把依赖固化到 `packages/` 并生成 `rip.lock`。
///
/// `packages/` 本身就是 vendored 目录，因此“拉取 + 锁定”是同一个动作；
/// 与 `rip lock` 共用 `generate_lock`，区别只在输出侧重点。
fn cmd_vendor(args: &[String]) -> i32 {
    let check = args.iter().any(|arg| arg == "--check");
    let root = match find_entry(None).and_then(|entry| project_root(&entry)) {
        Some(root) => root,
        None => {
            eprintln!("error: rip vendor requires a project with rip.toml");
            return 2;
        }
    };
    if check {
        return check_lock(&root);
    }
    match generate_lock(&root) {
        Ok(count) => {
            let packages = root.join("packages");
            if count == 0 {
                // 没有 [dependencies] 时 packages/ 可能根本不存在，别声称写入了它。
                println!("no packages to vendor; wrote {}", lock_file(&root).display());
            } else {
                println!(
                    "vendored {} package(s) into {}; wrote {}",
                    count,
                    packages.display(),
                    lock_file(&root).display()
                );
            }
            0
        }
        Err(messages) => {
            for message in messages {
                eprintln!("{}", message);
            }
            1
        }
    }
}

/// 预编译包解码后的源码：支持 `rip-package 1` 头部与纯 Rip 源码两种形态。
fn decode_package(bytes: &[u8], label: &str) -> Result<String, String> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| format!("precompiled package '{}' is not UTF-8 text", label))?;
    if let Some(index) = text.find("--- source ---\n") {
        return Ok(text[index + "--- source ---\n".len()..].to_string());
    }
    Ok(text)
}

/// 把非入口文件的 `mod` 声明替换成空行，行数保持不变。
fn blank_package_line(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 1);
    let mut replaced = false;
    for line in source.split_inclusive('\n') {
        if !replaced && line.trim_start().starts_with("mod ") {
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn unit_at(units: &[ProjectUnit], line: u32) -> Option<&ProjectUnit> {
    let mut current: Option<&ProjectUnit> = None;
    for unit in units {
        if unit.start <= line {
            current = Some(unit);
        } else {
            break;
        }
    }
    current
}

/// 返回 (文件, 原文件行号)。
fn locate(units: &[ProjectUnit], line: u32, fallback: &str) -> (String, u32) {
    match unit_at(units, line) {
        Some(unit) => (unit.display.clone(), line - unit.start + 1),
        None => (fallback.to_string(), line),
    }
}

/// 合并后的编译单元里，函数 / 类型 / 全局变量各自只能有一个定义。
///
/// 项目与 `packages/` 会被拼成单一程序，同名项在函数表里后写覆盖先写；
/// 这里在类型检查前显式报错，并同时给出两处位置，避免"导入哪个包才生效"
/// 这种依赖顺序的隐形行为。
fn duplicate_items(program: &Program, units: &[ProjectUnit], entry_display: &str) -> Vec<String> {
    use std::collections::hash_map::Entry;
    let mut functions: HashMap<String, (String, u32)> = HashMap::new();
    let mut types: HashMap<String, (String, u32)> = HashMap::new();
    let mut globals: HashMap<String, (String, u32)> = HashMap::new();
    let mut errors = Vec::new();
    for item in &program.items {
        let (seen, name, kind, line) = match item {
            Item::Func(func) => (&mut functions, func.name.as_str(), "function", func.line),
            Item::Type(decl) => (&mut types, decl.name.as_str(), "type", decl.line),
            Item::Interface(decl) => (&mut types, decl.name.as_str(), "interface", decl.line),
            Item::Enum(decl) => (&mut types, decl.name.as_str(), "enum", decl.line),
            Item::Const(decl) => (&mut globals, decl.name.as_str(), "global", decl.line),
            Item::Static(decl) => (&mut globals, decl.name.as_str(), "global", decl.line),
            Item::Import(_) => continue,
        };
        let (file, local_line) = locate(units, line, entry_display);
        match seen.entry(name.to_string()) {
            Entry::Occupied(previous) => {
                let (first_file, first_line) = previous.get();
                errors.push(format!(
                    "{}:{}: error: duplicate {} '{}' (first declared at {}:{})",
                    file, local_line, kind, name, first_file, first_line
                ));
            }
            Entry::Vacant(slot) => {
                slot.insert((file, local_line));
            }
        }
    }
    errors
}

fn level_label(level: Level) -> &'static str {
    match level {
        Level::Error => "error",
        Level::Warning => "warning",
    }
}

fn dir_name(parent: &Path) -> String {
    if let Some(name) = parent.file_name().and_then(|name| name.to_str()) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    // 父路径是 "." 或空：取当前工作目录名
    std::env::current_dir()
        .ok()
        .and_then(|dir| dir.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_default()
}

fn display_path(root: &Path, file: &Path) -> String {
    match file.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        _ => file.display().to_string(),
    }
}

/// 读取入口所在项目。目录中没有 rip.toml 时返回 None，按单文件处理。
fn load_project(path: &Path) -> Result<Option<Prepared>, Vec<String>> {
    let entry = match resolve_entry(path) {
        Ok(entry) => entry,
        Err(error) => return Err(vec![error]),
    };
    let root = match project_root(&entry) {
        Some(root) => root,
        None => return Ok(None),
    };
    // 依赖拉取：`[dependencies]` 里缺失或版本不符的包先从 `[registry]` 拉取。
    // 已 vendored 且版本匹配时不访问网络。
    if let Err(messages) = registry::ensure(&root) {
        return Err(messages);
    }
    // 依赖锁定：存在 rip.lock 时先校验本地包未漂移；离线模式必须有 rip.lock。
    if let Err(messages) = verify_lock(&root) {
        return Err(messages);
    }
    // 依赖约束：`[dependencies]` 声明的包必须已 vendored，且版本满足约束（含传递依赖）。
    if let Err(messages) = resolve_dependencies(&root) {
        return Err(messages);
    }

    let entry_canon = fs::canonicalize(&entry).unwrap_or_else(|_| entry.clone());
    let mut files = vec![entry.clone()];
    let mut others = Vec::new();
    let excludes = read_manifest(&root).exclude;
    collect_project_files(&root, &root, &excludes, &mut others);
    for file in others {
        let canon = fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
        if canon != entry_canon {
            files.push(file);
        }
    }

    let mut messages = Vec::new();
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    let mut file_packages: HashMap<PathBuf, String> = HashMap::new();
    let mut dir_packages: HashMap<PathBuf, String> = HashMap::new();
    for file in &files {
        let source = match fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                messages.push(format!("{}: {}", file.display(), error));
                continue;
            }
        };
        let dir = match file.parent() {
            Some(parent) => dir_name(parent),
            None => String::new(),
        };
        match read_package_name(file) {
            Some((name, line)) => {
                file_packages.insert(file.clone(), name.clone());
                if name != dir {
                    messages.push(format!(
                        "{}:{}: error: mod name '{}' must match directory name '{}'",
                        file.display(),
                        line,
                        name,
                        dir
                    ));
                }
                if let Some(parent) = file.parent() {
                    match dir_packages.get(parent) {
                        Some(previous) if previous != &name => messages.push(format!(
                            "{}:{}: error: all .rp files in a directory must share one mod; expected '{}', found '{}'",
                            file.display(),
                            line,
                            previous,
                            name
                        )),
                        Some(_) => {}
                        None => {
                            dir_packages.insert(parent.to_path_buf(), name);
                        }
                    }
                }
            }
            None => messages.push(format!(
                "{}:1: error: every .rp file in directory '{}' must start with 'mod {};'",
                file.display(),
                dir,
                dir
            )),
        }
        sources.push((file.clone(), source));
    }
    if !messages.is_empty() {
        return Err(messages);
    }

    // 第三方包：`packages/包路径/包名.rp`（源码）或 `.rpa`（预编译）。
    // 包名取相对 packages/ 的路径（`utils/http`），与导入串一一对应；
    // 包内文件不参与"package 名必须等于目录名"的校验。
    let packages_root = packages_root_of(&root);
    let mut package_sources: Vec<(String, String, String)> = Vec::new();
    if let Some(packages_root) = &packages_root {
        let mut package_files = Vec::new();
        collect_package_files(packages_root, &mut package_files);
        for file in package_files {
            let display = format!(
                "{}/{}",
                packages_root
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "packages".to_string()),
                file.strip_prefix(packages_root)
                    .unwrap_or(&file)
                    .display()
                    .to_string()
                    .replace('\\', "/")
            );
            let source = match fs::read(&file) {
                Ok(bytes) => match package_file_kind(&file) {
                    Some("rpa") => match precompiled_version(&bytes) {
                        Some(version) if version != env!("CARGO_PKG_VERSION") => {
                            messages.push(format!(
                                "{}: precompiled package was built with rip {}, current rip {}",
                                display,
                                version,
                                env!("CARGO_PKG_VERSION")
                            ));
                            continue;
                        }
                        _ => match decode_package(&bytes, &display) {
                            Ok(source) => source,
                            Err(error) => {
                                messages.push(error);
                                continue;
                            }
                        },
                    },
                    _ => match String::from_utf8(bytes) {
                        Ok(source) => source,
                        Err(_) => {
                            messages.push(format!(
                                "{}: package source is not valid UTF-8",
                                display
                            ));
                            continue;
                        }
                    },
                },
                Err(error) => {
                    messages.push(format!("{}: {}", display, error));
                    continue;
                }
            };
            package_sources.push((
                display,
                package_name_of(file.parent().unwrap_or(&file), packages_root),
                source,
            ));
        }
    }
    if !messages.is_empty() {
        return Err(messages);
    }

    // 包间循环依赖：告警（--deny-warnings 时升级为错误）。
    for warning in package_cycles(&package_sources) {
        if deny_warnings() {
            messages.push(warning);
        } else {
            eprintln!("{}", warning);
        }
    }
    if !messages.is_empty() {
        return Err(messages);
    }

    let entry_display = display_path(&root, &entry);
    let entry_package = file_packages.get(&entry).cloned().unwrap_or_default();
    let mut merged = sources[0].1.clone();
    let mut units = vec![ProjectUnit {
        display: entry_display.clone(),
        start: 1,
        package: entry_package,
    }];
    for (file, source) in sources.iter().skip(1) {
        if !merged.ends_with('\n') {
            merged.push('\n');
        }
        let start = merged.matches('\n').count() as u32 + 1;
        merged.push_str(&blank_package_line(source));
        units.push(ProjectUnit {
            display: display_path(&root, file),
            start,
            package: file_packages.get(file).cloned().unwrap_or_default(),
        });
    }
    for (display, package, source) in &package_sources {
        if !merged.ends_with('\n') {
            merged.push('\n');
        }
        let start = merged.matches('\n').count() as u32 + 1;
        merged.push_str(&blank_package_line(source));
        units.push(ProjectUnit {
            display: display.clone(),
            start,
            package: package.clone(),
        });
    }

    let mut program = match parse_program(&merged, &entry_display) {
        Ok(program) => program,
        Err(error) => {
            let (file, line) = locate(&units, error.line, &entry_display);
            return Err(vec![format!("{}:{}: parse error: {}", file, line, error.message)]);
        }
    };
    // 合并后包声明被抹空，这里按行号把每个函数归回它所在文件的包，
    // 供 `self func` 的包内可见性检查使用。
    for item in &mut program.items {
        if let Item::Func(func) = item {
            if let Some(unit) = unit_at(&units, func.line) {
                func.package = unit.package.clone();
            }
        }
    }
    // 跨包 / 跨文件的同名项：合并成单一程序后会互相覆盖，必须在类型检查前报错。
    let duplicates = duplicate_items(&program, &units, &entry_display);
    if !duplicates.is_empty() {
        return Err(duplicates);
    }
    let checked = check_program(&program);
    let ownership = check_ownership(&program, &checked);
    let mut errors = Vec::new();
    for diagnostic in checked.diagnostics.iter().chain(ownership.iter()) {
        let (file, line) = locate(&units, diagnostic.line, &entry_display);
        if diagnostic.level == Level::Error
            || (diagnostic.level == Level::Warning && deny_warnings())
        {
            errors.push(format!(
                "{}:{}: {}: {}",
                file,
                line,
                level_label(diagnostic.level),
                diagnostic.message
            ));
        } else {
            eprintln!(
                "{}:{}: {}: {}",
                file,
                line,
                level_label(diagnostic.level),
                diagnostic.message
            );
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(Some(Prepared {
        program,
        checked,
        units,
    }))
}

fn prepare(path: &Path) -> Option<Prepared> {
    match prepare_result(path) {
        Ok(prepared) => Some(prepared),
        Err(messages) => {
            print_messages(&messages);
            None
        }
    }
}

/// 与 `prepare` 相同，但把诊断以结构化字符串返回，供 `--json` 输出。
fn prepare_result(path: &Path) -> Result<Prepared, Vec<String>> {
    match load_project(path) {
        Ok(Some(prepared)) => Ok(prepared),
        Ok(None) => {
            let (source, real_path) = read_source(path)
                .map_err(|error| vec![format!("error: {}: {}", path.display(), error)])?;
            let (program, checked) = compile_source_result(&source, &real_path)?;
            Ok(Prepared {
                program,
                checked,
                units: Vec::new(),
            })
        }
        Err(messages) => Err(messages),
    }
}

fn compile_source_result(
    source: &str,
    real_path: &str,
) -> Result<(Program, CheckResult), Vec<String>> {
    let program = parse_program(source, real_path).map_err(|error| {
        vec![format!(
            "{}:{}: parse error: {}",
            real_path, error.line, error.message
        )]
    })?;
    let layout = check_package_layout(&program, real_path);
    if !layout.is_empty() {
        return Err(layout);
    }
    let checked = check_program(&program);
    let ownership = check_ownership(&program, &checked);
    let mut errors = Vec::new();
    for diagnostic in checked.diagnostics.iter().chain(ownership.iter()) {
        let line = format!(
            "{}:{}: {}: {}",
            real_path,
            diagnostic.line,
            level_label(diagnostic.level),
            diagnostic.message
        );
        if diagnostic.level == Level::Error
            || (diagnostic.level == Level::Warning && deny_warnings())
        {
            errors.push(line);
        } else {
            eprintln!("{}", line);
        }
    }
    if errors.is_empty() {
        Ok((program, checked))
    } else {
        Err(errors)
    }
}

/// 包布局规则：包名必须等于文件所在目录名，同一目录下的 .rp 文件必须同包。
fn check_package_layout(program: &Program, real_path: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let path = Path::new(real_path);
    if path.extension().and_then(|e| e.to_str()) != Some("rp") {
        return messages;
    }
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return messages,
    };
    let dir = match parent.file_name().and_then(|name| name.to_str()) {
        Some(dir) => dir,
        None => return messages,
    };
    if program.package != dir {
        messages.push(format!(
            "{}:{}: error: package '{}' must match directory name '{}'",
            real_path, program.package_line, program.package, dir
        ));
        return messages;
    }
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return messages,
    };
    let mut siblings: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rp")
        })
        .collect();
    siblings.sort();
    for sibling in siblings {
        let display = sibling.display();
        match read_package_name(&sibling) {
            Some((name, _)) if name == dir => {}
            Some((name, line)) => messages.push(format!(
                "{}:{}: error: all .rp files in a directory must share one mod; expected '{}', found '{}'",
                display, line, dir, name
            )),
            None => messages.push(format!(
                "{}:1: error: every .rp file in directory '{}' must start with 'mod {};'",
                display, dir, dir
            )),
        }
    }
    messages
}

/// 只读取文件开头的 `mod` 声明，不依赖完整解析，避免把无关错误算到相邻文件上。
fn read_package_name(path: &Path) -> Option<(String, u32)> {
    let source = fs::read_to_string(path).ok()?;
    let tokens = lex(&source).ok()?;
    let first = tokens.first()?;
    if first.kind != TokenKind::Kw(Keyword::Mod) {
        return None;
    }
    let name = match tokens.get(1).map(|token| &token.kind) {
        Some(TokenKind::Ident(name)) => name.clone(),
        _ => return None,
    };
    match tokens.get(2).map(|token| &token.kind) {
        Some(TokenKind::Semi) => Some((name, first.line)),
        _ => None,
    }
}

fn read_source(path: &Path) -> Result<(String, String), String> {
    let path = resolve_entry(path)?;
    let bytes = fs::read(&path).map_err(|error| format!("cannot read file: {}", error))?;
    let text = String::from_utf8(bytes).map_err(|_| "cannot read file: invalid UTF-8".to_string())?;
    if text.starts_with(ARCHIVE_HEADER) {
        let source = match text.find(ARCHIVE_MARKER) {
            Some(index) => text[index + ARCHIVE_MARKER.len()..].to_string(),
            None => return Err("invalid .rpp archive: missing source marker".to_string()),
        };
        return Ok((source, path.display().to_string()));
    }
    Ok((text, path.display().to_string()))
}

/// 包即目录：默认入口是同名文件，如 `user_api/user_api.rp`。
fn dir_entry(path: &Path) -> Option<PathBuf> {
    let name = path.file_name().and_then(|name| name.to_str())?;
    let candidate = path.join(format!("{}.rp", name));
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

fn resolve_entry(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        if let Some(entry) = dir_entry(path) {
            return Ok(entry);
        }
        for candidate in ["src/main.rp", "main.rp", "src/main.rpp", "main.rpp"] {
            let candidate = path.join(candidate);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err("directory has no src/main.rp or main.rp".to_string());
    }
    Err(format!("file '{}' does not exist", path.display()))
}

fn find_entry(base: Option<&Path>) -> Option<PathBuf> {
    let base = base.unwrap_or_else(|| Path::new("."));
    if let Some(entry) = manifest_entry(base) {
        let path = base.join(entry);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(entry) = dir_entry(base) {
        return Some(entry);
    }
    for candidate in ["main.rp", "src/main.rp", "main.rpp", "src/main.rpp"] {
        let path = base.join(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn manifest_entry(base: &Path) -> Option<String> {
    let text = fs::read_to_string(base.join("rip.toml")).ok()?;
    let mut in_module = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_module = line == "[module]";
            continue;
        }
        if !in_module || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "entry" {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn collect_rp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().map(|n| n == "target").unwrap_or(false) {
                continue;
            }
            collect_rp_files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rp") | Some("rpp")
        ) {
            out.push(path);
        }
    }
}

fn cmd_repl(_args: &[String]) -> i32 {
    println!("Rip REPL (v{}). 输入 :help 查看帮助，:quit 退出。", env!("CARGO_PKG_VERSION"));
    let stdin = io::stdin();
    let mut defs: Vec<String> = Vec::new();
    let mut vars: Vec<String> = Vec::new();
    loop {
        print!("rip> ");
        let _ = io::stdout().flush();
        let mut entry = String::new();
        loop {
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => {
                    println!();
                    return 0;
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("error: {}", error);
                    return 1;
                }
            }
            let trimmed = line.trim_end().to_string();
            if entry.is_empty() {
                let command = trimmed.trim();
                if command.is_empty() {
                    continue;
                }
                if command == ":quit" || command == ":exit" || command == ":q" {
                    return 0;
                }
                if command == ":help" || command == ":h" {
                    println!("  :help   显示帮助");
                    println!("  :quit   退出");
                    println!("  直接输入表达式会打印结果；let/赋值/函数/类型声明会被保存。");
                    entry.clear();
                    break;
                }
                if command == ":clear" {
                    defs.clear();
                    vars.clear();
                    println!("session cleared");
                    entry.clear();
                    break;
                }
            }
            entry.push_str(&line);
            if balance(&entry) <= 0 {
                break;
            }
            print!("...> ");
            let _ = io::stdout().flush();
        }
        let input = entry.trim();
        if input.is_empty() {
            continue;
        }
        if is_declaration(input) {
            let candidate = input.to_string();
            let mut candidate_defs = defs.clone();
            candidate_defs.push(candidate);
            match eval_session(&candidate_defs, &vars, None) {
                Ok(_) => defs = candidate_defs,
                Err(messages) => print_messages(&messages),
            }
            continue;
        }
        let mut statement = if input.ends_with(';') {
            input.to_string()
        } else if is_let(input) || input.contains(" = ") {
            format!("{};", input)
        } else {
            format!("print({});", input)
        };
        if statement.is_empty() {
            statement.push(';');
        }
        match eval_session(&defs, &vars, Some(&statement)) {
            Ok(_) => {
                if is_let(input) {
                    vars.push(statement);
                }
            }
            Err(messages) => print_messages(&messages),
        }
    }
}

fn is_declaration(input: &str) -> bool {
    let trimmed = input.trim_start();
    trimmed.starts_with("func ")
        || trimmed.starts_with("pub func ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("pub type ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("static ")
        || trimmed.starts_with("use ")
}

fn is_let(input: &str) -> bool {
    input.trim_start().starts_with("let ")
}

fn balance(input: &str) -> i32 {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next();
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn eval_session(
    defs: &[String],
    vars: &[String],
    current: Option<&str>,
) -> Result<(), Vec<String>> {
    let mut source = String::from("mod repl;\n");
    for def in defs {
        source.push_str(def);
        source.push('\n');
    }
    source.push_str("func main() -> void {\n");
    for var in vars {
        source.push_str(var);
        source.push('\n');
    }
    if let Some(statement) = current {
        source.push_str(statement);
        source.push('\n');
    }
    source.push_str("}\n");
    let program = parse_program(&source, "<repl>")
        .map_err(|e| vec![format!("<repl>:{}: parse error: {}", e.line, e.message)])?;
    let checked = check_program(&program);
    let errors: Vec<String> = checked
        .diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .map(|d| format!("<repl>:{}: {}", d.line, d.message))
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut runtime = Runtime::new(&program, checked, Vec::new());
    match runtime.run_main() {
        Ok(_) => Ok(()),
        Err(error) => Err(vec![format!("<repl>:{}: {}", error.line, error.message)]),
    }
}

fn print_messages(messages: &[String]) {
    for message in messages {
        eprintln!("{}", message);
    }
}

fn json_escape(input: &str) -> String {
    let mut out = String::from("\"");
    for c in input.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个临时项目：`files` 是相对项目根的路径与内容。
    fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rip-deps-{}-{}-{}",
            tag,
            process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        for (path, content) in files {
            let file = root.join(path);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).expect("create dirs");
            }
            fs::write(file, content).expect("write fixture");
        }
        root
    }

    fn dependency_manifest(dependencies: &[(&str, &str)]) -> String {
        let mut text = String::from("[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[dependencies]\n");
        for (name, requirement) in dependencies {
            text.push_str(&format!("\"{}\" = \"{}\"\n", name, requirement));
        }
        text
    }

    #[test]
    fn resolves_transitive_dependencies() {
        let root = project(
            "ok",
            &[
                ("rip.toml", &dependency_manifest(&[("acme/tools", "^1.2")])),
                (
                    "packages/acme/tools/rip.toml",
                    "[module]\nname = \"tools\"\nversion = \"1.4.0\"\n\n[dependencies]\n\"acme/base\" = \"~0.3\"\n",
                ),
                (
                    "packages/acme/base/rip.toml",
                    "[module]\nname = \"base\"\nversion = \"0.3.2\"\n",
                ),
            ],
        );
        let packages = resolve_dependencies(&root).expect("resolved");
        let names: Vec<&str> = packages.iter().map(|package| package.name.as_str()).collect();
        assert_eq!(names, vec!["acme/base", "acme/tools"]);
        assert_eq!(packages[1].version, "1.4.0");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_version_outside_requirement() {
        let root = project(
            "mismatch",
            &[
                ("rip.toml", &dependency_manifest(&[("acme/tools", "^2.0")])),
                (
                    "packages/acme/tools/rip.toml",
                    "[module]\nname = \"tools\"\nversion = \"1.4.0\"\n",
                ),
            ],
        );
        let error = resolve_dependencies(&root).expect_err("must fail");
        assert!(error.iter().any(|message| message.contains("vendored version is 1.4.0")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing_package() {
        let root = project(
            "missing",
            &[
                (
                    "rip.toml",
                    &dependency_manifest(&[("acme/tools", "^1.0")]),
                ),
                ("packages/.keep", ""),
            ],
        );
        let error = resolve_dependencies(&root).expect_err("must fail");
        assert!(error.iter().any(|message| message.contains("not vendored")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_conflicting_requirements() {
        let root = project(
            "conflict",
            &[
                (
                    "rip.toml",
                    &dependency_manifest(&[("acme/a", "^1.0"), ("acme/b", "^1.0")]),
                ),
                (
                    "packages/acme/a/rip.toml",
                    "[module]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/shared\" = \"^1.0\"\n",
                ),
                (
                    "packages/acme/b/rip.toml",
                    "[module]\nname = \"b\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/shared\" = \"^2.0\"\n",
                ),
                (
                    "packages/acme/shared/rip.toml",
                    "[module]\nname = \"shared\"\nversion = \"1.5.0\"\n",
                ),
            ],
        );
        let error = resolve_dependencies(&root).expect_err("must fail");
        assert!(error
            .iter()
            .any(|message| message.contains("one version per package")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_diagnostic_messages_for_json() {
        let item = json_diagnostic_from_message("main.rp:4: error: string literal cannot be used as int");
        assert!(item.contains("\"file\":\"main.rp\""), "{}", item);
        assert!(item.contains("\"level\":\"error\""), "{}", item);
        assert!(item.contains("\"line\":4"), "{}", item);
        assert!(item.contains("string literal cannot be used as int"), "{}", item);

        // Windows 盘符路径也要能拆出行号。
        let item = json_diagnostic_from_message("C:\\app\\main.rp:9: warning: unused variable 'x'");
        assert!(item.contains("\"file\":\"C:\\\\app\\\\main.rp\""), "{}", item);
        assert!(item.contains("\"line\":9"), "{}", item);

        // 非诊断信息按错误处理，行号缺省为 0。
        let item = json_diagnostic_from_message("error: cannot read missing.rp");
        assert!(item.contains("\"line\":0"), "{}", item);
        assert!(item.contains("cannot read missing.rp"), "{}", item);
    }

    #[test]
    fn lock_entry_is_one_per_package_directory() {
        let root = project(
            "lock-dir",
            &[
                (
                    "rip.toml",
                    "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n",
                ),
                (
                    "packages/acme/tools/rip.toml",
                    "[module]\nname = \"tools\"\nversion = \"1.0.0\"\n",
                ),
                ("packages/acme/tools/tools.rp", "mod tools;\n"),
                ("packages/acme/tools/extra.rp", "mod tools;\n"),
            ],
        );
        let packages = current_packages(&root);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "acme/tools");
        assert_eq!(packages[0].version, "1.0.0");
        assert!(!packages[0].sha256.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn projects_without_dependencies_are_unchanged() {
        let root = project(
            "empty",
            &[(
                "rip.toml",
                "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n",
            )],
        );
        let packages = resolve_dependencies(&root).expect("resolved");
        assert!(packages.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_duplicate_items_across_packages() {
        let base = project(
            "duplicate-items",
            &[
                (
                    "app/rip.toml",
                    &dependency_manifest(&[("acme/tools", "^1.0")]),
                ),
                (
                    "app/main.rp",
                    "mod app;\n\ntype item = [id: int];\n\nfunc helper() -> void {}\n\nfunc main() -> void {\n    helper();\n}\n",
                ),
                (
                    "app/other.rp",
                    "mod app;\n\ntype item = [name: string];\n",
                ),
                (
                    "app/packages/acme/tools/rip.toml",
                    "[module]\nname = \"tools\"\nversion = \"1.0.0\"\n",
                ),
                (
                    "app/packages/acme/tools/tools.rp",
                    "mod tools;\n\nfunc helper() -> void {}\n",
                ),
            ],
        );
        let root = base.join("app");
        let errors = match load_project(&root.join("main.rp")) {
            Ok(_) => panic!("duplicate items must fail"),
            Err(errors) => errors,
        };
        assert!(
            errors
                .iter()
                .any(|message| message.contains("duplicate function 'helper'")),
            "{:?}",
            errors
        );
        assert!(
            errors
                .iter()
                .any(|message| message.contains("duplicate type 'item'")),
            "{:?}",
            errors
        );
        // 冲突信息必须同时给出两处位置（本项目与第三方包）。
        assert!(
            errors.iter().any(|message| {
                message.contains("main.rp") && message.contains("packages/acme/tools/tools.rp")
            }),
            "{:?}",
            errors
        );
        let _ = fs::remove_dir_all(&base);
    }
}

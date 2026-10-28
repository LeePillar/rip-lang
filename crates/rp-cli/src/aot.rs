//! 完整 AOT 后端（阶段 A）：把 Rip 全量语法翻译成 Rust，运行时只调用原生宿主，
//! 不经过 AST 解释器。生成代码使用运行时 `Value` 模型，语义与解释器一致。

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use rp_ast::*;
use rp_types::{CheckResult, Type};

/// 声明为固定位宽整数时返回类型名；`int` / `i64` 与其它类型返回 `None`（不需要范围检查）。
fn width_type_name(ty: &TypeExpr) -> Option<&'static str> {
    let name = match &ty.kind {
        TypeExprKind::Named(path) => path.last()?,
        _ => return None,
    };
    match name.as_str() {
        "i8" => Some("i8"),
        "i16" => Some("i16"),
        "i32" => Some("i32"),
        "u8" => Some("u8"),
        "u16" => Some("u16"),
        "u32" => Some("u32"),
        "u64" => Some("u64"),
        _ => None,
    }
}

pub(crate) fn generate(
    program: &Program,
    checked: &CheckResult,
    setup: &Setup,
    host: &str,
    static_natives: &[StaticNative],
) -> Result<String, String> {
    let mut generator = Generator {
        program,
        checked,
        setup,
        host,
        static_natives: static_natives.to_vec(),
        imports: checked.imports.clone(),
        funcs: HashMap::new(),
        async_funcs: HashSet::new(),
        globals: HashSet::new(),
        init_items: Vec::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
        counter: 0,
        indent: 0,
        body: String::new(),
    };
    generator.run()
}

/// 静态链接进产物的原生包：签名来自 `.rp.decl`，实现由 rustc 直接链接。
///
/// 只有包目录里存在静态库（`libX.a` / `X.lib`）时才会静态链接，
/// 否则回退到运行时动态加载（产物带 `lib/native/`）。
#[derive(Clone)]
pub(crate) struct StaticNative {
    /// 包路径，例如 `acme/tools`。
    pub package: String,
    /// 链接名，例如 `librp_native_demo.a` → `rp_native_demo`。
    pub link_name: String,
    /// 符号前缀：声明里写了 `prefix` 才有，用于多个包共存。
    pub symbol_prefix: Option<String>,
    /// 声明里的函数：`(函数名, 是否返回 result)`。
    pub funcs: Vec<(String, bool)>,
}

impl StaticNative {
    /// 四个 ABI 符号的真实名字。
    fn symbols(&self) -> (String, String, String) {
        match &self.symbol_prefix {
            Some(prefix) => (
                format!("rp_native_{}_abi", prefix),
                format!("rp_native_{}_call", prefix),
                format!("rp_native_{}_free", prefix),
            ),
            None => (
                "rp_native_abi".to_string(),
                "rp_native_call".to_string(),
                "rp_native_free".to_string(),
            ),
        }
    }
}

/// 构建期注入的运行时配置，来自 `rip.toml` 的 `[runtime]`、`[server]`、`[cluster]`。
#[derive(Clone)]
pub(crate) struct Setup {
    pub chinese: bool,
    pub server: rp_runtime::ServerConfig,
    pub cluster: rp_runtime::ClusterConfig,
}

struct Generator<'a> {
    program: &'a Program,
    checked: &'a CheckResult,
    setup: &'a Setup,
    /// 生成代码使用的宿主 crate 名：`rp_core`（纯净）或 `rp_runtime`（含解释器）。
    host: &'a str,
    /// 静态链接进产物的原生包及其生成的 Rust 模块名。
    static_natives: Vec<StaticNative>,
    imports: HashMap<String, String>,
    funcs: HashMap<String, String>,
    /// `async func` 的名字：调用点生成 `host.call_async` 而不是直接调用。
    async_funcs: HashSet<String>,
    globals: HashSet<String>,
    init_items: Vec<&'a VarDecl>,
    scopes: Vec<HashMap<String, String>>,
    loops: Vec<Option<String>>,
    counter: usize,
    indent: usize,
    body: String,
}

impl<'a> Generator<'a> {
    fn run(&mut self) -> Result<String, String> {
        let mut main: Option<&'a FuncDecl> = None;
        // 原生函数表：解释器按名字调度的能力（任务、HTTP 处理函数）在 AOT 下靠它恢复。
        let mut table: Vec<(String, String, usize)> = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Func(decl) => {
                    if decl.is_async {
                        self.async_funcs.insert(decl.name.clone());
                    }
                    // 泛型函数在运行时类型擦除：AOT 只生成一份实现，类型实参不参与运行期。
                    if decl.name == "main" {
                        main = Some(decl);
                        table.push((
                            decl.name.clone(),
                            "rip_native_main".to_string(),
                            0,
                        ));
                    } else {
                        let rust_name = format!("rip_fn_{}", self.funcs.len());
                        let wrapper = format!("rip_native_{}", self.funcs.len());
                        self.funcs.insert(decl.name.clone(), rust_name);
                        table.push((decl.name.clone(), wrapper, decl.params.len()));
                    }
                }
                Item::Const(decl) | Item::Static(decl) => {
                    self.globals.insert(decl.name.clone());
                    self.init_items.push(decl);
                }
                _ => {}
            }
        }
        let main = main.ok_or("AOT build requires func main() -> void")?;
        table.sort_by(|a, b| a.0.cmp(&b.0));

        // 先占位，保持先声明后使用的顺序无所谓（Rust 允许）。
        let mut definitions = String::new();
        for item in &self.program.items {
            if let Item::Func(decl) = item {
                if decl.name != "main" {
                    definitions.push_str(&self.emit_function(decl)?);
                }
            }
        }
        definitions.push_str(&self.emit_main(main)?);

        let mut out = String::new();
        out.push_str("use std::collections::HashMap;\n");
        out.push_str("use std::rc::Rc;\n");
        out.push_str(&format!(
            "use {}::{{coerce_int_name, format_value, Deferred, Flow, FlowResult, FuncKind, Function, NativeEntry, NativeModule, NativeRoute, NativeTask, Runtime, Value}};\n\n",
            self.host
        ));
        out.push_str(&definitions);
        out.push_str(&self.emit_native_wrappers(&table));
        out.push_str(&self.emit_init()?);
        out.push_str(&self.emit_imports());
        out.push_str(&self.emit_static_natives());
        out.push_str(&self.emit_module(&table));
        out.push_str(&self.emit_config());
        out.push_str("fn main() {\n");
        out.push_str("    std::panic::set_hook(Box::new(|info| {\n");
        out.push_str("        eprintln!(\"[panic] {}\", info);\n");
        out.push_str("    }));\n");
        if !self.static_natives.is_empty() {
            out.push_str("    rip_static_native_abi();\n");
        }
        out.push_str("    let mut host = Runtime::new_native(rip_module());\n");
        out.push_str("    rip_config(&mut host);\n");
        out.push_str("    if let Err(error) = host.init_entry() {\n");
        out.push_str("        eprintln!(\"runtime error: {}\", error.message);\n");
        out.push_str("        std::process::exit(1);\n");
        out.push_str("    }\n");
        out.push_str("    host.start_scheduled_tasks();\n");
        out.push_str("    match rip_main(&mut host) {\n");
        out.push_str("        Ok(_) => {}\n");
        out.push_str("        Err(Flow::Error(error)) => {\n");
        out.push_str("            eprintln!(\"runtime error: {}\", error.message);\n");
        out.push_str("            for frame in error.stack_lines() {\n");
        out.push_str("                eprintln!(\"{}\", frame);\n");
        out.push_str("            }\n");
        out.push_str("            std::process::exit(1);\n");
        out.push_str("        }\n");
        out.push_str("        Err(Flow::Propagate(value)) => {\n");
        out.push_str("            eprintln!(\"runtime error: uncaught error: {}\", format_value(&value));\n");
        out.push_str("            std::process::exit(1);\n");
        out.push_str("        }\n");
        out.push_str("        Err(_) => {}\n");
        out.push_str("    }\n");
        out.push_str("}\n");
        Ok(out)
    }

    /// 生成 `NativeFn` 适配器：把带行号的统一入口转给各函数。
    fn emit_native_wrappers(&self, table: &[(String, String, usize)]) -> String {
        let mut text = String::new();
        for (name, wrapper, _) in table {
            if name == "main" {
                let _ = writeln!(
                    text,
                    "fn rip_native_main(host: &mut Runtime, _args: Vec<Value>, _line: u32) -> FlowResult<Value> {{ rip_main(host) }}"
                );
                continue;
            }
            let rust_name = match self.funcs.get(name) {
                Some(rust_name) => rust_name,
                None => continue,
            };
            let _ = writeln!(
                text,
                "fn {}(host: &mut Runtime, args: Vec<Value>, _line: u32) -> FlowResult<Value> {{ {}(host, args) }}",
                wrapper, rust_name
            );
        }
        text.push('\n');
        text
    }

    /// 生成构建期配置注入：语言、服务端参数、集群配置与命令行覆盖。
    fn emit_config(&self) -> String {
        let mut text = String::from("fn rip_config(host: &mut Runtime) {\n");
        let language = if self.setup.chinese {
            format!("{}::Language::Chinese", self.host)
        } else {
            format!("{}::Language::English", self.host)
        };
        let _ = writeln!(text, "    host.set_language({});", language);
        let server = &self.setup.server;
        let trusted_proxies: Vec<String> = server
            .trusted_proxies
            .iter()
            .map(|proxy| format!("{:?}.to_string()", proxy))
            .collect();
        let _ = writeln!(
            text,
            "    host.set_server_config({host}::ServerConfig {{ host: {:?}.to_string(), workers: {}, max_connections: {}, max_body_bytes: {}, max_header_bytes: {}, max_headers: {}, read_timeout: std::time::Duration::from_millis({}), write_timeout: std::time::Duration::from_millis({}), keep_alive: std::time::Duration::from_millis({}), max_requests: {}, health_path: {:?}.to_string(), ready_path: {:?}.to_string(), metrics_path: {:?}.to_string(), drain_timeout: std::time::Duration::from_millis({}), request_timeout: std::time::Duration::from_millis({}), access_log: {}, max_heap_bytes: {}, trust_proxy: {}, trusted_proxies: vec![{}], rate_limit: {}, rate_limit_burst: {}, rate_limit_per_ip: {}, ..{host}::ServerConfig::default() }});",
            server.host,
            server.workers,
            server.max_connections,
            server.max_body_bytes,
            server.max_header_bytes,
            server.max_headers,
            server.read_timeout.as_millis() as u64,
            server.write_timeout.as_millis() as u64,
            server.keep_alive.as_millis() as u64,
            server.max_requests,
            server.health_path,
            server.ready_path,
            server.metrics_path,
            server.drain_timeout.as_millis() as u64,
            server.request_timeout.as_millis() as u64,
            server.access_log,
            server.max_heap_bytes,
            server.trust_proxy,
            trusted_proxies.join(", "),
            server.rate_limit,
            server.rate_limit_burst,
            server.rate_limit_per_ip,
            host = self.host
        );
        match &self.setup.cluster.redis_url {
            Some(url) => {
                let _ = writeln!(
                    text,
                    "    host.set_cluster({host}::ClusterConfig {{ redis_url: Some({:?}.to_string()), task_lock: {} }});",
                    url,
                    self.setup.cluster.task_lock,
                    host = self.host
                );
            }
            None => {
                let _ = writeln!(
                    text,
                    "    host.set_cluster({host}::ClusterConfig {{ redis_url: None, task_lock: {} }});",
                    self.setup.cluster.task_lock,
                    host = self.host
                );
            }
        }
        text.push_str(
            r#"    // 外部配置：config/rip.toml 与环境变量覆盖内嵌值，命令行再覆盖它们。
    host.apply_external_config();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port: Option<u16> = None;
    let mut bind: Option<String> = None;
    let mut cert: Option<String> = None;
    let mut key: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" if index + 1 < args.len() => {
                port = args[index + 1].parse().ok();
                index += 2;
            }
            "--host" if index + 1 < args.len() => {
                bind = Some(args[index + 1].clone());
                index += 2;
            }
            "--cert" if index + 1 < args.len() => {
                cert = Some(args[index + 1].clone());
                index += 2;
            }
            "--key" if index + 1 < args.len() => {
                key = Some(args[index + 1].clone());
                index += 2;
            }
            _ => index += 1,
        }
    }
    host.set_http_port(port);
    host.set_http_host(bind);
    host.set_http_tls(cert, key);
    host.set_cli_args(args);
}

"#,
        );
        text
    }

    /// 生成静态链接原生包的符号公告：`#[link]` 让 rustc 把静态库直接并进产物。
    fn emit_static_natives(&self) -> String {
        if self.static_natives.is_empty() {
            return String::new();
        }
        let mut text = String::new();
        for (index, package) in self.static_natives.iter().enumerate() {
            let (abi, call, free) = package.symbols();
            let _ = writeln!(
                text,
                "mod {} {{\n    #[link(name = {:?}, kind = \"static\")]\n    extern \"C\" {{\n        #[link_name = {:?}]\n        pub fn abi() -> u32;\n        #[link_name = {:?}]\n        pub fn call(name: *const u8, name_len: usize, args: *const u8, args_len: usize, out_len: *mut usize) -> *mut u8;\n        #[link_name = {:?}]\n        pub fn free(pointer: *mut u8, len: usize);\n    }}\n}}\n",
                static_module_name(index),
                package.link_name,
                abi,
                call,
                free
            );
        }
        // 原生包的 ABI 版本在启动时校验，避免把不匹配的实现链接进来后带病运行。
        text.push_str("fn rip_static_native_abi() {\n    unsafe {\n");
        for (index, package) in self.static_natives.iter().enumerate() {
            let _ = writeln!(
                text,
                "        let __abi_{index} = {}::abi();\n        if __abi_{index} < {}u32 || __abi_{index} > {}u32 {{\n            eprintln!(\"runtime error: native package '{}' has an incompatible abi version (found {{}}, supported {}..={})\", __abi_{index});\n            std::process::exit(1);\n        }}",
                static_module_name(index),
                rp_types::native::MIN_ABI_VERSION,
                rp_types::native::ABI_VERSION,
                package.package,
                rp_types::native::MIN_ABI_VERSION,
                rp_types::native::ABI_VERSION
            );
        }
        text.push_str("    }\n}\n\n");
        text
    }

    /// 生成模块描述：导入表、原生函数表、定时任务与初始化入口。
    fn emit_module(&self, table: &[(String, String, usize)]) -> String {
        let mut funcs = String::new();
        for (name, wrapper, arity) in table {
            let _ = writeln!(
                funcs,
                "    funcs.insert({:?}.to_string(), NativeEntry {{ arity: {}, body: {}, is_async: {} }});",
                name,
                arity,
                wrapper,
                self.async_funcs.contains(name)
            );
        }
        let mut tasks = String::new();
        for item in &self.program.items {
            if let Item::Func(decl) = item {
                for attribute in &decl.attributes {
                    if attribute.name == "interval" && attribute.args.len() == 1 {
                        let _ = writeln!(
                            tasks,
                            "    tasks.push(NativeTask {{ name: {:?}.to_string(), interval: Some({:?}.to_string()), line: {} }});",
                            decl.name, attribute.args[0], attribute.line
                        );
                    }
                }
            }
        }
        // 注解路由按函数名排序，与解释器的注册顺序保持一致。
        let mut route_items: Vec<(String, String, String)> = Vec::new();
        for item in &self.program.items {
            if let Item::Func(decl) = item {
                for attribute in &decl.attributes {
                    let method = match attribute.name.as_str() {
                        "get" => "GET",
                        "post" => "POST",
                        "put" => "PUT",
                        "delete" => "DELETE",
                        "patch" => "PATCH",
                        "options" => "OPTIONS",
                        "head" => "HEAD",
                        _ => continue,
                    };
                    if attribute.args.len() != 1 {
                        continue;
                    }
                    route_items.push((
                        decl.name.clone(),
                        method.to_string(),
                        attribute.args[0].clone(),
                    ));
                }
            }
        }
        route_items.sort_by(|a, b| a.0.cmp(&b.0));
        let mut routes = String::new();
        for (handler, method, path) in route_items {
            let _ = writeln!(
                routes,
                "    routes.push(NativeRoute {{ method: {:?}.to_string(), path: {:?}.to_string(), handler: {:?}.to_string() }});",
                method, path, handler
            );
        }
        format!(
            "fn rip_module() -> NativeModule {{\n    let mut funcs: HashMap<String, NativeEntry> = HashMap::new();\n{}\n    let mut routes: Vec<NativeRoute> = Vec::new();\n{}\n    let mut tasks: Vec<NativeTask> = Vec::new();\n{}\n    NativeModule {{ imports: rip_imports(), funcs: std::sync::Arc::new(funcs), routes: std::sync::Arc::new(routes), tasks: std::sync::Arc::new(tasks), init: Some(rip_init) }}\n}}\n\n",
            funcs.trim_end(),
            routes.trim_end(),
            tasks.trim_end()
        )
    }

    fn emit_function(&mut self, decl: &FuncDecl) -> Result<String, String> {
        let rust_name = self.funcs.get(&decl.name).cloned().unwrap_or_default();
        self.scopes.clear();
        self.loops.clear();
        self.body.clear();
        self.indent = 1;
        let mut scope = HashMap::new();
        self.scopes.push(scope.clone());
        for (index, param) in decl.params.iter().enumerate() {
            let var = format!("v{}_{}", self.next_id(), param.name);
            scope.insert(param.name.clone(), var.clone());
            self.line(&format!(
                "let mut {} = __args.get({}).cloned().unwrap_or(Value::Null);",
                var, index
            ));
            if let Some(ty_name) = width_type_name(&param.ty) {
                self.line(&format!(
                    "{} = match coerce_int_name({}.clone(), {:?}, {}) {{ Ok(value) => value, Err(message) => return Err(host.fail(message, {})) }};",
                    var, var, ty_name, decl.line, decl.line
                ));
            }
        }
        if let Some(scope_slot) = self.scopes.last_mut() {
            *scope_slot = scope;
        }
        for stmt in &decl.body {
            self.stmt(stmt)?;
        }
        self.line("Ok(Value::Null)");

        // 外层只负责调用帧：进入时登记，退出时按深度截断（与解释器一致）。
        // 函数体里的 `?` 会提前 return，交给外层统一收尾，不会漏掉出栈。
        let mut text = String::new();
        let _ = writeln!(
            text,
            "fn {}(host: &mut Runtime, __args: Vec<Value>) -> FlowResult<Value> {{\n    let __depth = host.frame_depth();\n    host.enter_frame({:?}, {});\n    host.defer_begin();\n    let __result = {}_body(host, __args);\n    let __deferred = host.defer_end();\n    host.exit_frame(__depth);\n    let __result = match __result {{ Err(Flow::Propagate(value)) => Ok(value), other => other }};\n    __deferred.and(__result)\n}}",
            rust_name, decl.name, decl.line, rust_name
        );
        let _ = writeln!(
            text,
            "\nfn {}_body(host: &mut Runtime, __args: Vec<Value>) -> FlowResult<Value> {{",
            rust_name
        );
        // 函数入口检查一次，递归/深度调用也能被截止时间中断。
        let _ = writeln!(text, "    host.check_deadline({})?;", decl.line);
        text.push_str(&self.body);
        text.push_str("}\n\n");
        Ok(text)
    }

    fn emit_main(&mut self, decl: &FuncDecl) -> Result<String, String> {
        self.scopes.clear();
        self.loops.clear();
        self.body.clear();
        self.indent = 1;
        self.scopes.push(HashMap::new());
        for stmt in &decl.body {
            self.stmt(stmt)?;
        }
        self.line("Ok(Value::Null)");
        let mut text = format!(
            "fn rip_main(host: &mut Runtime) -> FlowResult<Value> {{\n    let __depth = host.frame_depth();\n    host.enter_frame(\"main\", {});\n    host.defer_begin();\n    let __result = rip_main_body(host);\n    let __deferred = host.defer_end();\n    host.exit_frame(__depth);\n    __deferred.and(__result)\n}}\n\n",
            decl.line
        );
        text.push_str("fn rip_main_body(host: &mut Runtime) -> FlowResult<Value> {\n");
        let _ = writeln!(text, "    host.check_deadline({})?;", decl.line);
        text.push_str(&self.body);
        text.push_str("}\n\n");
        Ok(text)
    }

    fn emit_init(&mut self) -> Result<String, String> {
        self.scopes.clear();
        self.loops.clear();
        self.body.clear();
        self.indent = 1;
        self.scopes.push(HashMap::new());
        let items = self.init_items.clone();
        for decl in items {
            let value = self.expr(&decl.init)?;
            let (setup, vars) = self.hoist(&[value]);
            self.line("{");
            self.indent += 1;
            self.line(&setup);
            self.line(&format!(
                "host.set_global({:?}, {});",
                decl.name, vars[0]
            ));
            self.indent -= 1;
            self.line("}");
        }
        let mut text = String::from("fn rip_init(host: &mut Runtime) -> FlowResult<Value> {\n");
        text.push_str(&self.body);
        text.push_str("    Ok(Value::Null)\n}\n\n");
        Ok(text)
    }

    fn emit_imports(&self) -> String {
        let mut text = String::from("fn rip_imports() -> HashMap<String, String> {\n");
        text.push_str("    let mut imports = HashMap::new();\n");
        let mut pairs: Vec<(&String, &String)> = self.imports.iter().collect();
        pairs.sort();
        for (alias, path) in pairs {
            let _ = writeln!(
                text,
                "    imports.insert({:?}.to_string(), {:?}.to_string());",
                alias, path
            );
        }
        text.push_str("    imports\n}\n\n");
        text
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.kind {
            StmtKind::Let {
                name, ty, init, ..
            } => {
                let value = self.expr(init)?;
                let var = self.new_var(name);
                self.line(&format!("let mut {} = {};", var, value));
                // 固定位宽整数在绑定时检查范围，语义与解释器一致。
                if let Some(ty_name) = width_type_name(ty) {
                    self.line(&format!(
                        "{} = match coerce_int_name({}.clone(), {:?}, {}) {{ Ok(value) => value, Err(message) => return Err(host.fail(message, {})) }};",
                        var, var, ty_name, stmt.line, stmt.line
                    ));
                }
                Ok(())
            }
            StmtKind::Expr(expr) => {
                let code = self.expr(expr)?;
                self.line(&format!("{};", code));
                Ok(())
            }
            StmtKind::Assign { target, value } => {
                let value_code = self.expr(value)?;
                match &target.kind {
                    ExprKind::Ident(name) => {
                        if let Some(var) = self.lookup(name) {
                            self.line(&format!("{} = {};", var, value_code));
                        } else if self.globals.contains(name) {
                            let (setup, vars) = self.hoist(&[value_code]);
                            self.line("{");
                            self.indent += 1;
                            self.line(&setup);
                            self.line(&format!("host.set_global({:?}, {});", name, vars[0]));
                            self.indent -= 1;
                            self.line("}");
                        } else {
                            return Err(format!(
                                "AOT: unknown assignment target '{}' (line {})",
                                name, stmt.line
                            ));
                        }
                    }
                    ExprKind::Field { .. } => {
                        let (root, path) = field_path(target).ok_or_else(|| {
                            format!("AOT: unsupported assignment target (line {})", stmt.line)
                        })?;
                        let root_var = self.lookup(&root).ok_or_else(|| {
                            format!(
                                "AOT: assignment to non-local '{}' is not supported (line {})",
                                root, stmt.line
                            )
                        })?;
                        let path_literal: Vec<String> = path
                            .iter()
                            .map(|part| format!("{:?}.to_string()", part))
                            .collect();
                        let (setup, vars) = self.hoist(&[value_code]);
                        self.line("{");
                        self.indent += 1;
                        self.line(&setup);
                        self.line(&format!(
                            "let __updated = host.assign_path({}.clone(), &[{}], {}, {});",
                            root_var,
                            path_literal.join(", "),
                            vars[0],
                            stmt.line
                        ));
                        self.line(&format!("{} = __updated?;", root_var));
                        self.indent -= 1;
                        self.line("}");
                    }
                    _ => {
                        return Err(format!(
                            "AOT: unsupported assignment target (line {})",
                            stmt.line
                        ))
                    }
                }
                Ok(())
            }
            StmtKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_code = self.truthy_code(cond)?;
                self.line(&format!("if {} {{", cond_code));
                self.block(then_body)?;
                if let Some(body) = else_body {
                    self.line("} else {");
                    self.block(body)?;
                }
                self.line("}");
                Ok(())
            }
            StmtKind::Loop { body } => {
                self.line("loop {");
                self.indent += 1;
                self.line(&format!("host.check_deadline({})?;", stmt.line));
                self.loops.push(None);
                self.block(body)?;
                self.loops.pop();
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            StmtKind::While { cond, body } => {
                let cond_code = self.truthy_code(cond)?;
                self.line(&format!("while {} {{", cond_code));
                // 每轮迭代检查一次截止时间，长时间循环能被 504 中断。
                self.indent += 1;
                self.line(&format!("host.check_deadline({})?;", stmt.line));
                self.indent -= 1;
                self.loops.push(None);
                self.block(body)?;
                self.loops.pop();
                self.line("}");
                Ok(())
            }
            StmtKind::WhileLet {
                pattern,
                scrutinee,
                body,
            } => {
                self.line("loop {");
                self.indent += 1;
                self.line(&format!("host.check_deadline({})?;", stmt.line));
                let subject_code = self.expr(scrutinee)?;
                let subject_var = format!("__subject_{}", self.next_id());
                self.line(&format!("let {} = {};", subject_var, subject_code));
                let condition = self.pattern_condition(pattern, &subject_var);
                self.line(&format!("if !({}) {{", condition));
                self.indent += 1;
                self.line("break;");
                self.indent -= 1;
                self.line("}");
                self.push_scope();
                self.pattern_bindings(pattern, &subject_var)?;
                self.loops.push(None);
                for inner in body {
                    self.stmt(inner)?;
                }
                self.loops.pop();
                self.pop_scope();
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            StmtKind::ForIn { name, iter, body } => {
                let iter_code = self.expr(iter)?;
                let (setup, vars) = self.hoist(&[iter_code]);
                self.line("{");
                self.indent += 1;
                self.line(&setup);
                let items = format!("__items_{}", self.next_id());
                self.line(&format!(
                    "let {} = host.iter_items(&{}, {})?;",
                    items, vars[0], stmt.line
                ));
                self.line(&format!("for __item in {} {{", items));
                self.indent += 1;
                self.line(&format!("host.check_deadline({})?;", stmt.line));
                self.push_scope();
                let var = self.new_var(name);
                self.line(&format!("let mut {} = __item;", var));
                for inner in body {
                    self.stmt(inner)?;
                }
                self.pop_scope();
                self.indent -= 1;
                self.line("}");
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            StmtKind::Defer { call } => {
                enum Form {
                    Named(String),
                    Module(String, String),
                    Func,
                    Method(String),
                }
                let mut codes: Vec<String> = Vec::new();
                let form = match &call.kind {
                    ExprKind::Call { callee, args, .. } => {
                        let early = if let ExprKind::Ident(name) = &callee.kind {
                            Some(Form::Named(name.clone()))
                        } else if let ExprKind::Field { base, name } = &callee.kind {
                            match &base.kind {
                                ExprKind::Ident(alias) => self
                                    .imports
                                    .get(alias)
                                    .cloned()
                                    .map(|path| Form::Module(path, name.clone())),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        match early {
                            Some(form) => {
                                for arg in args {
                                    codes.push(self.expr(arg)?);
                                }
                                form
                            }
                            None => {
                                codes.push(self.expr(callee)?);
                                for arg in args {
                                    codes.push(self.expr(arg)?);
                                }
                                Form::Func
                            }
                        }
                    }
                    ExprKind::Method {
                        receiver,
                        name,
                        args,
                    } => {
                        let module = if let ExprKind::Ident(alias) = &receiver.kind {
                            self.imports.get(alias).cloned()
                        } else {
                            None
                        };
                        match module {
                            Some(path) => {
                                for arg in args {
                                    codes.push(self.expr(arg)?);
                                }
                                Form::Module(path, name.clone())
                            }
                            None => {
                                codes.push(self.expr(receiver)?);
                                for arg in args {
                                    codes.push(self.expr(arg)?);
                                }
                                Form::Method(name.clone())
                            }
                        }
                    }
                    _ => {
                        return Err(format!(
                            "AOT: defer requires a function call (line {})",
                            call.line
                        ))
                    }
                };
                let (setup, vars) = self.hoist(&codes);
                let expr = match &form {
                    Form::Named(name) => format!(
                        "Deferred::Named({:?}.to_string(), vec![{}])",
                        name,
                        vars.join(", ")
                    ),
                    Form::Module(path, name) => format!(
                        "Deferred::Module({:?}.to_string(), {:?}.to_string(), vec![{}])",
                        path,
                        name,
                        vars.join(", ")
                    ),
                    Form::Func => {
                        format!("Deferred::Func({}, vec![{}])", vars[0], vars[1..].join(", "))
                    }
                    Form::Method(name) => format!(
                        "Deferred::Method({}, {:?}.to_string(), vec![{}])",
                        vars[0],
                        name,
                        vars[1..].join(", ")
                    ),
                };
                self.line("{");
                self.indent += 1;
                self.line(&setup);
                self.line(&format!("host.defer_push({}, {});", expr, stmt.line));
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            StmtKind::Return(value) => {
                match value {
                    Some(expr) => {
                        let code = self.expr(expr)?;
                        self.line(&format!("return Ok({});", code));
                    }
                    None => self.line("return Ok(Value::Null);"),
                }
                Ok(())
            }
            StmtKind::Break => {
                self.line("break;");
                Ok(())
            }
            StmtKind::Continue => {
                if let Some(Some(step)) = self.loops.last().cloned() {
                    self.line(&format!("{};", step));
                }
                self.line("continue;");
                Ok(())
            }
            StmtKind::Match { subject, arms } => self.match_stmt(subject, arms, stmt.line),
            StmtKind::Parallel { branches } => self.parallel_stmt(branches, stmt.line),
        }
    }

    /// `parallel`：每条分支生成为一个独立函数，由运行时在线程里并发执行。
    ///
    /// 分支看不到块外局部变量（类型检查已拒绝），因此生成函数只接 `host`。
    fn parallel_stmt(&mut self, branches: &[Stmt], line: u32) -> Result<(), String> {
        let id = self.next_id();
        let mut names = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let name = format!("rip_par_{}_{}", id, index);
            self.emit_branch_fn(&name, branch)?;
            names.push(name);
        }
        self.line("{");
        self.indent += 1;
        self.line(&format!(
            "let __branches: Vec<fn(&mut Runtime) -> FlowResult<Value>> = vec![{}];",
            names.join(", ")
        ));
        self.line(&format!(
            "host.run_parallel_fns({:?}, {}, __branches)?;",
            format!("parallel{}", id),
            line
        ));
        self.indent -= 1;
        self.line("}");
        Ok(())
    }

    /// 生成一条并行分支的 Rust 函数，调用方的生成状态原样保留。
    fn emit_branch_fn(&mut self, name: &str, branch: &Stmt) -> Result<(), String> {
        let saved_scopes = std::mem::take(&mut self.scopes);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_body = std::mem::take(&mut self.body);
        let saved_indent = self.indent;
        self.scopes.clear();
        self.loops.clear();
        self.scopes.push(HashMap::new());
        self.indent = saved_indent + 1;

        let generated = (|| -> Result<(), String> {
            self.stmt(branch)?;
            self.line("Ok(Value::Null)");
            Ok(())
        })();
        let branch_body = std::mem::take(&mut self.body);

        self.scopes = saved_scopes;
        self.loops = saved_loops;
        self.body = saved_body;
        self.indent = saved_indent;
        generated?;

        self.line(&format!(
            "fn {}(host: &mut Runtime) -> FlowResult<Value> {{",
            name
        ));
        self.body.push_str(&branch_body);
        self.line("}");
        Ok(())
    }

    fn match_stmt(&mut self, subject: &Expr, arms: &[MatchArm], line: u32) -> Result<(), String> {
        let subject_code = self.expr(subject)?;
        let subject_var = format!("__subject_{}", self.next_id());
        self.line("{");
        self.indent += 1;
        self.line(&format!("let {} = {};", subject_var, subject_code));
        for (index, arm) in arms.iter().enumerate() {
            let condition = self.pattern_condition(&arm.pattern, &subject_var);
            if index == 0 {
                self.line(&format!("if {} {{", condition));
            } else {
                self.line(&format!("}} else if {} {{", condition));
            }
            self.push_scope();
            self.indent += 1;
            self.pattern_bindings(&arm.pattern, &subject_var)?;
            for stmt in &arm.body {
                self.stmt(stmt)?;
            }
            self.indent -= 1;
            self.pop_scope();
        }
        self.line("} else {");
        self.indent += 1;
        self.line(&format!(
            "return Err(host.fail(\"no match arm matched value\".to_string(), {}));",
            line
        ));
        self.indent -= 1;
        self.line("}");
        self.indent -= 1;
        self.line("}");
        Ok(())
    }

    fn pattern_condition(&mut self, pattern: &Pattern, subject: &str) -> String {
        match pattern {
            Pattern::Some(_) => format!("matches!(&{}, Value::Some(_))", subject),
            Pattern::None => format!("matches!(&{}, Value::None)", subject),
            Pattern::Ok(_) => format!("matches!(&{}, Value::Ok(_))", subject),
            Pattern::Err(_) => format!("matches!(&{}, Value::Err(_))", subject),
            Pattern::Ary(fields) => {
                let names: Vec<String> = fields
                    .iter()
                    .filter(|field| field.name != "_")
                    .map(|field| format!("{:?}", field.name))
                    .collect();
                format!(
                    "host.match_ary_fields(&{}, &[{}]).is_some()",
                    subject,
                    names.join(", ")
                )
            }
            Pattern::Enum {
                name,
                variant,
                fields,
            } => {
                let names: Vec<String> = fields
                    .iter()
                    .filter(|field| field.name != "_")
                    .map(|field| format!("{:?}", field.name))
                    .collect();
                format!(
                    "host.match_enum(&{}, {:?}, {:?}, &[{}]).is_some()",
                    subject,
                    name,
                    variant,
                    names.join(", ")
                )
            }
        }
    }

    fn pattern_bindings(&mut self, pattern: &Pattern, subject: &str) -> Result<(), String> {
        match pattern {
            Pattern::Some(name) => {
                let var = self.new_var(name);
                self.line(&format!(
                    "let mut {} = if let Value::Some(inner) = &{} {{ (**inner).clone() }} else {{ Value::Null }};",
                    var, subject
                ));
            }
            Pattern::Ok(name) => {
                let var = self.new_var(name);
                self.line(&format!(
                    "let mut {} = if let Value::Ok(inner) = &{} {{ (**inner).clone() }} else {{ Value::Null }};",
                    var, subject
                ));
            }
            Pattern::Err(name) => {
                let var = self.new_var(name);
                self.line(&format!(
                    "let mut {} = if let Value::Err(inner) = &{} {{ (**inner).clone() }} else {{ Value::Null }};",
                    var, subject
                ));
            }
            Pattern::None => {}
            Pattern::Ary(fields) => {
                let names: Vec<String> = fields
                    .iter()
                    .filter(|field| field.name != "_")
                    .map(|field| format!("{:?}", field.name))
                    .collect();
                let fields_var = format!("__fields_{}", self.next_id());
                self.line(&format!(
                    "let {} = host.match_ary_fields(&{}, &[{}]).unwrap_or_default();",
                    fields_var,
                    subject,
                    names.join(", ")
                ));
                let mut index = 0;
                for field in fields {
                    if field.name == "_" {
                        continue;
                    }
                    let var = self.new_var(&field.name);
                    self.line(&format!(
                        "let mut {} = {}.get({}).cloned().unwrap_or(Value::Null);",
                        var, fields_var, index
                    ));
                    index += 1;
                }
            }
            Pattern::Enum {
                name,
                variant,
                fields,
            } => {
                let names: Vec<String> = fields
                    .iter()
                    .filter(|field| field.name != "_")
                    .map(|field| format!("{:?}", field.name))
                    .collect();
                let fields_var = format!("__fields_{}", self.next_id());
                self.line(&format!(
                    "let {} = host.match_enum(&{}, {:?}, {:?}, &[{}]).unwrap_or_default();",
                    fields_var,
                    subject,
                    name,
                    variant,
                    names.join(", ")
                ));
                let mut index = 0;
                for field in fields {
                    if field.name == "_" {
                        continue;
                    }
                    let var = self.new_var(&field.name);
                    self.line(&format!(
                        "let mut {} = {}.get({}).cloned().unwrap_or(Value::Null);",
                        var, fields_var, index
                    ));
                    index += 1;
                }
            }
        }
        Ok(())
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        self.indent += 1;
        self.push_scope();
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        self.pop_scope();
        self.indent -= 1;
        Ok(())
    }

    fn truthy_code(&mut self, expr: &Expr) -> Result<String, String> {
        let code = self.expr(expr)?;
        let (setup, vars) = self.hoist(&[code]);
        Ok(format!(
            "{{ {}host.truthy({}, {})? }}",
            setup, vars[0], expr.line
        ))
    }

    fn expr(&mut self, expr: &Expr) -> Result<String, String> {
        let line = expr.line;
        match &expr.kind {
            ExprKind::Int { value, suffix, .. } => Ok(match suffix {
                Some(suffix) if !suffix.is_signed() => format!("Value::UInt({}u64)", value),
                _ => format!("Value::Int({}i64)", value),
            }),
            ExprKind::Float(value) => Ok(format!("Value::Float({:?})", value)),
            ExprKind::Str(value) => Ok(format!("Value::string({:?})", value)),
            ExprKind::Char(value) => Ok(format!("Value::Char({:?}.to_string())", value)),
            ExprKind::Bool(value) => Ok(format!("Value::Bool({})", value)),
            ExprKind::Null => Ok("Value::Null".to_string()),
            ExprKind::None => Ok("Value::None".to_string()),
            ExprKind::Ident(name) => {
                if let Some(var) = self.lookup(name) {
                    return Ok(format!("{}.clone()", var));
                }
                if self.globals.contains(name) {
                    return Ok(format!("host.get_global({:?})", name));
                }
                if let Some(rust_name) = self.funcs.get(name).cloned() {
                    let arity = self.function_arity(name)?;
                    return Ok(format!(
                        "Value::func_rc(Rc::new(Function {{ name: {:?}.to_string(), kind: FuncKind::Native {{ arity: {}, body: Rc::new(|host: &mut Runtime, args: Vec<Value>, _line: u32| {}(host, args)) }} }}))",
                        name, arity, rust_name
                    ));
                }
                Err(format!("AOT: unknown identifier '{}' (line {})", name, line))
            }
            ExprKind::Field { base, name } => {
                // 枚举单元变体：`status.active`（base 是类型名，不是变量）。
                if let ExprKind::Ident(enum_name) = &base.kind {
                    if self.lookup(enum_name).is_none()
                        && !self.globals.contains(enum_name)
                        && !self.imports.contains_key(enum_name)
                    {
                        if let Some(info) = self.checked.enums.get(enum_name) {
                            if let Some(variant) =
                                info.variants.iter().find(|variant| &variant.name == name)
                            {
                                if variant.fields.is_empty() {
                                    return Ok(format!(
                                        "host.make_enum({:?}, {:?}, vec![])",
                                        enum_name, name
                                    ));
                                }
                                return Err(format!(
                                    "AOT: variant '{}.{}' has fields; construct it with '[]' (line {})",
                                    enum_name, name, line
                                ));
                            }
                        }
                    }
                }
                let base_code = self.expr(base)?;
                let (setup, vars) = self.hoist(&[base_code]);
                Ok(format!(
                    "{{ {}host.value_field({}, {:?}, {})? }}",
                    setup, vars[0], name, line
                ))
            }
            ExprKind::Call { callee, args, .. } => self.call(callee, args, line),
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                let mut codes = Vec::new();
                for arg in args {
                    codes.push(self.expr(arg)?);
                }
                if let ExprKind::Ident(alias) = &receiver.kind {
                    if let Some(path) = self.imports.get(alias).cloned() {
                        return self.module_call(&path, name, &codes, line);
                    }
                }
                let receiver_code = self.expr(receiver)?;
                codes.insert(0, receiver_code);
                let (setup, vars) = self.hoist(&codes);
                let args_code = format!("vec![{}]", vars[1..].join(", "));
                let code = format!(
                    "{{ {}host.call_method({}, {:?}, {}, {})? }}",
                    setup, vars[0], name, args_code, line
                );
                Ok(self.width_check(expr, code, line))
            }
            ExprKind::Unary { op, expr: inner } => {
                let inner_code = self.expr(inner)?;
                let (setup, vars) = self.hoist(&[inner_code]);
                let code = format!(
                    "{{ {}host.unary_value({}, {}, {})? }}",
                    setup,
                    unop_code(self.host, *op),
                    vars[0],
                    line
                );
                Ok(self.width_check(expr, code, line))
            }
            ExprKind::Binary { op, left, right } => match op {
                BinOp::And | BinOp::Or => {
                    let left_code = self.expr(left)?;
                    let right_code = self.expr(right)?;
                    let (setup_left, vars_left) = self.hoist(&[left_code]);
                    let (setup_right, vars_right) = self.hoist(&[right_code]);
                    if *op == BinOp::And {
                        Ok(format!(
                            "{{ {}Value::Bool(if host.truthy({}, {})? {{ {}host.truthy({}, {})? }} else {{ false }}) }}",
                            setup_left, vars_left[0], line, setup_right, vars_right[0], line
                        ))
                    } else {
                        Ok(format!(
                            "{{ {}Value::Bool(if host.truthy({}, {})? {{ true }} else {{ {}host.truthy({}, {})? }}) }}",
                            setup_left, vars_left[0], line, setup_right, vars_right[0], line
                        ))
                    }
                }
                other => {
                    let left_code = self.expr(left)?;
                    let right_code = self.expr(right)?;
                    let (setup, vars) = self.hoist(&[left_code, right_code]);
                    // 两侧静态类型都是 int 时走原生快速路径：跳过字符串尝试与通用 match。
                    let entry = if self.is_int(left) && self.is_int(right) {
                        "int_binary"
                    } else {
                        "binary_values"
                    };
                    let code = format!(
                        "{{ {}host.{}({}, {}, {}, {})? }}",
                        setup,
                        entry,
                        binop_code(self.host, *other),
                        vars[0],
                        vars[1],
                        line
                    );
                    Ok(self.width_check(expr, code, line))
                }
            },
            ExprKind::List(items) => self.list_literal(items, expr),
            ExprKind::Struct { path, fields } => {
                let mut codes = Vec::new();
                let mut names = Vec::new();
                for (name, value) in fields {
                    names.push(format!("{:?}.to_string()", name));
                    codes.push(self.expr(value)?);
                }
                let (setup, vars) = self.hoist(&codes);
                let fields_code: Vec<String> = names
                    .into_iter()
                    .zip(vars.iter())
                    .map(|(name, var)| format!("({}, {})", name, var))
                    .collect();
                // `枚举.变体[字段 = 值]`：编译期就能区分枚举构造与普通结构体。
                if path.len() == 2 && self.checked.enums.contains_key(&path[0]) {
                    return Ok(format!(
                        "{{ {}host.make_enum({:?}, {:?}, vec![{}]) }}",
                        setup,
                        path[0],
                        path[1],
                        fields_code.join(", ")
                    ));
                }
                Ok(format!(
                    "{{ {}host.make_struct({:?}, vec![{}]) }}",
                    setup,
                    path.join("."),
                    fields_code.join(", ")
                ))
            }
            ExprKind::Some(inner) => {
                let code = self.expr(inner)?;
                Ok(format!("Value::Some(Box::new({}))", code))
            }
            ExprKind::Ok(inner) => {
                let code = self.expr(inner)?;
                Ok(format!("Value::Ok(Box::new({}))", code))
            }
            ExprKind::Err(inner) => {
                let code = self.expr(inner)?;
                Ok(format!("Value::Err(Box::new({}))", code))
            }
            ExprKind::Try(inner) => {
                let code = self.expr(inner)?;
                let value_var = format!("__try_{}", self.next_id());
                Ok(format!(
                    "{{ let {} = {}; match {} {{ Value::Ok(inner) => *inner, Value::Err(error) => return Err(Flow::Propagate(Value::Err(error))), other => return Err(host.fail(format!(\"'?' requires result, found {{}}\", format_value(&other)), {})) }} }}",
                    value_var, code, value_var, line
                ))
            }
            ExprKind::Await(inner) => {
                let code = self.expr(inner)?;
                Ok(format!("host.await_task({}, {})?", code, line))
            }
            ExprKind::Closure { params, body, .. } => self.closure(params, body, line),
        }
    }

    /// 静态类型是固定位宽整数时，给表达式结果套一层范围检查（与解释器一致：
    /// 同类型运算溢出在运算处 panic，而不是等到赋值）。
    fn width_check(&self, expr: &Expr, code: String, line: u32) -> String {
        let suffix = match self
            .checked
            .expr_types
            .get(&expr.id)
            .and_then(|ty| ty.int_suffix())
        {
            Some(suffix) if suffix != IntSuffix::I64 => suffix,
            _ => return code,
        };
        format!(
            "match coerce_int_name({}, {:?}, {}) {{ Ok(value) => value, Err(message) => return Err(host.fail(message, {})) }}",
            code,
            suffix.name(),
            line,
            line
        )
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], line: u32) -> Result<String, String> {
        let mut codes = Vec::new();
        for arg in args {
            codes.push(self.expr(arg)?);
        }
        match &callee.kind {
            ExprKind::Ident(name) if name == "print" => {
                let (setup, vars) = self.hoist(&codes);
                Ok(format!(
                    "{{ {}host.print_values(vec![{}]); Value::Null }}",
                    setup,
                    vars.join(", ")
                ))
            }
            ExprKind::Ident(name) => {
                if self.async_funcs.contains(name) {
                    let (setup, vars) = self.hoist(&codes);
                    return Ok(format!(
                        "{{ {}host.call_async({:?}, vec![{}], {})? }}",
                        setup,
                        name,
                        vars.join(", "),
                        line
                    ));
                }
                if let Some(rust_name) = self.funcs.get(name).cloned() {
                    let (setup, vars) = self.hoist(&codes);
                    return Ok(format!(
                        "{{ {}{}(host, vec![{}])? }}",
                        setup,
                        rust_name,
                        vars.join(", ")
                    ));
                }
                let callee_code = self.expr(callee)?;
                self.call_value(callee_code, &codes, line)
            }
            ExprKind::Field { base, name } => {
                if let ExprKind::Ident(alias) = &base.kind {
                    if let Some(path) = self.imports.get(alias).cloned() {
                        return self.module_call(&path, name, &codes, line);
                    }
                }
                let callee_code = self.expr(callee)?;
                self.call_value(callee_code, &codes, line)
            }
            _ => {
                let callee_code = self.expr(callee)?;
                self.call_value(callee_code, &codes, line)
            }
        }
    }

    fn call_value(
        &mut self,
        callee_code: String,
        arg_codes: &[String],
        line: u32,
    ) -> Result<String, String> {
        let mut codes = vec![callee_code];
        codes.extend_from_slice(arg_codes);
        let (setup, vars) = self.hoist(&codes);
        let args_code = vars[1..].join(", ");
        Ok(format!(
            "{{ {}match {} {{ Value::Func(func) => host.call_value(&func, vec![{}], {})?, other => return Err(host.fail(format!(\"cannot call value {{}}\", format_value(&other)), {})) }} }}",
            setup, vars[0], args_code, line, line
        ))
    }

    /// 模块/第三方包调用：std/rust 走运行时，第三方包解析到合并后的本地函数。
    fn module_call(
        &mut self,
        path: &str,
        name: &str,
        arg_codes: &[String],
        line: u32,
    ) -> Result<String, String> {
        let (setup, vars) = self.hoist(arg_codes);
        let args_code = vars.join(", ");
        // 静态链接的原生包：直接调用链接进产物的符号，产物无需随带 lib/native。
        if let Some((index, package)) = self
            .static_natives
            .iter()
            .enumerate()
            .find(|(_, package)| package.package == path)
        {
            let returns_result = package
                .funcs
                .iter()
                .find(|(declared, _)| declared == name)
                .map(|(_, returns_result)| *returns_result)
                .ok_or_else(|| {
                    format!(
                        "native package \"{}\" has no function '{}' (line {})",
                        path, name, line
                    )
                })?;
            let module = static_module_name(index);
            return Ok(format!(
                "{{ {}host.call_static_native({:?}, vec![{}], {}, {}::call, {}::free, {})? }}",
                setup, name, args_code, line, module, module, returns_result
            ));
        }
        if !path.starts_with("std/") && !path.starts_with("rust/") {
            if self.async_funcs.contains(name) {
                return Ok(format!(
                    "{{ {}host.call_async({:?}, vec![{}], {})? }}",
                    setup, name, args_code, line
                ));
            }
            if let Some(rust_name) = self.funcs.get(name).cloned() {
                return Ok(format!(
                    "{{ {}{}(host, vec![{}])? }}",
                    setup, rust_name, args_code
                ));
            }
        }
        Ok(format!(
            "{{ {}host.call_module({:?}, {:?}, vec![{}], {})? }}",
            setup, path, name, args_code, line
        ))
    }

    fn list_literal(&mut self, items: &[ListItem], whole: &Expr) -> Result<String, String> {
        let ty = self.ty_of(whole);
        let all_named = items
            .iter()
            .all(|item| matches!(item, ListItem::Named(..) | ListItem::Key(..)));
        match ty {
            Type::Map(_, _) => self.map_literal(items, whole.line),
            Type::Ary(_) => self.ary_literal(items, &ty, whole.line),
            Type::Vec(_) | Type::VecHetero(_) | Type::VecDynamic => self.vec_literal(items),
            _ => {
                if all_named {
                    self.map_literal(items, whole.line)
                } else {
                    self.vec_literal(items)
                }
            }
        }
    }

    fn map_literal(&mut self, items: &[ListItem], line: u32) -> Result<String, String> {
        let mut entries = Vec::new();
        for item in items {
            match item {
                ListItem::Named(name, value) => {
                    let code = self.expr(value)?;
                    entries.push(format!("(Value::string({:?}), {})", name, code));
                }
                ListItem::Key(key, value) => {
                    let key_code = self.expr(key)?;
                    let value_code = self.expr(value)?;
                    entries.push(format!("({}, {})", key_code, value_code));
                }
                _ => {
                    return Err(format!(
                        "AOT: map literal requires key = value entries (line {})",
                        line
                    ))
                }
            }
        }
        Ok(format!("Value::map(vec![{}])", entries.join(", ")))
    }

    fn vec_literal(&mut self, items: &[ListItem]) -> Result<String, String> {
        let mut values = Vec::new();
        for item in items {
            let value = match item {
                ListItem::Value(value)
                | ListItem::Named(_, value)
                | ListItem::Key(_, value)
                | ListItem::Typed(_, value) => value,
            };
            values.push(self.expr(value)?);
        }
        Ok(format!("Value::vec(vec![{}])", values.join(", ")))
    }

    fn ary_literal(&mut self, items: &[ListItem], ty: &Type, line: u32) -> Result<String, String> {
        let class = self.class_name(ty);
        let mut fields = Vec::new();
        for item in items {
            match item {
                ListItem::Named(name, value) => {
                    let code = self.expr(value)?;
                    fields.push(format!("({:?}.to_string(), {})", name, code));
                }
                _ => {
                    return Err(format!(
                        "AOT: ary literal requires named fields (line {})",
                        line
                    ))
                }
            }
        }
        Ok(format!(
            "Value::ary({:?}, vec![{}])",
            class,
            fields.join(", ")
        ))
    }

    fn closure(&mut self, params: &[Param], body: &[Stmt], _line: u32) -> Result<String, String> {
        // 捕获当前作用域可见的全部局部变量（与解释器拷贝 Env 的语义一致）。
        let mut captured: Vec<(String, String)> = Vec::new();
        let mut seen = HashSet::new();
        for scope in self.scopes.iter().rev() {
            for (name, var) in scope {
                if seen.insert(name.clone()) {
                    captured.push((name.clone(), var.clone()));
                }
            }
        }
        let mut capture_code = String::new();
        for (name, var) in &captured {
            let _ = write!(
                capture_code,
                "({:?}.to_string(), {}.clone()), ",
                name, var
            );
        }

        // 生成闭包函数体：参数 + 捕获变量都映射到本地 Rust 变量。
        let saved_scopes = std::mem::take(&mut self.scopes);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_body = std::mem::take(&mut self.body);
        let saved_indent = self.indent;
        self.indent = 2;
        self.scopes.push(HashMap::new());
        let mut param_names = Vec::new();
        for (index, param) in params.iter().enumerate() {
            let var = format!("v{}_{}", self.next_id(), param.name);
            param_names.push(format!(
                "let mut {} = __args.get({}).cloned().unwrap_or(Value::Null);",
                var, index
            ));
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(param.name.clone(), var);
            }
        }
        for stmt in &param_names {
            self.line(stmt);
        }
        for (name, _) in &captured {
            let var = format!("v{}_{}", self.next_id(), name);
            self.line(&format!(
                "let mut {} = __captured.iter().find(|(key, _)| key == {:?}).map(|(_, value)| value.clone()).unwrap_or(Value::Null);",
                var, name
            ));
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(name.clone(), var);
            }
        }
        self.line("host.defer_begin();");
        self.line("let __result = (|| -> FlowResult<Value> {");
        self.indent += 1;
        for stmt in body {
            self.stmt(stmt)?;
        }
        self.line("Ok(Value::Null)");
        self.indent -= 1;
        self.line("})();");
        self.line("let __deferred = host.defer_end();");
        self.line("let __result = match __result { Err(Flow::Propagate(value)) => Ok(value), other => other };");
        self.line("__deferred.and(__result)");
        let closure_body = std::mem::replace(&mut self.body, saved_body);
        self.scopes = saved_scopes;
        self.loops = saved_loops;
        self.indent = saved_indent;

        Ok(format!(
            "{{ let __captured: Vec<(String, Value)> = vec![{}]; Value::func_rc(Rc::new(Function {{ name: \"<closure>\".to_string(), kind: FuncKind::Native {{ arity: {}, body: Rc::new(move |host: &mut Runtime, __args: Vec<Value>, _line: u32| -> FlowResult<Value> {{\n{}\n}}) }} }})) }}",
            capture_code.trim_end_matches(", "),
            params.len(),
            closure_body.trim_end_matches('\n')
        ))
    }

    fn function_arity(&self, name: &str) -> Result<usize, String> {
        for item in &self.program.items {
            if let Item::Func(decl) = item {
                if decl.name == name {
                    return Ok(decl.params.len());
                }
            }
        }
        Err(format!("AOT: unknown function '{}'", name))
    }

    fn ty_of(&self, expr: &Expr) -> Type {
        self.checked
            .expr_types
            .get(&expr.id)
            .map(|ty| self.checked.type_table.resolve(ty))
            .unwrap_or(Type::Unknown)
    }

    /// 表达式静态类型是否确定是 `int`（用于选择原生整数快速路径）。
    fn is_int(&self, expr: &Expr) -> bool {
        matches!(self.ty_of(expr), Type::Int)
    }

    fn class_name(&self, ty: &Type) -> String {
        if let Type::Named(name) = ty {
            return name.clone();
        }
        for (name, def) in &self.checked.type_table.defs {
            if &self.checked.type_table.resolve(def) == ty {
                return name.clone();
            }
        }
        String::new()
    }

    fn lookup(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(value) = scope.get(name) {
                return Some(value.clone());
            }
        }
        None
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn new_var(&mut self, name: &str) -> String {
        let var = format!("v{}_{}", self.next_id(), name);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), var.clone());
        }
        var
    }

    fn next_id(&mut self) -> usize {
        self.counter += 1;
        self.counter
    }

    /// 把若干子表达式提前求值到临时变量，避免 `host` 的嵌套可变借用。
    fn hoist(&mut self, codes: &[String]) -> (String, Vec<String>) {
        let mut setup = String::new();
        let mut vars = Vec::with_capacity(codes.len());
        for code in codes {
            let var = format!("__h{}", self.next_id());
            let _ = write!(setup, "let {} = {}; ", var, code);
            vars.push(var);
        }
        (setup, vars)
    }

    fn line(&mut self, code: &str) {
        for _ in 0..self.indent {
            self.body.push_str("    ");
        }
        self.body.push_str(code);
        self.body.push('\n');
    }
}

fn static_module_name(index: usize) -> String {
    format!("rip_static_native_{}", index)
}

fn field_path(expr: &Expr) -> Option<(String, Vec<String>)> {
    match &expr.kind {
        ExprKind::Ident(name) => Some((name.clone(), Vec::new())),
        ExprKind::Field { base, name } => {
            let (root, mut path) = field_path(base)?;
            path.push(name.clone());
            Some((root, path))
        }
        _ => None,
    }
}

fn unop_code(host: &str, op: UnOp) -> String {
    let variant = match op {
        UnOp::Not => "Not",
        UnOp::Neg => "Neg",
    };
    format!("{}::UnOp::{}", host, variant)
}

fn binop_code(host: &str, op: BinOp) -> String {
    let variant = match op {
        BinOp::Add => "Add",
        BinOp::Sub => "Sub",
        BinOp::Mul => "Mul",
        BinOp::Div => "Div",
        BinOp::Mod => "Mod",
        BinOp::Eq => "Eq",
        BinOp::Ne => "Ne",
        BinOp::Lt => "Lt",
        BinOp::Gt => "Gt",
        BinOp::Le => "Le",
        BinOp::Ge => "Ge",
        BinOp::And => "And",
        BinOp::Or => "Or",
    };
    format!("{}::BinOp::{}", host, variant)
}

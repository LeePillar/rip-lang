//! 外部原生包（Rust 实现的动态库）的加载与调用。
//!
//! 与内置包共用 `Runtime::call_module` 入口，区别只在这里多做一层
//! 参数序列化与 ABI 校验。插件必须实现：
//!
//! ```c
//! rp_native_abi() -> u32
//! rp_native_decl(out_len: *mut usize) -> *const u8
//! rp_native_call(name_ptr, name_len, args_ptr, args_len, out_len) -> *mut u8
//! rp_native_free(ptr, len)
//! ```

use std::path::Path;

use libloading::{Library, Symbol};
use sha2::Digest as _;

use crate::rt::{FlowResult, Runtime};
use crate::value::{json_to_value, value_to_json, Value};

const ABI_VERSION: u32 = rp_types::native::ABI_VERSION;

type AbiFn = unsafe extern "C" fn() -> u32;
type DeclFn = unsafe extern "C" fn(*mut usize) -> *const u8;

/// 插件的调用入口；AOT 静态链接时由生成代码直接传入该符号的地址。
pub type NativeCallFn =
    unsafe extern "C" fn(*const u8, usize, *const u8, usize, *mut usize) -> *mut u8;
/// 释放插件返回的缓冲区。
pub type NativeFreeFn = unsafe extern "C" fn(*mut u8, usize);

type CallFn = NativeCallFn;
type FreeFn = NativeFreeFn;

/// ABI 符号名。声明里写了 `prefix` 时四个符号统一加 `rp_native_<prefix>_` 前缀，
/// 这样多个原生包可以共存于同一个进程或同一个静态链接产物。
fn symbol_name(prefix: Option<&str>, suffix: &str) -> Vec<u8> {
    let mut name = match prefix {
        Some(prefix) => format!("rp_native_{}_{}", prefix, suffix),
        None => format!("rp_native_{}", suffix),
    };
    name.push('\0');
    name.into_bytes()
}

/// 一个已加载的插件包。`Library` 必须与函数指针同生共死。
pub(crate) struct PluginPackage {
    _lib: Library,
    pub(crate) decl: rp_types::native::NativeDecl,
    call_fn: CallFn,
    free_fn: FreeFn,
}

impl PluginPackage {
    /// 从固定目录加载并校验 ABI、平台与摘要。
    pub(crate) fn load(module: &str) -> Result<Self, String> {
        let dir = rp_rust_interop::package_dir(module)
            .ok_or_else(|| "native package not found in any plugin directory".to_string())?;
        let lib_path = rp_rust_interop::library_file(&dir).ok_or_else(|| {
            format!(
                "{}: no library file for this platform ({})",
                dir.display(),
                std::env::consts::DLL_EXTENSION
            )
        })?;
        let decl_path = dir.join("package.decl");
        let decl_text = std::fs::read_to_string(&decl_path)
            .map_err(|error| format!("cannot read {}: {}", decl_path.display(), error))?;
        let decl = rp_types::native::parse_decl(&decl_text)?;
        if let Some(platform) = &decl.platform {
            let declared = rp_rust_interop::normalize_platform(platform);
            let current = rp_rust_interop::normalize_platform(&rp_rust_interop::platform_tag());
            if declared != current {
                return Err(format!(
                    "built for {}, current platform is {}",
                    declared, current
                ));
            }
        }
        if let Some(expected) = &decl.sha256 {
            let actual = sha256_file(&lib_path)?;
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(format!(
                    "checksum mismatch: expected {}, got {}",
                    expected, actual
                ));
            }
        }
        unsafe {
            let lib = Library::new(&lib_path)
                .map_err(|error| format!("cannot load {}: {}", lib_path.display(), error))?;
            let prefix = decl.prefix.as_deref();
            let abi_name = symbol_name(prefix, "abi");
            let version = {
                let abi: Symbol<AbiFn> = lib.get(&abi_name).map_err(|error| {
                    format!(
                        "missing {}: {}",
                        String::from_utf8_lossy(&abi_name[..abi_name.len() - 1]),
                        error
                    )
                })?;
                abi()
            };
            if version < rp_types::native::MIN_ABI_VERSION || version > ABI_VERSION {
                return Err(format!(
                    "plugin abi {} is incompatible with this build ({}..={})",
                    version,
                    rp_types::native::MIN_ABI_VERSION,
                    ABI_VERSION
                ));
            }
            let decl_name = symbol_name(prefix, "decl");
            let embedded = {
                let decl_fn: Symbol<DeclFn> = lib.get(&decl_name).map_err(|error| {
                    format!(
                        "missing {}: {}",
                        String::from_utf8_lossy(&decl_name[..decl_name.len() - 1]),
                        error
                    )
                })?;
                let mut length = 0usize;
                let pointer = decl_fn(&mut length);
                if pointer.is_null() {
                    return Err("rp_native_decl returned a null pointer".to_string());
                }
                let text = std::str::from_utf8(std::slice::from_raw_parts(pointer, length))
                    .map_err(|error| format!("rp_native_decl returned invalid UTF-8: {}", error))?;
                rp_types::native::parse_decl(text)?
            };
            // 插件内置声明必须与随包 `.rp.decl` 一致，避免签名漂移。
            if embedded.funcs.len() != decl.funcs.len()
                || !embedded
                    .funcs
                    .iter()
                    .zip(decl.funcs.iter())
                    .all(|(a, b)| a.name == b.name && a.params == b.params && a.ret == b.ret)
            {
                return Err("plugin declarations disagree with package.decl".to_string());
            }
            let call_name = symbol_name(prefix, "call");
            let call_fn = {
                let call: Symbol<CallFn> = lib.get(&call_name).map_err(|error| {
                    format!(
                        "missing {}: {}",
                        String::from_utf8_lossy(&call_name[..call_name.len() - 1]),
                        error
                    )
                })?;
                *call
            };
            let free_name = symbol_name(prefix, "free");
            let free_fn = {
                let free: Symbol<FreeFn> = lib.get(&free_name).map_err(|error| {
                    format!(
                        "missing {}: {}",
                        String::from_utf8_lossy(&free_name[..free_name.len() - 1]),
                        error
                    )
                })?;
                *free
            };
            Ok(Self {
                _lib: lib,
                decl,
                call_fn,
                free_fn,
            })
        }
    }

    /// 调用一个函数；参数与返回值都走 JSON 边界。
    pub(crate) fn call(&self, name: &str, args: &[Value]) -> Result<Value, String> {
        invoke_json(self.call_fn, self.free_fn, name, args)
    }
}

/// 通过 C ABI 调用一次插件函数：参数编码为 JSON 数组，响应为 `{"ok":…}` / `{"err":…}`。
fn invoke_json(
    call: CallFn,
    free: FreeFn,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    let payload = serde_json::Value::Array(args.iter().map(value_to_json).collect());
    let encoded = serde_json::to_vec(&payload)
        .map_err(|error| format!("cannot encode arguments: {}", error))?;
    let mut out_len = 0usize;
    let out = unsafe {
        call(
            name.as_ptr(),
            name.len(),
            encoded.as_ptr(),
            encoded.len(),
            &mut out_len,
        )
    };
    if out.is_null() {
        return Err(format!("function '{}' returned no data", name));
    }
    let text =
        unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(out, out_len)).to_string() };
    unsafe { free(out, out_len) };
    let reaction: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("function '{}' returned invalid JSON: {}", name, error))?;
    if let Some(value) = reaction.get("ok") {
        return Ok(json_to_value(value));
    }
    if let Some(message) = reaction.get("err").and_then(|value| value.as_str()) {
        return Err(message.to_string());
    }
    Err(format!(
        "function '{}' returned an unexpected response",
        name
    ))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {}", path.display(), error))?;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{:02x}", byte)).collect())
}

impl Runtime {
    /// 尝试把调用转给外部原生包；返回 `None` 表示这个路径不是原生包。
    pub(crate) fn call_native_package(
        &mut self,
        path: &str,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> Option<FlowResult<Value>> {
        if !path.contains('/') || path.starts_with("std/") {
            return None;
        }
        self.ensure_native_package(path);
        if let Some(message) = self.native_errors.get(path).cloned() {
            return Some(self.error(
                format!("native package \"{}\" cannot be loaded: {}", path, message),
                line,
            ));
        }
        let declared = self
            .native_packages
            .get(path)
            .and_then(|package| package.decl.func(name).cloned());
        if declared.is_none() {
            // 声明里没有这个函数：交回调用方给出统一的“无此函数”错误。
            return Some(self.error(
                format!("package \"{}\" has no function '{}'", path, name),
                line,
            ));
        }
        let returns_result = declared
            .as_ref()
            .map(|func| matches!(func.ret, rp_types::Type::Result(_, _)))
            .unwrap_or(false);
        // 先结束对 self 的借用，再在出错时调用 self.error。
        let outcome = self
            .native_packages
            .get(path)
            .map(|package| package.call(name, &args))
            .unwrap_or_else(|| Err("native package disappeared".to_string()));
        match outcome {
            Ok(value) => Some(Ok(value)),
            Err(message) if returns_result => {
                Some(Ok(Value::Err(Box::new(Value::string(message)))))
            }
            Err(message) => Some(self.error(message, line)),
        }
    }

    /// AOT 静态链接的调用入口：实现已编译进产物，直接调用链接进来的符号。
    ///
    /// `call` / `free` 由生成代码从 `#[link]` 公告的 C 符号取得，
    /// 参数与返回值仍走 JSON 边界，行为与动态加载完全一致。
    pub fn call_static_native(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
        call: NativeCallFn,
        free: NativeFreeFn,
        returns_result: bool,
    ) -> FlowResult<Value> {
        match invoke_json(call, free, name, &args) {
            Ok(value) => Ok(value),
            Err(message) if returns_result => Ok(Value::Err(Box::new(Value::string(message)))),
            Err(message) => self.error(message, line),
        }
    }

    fn ensure_native_package(&mut self, path: &str) {
        if self.native_packages.contains_key(path) || self.native_errors.contains_key(path) {
            return;
        }
        match PluginPackage::load(path) {
            Ok(package) => {
                self.native_packages.insert(path.to_string(), package);
            }
            Err(error) => {
                if std::env::var_os("RIP_NATIVE_DEBUG").is_some() {
                    eprintln!("[native] {}: {}", path, error);
                }
                self.native_errors.insert(path.to_string(), error);
            }
        }
    }
}

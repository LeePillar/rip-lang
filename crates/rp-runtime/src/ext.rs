//! 阶段 0 补齐的标准库命名空间：路径、进程、正则、随机、UUID、压缩。
//!
//! 这些能力原先只能靠字符串拼接或外部进程绕开，是大型项目的常见阻塞点；
//! 语义与 `std/fs` 等既有模块保持一致：可能失败的操作返回 `result<T, string>`。

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::value::{FlowResult, Value};
use crate::Runtime;

impl Runtime {
    /// `std/fs` 的扩展函数；命中不了时返回 `std/fs` 的错误。
    pub(crate) fn fs_ext(&mut self, name: &str, args: Vec<Value>, line: u32) -> FlowResult<Value> {
        // RIP_FS_DENY：运维路径拒绝开关，分发前统一检查路径参数。
        for &index in fs_path_arg_indexes(name) {
            if let Some(text) = args.get(index).and_then(Value::as_str) {
                if let Some(message) = fs_deny_error(&text) {
                    return self.error(message, line);
                }
            }
        }
        match name {
            "append_file" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let content = self.string_arg(&args, 1, name, line)?;
                use std::io::Write as _;
                let result = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut file| file.write_all(content.as_bytes()))
                    .map(|()| content.len() as i64);
                Ok(match result {
                    Ok(bytes) => Value::Ok(Box::new(Value::Int(bytes))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "remove_dir" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::remove_dir_all(&path) {
                    Ok(()) => Value::Ok(Box::new(Value::Bool(true))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "rename" => {
                let from = self.string_arg(&args, 0, name, line)?;
                let to = self.string_arg(&args, 1, name, line)?;
                Ok(match fs::rename(&from, &to) {
                    Ok(()) => Value::Ok(Box::new(Value::Bool(true))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "list_dir_all" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let mut files = Vec::new();
                match collect_files(Path::new(&path), &mut files) {
                    Ok(()) => {
                        files.sort();
                        Ok(Value::Ok(Box::new(Value::vec(
                            files
                                .into_iter()
                                .map(|file| Value::string(file.to_string_lossy().to_string()))
                                .collect(),
                        ))))
                    }
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error.to_string())))),
                }
            }
            "metadata" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let meta = match fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        return Ok(Value::Err(Box::new(Value::string(error.to_string()))))
                    }
                };
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0);
                Ok(Value::Ok(Box::new(Value::ary(
                    "fs.metadata",
                    vec![
                        ("size".to_string(), Value::Int(meta.len() as i64)),
                        ("is_dir".to_string(), Value::Bool(meta.is_dir())),
                        ("modified_ms".to_string(), Value::Int(modified)),
                    ],
                ))))
            }
            "temp_file" => {
                let prefix = self.string_arg(&args, 0, name, line)?;
                match unique_temp_path(&prefix, false) {
                    Ok(path) => Ok(Value::Ok(Box::new(Value::string(
                        path.to_string_lossy().to_string(),
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error)))),
                }
            }
            "temp_dir" => {
                let prefix = self.string_arg(&args, 0, name, line)?;
                match unique_temp_path(&prefix, true) {
                    Ok(path) => Ok(Value::Ok(Box::new(Value::string(
                        path.to_string_lossy().to_string(),
                    )))),
                    Err(error) => Ok(Value::Err(Box::new(Value::string(error)))),
                }
            }
            _ => self.error(format!("std/fs has no function '{}'", name), line),
        }
    }

    /// `std/path`：只做字符串层面的路径运算，不触碰文件系统。
    pub(crate) fn path_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "join" => {
                let left = self.string_arg(&args, 0, name, line)?;
                let right = self.string_arg(&args, 1, name, line)?;
                let joined = Path::new(&left).join(right);
                Ok(Value::string(portable(&joined)))
            }
            "file_name" | "parent" | "extension" | "stem" => {
                let input = self.string_arg(&args, 0, name, line)?;
                let path = Path::new(&input);
                let part: Option<String> = match name {
                    "file_name" => path.file_name().map(|p| p.to_string_lossy().to_string()),
                    "parent" => path.parent().map(|p| portable(p)),
                    "extension" => path.extension().map(|p| p.to_string_lossy().to_string()),
                    _ => path.file_stem().map(|p| p.to_string_lossy().to_string()),
                };
                Ok(match part {
                    Some(part) => Value::Some(Box::new(Value::string(part))),
                    None => Value::None,
                })
            }
            "absolute" => {
                let input = self.string_arg(&args, 0, name, line)?;
                Ok(match fs::canonicalize(&input) {
                    Ok(path) => Value::Ok(Box::new(Value::string(portable(&path)))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "normalize" => {
                let input = self.string_arg(&args, 0, name, line)?;
                Ok(Value::string(normalize_path(&input)))
            }
            _ => self.error(format!("std/path has no function '{}'", name), line),
        }
    }

    /// `std/process`：同步执行子进程并捕获输出。
    pub(crate) fn process_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let output = match name {
            "run" => {
                let command = self.string_arg(&args, 0, name, line)?;
                let mut items: Vec<String> = Vec::new();
                if let Some(Value::Vec(list)) = args.get(1) {
                    for item in list.borrow().iter() {
                        if let Some(text) = item.as_str() {
                            items.push(text);
                        }
                    }
                }
                Command::new(&command).args(&items).output()
            }
            "run_shell" => {
                // 运维开关：设 RIP_DISABLE_SHELL=1 后整个进程禁用完整 shell 逃生口。
                if std::env::var_os("RIP_DISABLE_SHELL").is_some() {
                    return self.error(
                        "process.run_shell is disabled by RIP_DISABLE_SHELL",
                        line,
                    );
                }
                let command = self.string_arg(&args, 0, name, line)?;
                if cfg!(windows) {
                    Command::new("cmd").args(["/C", &command]).output()
                } else {
                    Command::new("sh").args(["-c", &command]).output()
                }
            }
            _ => return self.error(format!("std/process has no function '{}'", name), line),
        };
        Ok(match output {
            Ok(output) => Value::Ok(Box::new(Value::ary(
                "process.output",
                vec![
                    (
                        "code".to_string(),
                        Value::Int(output.status.code().unwrap_or(-1) as i64),
                    ),
                    (
                        "stdout".to_string(),
                        Value::string(String::from_utf8_lossy(&output.stdout).to_string()),
                    ),
                    (
                        "stderr".to_string(),
                        Value::string(String::from_utf8_lossy(&output.stderr).to_string()),
                    ),
                ],
            ))),
            Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
        })
    }

    /// `std/regex`：基于 Rust `regex`，不支持反向引用等回溯语法。
    pub(crate) fn regex_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let pattern = self.string_arg(&args, 0, name, line)?;
        let text = self.string_arg(&args, 1, name, line)?;
        let compiled = match compile_regex(&pattern) {
            Ok(regex) => regex,
            Err(error) => return Ok(Value::Err(Box::new(Value::string(error)))),
        };
        Ok(match name {
            "is_match" => Value::Ok(Box::new(Value::Bool(compiled.is_match(&text)))),
            "find" => match compiled.find(&text) {
                Some(found) => Value::Ok(Box::new(Value::Some(Box::new(Value::string(
                    found.as_str().to_string(),
                ))))),
                None => Value::Ok(Box::new(Value::None)),
            },
            "find_all" => Value::Ok(Box::new(Value::vec(
                compiled
                    .find_iter(&text)
                    .map(|found| Value::string(found.as_str().to_string()))
                    .collect(),
            ))),
            "captures" => match compiled.captures(&text) {
                Some(captures) => Value::Ok(Box::new(Value::vec(
                    captures
                        .iter()
                        .map(|group| {
                            Value::string(group.map(|m| m.as_str()).unwrap_or("").to_string())
                        })
                        .collect(),
                ))),
                None => Value::Ok(Box::new(Value::vec(Vec::new()))),
            },
            "replace" => {
                let replacement = self.string_arg(&args, 2, name, line)?;
                Value::Ok(Box::new(Value::string(
                    compiled
                        .replace_all(&text, replacement.as_str())
                        .to_string(),
                )))
            }
            _ => return self.error(format!("std/regex has no function '{}'", name), line),
        })
    }

    /// `std/rand`：密码学安全随机源（与 `std/crypto.random_hex` 同源）。
    pub(crate) fn rand_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "int" => {
                let min = match args.first().and_then(|value| value.as_int()) {
                    Some(value) => value,
                    None => return self.error("rand.int expects (min: int, max: int)", line),
                };
                let max = match args.get(1).and_then(|value| value.as_int()) {
                    Some(value) => value,
                    None => return self.error("rand.int expects (min: int, max: int)", line),
                };
                if min > max {
                    return self.error(
                        format!("rand.int requires min <= max, found {} > {}", min, max),
                        line,
                    );
                }
                let bound = (max as i128 - min as i128 + 1) as u128;
                let offset = match random_below(bound) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                Ok(Value::Int((min as i128 + offset as i128) as i64))
            }
            "float" => {
                let value = match random_below(1u128 << 53) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                Ok(Value::Float(value as f64 / (1u64 << 53) as f64))
            }
            "bool" => {
                let value = match random_below(2) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                Ok(Value::Bool(value == 1))
            }
            _ => self.error(format!("std/rand has no function '{}'", name), line),
        }
    }

    /// `std/uuid`：目前只提供 v4（随机）与 nil。
    pub(crate) fn uuid_module(
        &mut self,
        name: &str,
        _args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "v4" => match uuid_v4() {
                Ok(value) => Ok(Value::string(value)),
                Err(message) => self.error(format!("cannot generate uuid: {}", message), line),
            },
            "nil" => Ok(Value::string("00000000-0000-0000-0000-000000000000")),
            _ => self.error(format!("std/uuid has no function '{}'", name), line),
        }
    }

    /// `std/compress`：gzip 以 base64 承载，避免 Rip 字符串的 UTF-8 限制。
    pub(crate) fn compress_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "gzip" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                let encoded = encoder
                    .write_all(text.as_bytes())
                    .and_then(|()| encoder.finish());
                Ok(match encoded {
                    Ok(bytes) => Value::Ok(Box::new(Value::string(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ))),
                    Err(error) => Value::Err(Box::new(Value::string(error.to_string()))),
                })
            }
            "gunzip" => {
                let text = self.string_arg(&args, 0, name, line)?;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(text.as_bytes())
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        let mut decoder = GzDecoder::new(&bytes[..]);
                        let mut output = Vec::new();
                        decoder
                            .read_to_end(&mut output)
                            .map(|_| output)
                            .map_err(|error| error.to_string())
                    })
                    .and_then(|bytes| {
                        String::from_utf8(bytes).map_err(|error| error.to_string())
                    });
                Ok(match decoded {
                    Ok(text) => Value::Ok(Box::new(Value::string(text))),
                    Err(error) => Value::Err(Box::new(Value::string(error))),
                })
            }
            _ => self.error(format!("std/compress has no function '{}'", name), line),
        }
    }
}

/// 递归收集目录下的文件路径。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// 词法规范化：折叠 `.` 与 `..`，不解析符号链接。
fn normalize_path(input: &str) -> String {
    let mut out = PathBuf::new();
    for component in Path::new(input).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        ".".to_string()
    } else {
        portable(&out)
    }
}

/// 统一用 `/` 输出路径，避免同一段 Rip 代码在不同平台比较结果不一致。
fn portable(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if std::path::MAIN_SEPARATOR == '\\' {
        text.replace('\\', "/")
    } else {
        text
    }
}

/// 运维开关 RIP_FS_DENY：平台路径分隔符隔开的拒绝前缀列表。
/// `std/fs` 任何操作的路径参数落在前缀之下即报错；进程启动时快照一次，
/// 运行中的代码无法修改进程环境变量，因此该开关不可被业务代码绕过。
pub(crate) fn fs_deny_error(path: &str) -> Option<String> {
    let prefixes = fs_deny_prefixes();
    if prefixes.is_empty() {
        return None;
    }
    let target = absolute_normalized(path);
    prefixes.iter().find(|prefix| path_under(prefix, &target)).map(|prefix| {
        format!(
            "path '{}' is denied by RIP_FS_DENY (under '{}')",
            path,
            prefix.display()
        )
    })
}

fn fs_deny_prefixes() -> &'static [PathBuf] {
    static DENY: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DENY.get_or_init(|| {
        std::env::var_os("RIP_FS_DENY")
            .map(|raw| {
                std::env::split_paths(&raw)
                    .filter(|part| !part.as_os_str().is_empty())
                    .map(|part| absolute_normalized(&part.to_string_lossy()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// 相对路径按当前工作目录补全，再做词法归一化（消解 `.` 与 `..`）。
fn absolute_normalized(path: &str) -> PathBuf {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    };
    PathBuf::from(normalize_path(&joined.to_string_lossy()))
}

/// `target` 是否位于 `prefix` 之下；按组件比较，Windows 下大小写不敏感。
fn path_under(prefix: &Path, target: &Path) -> bool {
    let mut left = prefix.components();
    let mut right = target.components();
    loop {
        match (left.next(), right.next()) {
            (None, _) => return true,
            (Some(a), Some(b)) => {
                if !component_eq(&a, &b) {
                    return false;
                }
            }
            (Some(_), None) => return false,
        }
    }
}

fn component_eq(a: &Component, b: &Component) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str().to_ascii_lowercase() == b.as_os_str().to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// fs 操作中哪些参数是路径（需要过 RIP_FS_DENY 检查）。
pub(crate) fn fs_path_arg_indexes(name: &str) -> &'static [usize] {
    match name {
        "copy_file" | "rename" => &[0, 1],
        "read_file" | "write_file" | "exists" | "make_dir" | "remove_file" | "file_size"
        | "list_dir" | "append_file" | "remove_dir" | "list_dir_all" | "metadata" => &[0],
        _ => &[],
    }
}

/// 在系统临时目录下生成不冲突的路径；`dir` 为真时创建目录。
fn unique_temp_path(prefix: &str, dir: bool) -> Result<PathBuf, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{}-{}-{}-{}", prefix, std::process::id(), stamp, seq);
    let path = std::env::temp_dir().join(name);
    if dir {
        fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    }
    Ok(path)
}

/// 正则缓存：同一模式在进程内只编译一次。
fn regex_cache() -> &'static Mutex<HashMap<String, regex::Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, regex::Regex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn compile_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut cache = regex_cache()
        .lock()
        .map_err(|_| "regex cache is poisoned".to_string())?;
    if let Some(regex) = cache.get(pattern) {
        return Ok(regex.clone());
    }
    let regex = regex::Regex::new(pattern).map_err(|error| error.to_string())?;
    cache.insert(pattern.to_string(), regex.clone());
    Ok(regex)
}

fn random_u64() -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

/// 无偏的 `0..bound` 随机数。
fn random_below(bound: u128) -> Result<u128, String> {
    if bound == 0 {
        return Ok(0);
    }
    let limit = u128::MAX - (u128::MAX % bound);
    loop {
        let value = ((random_u64()? as u128) << 64) | random_u64()? as u128;
        if value < limit {
            return Ok(value % bound);
        }
    }
}

fn uuid_v4() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{:02x}", byte)).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize_path("a/./b/../c"), "a/c");
        assert_eq!(normalize_path("a/.."), ".");
        assert_eq!(normalize_path("../a"), "../a");
    }

    #[test]
    fn deny_matching_is_component_wise() {
        let prefix = PathBuf::from(absolute_normalized("/etc"));
        assert!(path_under(&prefix, &absolute_normalized("/etc/passwd")));
        assert!(path_under(&prefix, &absolute_normalized("/etc")));
        // 前缀必须按整组件匹配，`/etcx` 不算在 `/etc` 之下。
        assert!(!path_under(&prefix, &absolute_normalized("/etcx/passwd")));
        assert!(!path_under(&prefix, &absolute_normalized("/tmp")));
        // `..` 穿越归一化后仍会命中被拒绝前缀。
        assert!(path_under(&prefix, &absolute_normalized("/var/../etc/passwd")));
    }

    #[test]
    fn caches_and_reuses_regex() {
        let first = compile_regex("a+b").expect("valid");
        let second = compile_regex("a+b").expect("valid");
        assert!(first.is_match("aaab"));
        assert!(second.is_match("aaab"));
        assert!(compile_regex("(").is_err());
    }

    #[test]
    fn random_range_is_bounded() {
        for _ in 0..64 {
            let value = random_below(6).expect("random");
            assert!(value < 6);
        }
        assert_eq!(random_below(0).expect("random"), 0);
    }

    #[test]
    fn uuid_has_version_four_layout() {
        let value = uuid_v4().expect("uuid");
        assert_eq!(value.len(), 36);
        assert_eq!(value.as_bytes()[14], b'4');
        assert!(matches!(value.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }
}

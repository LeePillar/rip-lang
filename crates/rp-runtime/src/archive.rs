//! `std/zip`：ZIP 归档的创建、追加、列举、解压与读取。
//!
//! 只启用 deflate（与 `rip build` 打包用的是同一实现），条目名统一使用
//! `/` 分隔；写入与解压都拒绝绝对路径与 `..`，避免 zip-slip。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::rt::{FlowResult, Runtime};
use crate::value::{MapData, Value};

/// 单个条目解压上限：256 MiB。
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
/// 一次解压的总上限：2 GiB。
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

impl Runtime {
    pub(crate) fn zip_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "create" | "add" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let entries = match args.get(1) {
                    Some(Value::Map(map)) => map.borrow().clone(),
                    _ => return self.error("std/zip expects a map<string, string>", line),
                };
                Ok(wrap(
                    write_archive(&path, &entries, name == "add"),
                    Value::Int,
                ))
            }
            "list" => {
                let path = self.string_arg(&args, 0, name, line)?;
                Ok(wrap(list_archive(&path), |value| value))
            }
            "extract" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let dir = self.string_arg(&args, 1, name, line)?;
                Ok(wrap(extract_archive(&path, &dir), Value::Int))
            }
            "read_text" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let entry = self.string_arg(&args, 1, name, line)?;
                Ok(wrap(read_entry(&path, &entry, false), Value::string))
            }
            "read_base64" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let entry = self.string_arg(&args, 1, name, line)?;
                Ok(wrap(read_entry(&path, &entry, true), Value::string))
            }
            "contains" => {
                let path = self.string_arg(&args, 0, name, line)?;
                let entry = self.string_arg(&args, 1, name, line)?;
                Ok(wrap(archive_contains(&path, &entry), Value::Bool))
            }
            _ => self.error(format!("std/zip has no function '{}'", name), line),
        }
    }
}

fn wrap<T>(outcome: Result<T, String>, ok: impl FnOnce(T) -> Value) -> Value {
    match outcome {
        Ok(value) => Value::Ok(Box::new(ok(value))),
        Err(message) => Value::Err(Box::new(Value::string(message))),
    }
}

/// 校验条目名并转成归档内的 `/` 分隔形式。
fn zip_name(raw: &str) -> Result<String, String> {
    if raw.is_empty() || raw.contains('\0') {
        return Err("zip entry name must not be empty".to_string());
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(format!("zip entry name '{}' must be relative", raw));
    }
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("zip entry name '{}' must be relative", raw))
            }
            _ => return Err(format!("unsafe zip entry name '{}'", raw)),
        }
    }
    if parts.is_empty() {
        return Err(format!("unsafe zip entry name '{}'", raw));
    }
    Ok(parts.join("/"))
}

/// 把归档条目名解析成解压目录下的相对路径。
fn safe_relative(raw: &str) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for part in zip_name(raw)?.split('/') {
        out.push(part);
    }
    Ok(out)
}

fn write_archive(path: &str, entries: &MapData, append: bool) -> Result<i64, String> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut writer = if append && Path::new(path).exists() {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("open {}: {}", path, error))?;
        ZipWriter::new_append(file).map_err(|error| format!("open zip {}: {}", path, error))?
    } else {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {}", parent.display(), error))?;
            }
        }
        let file = File::create(path).map_err(|error| format!("create {}: {}", path, error))?;
        ZipWriter::new(file)
    };
    let mut count = 0i64;
    for (key, value) in &entries.entries {
        let name = key.as_str().ok_or_else(|| "zip entry name must be a string".to_string())?;
        let name = zip_name(&name)?;
        let content = value
            .as_str()
            .ok_or_else(|| format!("zip entry '{}' must be a string", name))?;
        writer
            .start_file(name.clone(), options)
            .map_err(|error| format!("write {}: {}", name, error))?;
        writer
            .write_all(content.as_bytes())
            .map_err(|error| format!("write {}: {}", name, error))?;
        count += 1;
    }
    writer
        .finish()
        .map_err(|error| format!("finish {}: {}", path, error))?;
    Ok(count)
}

fn read_archive(path: &str) -> Result<ZipArchive<File>, String> {
    let file = File::open(path).map_err(|error| format!("open {}: {}", path, error))?;
    ZipArchive::new(file).map_err(|error| format!("read zip {}: {}", path, error))
}

fn list_archive(path: &str) -> Result<Value, String> {
    let mut archive = read_archive(path)?;
    let mut items = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("read entry {}: {}", index, error))?;
        items.push(Value::ary(
            "zip.entry",
            vec![
                ("name".to_string(), Value::string(entry.name())),
                ("size".to_string(), Value::Int(entry.size() as i64)),
                (
                    "compressed_size".to_string(),
                    Value::Int(entry.compressed_size() as i64),
                ),
                ("crc32".to_string(), Value::Int(entry.crc32() as i64)),
                ("is_dir".to_string(), Value::Bool(entry.is_dir())),
            ],
        ));
    }
    Ok(Value::vec(items))
}

fn extract_archive(path: &str, dir: &str) -> Result<i64, String> {
    let mut archive = read_archive(path)?;
    let root = Path::new(dir);
    fs::create_dir_all(root).map_err(|error| format!("create {}: {}", root.display(), error))?;
    let mut total = 0u64;
    let mut count = 0i64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("read entry {}: {}", index, error))?;
        let relative = safe_relative(entry.name())?;
        let target = root.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("create {}: {}", target.display(), error))?;
            continue;
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(format!(
                "entry '{}' is too large ({} bytes, limit {})",
                entry.name(),
                entry.size(),
                MAX_ENTRY_BYTES
            ));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "archive expands beyond {} bytes",
                MAX_TOTAL_BYTES
            ));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {}", parent.display(), error))?;
        }
        let mut file =
            File::create(&target).map_err(|error| format!("create {}: {}", target.display(), error))?;
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_ENTRY_BYTES + 1), &mut file)
            .map_err(|error| format!("extract {}: {}", target.display(), error))?;
        if copied > MAX_ENTRY_BYTES {
            return Err(format!(
                "entry '{}' expands beyond {} bytes",
                entry.name(),
                MAX_ENTRY_BYTES
            ));
        }
        count += 1;
    }
    Ok(count)
}

fn read_entry(path: &str, entry: &str, base64: bool) -> Result<String, String> {
    let mut archive = read_archive(path)?;
    let mut file = archive
        .by_name(entry)
        .map_err(|_| format!("entry '{}' not found in {}", entry, path))?;
    if file.size() > MAX_ENTRY_BYTES {
        return Err(format!(
            "entry '{}' is too large ({} bytes, limit {})",
            entry,
            file.size(),
            MAX_ENTRY_BYTES
        ));
    }
    let mut bytes = Vec::with_capacity(file.size().min(64 * 1024) as usize);
    file.by_ref()
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {}", entry, error))?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(format!(
            "entry '{}' expands beyond {} bytes",
            entry, MAX_ENTRY_BYTES
        ));
    }
    if base64 {
        return Ok(base64::engine::general_purpose::STANDARD.encode(&bytes));
    }
    String::from_utf8(bytes)
        .map_err(|_| format!("entry '{}' is not valid UTF-8; use read_base64", entry))
}

fn archive_contains(path: &str, entry: &str) -> Result<bool, String> {
    let mut archive = read_archive(path)?;
    let found = archive.by_name(entry).is_ok();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_names_reject_traversal() {
        assert_eq!(zip_name("a/b.txt").unwrap(), "a/b.txt");
        assert!(zip_name("../evil").is_err());
        assert!(zip_name("/etc/passwd").is_err());
        assert!(zip_name("").is_err());
    }

    #[test]
    fn round_trip_archive() {
        let path = std::env::temp_dir().join("rip_zip_test.zip");
        let _ = fs::remove_file(&path);
        let mut entries = MapData::default();
        entries.set(Value::string("a.txt"), Value::string("hello"));
        entries.set(Value::string("dir/b.txt"), Value::string("world"));
        let count = write_archive(path.to_str().unwrap(), &entries, false).unwrap();
        assert_eq!(count, 2);

        let listed = match list_archive(path.to_str().unwrap()).unwrap() {
            Value::Vec(items) => items.borrow().len(),
            _ => 0,
        };
        assert_eq!(listed, 2);
        assert_eq!(
            read_entry(path.to_str().unwrap(), "dir/b.txt", false).unwrap(),
            "world"
        );
        assert!(archive_contains(path.to_str().unwrap(), "a.txt").unwrap());
        assert!(!archive_contains(path.to_str().unwrap(), "missing.txt").unwrap());

        let dir = std::env::temp_dir().join("rip_zip_test_out");
        let _ = fs::remove_dir_all(&dir);
        let extracted = extract_archive(path.to_str().unwrap(), dir.to_str().unwrap()).unwrap();
        assert_eq!(extracted, 2);
        assert_eq!(fs::read_to_string(dir.join("dir/b.txt")).unwrap(), "world");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&path);
    }
}

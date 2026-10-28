//! 原生包（Rust 实现）在 Rip 侧的统一入口：声明表、目录约定与安装。
//!
//! 目录约定（可放在 `rip` 安装目录或 `RIP_NATIVE_PATH` 指定的任意根下）：
//!
//! ```text
//! <root>/acme/tools/
//! ├── package.decl      # 接口声明（唯一签名来源）
//! └── tools.dll         # 平台动态库（libtools.so / libtools.dylib）
//! ```
//!
//! 包路径即相对路径：上例为 `acme/tools`，代码里写 `import "acme/tools"`。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 是否把当前工作目录纳入原生包搜索根。
/// 缺省关闭：打包交付的二进制不搜索 CWD，避免在恶意工作目录启动时
/// 误加载 `./lib/native` 下的动态库；只有 `rip` CLI（源码运行与构建）
/// 在启动时显式打开，保留本地开发布局的便利性。
static SEARCH_CWD: AtomicBool = AtomicBool::new(false);

/// 由 `rip` CLI 在进程启动时调用，开启工作目录搜索根。
pub fn set_search_cwd(enabled: bool) {
    SEARCH_CWD.store(enabled, Ordering::Relaxed);
}

pub const PACKAGES: &[(&str, &str, &[&str])] = &[(
    "rust/serde_json",
    "serde_json",
    &["to_string", "to_string_pretty", "from_str"],
)];

pub fn is_known(package: &str) -> bool {
    PACKAGES.iter().any(|(name, _, _)| *name == package)
}

/// 当前平台标识，与 `.rp.decl` 的 `platform` 字段对应。
pub fn platform_tag() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" | "x64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => other,
    };
    format!("{}-{}", std::env::consts::OS, arch)
}

/// 归一化声明里的平台写法（`windows-x64` 与 `windows-x86_64` 等价）。
pub fn normalize_platform(tag: &str) -> String {
    let tag = tag.trim().to_ascii_lowercase();
    let (os, arch) = match tag.split_once('-') {
        Some(parts) => parts,
        None => return tag,
    };
    let arch = match arch {
        "x64" | "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    };
    format!("{}-{}", os, arch)
}

/// 搜索原生包的根目录，按优先级排列。
pub fn plugin_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("RIP_NATIVE_PATH") {
        roots.extend(std::env::split_paths(&dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 交付布局：rip/lib/rip/plugins 与产物旁的 lib/native。
            roots.push(dir.join("lib").join("rip").join("plugins"));
            roots.push(dir.join("lib").join("native"));
            roots.push(dir.join("native"));
        }
    }
    // 开发布局：当前工作目录下的同名目录；仅源码运行/构建模式生效。
    if SEARCH_CWD.load(Ordering::Relaxed) {
        roots.push(PathBuf::from("lib").join("rip").join("plugins"));
        roots.push(PathBuf::from("lib").join("native"));
    }
    roots
}

/// 定位某个包（`acme/tools`）所在目录。
pub fn package_dir(module: &str) -> Option<PathBuf> {
    let relative = module.replace('\\', "/");
    if relative.is_empty() || relative.split('/').any(|part| part == ".." || part.is_empty()) {
        return None;
    }
    for root in plugin_roots() {
        let candidate = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        if candidate.join("package.decl").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 包目录里的动态库文件（按平台扩展名匹配）。
pub fn library_file(dir: &Path) -> Option<PathBuf> {
    let ext = std::env::consts::DLL_EXTENSION;
    let mut found: Option<PathBuf> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let matches = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case(ext))
            .unwrap_or(false);
        if matches {
            // 目录内只应有一个库文件；有多个时取名字排序最前的，保证可复现。
            match &found {
                Some(current) if current <= &path => {}
                _ => found = Some(path),
            }
        }
    }
    found
}

/// 包目录里的静态库文件（`libX.a` / `X.lib`），供 `--aot` 直接链接进产物。
///
/// 排除 `X.dll.a` / `X.dll.lib`：那是动态库的导入库，仍会在运行时要求 DLL。
pub fn static_library_file(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<(u8, PathBuf)> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if stem.to_ascii_lowercase().ends_with(".dll") {
            continue;
        }
        // `.a` 优先（MinGW/GNU 的 staticlib），其次是 MSVC 的 `.lib`。
        let rank = match path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
        {
            Some(ext) if ext == "a" => 0,
            Some(ext) if ext == "lib" => 1,
            _ => continue,
        };
        // 同优先级的多个候选按名字取最小，保证结果可复现。
        let better = match &found {
            Some((current_rank, current_path)) => {
                rank < *current_rank || (rank == *current_rank && path < *current_path)
            }
            None => true,
        };
        if better {
            found = Some((rank, path));
        }
    }
    found.map(|(_, path)| path)
}

/// 静态库对应的链接名：去掉 `lib` 前缀与扩展名。
pub fn static_link_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let name = stem.strip_prefix("lib").unwrap_or(stem);
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// 扫描所有根目录，把 `.rp.decl` 注册进类型检查器，返回注册数量。
pub fn install_native_packages() -> usize {
    let mut seen = HashSet::new();
    let mut count = 0usize;
    for root in plugin_roots() {
        if root.is_dir() {
            scan_root(&root, &root, 0, &mut seen, &mut count);
        }
    }
    count
}

fn scan_root(
    root: &Path,
    dir: &Path,
    depth: usize,
    seen: &mut HashSet<String>,
    count: &mut usize,
) {
    if depth > 4 {
        return;
    }
    let decl_path = dir.join("package.decl");
    if decl_path.is_file() {
        let module = dir
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|value| !value.is_empty());
        if let Some(module) = module {
            if !seen.contains(&module) {
                match std::fs::read_to_string(&decl_path) {
                    Ok(text) => match rp_types::native::parse_decl(&text) {
                        Ok(decl) => {
                            rp_types::native::register_native_package(&module, decl);
                            seen.insert(module);
                            *count += 1;
                        }
                        Err(error) => {
                            eprintln!("warning: {}: {}", decl_path.display(), error);
                        }
                    },
                    Err(error) => {
                        eprintln!("warning: cannot read {}: {}", decl_path.display(), error);
                    }
                }
            }
        }
    }
    let mut children: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(_) => return,
    };
    children.sort();
    for child in children {
        if let Some(name) = child.file_name().and_then(|value| value.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        scan_root(root, &child, depth + 1, seen, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_platform_tags() {
        assert_eq!(normalize_platform("windows-x64"), "windows-x86_64");
        assert_eq!(normalize_platform("WINDOWS-X86_64"), "windows-x86_64");
        assert_eq!(normalize_platform("linux-arm64"), "linux-aarch64");
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(package_dir("../etc").is_none());
        assert!(package_dir("a//b").is_none());
    }

    #[test]
    fn plugin_roots_skip_cwd_unless_enabled() {
        let cwd_root = PathBuf::from("lib").join("native");
        // 打包二进制默认不搜索工作目录；CLI 模式才开启。
        set_search_cwd(false);
        assert!(!plugin_roots().contains(&cwd_root));
        set_search_cwd(true);
        assert!(plugin_roots().contains(&cwd_root));
        set_search_cwd(false);
    }

    #[test]
    fn prefers_static_library_over_import_library() {
        let dir = std::env::temp_dir().join(format!(
            "rip-static-lib-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("libacme.a"), b"static").expect("write static");
        std::fs::write(dir.join("libacme.dll.a"), b"import").expect("write import");
        std::fs::write(dir.join("acme.dll"), b"dynamic").expect("write dll");
        let found = static_library_file(&dir).expect("static library");
        assert_eq!(found.file_name().unwrap(), "libacme.a");
        assert_eq!(static_link_name(&found).as_deref(), Some("acme"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

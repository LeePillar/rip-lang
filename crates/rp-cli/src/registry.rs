//! 远端包源（registry）拉取。
//!
//! 协议 v1，所有路径相对 `rip.toml` 的 `[registry] source`：
//!
//! ```text
//! GET {source}/v1/index/{package}
//!     -> {"versions": [{"version": "1.4.0", "sha256": "<zip 摘要>", "deps": {...}}]}
//!        也接受裸数组 [{"version": "1.4.0", ...}]
//! GET {source}/v1/packages/{package}/{version}.zip
//!     -> 包目录的 zip（rip.toml 与 *.rp 位于包根）
//! ```
//!
//! `source` 支持 `https://`、`http://`、`file://` 与本地目录路径；后两者用于
//! 离线镜像与测试，不需要网络。
//!
//! 安全约束：
//! - 索引上限 4 MiB、包上限 32 MiB、解压总量上限 64 MiB；
//! - zip 条目必须是包内的相对路径，拒绝 `..`、绝对路径与盘符；
//! - 索引提供 `sha256` 时必须与下载内容一致；
//! - 只会覆盖由本模块拉取的包（存在 `.rip-registry` 标记），
//!   手工 vendored 的包不会被静默替换。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value as Json;

use super::semver::{Version, VersionReq};
use super::{
    offline, package_manifest, packages_root_of, read_lock, read_manifest, sha256_hex,
    vendored_package_dir, LockedPackage,
};

/// 索引响应体上限。
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
/// 单个包（zip）上限。
const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
/// 解压后总字节上限。
const MAX_EXTRACT_BYTES: u64 = 64 * 1024 * 1024;
/// 解压条目数上限。
const MAX_ENTRIES: usize = 2000;
/// 由本模块写入的包标记文件。
const MARKER: &str = ".rip-registry";
/// 缺省公共包源：没有 `[registry] source` 也没有 `RIP_REGISTRY` 时使用。
pub(crate) const DEFAULT_REGISTRY: &str = "https://registry.rip-lang.dev";
/// 覆盖缺省包源的环境变量；优先级低于 `[registry] source` 与 `--registry`。
pub(crate) const REGISTRY_ENV: &str = "RIP_REGISTRY";

/// 索引中的一条版本记录。
#[derive(Debug, Clone)]
struct IndexEntry {
    version: String,
    sha256: Option<String>,
    deps: HashMap<String, String>,
}

/// 包源地址：HTTP(S) 或本地目录。
enum RegistrySource {
    Http(String),
    Local(PathBuf),
}

impl RegistrySource {
    fn parse(source: &str, root: &Path) -> Result<RegistrySource, String> {
        let source = source.trim();
        if let Some(rest) = source.strip_prefix("file://") {
            return Ok(RegistrySource::Local(resolve_local(rest, root)));
        }
        if source.starts_with("https://") {
            return Ok(RegistrySource::Http(source.trim_end_matches('/').to_string()));
        }
        if source.starts_with("http://") {
            // 明文源只允许本机开发用 registry（防 MITM 注入包代码）。
            let rest = &source["http://".len()..];
            let host = rest
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
                return Ok(RegistrySource::Http(source.trim_end_matches('/').to_string()));
            }
            return Err(format!(
                "insecure registry source '{}': use https:// (http:// is only allowed for localhost)",
                source
            ));
        }
        // 本地目录：允许尚不存在（`rip pub --registry ./new-registry` 会按需创建）。
        Ok(RegistrySource::Local(resolve_local(source, root)))
    }

    fn describe(&self) -> String {
        match self {
            RegistrySource::Http(base) => base.clone(),
            RegistrySource::Local(dir) => dir.display().to_string(),
        }
    }

    /// 是否为 HTTP(S) 远端源（远端下载强制校验 sha256）。
    fn is_http(&self) -> bool {
        matches!(self, RegistrySource::Http(_))
    }
}

fn resolve_local(value: &str, root: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

/// 已配置好的包源：地址 + 可选令牌。
struct Registry {
    source: RegistrySource,
    token: Option<String>,
    /// 是否在用缺省公共包源：网络错误时据此提示如何换源。
    fallback: bool,
}

impl Registry {
    fn open(root: &Path, manifest: &super::ManifestInfo) -> Result<Registry, String> {
        Self::open_with(root, manifest, None)
    }

    /// `open` 的可选覆盖版本：`rip search` / `rip info` 允许用 `--registry` 临时指定包源。
    ///
    /// 解析顺序：`--registry` > `[registry] source` > 环境变量 `RIP_REGISTRY` > 公共包源。
    fn open_with(
        root: &Path,
        manifest: &super::ManifestInfo,
        source_override: Option<&str>,
    ) -> Result<Registry, String> {
        let configured = source_override
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| manifest.registry.clone())
            .or_else(|| {
                std::env::var(REGISTRY_ENV)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            });
        let (text, fallback) = match configured {
            Some(value) => (value, false),
            None => (DEFAULT_REGISTRY.to_string(), true),
        };
        Ok(Registry {
            source: RegistrySource::parse(&text, root)?,
            token: resolve_token(manifest)?,
            fallback,
        })
    }

    /// 给包源错误补一句「当前用的是哪个源、怎么换」。
    fn explain(&self, error: String) -> String {
        if self.fallback {
            format!(
                "{}（当前使用公共包源 {}；如需其它源，在 rip.toml 写 [registry] source 或设置 {}）",
                error,
                self.describe(),
                REGISTRY_ENV
            )
        } else {
            error
        }
    }

    fn describe(&self) -> String {
        self.source.describe()
    }

    /// 读取一个 registry 路径（HTTP 或本地文件），并限制响应大小。
    fn read(&self, relative: &str, limit: u64) -> Result<Vec<u8>, String> {
        self.read_raw(relative, limit)
            .map_err(|error| self.explain(error))
    }

    fn read_raw(&self, relative: &str, limit: u64) -> Result<Vec<u8>, String> {
        match &self.source {
            RegistrySource::Local(root) => {
                let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
                let meta = fs::metadata(&path)
                    .map_err(|error| format!("error: cannot read {}: {}", path.display(), error))?;
                if meta.len() > limit {
                    return Err(format!(
                        "error: {} is larger than the {} byte limit",
                        path.display(),
                        limit
                    ));
                }
                fs::read(&path)
                    .map_err(|error| format!("error: cannot read {}: {}", path.display(), error))
            }
            RegistrySource::Http(base) => {
                let url = format!("{}/{}", base, relative);
                let response = self
                    .with_auth(ureq::get(&url))
                    .timeout(Duration::from_secs(30))
                    .call()
                    .map_err(|error| format!("error: GET {} failed: {}", url, error))?;
                let mut bytes = Vec::new();
                response
                    .into_reader()
                    .take(limit + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("error: cannot read {}: {}", url, error))?;
                if bytes.len() as u64 > limit {
                    return Err(format!("error: {} exceeded the {} byte limit", url, limit));
                }
                Ok(bytes)
            }
        }
    }

    /// 上传一个 registry 路径；HTTP 源用 `PUT`，本地源直接写文件。
    fn write(&self, relative: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_raw(relative, bytes)
            .map_err(|error| self.explain(error))
    }

    fn write_raw(&self, relative: &str, bytes: &[u8]) -> Result<(), String> {
        match &self.source {
            RegistrySource::Local(root) => {
                let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| format!("error: cannot create {}: {}", parent.display(), error))?;
                }
                fs::write(&path, bytes)
                    .map_err(|error| format!("error: cannot write {}: {}", path.display(), error))
            }
            RegistrySource::Http(base) => {
                let url = format!("{}/{}", base, relative);
                self.with_auth(ureq::put(&url))
                    .timeout(Duration::from_secs(60))
                    .send_bytes(bytes)
                    .map(|_| ())
                    .map_err(|error| format!("error: PUT {} failed: {}", url, error))
            }
        }
    }

    /// 目标路径是否已存在；网络错误按“不存在”处理，交给后续 PUT 报错。
    fn exists(&self, relative: &str) -> bool {
        match &self.source {
            RegistrySource::Local(root) => {
                root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
                    .is_file()
            }
            RegistrySource::Http(base) => {
                let url = format!("{}/{}", base, relative);
                matches!(
                    self.with_auth(ureq::head(&url))
                        .timeout(Duration::from_secs(15))
                        .call(),
                    Ok(_)
                )
            }
        }
    }

    fn with_auth(&self, request: ureq::Request) -> ureq::Request {
        match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {}", token)),
            None => request,
        }
    }
}

/// 解析访问令牌：`[registry] token_env` 指定的变量优先，否则读 `RIP_REGISTRY_TOKEN`。
fn resolve_token(manifest: &super::ManifestInfo) -> Result<Option<String>, String> {
    match manifest.registry_token_env.as_deref() {
        Some(name) => {
            let value = std::env::var(name).unwrap_or_default();
            if value.trim().is_empty() {
                return Err(format!(
                    "error: [registry] token_env names '{}', but that variable is empty or unset",
                    name
                ));
            }
            Ok(Some(value.trim().to_string()))
        }
        None => Ok(std::env::var("RIP_REGISTRY_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())),
    }
}

/// 确保 `[dependencies]`（含传递依赖）都能在本地找到且版本满足约束。
///
/// 在线且配置了包源时先做全局版本求解（有界回溯，选出满足所有约束的唯一版本）；
/// 离线或未配置包源时退回本地 vendored 校验。
/// 返回本次新拉取的 `(包名, 版本)`；失败时返回错误清单。
pub(crate) fn ensure(root: &Path) -> Result<Vec<(String, String)>, Vec<String>> {
    let manifest = read_manifest(root);
    if manifest.dependencies.is_empty() {
        return Ok(Vec::new());
    }
    if !offline() && !manifest.registry_offline {
        // 依赖已全部 vendored 且满足约束时不联网：本地项目不该被包源拖慢或被网络卡住。
        if all_vendored(root, &manifest) {
            return Ok(Vec::new());
        }
        if let Ok(registry) = Registry::open(root, &manifest) {
            return resolve_online(root, &manifest, &registry);
        }
    }
    ensure_vendored(root, &manifest)
}

/// `[dependencies]`（含传递依赖）是否都已在 `packages/` 里且满足约束。
fn all_vendored(root: &Path, manifest: &super::ManifestInfo) -> bool {
    let Some(packages) = packages_root_of(root) else {
        return false;
    };
    let mut queue: Vec<(String, String)> = manifest.dependencies.clone();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some((name, requirement_text)) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Ok(requirement) = VersionReq::parse(&requirement_text) else {
            return false;
        };
        let dir = vendored_package_dir(&packages, &name);
        if !dir.is_dir() {
            return false;
        }
        let (version, deps) = package_manifest(&dir);
        if !version_satisfies(&version, &requirement) {
            return false;
        }
        queue.extend(deps);
    }
    true
}

/// 离线 / 无包源：只校验 `packages/` 里已有的版本。
fn ensure_vendored(
    root: &Path,
    manifest: &super::ManifestInfo,
) -> Result<Vec<(String, String)>, Vec<String>> {
    let locked = read_lock(root).ok().flatten().unwrap_or_default();

    let mut queue: Vec<(String, String)> = manifest.dependencies.clone();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fetched: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    while let Some((name, requirement_text)) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        // 约束写错时交给 resolve_dependencies 报精确位置，这里不重复报错。
        let Ok(requirement) = VersionReq::parse(&requirement_text) else {
            continue;
        };
        let dir = packages_root_of(root).map(|packages| vendored_package_dir(&packages, &name));
        let ready = dir
            .as_ref()
            .filter(|dir| dir.is_dir())
            .map(|dir| {
                let (version, _) = package_manifest(dir);
                version_satisfies(&version, &requirement)
            })
            .unwrap_or(false);
        if !ready {
            match fetch_one(root, &name, &requirement_text, &requirement, &locked) {
                Ok(version) => {
                    eprintln!("fetched {} {}", name, version);
                    fetched.push((name.clone(), version));
                }
                Err(message) => {
                    errors.push(message);
                    continue;
                }
            }
        }
        // 拉取后重新读取依赖，把传递依赖入队。
        if let Some(packages) = packages_root_of(root) {
            let dir = vendored_package_dir(&packages, &name);
            let (_, deps) = package_manifest(&dir);
            for (dep, dep_requirement) in deps {
                if !seen.contains(&dep) {
                    queue.push((dep, dep_requirement));
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(fetched)
    } else {
        Err(errors)
    }
}

/// 在线解析：有界回溯，保证同一包只有一个版本且满足全部约束。
fn resolve_online(
    root: &Path,
    manifest: &super::ManifestInfo,
    registry: &Registry,
) -> Result<Vec<(String, String)>, Vec<String>> {
    let mut constraints: Vec<Constraint> = Vec::new();
    for (name, text) in &manifest.dependencies {
        match VersionReq::parse(text) {
            Ok(req) => constraints.push(Constraint {
                name: name.clone(),
                text: text.clone(),
                req,
                from: "rip.toml".to_string(),
            }),
            Err(error) => {
                return Err(vec![format!(
                    "error: rip.toml: invalid version requirement '{}' for '{}': {}",
                    text, name, error
                )])
            }
        }
    }
    let mut chosen: HashMap<String, IndexEntry> = HashMap::new();
    let mut cache: HashMap<String, Vec<IndexEntry>> = HashMap::new();
    let mut steps = 0u32;
    if let Err(errors) = solve(registry, &constraints, &mut chosen, &mut cache, &mut steps, 0) {
        return Err(errors);
    }

    let mut fetched = Vec::new();
    let mut names: Vec<String> = chosen.keys().cloned().collect();
    names.sort();
    for name in names {
        let entry = match chosen.get(&name) {
            Some(entry) => entry.clone(),
            None => continue,
        };
        let ready = packages_root_of(root)
            .map(|packages| vendored_package_dir(&packages, &name))
            .filter(|dir| dir.is_dir())
            .map(|dir| {
                let (version, _) = package_manifest(&dir);
                version == entry.version
            })
            .unwrap_or(false);
        if ready {
            continue;
        }
        match fetch_selected(root, registry, &name, &entry) {
            Ok(()) => {
                eprintln!("fetched {} {}", name, entry.version);
                fetched.push((name, entry.version));
            }
            Err(error) => return Err(vec![error]),
        }
    }
    Ok(fetched)
}

#[derive(Clone)]
struct Constraint {
    name: String,
    text: String,
    req: VersionReq,
    from: String,
}

/// 深度优先 + 回溯的版本求解：每一步选一个未决定的包，
/// 按版本从高到低尝试，并把该版本的依赖约束带入下一层。
fn solve(
    registry: &Registry,
    constraints: &[Constraint],
    chosen: &mut HashMap<String, IndexEntry>,
    cache: &mut HashMap<String, Vec<IndexEntry>>,
    steps: &mut u32,
    depth: usize,
) -> Result<(), Vec<String>> {
    const MAX_STEPS: u32 = 20_000;
    const MAX_DEPTH: usize = 64;
    if *steps > MAX_STEPS || depth > MAX_DEPTH {
        return Err(vec![
            "error: dependency resolution exceeded the search limit; simplify version constraints"
                .to_string(),
        ]);
    }
    let pending = constraints
        .iter()
        .map(|constraint| constraint.name.clone())
        .find(|name| !chosen.contains_key(name));
    let name = match pending {
        Some(name) => name,
        None => return Ok(()),
    };
    let entries = match cache.get(&name) {
        Some(entries) => entries.clone(),
        None => match fetch_index(registry, &name) {
            Ok(entries) => {
                cache.insert(name.clone(), entries.clone());
                entries
            }
            Err(error) => return Err(vec![error]),
        },
    };
    let requirements: Vec<&Constraint> = constraints
        .iter()
        .filter(|constraint| constraint.name == name)
        .collect();
    let allow_prerelease = requirements
        .iter()
        .any(|constraint| constraint.req.allows_prerelease());
    let mut candidates: Vec<&IndexEntry> = entries
        .iter()
        .filter(|entry| match Version::parse(&entry.version) {
            Ok(version) => {
                requirements
                    .iter()
                    .all(|constraint| constraint.req.matches(&version))
                    && (allow_prerelease || !version.is_prerelease())
            }
            Err(_) => false,
        })
        .collect();
    candidates.sort_by(|left, right| {
        match (Version::parse(&left.version), Version::parse(&right.version)) {
            (Ok(left), Ok(right)) => right.cmp(&left),
            _ => right.version.cmp(&left.version),
        }
    });
    if candidates.is_empty() {
        let detail = requirements
            .iter()
            .map(|constraint| {
                format!(
                    "{} requires '{} {}'",
                    constraint.from, constraint.name, constraint.text
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(vec![format!(
            "error: no version of '{}' satisfies all constraints: {}",
            name, detail
        )]);
    }

    let mut last_errors = None;
    for entry in candidates {
        *steps += 1;
        chosen.insert(name.clone(), entry.clone());
        let mut next: Vec<Constraint> = constraints.to_vec();
        let mut invalid = None;
        for (dep, dep_text) in &entry.deps {
            match VersionReq::parse(dep_text) {
                Ok(req) => next.push(Constraint {
                    name: dep.clone(),
                    text: dep_text.clone(),
                    req,
                    from: format!("{} {}", name, entry.version),
                }),
                Err(error) => {
                    invalid = Some(format!(
                        "error: package '{} {}' has an invalid dependency requirement '{}' for '{}': {}",
                        name, entry.version, dep_text, dep, error
                    ));
                    break;
                }
            }
        }
        if let Some(message) = invalid {
            last_errors = Some(vec![message]);
            chosen.remove(&name);
            continue;
        }
        // 已选版本必须满足新引入的约束。
        let conflict = next.iter().find_map(|constraint| {
            chosen.get(&constraint.name).and_then(|entry| {
                Version::parse(&entry.version)
                    .ok()
                    .filter(|version| !constraint.req.matches(version))
                    .map(|_| {
                        format!(
                            "error: {} requires '{} {}', but {} is selected",
                            constraint.from, constraint.name, constraint.text, entry.version
                        )
                    })
            })
        });
        if let Some(message) = conflict {
            last_errors = Some(vec![message]);
            chosen.remove(&name);
            continue;
        }
        match solve(registry, &next, chosen, cache, steps, depth + 1) {
            Ok(()) => return Ok(()),
            Err(errors) => {
                last_errors = Some(errors);
                chosen.remove(&name);
            }
        }
    }
    chosen.remove(&name);
    Err(last_errors.unwrap_or_else(|| {
        vec![format!("error: cannot resolve a version for '{}'", name)]
    }))
}

/// 索引的只读视图：供 `rip search` / `rip info` 使用。
#[derive(Debug, Clone)]
pub(crate) struct PackageVersion {
    pub version: String,
    pub sha256: Option<String>,
    pub deps: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageSummary {
    pub name: String,
    pub versions: Vec<PackageVersion>,
}

/// 搜索包名；`query` 为空时列出全部。
pub(crate) fn search(
    root: &Path,
    query: &str,
    registry_override: Option<&str>,
) -> Result<Vec<PackageSummary>, String> {
    let manifest = read_manifest(root);
    let registry = Registry::open_with(root, &manifest, registry_override)?;
    let names = registry.list_names()?;
    let needle = query.trim().to_ascii_lowercase();
    let mut result = Vec::new();
    for name in names {
        if !needle.is_empty() && !name.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        match fetch_index(&registry, &name) {
            Ok(entries) => result.push(summarize(name, entries)),
            // 单个包索引损坏不应让整个搜索失败。
            Err(error) => eprintln!("warning: {}", error),
        }
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

/// 查询单个包的全部版本与依赖。
pub(crate) fn info(
    root: &Path,
    name: &str,
    registry_override: Option<&str>,
) -> Result<PackageSummary, String> {
    let manifest = read_manifest(root);
    let registry = Registry::open_with(root, &manifest, registry_override)?;
    let entries = fetch_index(&registry, name)?;
    Ok(summarize(name.to_string(), entries))
}

fn summarize(name: String, entries: Vec<IndexEntry>) -> PackageSummary {
    let mut versions: Vec<PackageVersion> = entries
        .into_iter()
        .map(|entry| PackageVersion {
            version: entry.version,
            sha256: entry.sha256,
            deps: {
                let mut deps: Vec<(String, String)> = entry.deps.into_iter().collect();
                deps.sort_by(|left, right| left.0.cmp(&right.0));
                deps
            },
        })
        .collect();
    versions.sort_by(|left, right| {
        match (Version::parse(&left.version), Version::parse(&right.version)) {
            (Ok(left), Ok(right)) => right.cmp(&left),
            _ => right.version.cmp(&left.version),
        }
    });
    PackageSummary { name, versions }
}

impl Registry {
    /// 包源里的全部包名：本地源直接遍历索引目录，HTTP 源读取 `v1/index.json`。
    fn list_names(&self) -> Result<Vec<String>, String> {
        match &self.source {
            RegistrySource::Local(root) => {
                let base = root.join("v1").join("index");
                let mut names = Vec::new();
                collect_index_names(&base, &base, &mut names);
                names.sort();
                Ok(names)
            }
            RegistrySource::Http(_) => {
                let bytes = self.read("v1/index.json", MAX_INDEX_BYTES).map_err(|error| {
                    // 只有「目录不存在」才提示索引缺失；网络错误保留原文。
                    if error.contains("404") {
                        format!(
                            "{}; the registry should publish v1/index.json for search",
                            error
                        )
                    } else {
                        error
                    }
                })?;
                parse_catalog(&bytes)
            }
        }
    }
}

fn collect_index_names(base: &Path, dir: &Path, names: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_index_names(base, &path, names);
        } else if path.is_file() {
            if let Ok(relative) = path.strip_prefix(base) {
                let name = relative.to_string_lossy().replace('\\', "/");
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
}

fn parse_catalog(bytes: &[u8]) -> Result<Vec<String>, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let value: Json = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let list = match &value {
        Json::Array(items) => items,
        Json::Object(map) => map
            .get("packages")
            .and_then(|packages| packages.as_array())
            .ok_or("expected an array or an object with a 'packages' array")?,
        _ => return Err("expected an array or an object with a 'packages' array".to_string()),
    };
    let mut names = Vec::new();
    for item in list {
        match item {
            Json::String(name) => names.push(name.clone()),
            Json::Object(map) => {
                if let Some(name) = map.get("name").and_then(|value| value.as_str()) {
                    names.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(names)
}

/// 本地版本是否满足约束；没有版本声明的包只在无约束时可接受。
fn version_satisfies(version: &str, requirement: &VersionReq) -> bool {
    if version.is_empty() {
        return requirement.is_any();
    }
    Version::parse(version)
        .map(|parsed| requirement.matches(&parsed))
        .unwrap_or(false)
}

fn fetch_one(
    root: &Path,
    name: &str,
    requirement_text: &str,
    requirement: &VersionReq,
    locked: &[LockedPackage],
) -> Result<String, String> {
    let manifest = read_manifest(root);
    if offline() || manifest.registry_offline {
        return Err(format!(
            "error: package '{}' is not vendored and network access is disabled; run 'rip add {}' online once, or vendor it under packages/",
            name, name
        ));
    }
    let registry = Registry::open(root, &manifest)?;

    // 锁文件优先：保证可复现，即使索引里已出现更新的版本。
    let locked_version = locked
        .iter()
        .find(|package| package.name == name)
        .and_then(|package| {
            Version::parse(&package.version)
                .ok()
                .filter(|version| requirement.matches(version))
                .map(|_| package.version.clone())
        });

    let (version, expected) = match locked_version {
        Some(version) => {
            // 锁定的版本可能已从索引移除，索引只用来拿摘要，拿不到就算。
            let expected = fetch_index(&registry, name)
                .ok()
                .and_then(|entries| entries.into_iter().find(|entry| entry.version == version))
                .and_then(|entry| entry.sha256);
            (version, expected)
        }
        None => {
            let entries = fetch_index(&registry, name)?;
            let entry = select_version(&entries, requirement).ok_or_else(|| {
                format!(
                    "error: no version of '{}' in the registry satisfies '{}'",
                    name, requirement_text
                )
            })?;
            (entry.version.clone(), entry.sha256.clone())
        }
    };

    if registry.source.is_http() && expected.is_none() {
        return Err(format!(
            "error: registry index has no sha256 for '{} {}'; refusing unverified download",
            name, version
        ));
    }
    let archive = registry.read(
        &format!("v1/packages/{}/{}.zip", name, version),
        MAX_ARCHIVE_BYTES,
    )?;
    if let Some(expected) = expected {
        let actual = sha256_hex(&archive);
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err(format!(
                "error: checksum mismatch for '{} {}': expected {}, got {}",
                name, version, expected, actual
            ));
        }
    }

    let packages = packages_root_of(root).unwrap_or_else(|| root.join("packages"));
    let target = vendored_package_dir(&packages, name);
    prepare_target(&target, name, &manifest)?;
    extract(&archive, &target)?;
    write_marker(&target, &registry.describe(), &version)?;
    Ok(version)
}

/// 按索引里给出版本与摘要拉取并安装一个包（供版本求解器使用）。
fn fetch_selected(
    root: &Path,
    registry: &Registry,
    name: &str,
    entry: &IndexEntry,
) -> Result<(), String> {
    if registry.source.is_http() && entry.sha256.is_none() {
        return Err(format!(
            "error: registry index has no sha256 for '{} {}'; refusing unverified download",
            name, entry.version
        ));
    }
    let archive = registry.read(
        &format!("v1/packages/{}/{}.zip", name, entry.version),
        MAX_ARCHIVE_BYTES,
    )?;
    if let Some(expected) = &entry.sha256 {
        let actual = sha256_hex(&archive);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "error: checksum mismatch for '{} {}': expected {}, got {}",
                name, entry.version, expected, actual
            ));
        }
    }
    let manifest = read_manifest(root);
    let packages = packages_root_of(root).unwrap_or_else(|| root.join("packages"));
    let target = vendored_package_dir(&packages, name);
    prepare_target(&target, name, &manifest)?;
    extract(&archive, &target)?;
    write_marker(&target, &registry.describe(), &entry.version)?;
    Ok(())
}

/// 覆盖前检查：只允许替换由 registry 拉取的包。
fn prepare_target(
    target: &Path,
    name: &str,
    manifest: &super::ManifestInfo,
) -> Result<(), String> {
    if !target.exists() {
        return fs::create_dir_all(target)
            .map_err(|error| format!("error: cannot create {}: {}", target.display(), error));
    }
    if target.join(MARKER).is_file() {
        fs::remove_dir_all(target).map_err(|error| {
            format!(
                "error: cannot replace '{}' ({}): {}",
                name,
                target.display(),
                error
            )
        })?;
        return fs::create_dir_all(target)
            .map_err(|error| format!("error: cannot create {}: {}", target.display(), error));
    }
    let (version, _) = package_manifest(target);
    Err(format!(
        "error: '{}' already exists under packages/ with version '{}' and was not fetched by rip; \
         remove it or set the requirement to a matching version (registry: {})",
        name,
        if version.is_empty() { "unknown" } else { &version },
        manifest.registry.as_deref().unwrap_or("<unset>")
    ))
}

fn write_marker(target: &Path, source: &str, version: &str) -> Result<(), String> {
    let text = format!("source = \"{}\"\nversion = \"{}\"\n", source, version);
    fs::write(target.join(MARKER), text)
        .map_err(|error| format!("error: cannot write {}: {}", MARKER, error))
}

fn fetch_index(registry: &Registry, name: &str) -> Result<Vec<IndexEntry>, String> {
    let bytes = registry.read(&format!("v1/index/{}", name), MAX_INDEX_BYTES)?;
    parse_index(&bytes)
        .map_err(|error| format!("error: invalid registry index for '{}': {}", name, error))
}

/// 解析索引；同时接受 `{"versions": [...]}` 与裸数组。
///
/// 部分 registry 会输出带 BOM 的 UTF-8，这里先剥掉 BOM 再交给 JSON 解析。
fn parse_index(bytes: &[u8]) -> Result<Vec<IndexEntry>, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let value: Json = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let list = match &value {
        Json::Array(items) => items.clone(),
        Json::Object(map) => map
            .get("versions")
            .and_then(|versions| versions.as_array())
            .cloned()
            .ok_or("expected an array or an object with a 'versions' array")?,
        _ => return Err("expected an array or an object with a 'versions' array".to_string()),
    };
    let mut entries = Vec::new();
    for item in list {
        let version = item
            .get("version")
            .and_then(|value| value.as_str())
            .ok_or("each version entry needs a 'version' string")?
            .to_string();
        let sha256 = item
            .get("sha256")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
        let mut deps = HashMap::new();
        if let Some(map) = item.get("deps").and_then(|value| value.as_object()) {
            for (key, value) in map {
                if let Some(requirement) = value.as_str() {
                    deps.insert(key.clone(), requirement.to_string());
                }
            }
        }
        entries.push(IndexEntry {
            version,
            sha256,
            deps,
        });
    }
    Ok(entries)
}

/// 选出满足约束的最高版本；未显式要求预发布时跳过预发布版本。
fn select_version<'a>(entries: &'a [IndexEntry], requirement: &VersionReq) -> Option<&'a IndexEntry> {
    let allow_prerelease = requirement.allows_prerelease();
    entries
        .iter()
        .filter_map(|entry| Version::parse(&entry.version).ok().map(|version| (version, entry)))
        .filter(|(version, _)| requirement.matches(version))
        .filter(|(version, _)| allow_prerelease || !version.is_prerelease())
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, entry)| entry)
}

/// 解压包到目标目录；条目路径必须落在目标目录内。
fn extract(bytes: &[u8], target: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("error: package archive is not a valid zip: {}", error))?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!(
            "error: package archive has {} entries (limit {})",
            archive.len(),
            MAX_ENTRIES
        ));
    }
    fs::create_dir_all(target)
        .map_err(|error| format!("error: cannot create {}: {}", target.display(), error))?;
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("error: cannot read archive entry: {}", error))?;
        // `enclosed_name` 拒绝绝对路径、`..` 与盘符前缀。
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!(
                "error: package archive contains an unsafe path '{}'",
                entry.name()
            ));
        };
        if entry.is_dir() {
            continue;
        }
        let destination = target.join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("error: cannot create {}: {}", parent.display(), error))?;
        }
        let mut output = fs::File::create(&destination)
            .map_err(|error| format!("error: cannot write {}: {}", destination.display(), error))?;
        // 按实际写入的字节数计量，避免条目头里声明的大小不可信。
        let remaining = MAX_EXTRACT_BYTES.saturating_sub(total);
        let written = std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut output)
            .map_err(|error| format!("error: cannot write {}: {}", destination.display(), error))?;
        total += written;
        if total > MAX_EXTRACT_BYTES {
            return Err(format!(
                "error: extracted package exceeds the {} byte limit",
                MAX_EXTRACT_BYTES
            ));
        }
    }
    Ok(())
}

/// 发布一个包：打包 → 上传 →（本地源）更新索引。
///
/// 包名优先取相对 `packages/` 的目录路径（`acme/tools`），
/// 不在包目录树内时取 `[module] name`。
pub(crate) fn publish(
    dir: &Path,
    registry_override: Option<&str>,
) -> Result<(String, String, String), String> {
    if offline() {
        return Err("error: cannot publish with --offline".to_string());
    }
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    if !dir.join("rip.toml").is_file() {
        return Err(format!(
            "error: {} is not a package directory (missing rip.toml)",
            dir.display()
        ));
    }
    let (name, version) = package_identity(dir)?;
    let manifest = read_manifest(dir);
    let token = resolve_token(&manifest)?;
    // 发布必须显式指定目标：不回退到公共包源，避免误发。
    let source_text = match registry_override {
        Some(value) => value.to_string(),
        None => match manifest.registry.clone() {
            Some(value) => value,
            None => std::env::var(REGISTRY_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!(
                        "error: no [registry] source configured; pass --registry <url>, add it to rip.toml, or set {}",
                        REGISTRY_ENV
                    )
                })?,
        },
    };
    let registry = Registry {
        source: RegistrySource::parse(&source_text, dir)?,
        token,
        fallback: false,
    };

    let relative = format!("v1/packages/{}/{}.zip", name, version);
    if registry.exists(&relative) {
        return Err(format!(
            "error: {} {} already exists in {}; bump [module] version first",
            name,
            version,
            registry.describe()
        ));
    }
    let archive = build_package_zip(dir)?;
    let sha = sha256_hex(&archive);
    registry.write(&relative, &archive)?;
    // 本地目录源：同时维护索引；HTTP 源由服务端在收到 PUT 后更新。
    if let RegistrySource::Local(root) = &registry.source {
        update_local_index(root, dir, &name, &version, &sha)?;
    }
    Ok((name, version, registry.describe()))
}

/// 包名与版本：名称优先用 `packages/` 下的相对路径。
fn package_identity(dir: &Path) -> Result<(String, String), String> {
    let manifest = read_manifest(dir);
    let version = manifest.version.clone().unwrap_or_default();
    if version.is_empty() {
        return Err(format!(
            "error: {} is missing [module] version",
            dir.join("rip.toml").display()
        ));
    }
    if let Err(error) = Version::parse(&version) {
        return Err(format!(
            "error: package version '{}' is not valid semver: {}",
            version, error
        ));
    }
    let name = match packages_ancestor(dir) {
        Some(packages) => super::package_name_of(dir, &packages),
        None => manifest.name.clone().unwrap_or_default(),
    };
    validate_package_name(&name)?;
    Ok((name, version))
}

/// 向上找到 `packages/`（兼容单数 `package/`）。
fn packages_ancestor(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors().skip(1) {
        let name = ancestor.file_name().and_then(|name| name.to_str());
        if matches!(name, Some("packages") | Some("package")) {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn validate_package_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("error: package name is empty; set [module] name in rip.toml".to_string());
    }
    if name.starts_with('/') || name.contains('\\') || name.contains("://") {
        return Err(format!("error: invalid package name '{}'", name));
    }
    if name
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!("error: invalid package name '{}'", name));
    }
    Ok(())
}

/// 把包目录打成 zip（`rip.toml`、`*.rp`、`package.decl`），条目按路径排序。
fn build_package_zip(dir: &Path) -> Result<Vec<u8>, String> {
    let mut files = Vec::new();
    collect_package_tree(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "error: {} contains no rip.toml, *.rp or package.decl files",
            dir.display()
        ));
    }
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for file in files {
        let relative = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&file)
            .map_err(|error| format!("error: cannot read {}: {}", file.display(), error))?;
        writer
            .start_file(relative, options)
            .map_err(|error| format!("error: cannot add archive entry: {}", error))?;
        writer
            .write_all(&bytes)
            .map_err(|error| format!("error: cannot write archive entry: {}", error))?;
    }
    let cursor = writer
        .finish()
        .map_err(|error| format!("error: cannot finish package archive: {}", error))?;
    Ok(cursor.into_inner())
}

fn collect_package_tree(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("error: cannot read {}: {}", dir.display(), error))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_package_tree(&path, out)?;
            continue;
        }
        let keep = name == "rip.toml"
            || name == "package.decl"
            || path.extension().and_then(|value| value.to_str()) == Some("rp");
        if keep {
            out.push(path);
        }
    }
    Ok(())
}

/// 本地目录源的索引维护：同一版本覆盖，按版本升序输出。
fn update_local_index(
    root: &Path,
    package_dir: &Path,
    name: &str,
    version: &str,
    sha: &str,
) -> Result<(), String> {
    let path = root
        .join("v1")
        .join("index")
        .join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut entries = if path.is_file() {
        let bytes = fs::read(&path)
            .map_err(|error| format!("error: cannot read {}: {}", path.display(), error))?;
        parse_index(&bytes).map_err(|error| {
            format!("error: invalid existing index {}: {}", path.display(), error)
        })?
    } else {
        Vec::new()
    };
    let (_, deps) = package_manifest(package_dir);
    entries.retain(|entry| entry.version != version);
    entries.push(IndexEntry {
        version: version.to_string(),
        sha256: Some(sha.to_string()),
        deps: deps.into_iter().collect(),
    });
    entries.sort_by(|left, right| {
        Version::parse(&left.version)
            .ok()
            .cmp(&Version::parse(&right.version).ok())
    });

    let versions: Vec<Json> = entries
        .iter()
        .map(|entry| {
            let mut object = serde_json::Map::new();
            object.insert("version".to_string(), Json::String(entry.version.clone()));
            if let Some(sha) = &entry.sha256 {
                object.insert("sha256".to_string(), Json::String(sha.clone()));
            }
            if !entry.deps.is_empty() {
                let mut map = serde_json::Map::new();
                for (key, value) in &entry.deps {
                    map.insert(key.clone(), Json::String(value.clone()));
                }
                object.insert("deps".to_string(), Json::Object(map));
            }
            Json::Object(object)
        })
        .collect();
    let value = serde_json::json!({ "versions": versions });
    let mut text = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("error: cannot encode index: {}", error))?;
    text.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("error: cannot create {}: {}", parent.display(), error))?;
    }
    fs::write(&path, text)
        .map_err(|error| format!("error: cannot write {}: {}", path.display(), error))?;
    // 同步维护 `v1/index.json`：HTTP 源由服务端生成，本地源在这里生成，
    // 让 `rip search` 在两种源上行为一致。
    rebuild_local_catalog(root);
    Ok(())
}

/// 扫描本地索引并重写目录文件；失败不影响发布本身。
fn rebuild_local_catalog(root: &Path) {
    let base = root.join("v1").join("index");
    let mut names = Vec::new();
    collect_index_names(&base, &base, &mut names);
    names.sort();
    names.dedup();
    let packages: Vec<Json> = names
        .into_iter()
        .map(|name| serde_json::json!({ "name": name }))
        .collect();
    let value = serde_json::json!({ "packages": packages });
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = fs::create_dir_all(root.join("v1"));
        let _ = fs::write(root.join("v1").join("index.json"), format!("{}\n", text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rip-registry-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
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

    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(content.as_bytes()).expect("write entry");
        }
        writer.finish().expect("finish zip").into_inner()
    }

    /// 建一个本地 registry：`v1/index/<name>` 与 `v1/packages/<name>/<version>.zip`。
    fn registry(tag: &str, name: &str, version: &str, index: &str, entries: &[(&str, &str)]) -> PathBuf {
        let root = project(tag, &[]);
        let index_path = root.join("v1").join("index").join(name);
        fs::create_dir_all(index_path.parent().unwrap()).expect("index dir");
        fs::write(&index_path, index).expect("index");
        let package_path = root
            .join("v1")
            .join("packages")
            .join(name)
            .join(format!("{}.zip", version));
        fs::create_dir_all(package_path.parent().unwrap()).expect("package dir");
        fs::write(&package_path, make_zip(entries)).expect("zip");
        root
    }

    #[test]
    fn parses_object_and_array_index() {
        let object = br#"{"versions":[{"version":"1.2.0"},{"version":"1.3.0","sha256":"abcd"}]}"#;
        let entries = parse_index(object).expect("object");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].sha256.as_deref(), Some("abcd"));

        let array = br#"[{"version":"2.0.0","deps":{"acme/base":"^1"}}]"#;
        let entries = parse_index(array).expect("array");
        assert_eq!(entries[0].deps.get("acme/base").map(String::as_str), Some("^1"));

        assert!(parse_index(b"{}").is_err());
        assert!(parse_index(b"not json").is_err());
    }

    #[test]
    fn selects_highest_matching_version() {
        let entries = parse_index(
            br#"[{"version":"1.2.0"},{"version":"1.9.3"},{"version":"2.0.0"},{"version":"1.10.0-alpha"}]"#,
        )
        .expect("index");
        let requirement = VersionReq::parse("^1.2").expect("requirement");
        assert_eq!(select_version(&entries, &requirement).unwrap().version, "1.9.3");
        let requirement = VersionReq::parse("^3").expect("requirement");
        assert!(select_version(&entries, &requirement).is_none());
    }

    #[test]
    fn fetches_package_from_local_registry() {
        let index = r#"{"versions":[{"version":"1.4.0"}]}"#;
        let registry = registry(
            "fetch",
            "acme/tools",
            "1.4.0",
            index,
            &[
                ("rip.toml", "[module]\nname = \"tools\"\nversion = \"1.4.0\"\n"),
                ("tools.rp", "mod tools;\n\npub func greet() -> string {\n    return \"hi\";\n}\n"),
            ],
        );
        let root = project(
            "consumer",
            &[(
                "rip.toml",
                &format!(
                    "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[registry]\nsource = \"{}\"\n\n[dependencies]\n\"acme/tools\" = \"^1.2\"\n",
                    registry.display()
                ),
            )],
        );
        let fetched = ensure(&root).expect("fetched");
        assert_eq!(fetched, vec![("acme/tools".to_string(), "1.4.0".to_string())]);
        assert!(root.join("packages/acme/tools/tools.rp").is_file());
        assert!(root.join("packages/acme/tools/.rip-registry").is_file());
        // 拉取完成后本地解析应当通过。
        super::super::resolve_dependencies(&root).expect("resolved");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&registry);
    }

    #[test]
    fn refuses_to_overwrite_hand_vendored_package() {
        let index = r#"{"versions":[{"version":"2.0.0"}]}"#;
        let registry = registry(
            "overwrite",
            "acme/tools",
            "2.0.0",
            index,
            &[("rip.toml", "[module]\nname = \"tools\"\nversion = \"2.0.0\"\n")],
        );
        let root = project(
            "overwrite-consumer",
            &[
                (
                    "rip.toml",
                    &format!(
                        "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[registry]\nsource = \"{}\"\n\n[dependencies]\n\"acme/tools\" = \"^2.0\"\n",
                        registry.display()
                    ),
                ),
                ("packages/acme/tools/rip.toml", "[module]\nname = \"tools\"\nversion = \"1.0.0\"\n"),
            ],
        );
        let errors = ensure(&root).expect_err("must refuse");
        assert!(errors.iter().any(|message| message.contains("was not fetched by rip")));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&registry);
    }

    #[test]
    fn rejects_archive_path_traversal() {
        let root = project("traversal", &[]);
        let target = root.join("pkg");
        let archive = make_zip(&[("../escaped.rp", "mod evil;")]);
        let error = extract(&archive, &target).expect_err("must reject");
        assert!(error.contains("unsafe path"));
        assert!(!root.join("escaped.rp").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn vendor_generates_lock_after_fetching() {
        let index = r#"{"versions":[{"version":"1.0.0"}]}"#;
        let registry_root = registry(
            "vendor-lock",
            "acme/tools",
            "1.0.0",
            index,
            &[
                ("rip.toml", "[module]\nname = \"tools\"\nversion = \"1.0.0\"\n"),
                ("tools.rp", "mod tools;\n"),
            ],
        );
        let root = project(
            "vendor-app",
            &[(
                "rip.toml",
                &format!(
                    "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[registry]\nsource = \"{}\"\n\n[dependencies]\n\"acme/tools\" = \"^1.0\"\n",
                    registry_root.display()
                ),
            )],
        );
        assert!(!root.join("rip.lock").exists());

        let count = super::super::generate_lock(&root).expect("generated");
        assert_eq!(count, 1);
        let lock = fs::read_to_string(root.join("rip.lock")).expect("lock");
        assert!(lock.contains("name = \"acme/tools\""), "{}", lock);
        assert!(lock.contains("version = \"1.0.0\""), "{}", lock);
        assert!(root.join("packages/acme/tools/tools.rp").is_file());

        // 包已 vendored 时再次生成应当幂等，不再访问包源。
        let again = super::super::generate_lock(&root).expect("regenerated");
        assert_eq!(again, 1);
        assert_eq!(fs::read_to_string(root.join("rip.lock")).expect("lock"), lock);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&registry_root);
    }

    #[test]
    fn publishes_to_local_registry_and_roundtrips() {
        let registry_root = project("pub-registry", &[(".keep", "")]);
        let package = project(
            "pub-pkg",
            &[
                ("rip.toml", "[module]\nname = \"tools\"\nversion = \"1.2.0\"\n"),
                (
                    "tools.rp",
                    "mod tools;\n\npub func greet() -> string {\n    return \"hi\";\n}\n",
                ),
            ],
        );
        let (name, version, _) =
            publish(&package, Some(registry_root.to_str().unwrap())).expect("published");
        assert_eq!(name, "tools");
        assert_eq!(version, "1.2.0");
        assert!(registry_root.join("v1/packages/tools/1.2.0.zip").is_file());
        let index = fs::read_to_string(registry_root.join("v1/index/tools")).expect("index");
        assert!(index.contains("\"1.2.0\""), "{}", index);
        assert!(index.contains("sha256"), "{}", index);

        // 同一版本重复发布被拒绝。
        let error = publish(&package, Some(registry_root.to_str().unwrap())).expect_err("duplicate");
        assert!(error.contains("already exists"), "{}", error);

        // 消费者能按索引拉取刚发布的包。
        let consumer = project(
            "pub-consumer",
            &[(
                "rip.toml",
                &format!(
                    "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[registry]\nsource = \"{}\"\n\n[dependencies]\n\"tools\" = \"^1.0\"\n",
                    registry_root.display()
                ),
            )],
        );
        let fetched = ensure(&consumer).expect("fetched");
        assert_eq!(fetched, vec![("tools".to_string(), "1.2.0".to_string())]);
        assert!(consumer.join("packages/tools/tools.rp").is_file());
        let _ = fs::remove_dir_all(&consumer);
        let _ = fs::remove_dir_all(&package);
        let _ = fs::remove_dir_all(&registry_root);
    }

    #[test]
    fn publish_uses_packages_relative_name() {
        let root = project(
            "pub-nested",
            &[
                (
                    "packages/acme/tools/rip.toml",
                    "[module]\nname = \"tools\"\nversion = \"0.3.0\"\n",
                ),
                ("packages/acme/tools/tools.rp", "mod tools;\n"),
                ("registry/.keep", ""),
            ],
        );
        let (name, version, _) = publish(
            &root.join("packages/acme/tools"),
            Some(root.join("registry").to_str().unwrap()),
        )
        .expect("published");
        assert_eq!(name, "acme/tools");
        assert_eq!(version, "0.3.0");
        assert!(root.join("registry/v1/index/acme/tools").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_requires_version() {
        let package = project(
            "pub-noversion",
            &[("rip.toml", "[module]\nname = \"tools\"\n"), ("tools.rp", "")],
        );
        let error = publish(&package, Some("unused")).expect_err("must fail");
        assert!(error.contains("[module] version"), "{}", error);
        let _ = fs::remove_dir_all(&package);
    }

    #[test]
    fn uses_public_registry_when_source_missing() {
        let root = project(
            "no-source",
            &[(
                "rip.toml",
                "[module]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"main.rp\"\n\n[dependencies]\n\"acme/tools\" = \"^1.0\"\n",
            )],
        );
        let manifest = super::read_manifest(&root);
        // 没写 [registry] source：回退到公共包源，并标记为 fallback（错误信息会带上换源提示）。
        let fallback = Registry::open_with(&root, &manifest, None).expect("public registry");
        assert_eq!(fallback.describe(), DEFAULT_REGISTRY);
        assert!(fallback.fallback);
        // 显式覆盖优先，且不再走公共源。
        let overridden = Registry::open_with(&root, &manifest, Some("./mirror")).expect("override");
        assert!(!overridden.fallback);
        assert!(overridden.describe().ends_with("mirror"));
        let _ = fs::remove_dir_all(&root);
    }
}

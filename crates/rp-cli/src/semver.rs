//! 最小语义化版本实现：只覆盖依赖约束解析与比较所需的能力。
//!
//! 支持三种写法：
//! - 精确版本 `1.2.3`、`=1.2.3`
//! - 兼容约束 `^1.2.3`（缺省）、`~1.2`、`1.*`
//! - 区间约束 `>=1.0, <2.0`
//!
//! 预发布版本（`1.0.0-beta.1`）低于同版本正式版，按语义化版本规范比较。

use std::cmp::Ordering;
use std::fmt;

/// 一个语义化版本号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

impl Version {
    pub fn parse(text: &str) -> Result<Version, String> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        if text.is_empty() {
            return Err("empty version".to_string());
        }
        // 构建元数据（`+build`）不参与比较，直接丢弃。
        let text = match text.split_once('+') {
            Some((head, _)) => head,
            None => text,
        };
        let (numbers, pre) = match text.split_once('-') {
            Some((head, tail)) => (head, Some(tail.to_string())),
            None => (text, None),
        };
        let parts: Vec<&str> = numbers.split('.').collect();
        if parts.len() > 3 {
            return Err(format!("invalid version '{}'", text));
        }
        let mut values = [0u64; 3];
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(format!("invalid version '{}'", text));
            }
            values[index] = part
                .parse()
                .map_err(|_| format!("invalid version '{}'", text))?;
        }
        Ok(Version {
            major: values[0],
            minor: values[1],
            patch: values[2],
            pre,
        })
    }

    /// 是否为预发布版本（`1.0.0-beta.1`）。
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{}", pre)?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let core = (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            // 正式版高于同号预发布版。
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare_pre(left, right),
        }
    }
}

/// 预发布标识逐段比较：数字段按数值，其余按字典序，数字段低于非数字段。
fn compare_pre(left: &str, right: &str) -> Ordering {
    let left: Vec<&str> = left.split('.').collect();
    let right: Vec<&str> = right.split('.').collect();
    for (a, b) in left.iter().zip(right.iter()) {
        let ordering = match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// 单条比较条件。
#[derive(Debug, Clone)]
enum Comparator {
    Gte(Version),
    Gt(Version),
    Lte(Version),
    Lt(Version),
    Exact(Version),
}

/// 版本约束：多个比较条件之间是「与」关系。
#[derive(Debug, Clone)]
pub struct VersionReq {
    raw: String,
    any: bool,
    comparators: Vec<Comparator>,
}

impl VersionReq {
    /// 解析约束；`*`、`latest`、空串都表示不限制。
    pub fn parse(text: &str) -> Result<VersionReq, String> {
        let raw = text.trim().to_string();
        let normalized = raw.trim_matches('"').trim();
        if normalized.is_empty() || normalized == "*" || normalized == "x" || normalized == "latest" {
            return Ok(VersionReq {
                raw,
                any: true,
                comparators: Vec::new(),
            });
        }
        let mut comparators = Vec::new();
        for piece in normalized.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if let Some(rest) = piece.strip_prefix(">=") {
                comparators.push(Comparator::Gte(parse_version(rest)?));
            } else if let Some(rest) = piece.strip_prefix("<=") {
                comparators.push(Comparator::Lte(parse_version(rest)?));
            } else if let Some(rest) = piece.strip_prefix('>') {
                comparators.push(Comparator::Gt(parse_version(rest)?));
            } else if let Some(rest) = piece.strip_prefix('<') {
                comparators.push(Comparator::Lt(parse_version(rest)?));
            } else if let Some(rest) = piece.strip_prefix('=') {
                comparators.push(Comparator::Exact(parse_version(rest)?));
            } else if let Some(rest) = piece.strip_prefix('^') {
                caret(rest, &mut comparators)?;
            } else if let Some(rest) = piece.strip_prefix('~') {
                tilde(rest, &mut comparators)?;
            } else if piece.contains('*') || piece.contains('x') {
                wildcard(piece, &mut comparators)?;
            } else {
                // 裸版本按兼容约束处理，与 Cargo 的直觉一致。
                caret(piece, &mut comparators)?;
            }
        }
        Ok(VersionReq {
            raw,
            any: false,
            comparators,
        })
    }

    pub fn is_any(&self) -> bool {
        self.any
    }

    /// 约束是否显式提到预发布版本（如 `^1.10.0-beta`）。
    ///
    /// 未提及时，选版应跳过预发布版本，与 Cargo 的默认行为一致。
    pub fn allows_prerelease(&self) -> bool {
        self.comparators.iter().any(|comparator| match comparator {
            Comparator::Gte(bound)
            | Comparator::Gt(bound)
            | Comparator::Lte(bound)
            | Comparator::Lt(bound)
            | Comparator::Exact(bound) => bound.pre.is_some(),
        })
    }

    pub fn matches(&self, version: &Version) -> bool {
        if self.any {
            return true;
        }
        self.comparators.iter().all(|comparator| match comparator {
            Comparator::Gte(bound) => version >= bound,
            Comparator::Gt(bound) => version > bound,
            Comparator::Lte(bound) => version <= bound,
            Comparator::Lt(bound) => version < bound,
            Comparator::Exact(bound) => version == bound,
        })
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.any {
            write!(f, "*")
        } else {
            write!(f, "{}", self.raw)
        }
    }
}

fn parse_version(text: &str) -> Result<Version, String> {
    Version::parse(text)
}

/// `^1.2.3`：允许 minor/patch 升级，major 锁定；`0.x` 按 Cargo 规则收紧。
fn caret(text: &str, out: &mut Vec<Comparator>) -> Result<(), String> {
    let text = text.trim();
    let (version, specified) = parse_parts(text)?;
    let upper = if version.major > 0 {
        Version {
            major: version.major + 1,
            minor: 0,
            patch: 0,
            pre: None,
        }
    } else if specified <= 1 {
        // `^0`：只允许 0.x。
        Version::parse("1.0.0")?
    } else if version.minor > 0 {
        Version {
            major: 0,
            minor: version.minor + 1,
            patch: 0,
            pre: None,
        }
    } else if specified == 2 {
        // `^0.0`：只允许 0.0.x。
        Version::parse("0.1.0")?
    } else {
        Version {
            major: 0,
            minor: 0,
            patch: version.patch + 1,
            pre: None,
        }
    };
    out.push(Comparator::Gte(version));
    out.push(Comparator::Lt(upper));
    Ok(())
}

/// `~1.2.3`：只允许 patch 升级；`~1.2` 允许 patch；`~1` 允许 minor。
fn tilde(text: &str, out: &mut Vec<Comparator>) -> Result<(), String> {
    let (version, specified) = parse_parts(text)?;
    let upper = if specified <= 1 {
        Version {
            major: version.major + 1,
            minor: 0,
            patch: 0,
            pre: None,
        }
    } else {
        Version {
            major: version.major,
            minor: version.minor + 1,
            patch: 0,
            pre: None,
        }
    };
    out.push(Comparator::Gte(version));
    out.push(Comparator::Lt(upper));
    Ok(())
}

/// `1.*` / `1.2.x`：通配到已写出的最高段。
fn wildcard(text: &str, out: &mut Vec<Comparator>) -> Result<(), String> {
    let parts: Vec<&str> = text.trim().split('.').collect();
    // 通配符之后不能再出现具体段（`1.*.4` 非法）。
    let mut seen_wildcard = false;
    for part in &parts {
        let is_wildcard = part.contains('*') || part.eq_ignore_ascii_case("x");
        if seen_wildcard && !is_wildcard {
            return Err(format!("invalid version requirement '{}'", text));
        }
        seen_wildcard |= is_wildcard;
    }
    let fixed: Vec<u64> = parts
        .iter()
        .take_while(|part| !part.contains('*') && !part.eq_ignore_ascii_case("x"))
        .map(|part| part.parse::<u64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("invalid version requirement '{}'", text))?;
    let lower = match fixed.len() {
        0 => return Err(format!("invalid version requirement '{}'", text)),
        1 => Version::parse(&format!("{}.0.0", fixed[0]))?,
        2 => Version::parse(&format!("{}.{}.0", fixed[0], fixed[1]))?,
        _ => Version::parse(&format!("{}.{}.{}", fixed[0], fixed[1], fixed[2]))?,
    };
    let upper = match fixed.len() {
        1 => Version::parse(&format!("{}.0.0", fixed[0] + 1))?,
        2 => Version::parse(&format!("{}.{}.0", fixed[0], fixed[1] + 1))?,
        _ => Version::parse(&format!("{}.{}.{}", fixed[0], fixed[1], fixed[2] + 1))?,
    };
    out.push(Comparator::Gte(lower));
    out.push(Comparator::Lt(upper));
    Ok(())
}

/// 解析版本并返回写出的段数（用于区分 `1`、`1.2`、`1.2.3` 的约束语义）。
fn parse_parts(text: &str) -> Result<(Version, usize), String> {
    let text = text.trim();
    let version = Version::parse(text)?;
    let core = match text.split_once('-') {
        Some((head, _)) => head,
        None => text,
    };
    let core = core.strip_prefix('v').unwrap_or(core);
    let specified = core.split('.').filter(|part| !part.is_empty()).count();
    Ok((version, specified))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("valid version")
    }

    #[test]
    fn parses_short_versions() {
        assert_eq!(version("1").to_string(), "1.0.0");
        assert_eq!(version("1.2").to_string(), "1.2.0");
        assert_eq!(version("v1.2.3").to_string(), "1.2.3");
        assert_eq!(version("1.2.3+build.7").to_string(), "1.2.3");
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("").is_err());
    }

    #[test]
    fn compares_prerelease_below_release() {
        assert!(version("1.0.0-beta") < version("1.0.0"));
        assert!(version("1.0.0-alpha") < version("1.0.0-beta"));
        assert!(version("1.0.0-beta.2") < version("1.0.0-beta.10"));
        assert!(version("1.0.0-beta") < version("1.0.0-beta.1"));
    }

    fn matches(requirement: &str, candidate: &str) -> bool {
        VersionReq::parse(requirement)
            .expect("valid requirement")
            .matches(&version(candidate))
    }

    #[test]
    fn caret_matches_compatible_upgrades() {
        assert!(matches("^1.2.3", "1.2.3"));
        assert!(matches("^1.2.3", "1.9.0"));
        assert!(!matches("^1.2.3", "2.0.0"));
        assert!(!matches("^1.2.3", "1.2.2"));
        // 0.x 收紧到下一个 minor。
        assert!(matches("^0.2.3", "0.2.9"));
        assert!(!matches("^0.2.3", "0.3.0"));
        assert!(!matches("^0.0.3", "0.0.4"));
    }

    #[test]
    fn bare_version_is_caret() {
        assert!(matches("1.2", "1.5.0"));
        assert!(!matches("1.2", "2.0.0"));
    }

    #[test]
    fn tilde_matches_patch_upgrades() {
        assert!(matches("~1.2.3", "1.2.9"));
        assert!(!matches("~1.2.3", "1.3.0"));
        assert!(matches("~1", "1.9.0"));
        assert!(!matches("~1", "2.0.0"));
    }

    #[test]
    fn wildcard_and_ranges() {
        assert!(matches("1.*", "1.9.9"));
        assert!(!matches("1.*", "2.0.0"));
        assert!(matches("1.2.x", "1.2.7"));
        assert!(!matches("1.2.x", "1.3.0"));
        assert!(matches(">=1.0, <2.0", "1.5.0"));
        assert!(!matches(">=1.0, <2.0", "2.0.0"));
        assert!(matches("=1.2.3", "1.2.3"));
        assert!(!matches("=1.2.3", "1.2.4"));
    }

    #[test]
    fn latest_and_star_accept_anything() {
        for requirement in ["*", "latest", "", "x"] {
            let req = VersionReq::parse(requirement).expect("valid requirement");
            assert!(req.is_any());
            assert!(req.matches(&version("0.0.1")));
        }
    }

    #[test]
    fn rejects_invalid_requirements() {
        assert!(VersionReq::parse(">=abc").is_err());
        assert!(VersionReq::parse("1.2.*.4").is_err());
    }
}

//! 语句覆盖率采集（`rip test --coverage`）。
//!
//! 只有显式开启时才记录：解释器每条语句多一次 `bool` 判断，字节码在编译期
//! 决定是否插入 `Cover` 指令，关闭时既不增加指令也不增加判断，热路径零开销。
//!
//! 记录用「函数名 + 行号」的 64 位哈希存进全局列表：采集期间不加锁读取函数名，
//! 也不分配字符串；报告阶段再用同样的哈希回查声明位置，碰撞概率可忽略。

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static ENABLED: AtomicBool = AtomicBool::new(false);

fn hits() -> &'static Mutex<Vec<u64>> {
    static HITS: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    HITS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 开启采集。必须在构建 `RuntimeSeed` 之前调用：字节码编译器据此决定是否插桩。
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn disable() {
    ENABLED.store(false, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `(函数名, 行号)` 的稳定哈希；采集与报告共用。
pub fn key(function: &str, line: u32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    function.hash(&mut hasher);
    line.hash(&mut hasher);
    hasher.finish()
}

/// 记录一条语句被执行；未开启时立即返回。
pub fn record(function: &str, line: u32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let key = key(function, line);
    if let Ok(mut list) = hits().lock() {
        // 采集期只追加，去重在报告阶段做，避免每次命中都查集合。
        list.push(key);
    }
}

pub fn clear() {
    if let Ok(mut list) = hits().lock() {
        list.clear();
    }
}

#[derive(Debug, Clone)]
pub struct FunctionReport {
    pub name: String,
    /// 函数体内可执行的语句行数（去重后）。
    pub total: usize,
    pub covered: usize,
    /// 未被执行的语句行号（升序）。
    pub missing: Vec<u32>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub total: usize,
    pub covered: usize,
    pub functions: Vec<FunctionReport>,
}

impl Report {
    /// 行覆盖率（0..=100）；没有可执行语句时按 100 处理。
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.covered as f64 * 100.0 / self.total as f64
        }
    }
}

/// 按声明的函数与行号生成报告；`declared` 为 `(函数名, 函数体内所有语句行号)`。
pub fn report<I>(declared: I) -> Report
where
    I: IntoIterator<Item = (String, Vec<u32>)>,
{
    let executed: HashSet<u64> = hits()
        .lock()
        .map(|list| list.iter().copied().collect())
        .unwrap_or_default();
    let mut report = Report::default();
    for (name, lines) in declared {
        let mut unique: Vec<u32> = lines;
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            continue;
        }
        let mut missing = Vec::new();
        let mut covered = 0usize;
        for line in &unique {
            if executed.contains(&key(&name, *line)) {
                covered += 1;
            } else {
                missing.push(*line);
            }
        }
        report.total += unique.len();
        report.covered += covered;
        report.functions.push(FunctionReport {
            name,
            total: unique.len(),
            covered,
            missing,
        });
    }
    report.functions.sort_by(|left, right| left.name.cmp(&right.name));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// 覆盖率状态是进程级全局量：测试并行执行时必须串行，否则互相清空计数。
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn records_only_when_enabled() {
        let _guard = exclusive();
        clear();
        disable();
        record("main", 1);
        let idle = report(vec![("main".to_string(), vec![1])]);
        assert_eq!(idle.covered, 0);

        enable();
        record("main", 1);
        record("main", 1);
        let result = report(vec![("main".to_string(), vec![1, 2])]);
        assert_eq!(result.total, 2);
        assert_eq!(result.covered, 1);
        assert_eq!(result.functions[0].missing, vec![2]);
        assert!((result.percent() - 50.0).abs() < f64::EPSILON);
        disable();
        clear();
    }

    #[test]
    fn functions_are_kept_apart() {
        let _guard = exclusive();
        enable();
        clear();
        record("a", 1);
        let result = report(vec![
            ("a".to_string(), vec![1]),
            ("b".to_string(), vec![1]),
        ]);
        assert_eq!(result.covered, 1);
        assert_eq!(result.functions[1].missing, vec![1]);
        disable();
        clear();
    }
}

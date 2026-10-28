//! 轻量函数级 profiler：按函数累计调用次数、总耗时与自身耗时。
//!
//! 默认关闭时每次调用只多一次原子读；开启后按「进入/退出」记录调用栈，
//! 用子调用耗时反推自身耗时。调用栈是线程本地的（`parallel` / `task.spawn`
//! 互不干扰），统计结果全局归并。
//! 由 `rip profile` 启用，运行结束后读取结果。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// 单个函数的累计数据；耗时单位纳秒。
#[derive(Clone, Default)]
pub struct Stat {
    pub calls: u64,
    pub total_ns: u64,
    pub child_ns: u64,
}

impl Stat {
    /// 自身耗时：总耗时减去子调用耗时。
    pub fn self_ns(&self) -> u64 {
        self.total_ns.saturating_sub(self.child_ns)
    }
}

struct Frame {
    name: String,
    start: Instant,
    child_ns: u64,
}

thread_local! {
    /// 当前线程的调用栈；跨线程不会互相覆盖。
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
struct Totals {
    stats: HashMap<String, Stat>,
}

fn totals() -> &'static Mutex<Totals> {
    static TOTALS: OnceLock<Mutex<Totals>> = OnceLock::new();
    TOTALS.get_or_init(|| Mutex::new(Totals::default()))
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 开始采集：清空上一个程序的结果。
pub fn start() {
    if let Ok(mut totals) = totals().lock() {
        totals.stats.clear();
    }
    STACK.with(|stack| stack.borrow_mut().clear());
    ENABLED.store(true, Ordering::SeqCst);
}

/// 结束采集并返回按总耗时降序排列的结果。
pub fn stop() -> Vec<(String, Stat)> {
    ENABLED.store(false, Ordering::SeqCst);
    // 程序因错误提前退出时，当前线程栈上未闭合的帧也要结算。
    while pop_frame() {}
    let mut totals = match totals().lock() {
        Ok(totals) => totals,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut result: Vec<(String, Stat)> = totals.stats.drain().collect();
    result.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns));
    result
}

/// 弹出一个帧并计入统计；返回是否还有帧。
fn pop_frame() -> bool {
    let popped = STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let frame = stack.pop()?;
        let elapsed = frame.start.elapsed().as_nanos() as u64;
        if let Some(parent) = stack.last_mut() {
            parent.child_ns += elapsed;
        }
        Some((frame.name, elapsed, frame.child_ns))
    });
    match popped {
        Some((name, elapsed, child_ns)) => {
            record(&name, elapsed, child_ns);
            true
        }
        None => false,
    }
}

fn record(name: &str, elapsed: u64, child_ns: u64) {
    let mut totals = match totals().lock() {
        Ok(totals) => totals,
        Err(poisoned) => poisoned.into_inner(),
    };
    let stat = totals.stats.entry(name.to_string()).or_default();
    stat.calls += 1;
    stat.total_ns += elapsed;
    stat.child_ns += child_ns;
}

/// 函数帧守卫：离开作用域时结算。未开启采集时是空操作。
pub struct Guard {
    active: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.active {
            pop_frame();
        }
    }
}

/// 进入一个 Rip 函数帧。
pub fn enter(name: &str) -> Guard {
    if !ENABLED.load(Ordering::Relaxed) {
        return Guard { active: false };
    }
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
            name: name.to_string(),
            start: Instant::now(),
            child_ns: 0,
        });
    });
    Guard { active: true }
}

/// 进入一个内建模块函数帧；未开启时不构造 `module.name` 字符串。
pub fn enter_module(module: &str, name: &str) -> Guard {
    if !ENABLED.load(Ordering::Relaxed) {
        return Guard { active: false };
    }
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
            name: format!("{}.{}", module, name),
            start: Instant::now(),
            child_ns: 0,
        });
    });
    Guard { active: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// 两个测试共享全局采集状态（`ENABLED` / `TOTALS`），`stop()` 会整表排空，
    /// 并行执行时会互相偷走统计结果，必须串行。
    fn lock_profile() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn records_nested_and_self_time() {
        let _serial = lock_profile();
        start();
        {
            let _outer = enter("outer");
            std::thread::sleep(std::time::Duration::from_millis(1));
            {
                let _inner = enter("inner");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let stats = stop();
        let outer = stats.iter().find(|(name, _)| name == "outer").map(|(_, s)| s);
        let inner = stats.iter().find(|(name, _)| name == "inner").map(|(_, s)| s);
        let outer = outer.expect("outer recorded");
        let inner = inner.expect("inner recorded");
        assert_eq!(outer.calls, 1);
        assert_eq!(inner.calls, 1);
        assert!(outer.total_ns >= inner.total_ns);
        assert!(inner.total_ns > 0);
        assert!(outer.self_ns() < outer.total_ns);
    }

    #[test]
    fn attributes_calls_from_multiple_threads() {
        let _serial = lock_profile();
        start();
        let mut workers = Vec::new();
        for _ in 0..2 {
            workers.push(std::thread::spawn(|| {
                let _guard = enter("worker");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }));
        }
        for worker in workers {
            worker.join().expect("worker finished");
        }
        let stats = stop();
        let worker = stats
            .iter()
            .find(|(name, _)| name == "worker")
            .map(|(_, s)| s)
            .expect("worker recorded");
        assert_eq!(worker.calls, 2);
        assert!(worker.total_ns > 0);
    }
}

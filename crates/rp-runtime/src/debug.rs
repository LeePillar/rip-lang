//! 调试支持：断点、单步、暂停与变量快照。
//!
//! DAP 适配器（`rp-dap`）通过 [`Debugger`] 控制解释器；解释器在每条语句前
//! 调用 [`Debugger::check`]，命中时阻塞并发出 `stopped` 事件，收到
//! continue / next / stepIn / stepOut 后恢复。
//!
//! 同一模块还负责把程序输出改道为 DAP 的 `output` 事件，避免 stdout
//! 上同时承载调试协议与业务输出。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Condvar, Mutex};

/// 单步模式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepMode {
    /// 继续运行，只在断点处停下。
    Continue,
    /// 单步跳过：同一调用深度。
    Over,
    /// 单步进入：任意更深或相同的调用深度。
    In,
    /// 单步跳出：调用深度变小。
    Out,
}

/// 停止原因与现场快照。
#[derive(Clone, Debug)]
pub struct StopInfo {
    /// `entry` / `breakpoint` / `step` / `pause`。
    pub reason: String,
    pub line: u32,
    pub depth: usize,
    /// 外层到内层的调用帧：`(函数名, 行号)`。
    pub frames: Vec<(String, u32)>,
    pub locals: Vec<(String, String)>,
    pub globals: Vec<(String, String)>,
}

/// 调试器发给 DAP 适配器的事件。
#[derive(Clone, Debug)]
pub enum DebugEvent {
    Stopped(StopInfo),
    Output(String),
    Exited(i32),
    Terminated,
}

struct Inner {
    paused: bool,
    stop_requested: bool,
    step: StepMode,
    /// 恢复时的调用深度，用于 Over / Out 判定。
    step_depth: usize,
    /// 继续时跳过当前断点行一次（同一行可能有多条语句）。
    resume_line: Option<u32>,
    /// 首次停止前是否在入口处停下。
    stop_on_entry: bool,
    started: bool,
    stopped: Option<StopInfo>,
    events: Option<Sender<DebugEvent>>,
}

/// 解释器与 DAP 适配器共享的调试状态。全部字段可跨线程使用。
pub struct Debugger {
    breakpoints: Mutex<HashSet<u32>>,
    inner: Mutex<Inner>,
    resume: Condvar,
    /// 快路径：没有断点、没有暂停请求时，每条语句只读一次原子变量。
    active: AtomicBool,
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            breakpoints: Mutex::new(HashSet::new()),
            inner: Mutex::new(Inner {
                paused: false,
                stop_requested: false,
                step: StepMode::Continue,
                step_depth: 0,
                resume_line: None,
                stop_on_entry: false,
                started: false,
                stopped: None,
                events: None,
            }),
            resume: Condvar::new(),
            active: AtomicBool::new(false),
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock_breakpoints(&self) -> std::sync::MutexGuard<'_, HashSet<u32>> {
        match self.breakpoints.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn refresh_active(&self) {
        let inner = self.lock_inner();
        let has_breakpoints = !self.lock_breakpoints().is_empty();
        let active = has_breakpoints
            || inner.stop_requested
            || (!inner.started && inner.stop_on_entry)
            || inner.step != StepMode::Continue;
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn attach_events(&self, sender: Sender<DebugEvent>) {
        self.lock_inner().events = Some(sender);
    }

    pub fn set_stop_on_entry(&self, value: bool) {
        self.lock_inner().stop_on_entry = value;
        self.refresh_active();
    }

    /// 替换断点集合（行号基于入口源文件）。
    pub fn set_breakpoints(&self, lines: Vec<u32>) {
        let mut breakpoints = self.lock_breakpoints();
        breakpoints.clear();
        breakpoints.extend(lines);
        drop(breakpoints);
        self.refresh_active();
    }

    /// 解释器在每条语句执行前调用；命中时阻塞直到恢复。
    ///
    /// `snapshot` 只在真正停下时调用，避免热路径构造变量表。
    pub fn check(
        &self,
        line: u32,
        depth: usize,
        snapshot: impl FnOnce(&str) -> StopInfo,
    ) {
        if !self.active() {
            return;
        }
        let reason = {
            let mut inner = self.lock_inner();
            if inner.paused {
                // 已经在另一个检查点停下（例如递归里再次进入），不重复发事件。
                return;
            }
            if let Some(skip) = inner.resume_line {
                inner.resume_line = None;
                if skip == line {
                    return;
                }
            }
            let hit_breakpoint = self.lock_breakpoints().contains(&line);
            let stop = if inner.step != StepMode::Continue {
                match inner.step {
                    StepMode::In => Some("step"),
                    StepMode::Over if depth <= inner.step_depth => Some("step"),
                    StepMode::Out if depth < inner.step_depth => Some("step"),
                    _ => None,
                }
            } else if inner.stop_requested {
                Some("pause")
            } else if hit_breakpoint {
                Some("breakpoint")
            } else if !inner.started && inner.stop_on_entry {
                Some("entry")
            } else {
                None
            };
            match stop {
                Some(reason) => reason,
                None => return,
            }
        };

        let mut info = snapshot(reason);
        info.line = line;
        info.depth = depth;
        {
            let mut inner = self.lock_inner();
            inner.paused = true;
            inner.stop_requested = false;
            inner.step = StepMode::Continue;
            inner.started = true;
            inner.stopped = Some(info.clone());
            if let Some(events) = &inner.events {
                let _ = events.send(DebugEvent::Stopped(info));
            }
            // 停下后不再需要快路径；恢复时按新指令重新打开。
            self.active.store(false, Ordering::Relaxed);
            while inner.paused {
                inner = match self.resume.wait(inner) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
        }
    }

    /// 继续运行。
    pub fn resume_continue(&self) {
        let mut inner = self.lock_inner();
        if let Some(stop) = &inner.stopped {
            inner.resume_line = Some(stop.line);
        }
        inner.step = StepMode::Continue;
        inner.paused = false;
        drop(inner);
        self.refresh_active();
        self.resume.notify_all();
    }

    /// 单步（Over / In / Out）。
    pub fn resume_step(&self, mode: StepMode) {
        let mut inner = self.lock_inner();
        inner.step_depth = inner.stopped.as_ref().map(|stop| stop.depth).unwrap_or(0);
        inner.step = mode;
        inner.resume_line = None;
        inner.paused = false;
        drop(inner);
        self.refresh_active();
        self.resume.notify_all();
    }

    /// 请求在下一个语句边界暂停。
    pub fn request_pause(&self) {
        {
            let mut inner = self.lock_inner();
            if inner.paused {
                return;
            }
            inner.stop_requested = true;
        }
        self.refresh_active();
    }

    pub fn is_paused(&self) -> bool {
        self.lock_inner().paused
    }

    pub fn last_stop(&self) -> Option<StopInfo> {
        self.lock_inner().stopped.clone()
    }

    /// 变量的文本形式（供 `evaluate` 使用）：先查当前帧局部变量，再查全局。
    pub fn evaluate(&self, name: &str) -> Option<String> {
        let inner = self.lock_inner();
        let stop = inner.stopped.as_ref()?;
        if let Some((_, value)) = stop.locals.iter().find(|(key, _)| key == name) {
            return Some(value.clone());
        }
        stop.globals
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    /// 程序结束：通知适配器。
    pub fn finish(&self, code: i32) {
        let inner = self.lock_inner();
        if let Some(events) = &inner.events {
            let _ = events.send(DebugEvent::Exited(code));
            let _ = events.send(DebugEvent::Terminated);
        }
    }
}

impl Default for Debugger {
    fn default() -> Self {
        Self::new()
    }
}

/// 程序输出改道：设置后 `print` / `dev.log` / `log.*` 不再写 stdout，
/// 而是作为 DAP `output` 事件发送，避免破坏调试协议。
static OUTPUT: Mutex<Option<Sender<DebugEvent>>> = Mutex::new(None);

pub fn capture_output(sender: Option<Sender<DebugEvent>>) {
    let mut slot = match OUTPUT.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = sender;
}

/// 输出一行程序文本：调试会话中发给 DAP，否则写标准输出。
pub fn emit_line(text: String) {
    let sent = {
        let slot = match OUTPUT.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        match slot.as_ref() {
            Some(sender) => sender.send(DebugEvent::Output(text.clone())).is_ok(),
            None => false,
        }
    };
    if !sent {
        println!("{}", text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn snapshot(reason: &str) -> StopInfo {
        StopInfo {
            reason: reason.to_string(),
            line: 0,
            depth: 0,
            frames: Vec::new(),
            locals: Vec::new(),
            globals: Vec::new(),
        }
    }

    #[test]
    fn stops_only_on_breakpoints() {
        let debugger = Arc::new(Debugger::new());
        debugger.set_breakpoints(vec![10, 20]);
        assert!(debugger.active());
        // 未命中的行直接返回，不阻塞。
        debugger.check(5, 1, |reason| snapshot(reason));
        assert!(!debugger.is_paused());
        // 命中时阻塞，由另一个线程恢复。
        let worker = {
            let debugger = Arc::clone(&debugger);
            std::thread::spawn(move || {
                debugger.check(10, 1, |reason| snapshot(reason));
            })
        };
        let mut waited = 0;
        while !debugger.is_paused() && waited < 200 {
            std::thread::sleep(Duration::from_millis(5));
            waited += 1;
        }
        assert!(debugger.is_paused());
        assert_eq!(
            debugger.last_stop().expect("stop info").reason,
            "breakpoint"
        );
        debugger.resume_continue();
        worker.join().expect("worker finished");
    }

    #[test]
    fn pause_blocks_until_resume() {
        let debugger = Arc::new(Debugger::new());
        debugger.set_stop_on_entry(true);
        let worker = {
            let debugger = Arc::clone(&debugger);
            std::thread::spawn(move || {
                debugger.check(1, 1, |reason| snapshot(reason));
            })
        };
        let mut waited = 0;
        while !debugger.is_paused() && waited < 200 {
            std::thread::sleep(Duration::from_millis(5));
            waited += 1;
        }
        assert!(debugger.is_paused());
        let stop = debugger.last_stop().expect("stop info");
        assert_eq!(stop.reason, "entry");
        debugger.resume_continue();
        worker.join().expect("worker finished");
        assert!(!debugger.is_paused());
    }
}

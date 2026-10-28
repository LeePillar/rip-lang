//! 异步任务：`async func` 在独立运行时中执行，`await` 等待结果。
//!
//! Rip 的值是单线程引用计数，不能直接跨线程。跨线程的参数与返回值先转成
//! `WireValue`（不含 `Rc`），任务线程用 `RuntimeSeed` 重建运行时，因此
//! async 函数与普通函数共享代码与字节码，但不共享变量。
//!
//! 调度器是进程级的有界队列 + 固定工作线程；`await` 先等完成通知，超时后当前线程
//! 才顺手执行一个排队任务（协助调度），因此嵌套 `await` 不会因为工作线程耗尽而死锁。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::rt::{RuntimeSeed, ThreadOptions};
use crate::value::{format_value, Flow, FlowResult, RuntimeError, Value};

/// 队列上限。超过后在调用线程直接执行，形成自然的反压，避免任务无限堆积。
const MAX_QUEUE: usize = 8192;

/// `await` 先等多久再「协助执行」。
///
/// 正常情况下任务由调度线程并行跑完，完成通知会立刻唤醒等待者；只有任务迟迟
/// 不动（工作线程都被嵌套 `await` 占住）时才由等待者自己执行一个排队任务，
/// 这样既保住并行度，又不会死锁。
const HELP_DELAY: Duration = Duration::from_micros(200);

/// 跨线程值：结构与本线程的 `Value` 一致，但不含 `Rc`。
#[derive(Clone)]
pub enum WireValue {
    Null,
    None,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Char(String),
    Some(Box<WireValue>),
    Ok(Box<WireValue>),
    Err(Box<WireValue>),
    Vec(Vec<WireValue>),
    Ary {
        class: String,
        fields: Vec<(String, WireValue)>,
    },
    Map(Vec<(WireValue, WireValue)>),
}

impl WireValue {
    /// 在目标线程重建普通值；容器会重新登记到当前线程的环回收器。
    pub fn into_value(self) -> Value {
        match self {
            WireValue::Null => Value::Null,
            WireValue::None => Value::None,
            WireValue::Bool(value) => Value::Bool(value),
            WireValue::Int(value) => Value::Int(value),
            WireValue::UInt(value) => Value::UInt(value),
            WireValue::Float(value) => Value::Float(value),
            WireValue::Str(value) => Value::string(value),
            WireValue::Char(value) => Value::Char(value),
            WireValue::Some(inner) => Value::Some(Box::new(inner.into_value())),
            WireValue::Ok(inner) => Value::Ok(Box::new(inner.into_value())),
            WireValue::Err(inner) => Value::Err(Box::new(inner.into_value())),
            WireValue::Vec(items) => {
                Value::vec(items.into_iter().map(WireValue::into_value).collect())
            }
            WireValue::Ary { class, fields } => Value::ary(
                class,
                fields
                    .into_iter()
                    .map(|(name, value)| (name, value.into_value()))
                    .collect(),
            ),
            WireValue::Map(entries) => Value::map(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.into_value(), value.into_value()))
                    .collect(),
            ),
        }
    }
}

/// 把普通值转成可跨线程的值。闭包与函数值持有 `Rc`，不能跨线程。
pub fn wire_value(value: &Value) -> Result<WireValue, String> {
    match value {
        Value::Null => Ok(WireValue::Null),
        Value::None => Ok(WireValue::None),
        Value::Bool(value) => Ok(WireValue::Bool(*value)),
        Value::Int(value) => Ok(WireValue::Int(*value)),
        Value::UInt(value) => Ok(WireValue::UInt(*value)),
        Value::Float(value) => Ok(WireValue::Float(*value)),
        Value::Str(value) => Ok(WireValue::Str((**value).clone())),
        Value::Char(value) => Ok(WireValue::Char(value.clone())),
        Value::Some(inner) => Ok(WireValue::Some(Box::new(wire_value(inner)?))),
        Value::Ok(inner) => Ok(WireValue::Ok(Box::new(wire_value(inner)?))),
        Value::Err(inner) => Ok(WireValue::Err(Box::new(wire_value(inner)?))),
        Value::Vec(items) => Ok(WireValue::Vec(
            items
                .borrow()
                .iter()
                .map(wire_value)
                .collect::<Result<_, _>>()?,
        )),
        Value::Ary(ary) => {
            let ary = ary.borrow();
            let mut fields = Vec::with_capacity(ary.fields.len());
            for (name, value) in &ary.fields {
                fields.push((name.clone(), wire_value(value)?));
            }
            Ok(WireValue::Ary {
                class: ary.class.clone(),
                fields,
            })
        }
        Value::Map(map) => {
            let map = map.borrow();
            let mut entries = Vec::with_capacity(map.entries.len());
            for (key, value) in &map.entries {
                entries.push((wire_value(key)?, wire_value(value)?));
            }
            Ok(WireValue::Map(entries))
        }
        Value::Func(_) => Err(
            "closures and function values cannot cross an async boundary; pass data instead"
                .to_string(),
        ),
        Value::Task(_) => Err(
            "a task cannot be passed to another async function; await it first".to_string(),
        ),
    }
}

/// 一次 async 调用的结果槽：完成时写入结果并唤醒等待者。
pub struct TaskSlot {
    state: Mutex<SlotState>,
    ready: Condvar,
}

enum SlotState {
    Pending,
    Ready(Result<WireValue, RuntimeError>),
    Taken,
}

/// 一次轮询的结果：完成、已被其它 `await` 取走，或超时仍在运行。
enum TaskPoll {
    Ready(Result<WireValue, RuntimeError>),
    Taken,
    Timeout,
}

impl TaskSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
        }
    }

    fn finish(&self, result: Result<WireValue, RuntimeError>) {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if !matches!(*state, SlotState::Taken) {
            *state = SlotState::Ready(result);
        }
        self.ready.notify_all();
    }

    /// 非阻塞取结果：完成则取走，已被其它 `await` 取走返回 `Taken`，否则 `Timeout`。
    fn try_take(&self) -> TaskPoll {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        match &*state {
            SlotState::Ready(_) => {
                if let SlotState::Ready(result) =
                    std::mem::replace(&mut *state, SlotState::Taken)
                {
                    return TaskPoll::Ready(result);
                }
                TaskPoll::Taken
            }
            SlotState::Taken => TaskPoll::Taken,
            SlotState::Pending => TaskPoll::Timeout,
        }
    }

    /// 在锁内「复查 + 等待」：完成通知不会因为检查与等待之间的空隙而丢失。
    fn wait_pending(&self, timeout: Duration) {
        let state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if matches!(*state, SlotState::Pending) {
            let _ = self
                .ready
                .wait_timeout(state, timeout)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    /// 任务是否仍未完成（已被取走也视为不需要再等）。
    fn is_pending(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        matches!(*state, SlotState::Pending)
    }
}

struct Job {
    seed: RuntimeSeed,
    options: ThreadOptions,
    name: String,
    args: Vec<WireValue>,
    slot: Arc<TaskSlot>,
}

struct Scheduler {
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

impl Scheduler {
    fn submit(&self, job: Job) {
        let mut queue = self.queue.lock().unwrap_or_else(|poison| poison.into_inner());
        if queue.len() < MAX_QUEUE {
            queue.push_back(job);
            drop(queue);
            self.ready.notify_one();
        } else {
            // 过载：让调用线程直接执行，既不丢任务也不无限堆积内存。
            drop(queue);
            run_job(job);
        }
    }

    fn take(&self) -> Option<Job> {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop_front()
    }
}

/// 进程级调度器：首次使用时按 CPU 数创建工作线程（2..=32）。
fn scheduler() -> &'static Arc<Scheduler> {
    static INSTANCE: OnceLock<Arc<Scheduler>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let scheduler = Arc::new(Scheduler {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        // 任务多为 I/O 等待：4× CPU 核数（上限 64）能显著提高并发任务吞吐，
        // 又不会像无限线程那样把内存和调度开销推高。
        let cpus = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4);
        let workers = (cpus * 4).clamp(8, 64);
        for index in 0..workers {
            let shared = Arc::clone(&scheduler);
            let _ = std::thread::Builder::new()
                .name(format!("rip-async-{}", index))
                .spawn(move || loop {
                    let job = {
                        let mut queue = shared
                            .queue
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        loop {
                            if let Some(job) = queue.pop_front() {
                                break job;
                            }
                            queue = shared
                                .ready
                                .wait(queue)
                                .unwrap_or_else(|poison| poison.into_inner());
                        }
                    };
                    run_job(job);
                });
        }
        scheduler
    })
}

// 执行任务的线程复用一个已初始化的运行时池：省掉每个任务的 Runtime 构建与模块初始化。
// 用「池」而不是单个槽位，是因为 `await` 可能在任务内部再次协助执行任务（嵌套），
// 此时需要另一个运行时；执行期间不持有 `RefCell` 借用，嵌套安全。
thread_local! {
    static WORKER_RUNTIME_POOL: RefCell<Vec<(usize, crate::rt::Runtime)>> = RefCell::new(Vec::new());
}

/// 在复用的运行时（或新建运行时）里执行一次 async 调用并写回结果。
fn run_job(job: Job) {
    let Job {
        seed,
        options,
        name,
        args,
        slot,
    } = job;
    let key = seed.key();
    let cached = WORKER_RUNTIME_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.iter()
            .position(|(cached_key, _)| *cached_key == key)
            .map(|index| pool.swap_remove(index).1)
    });
    let (reuse, mut runtime) = match cached {
        Some(runtime) => (true, runtime),
        None => (false, seed.build()),
    };
    runtime.apply_thread_options(&options);
    let ready = if reuse {
        runtime.reset_for_task()
    } else {
        runtime.init_entry()
    };
    let valid = ready.is_ok();
    let outcome = match ready {
        Err(error) => Err(error),
        Ok(()) => {
            let values: Vec<Value> = args.into_iter().map(WireValue::into_value).collect();
            match runtime.call_named_now(&name, values, 0) {
                Ok(value) => wire_value(&value).map_err(|message| RuntimeError::new(message, 0)),
                Err(Flow::Error(error)) => Err(error),
                Err(Flow::Propagate(value)) => Err(RuntimeError::new(
                    format!("uncaught error: {}", format_value(&value)),
                    0,
                )),
                Err(_) => Err(RuntimeError::new("break/continue outside loop", 0)),
            }
        }
    };
    if valid {
        WORKER_RUNTIME_POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            // 池上限防止异常嵌套把运行时数量推得过高。
            if pool.len() < 64 {
                pool.push((key, runtime));
            }
        });
    }
    slot.finish(outcome);
}

impl crate::rt::Runtime {
    /// 发起一次 async 调用：把参数转成可跨线程的值，交给调度器执行。
    /// 返回的任务值只能由 `await` 消费（类型检查器保证）。
    pub fn call_async(&mut self, name: &str, args: Vec<Value>, line: u32) -> FlowResult<Value> {
        let mut wire = Vec::with_capacity(args.len());
        for arg in &args {
            match wire_value(arg) {
                Ok(value) => wire.push(value),
                Err(message) => return self.error(message, line),
            }
        }
        let slot = Arc::new(TaskSlot::new());
        let job = Job {
            seed: self.seed(),
            options: self.thread_options(),
            name: name.to_string(),
            args: wire,
            slot: Arc::clone(&slot),
        };
        scheduler().submit(job);
        Ok(Value::Task(slot))
    }

    /// `await`：等待任务完成；等待期间帮助执行队列里的任务，避免嵌套 await 死锁。
    pub fn await_task(&mut self, value: Value, line: u32) -> FlowResult<Value> {
        let Value::Task(slot) = value else {
            return self.error("await requires task<T> (the result of an async call)", line);
        };
        loop {
            match slot.try_take() {
                TaskPoll::Ready(result) => {
                    return match result {
                        Ok(value) => Ok(value.into_value()),
                        Err(error) => Err(Flow::Error(error)),
                    };
                }
                TaskPoll::Taken => return self.error("task has already been awaited", line),
                TaskPoll::Timeout => {
                    // 先等完成通知，让任务在调度线程上并行跑完；只有超时未完成时
                    // 才协助执行一个排队任务，避免嵌套 await 把工作线程占满后死锁。
                    slot.wait_pending(HELP_DELAY);
                    if slot.is_pending() {
                        if let Some(job) = scheduler().take() {
                            run_job(job);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::value_eq;

    #[test]
    fn wire_roundtrip_preserves_nested_values() {
        let value = Value::ary(
            "user",
            vec![
                ("name".to_string(), Value::string("ada")),
                (
                    "tags".to_string(),
                    Value::vec(vec![Value::string("a"), Value::string("b")]),
                ),
                (
                    "meta".to_string(),
                    Value::map(vec![(Value::string("age"), Value::Int(36))]),
                ),
            ],
        );
        let wire = wire_value(&value).expect("value should be portable");
        let back = wire.into_value();
        assert!(value_eq(&value, &back));
    }

    #[test]
    fn function_values_are_rejected() {
        let func = Value::func_rc(std::rc::Rc::new(crate::value::Function {
            name: "f".to_string(),
            kind: crate::value::FuncKind::Native {
                arity: 0,
                body: std::rc::Rc::new(|_, _, _| Ok(Value::Null)),
            },
        }));
        assert!(wire_value(&func).is_err());
    }
}

//! 跨线程并发原语：命名互斥锁、原子整数、通道。
//!
//! Rip 的 `vec` / `map` / `ary` 是 `Rc`，不能跨线程；这里用**进程级注册表**按名字
//! 共享真正的线程安全对象，任何线程（HTTP 工作线程、`task.spawn`、`#[interval]`）
//! 用同一个名字拿到的就是同一个锁 / 计数器 / 通道。
//!
//! 通道传输的是值的**深拷贝**（`Wire`），不共享引用，也不会把 `Rc` 送过线程边界。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::rt::{FlowResult, Runtime};
use crate::value::{Ary, Value};

/// 阻塞等待的检查切片：每 20ms 醒来一次检查请求截止时间。
const SLICE: Duration = Duration::from_millis(20);
/// 通道容量上限，避免一次误配置吃掉整个进程内存。
const MAX_CAPACITY: i64 = 1_000_000;
/// 单次超时参数上限（1 天）。
const MAX_TIMEOUT_MS: i64 = 86_400_000;
/// 通道值的最大嵌套深度：防止把含环的结构体送进通道时无限递归。
const MAX_DEPTH: usize = 128;

/// 跨线程传输的值：与 `Value` 同构，但不含 `Rc`，可以安全地放进线程间队列。
#[derive(Clone)]
pub(crate) enum Wire {
    Null,
    None,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Char(String),
    Some(Box<Wire>),
    Ok(Box<Wire>),
    Err(Box<Wire>),
    Vec(Vec<Wire>),
    Ary(String, Vec<(String, Wire)>),
    Map(Vec<(Wire, Wire)>),
}

impl Wire {
    fn from_value(value: &Value) -> Result<Wire, String> {
        Self::from_value_at(value, 0)
    }

    fn from_value_at(value: &Value, depth: usize) -> Result<Wire, String> {
        if depth > MAX_DEPTH {
            return Err(
                "value is nested too deeply for sync.channel (cycle or >128 levels)".to_string(),
            );
        }
        Ok(match value {
            Value::Null => Wire::Null,
            Value::None => Wire::None,
            Value::Bool(v) => Wire::Bool(*v),
            Value::Int(v) => Wire::Int(*v),
            Value::UInt(v) => Wire::UInt(*v),
            Value::Float(v) => Wire::Float(*v),
            Value::Str(v) => Wire::Str((**v).clone()),
            Value::Char(v) => Wire::Char(v.clone()),
            Value::Some(inner) => Wire::Some(Box::new(Self::from_value_at(inner, depth + 1)?)),
            Value::Ok(inner) => Wire::Ok(Box::new(Self::from_value_at(inner, depth + 1)?)),
            Value::Err(inner) => Wire::Err(Box::new(Self::from_value_at(inner, depth + 1)?)),
            Value::Vec(items) => {
                let mut out = Vec::with_capacity(items.borrow().len());
                for item in items.borrow().iter() {
                    out.push(Self::from_value_at(item, depth + 1)?);
                }
                Wire::Vec(out)
            }
            Value::Ary(ary) => {
                let ary = ary.borrow();
                let mut fields = Vec::with_capacity(ary.fields.len());
                for (name, item) in &ary.fields {
                    fields.push((name.clone(), Self::from_value_at(item, depth + 1)?));
                }
                Wire::Ary(ary.class.clone(), fields)
            }
            Value::Map(map) => {
                let map = map.borrow();
                let mut entries = Vec::with_capacity(map.entries.len());
                for (key, item) in &map.entries {
                    entries.push((
                        Self::from_value_at(key, depth + 1)?,
                        Self::from_value_at(item, depth + 1)?,
                    ));
                }
                Wire::Map(entries)
            }
            Value::Func(_) => {
                return Err("cannot send a function through sync.channel".to_string())
            }
            Value::Task(_) => {
                return Err("cannot send a task through sync.channel; await it first".to_string())
            }
        })
    }

    fn into_value(self) -> Value {
        match self {
            Wire::Null => Value::Null,
            Wire::None => Value::None,
            Wire::Bool(v) => Value::Bool(v),
            Wire::Int(v) => Value::Int(v),
            Wire::UInt(v) => Value::UInt(v),
            Wire::Float(v) => Value::Float(v),
            Wire::Str(v) => Value::string(v),
            Wire::Char(v) => Value::Char(v),
            Wire::Some(inner) => Value::Some(Box::new(inner.into_value())),
            Wire::Ok(inner) => Value::Ok(Box::new(inner.into_value())),
            Wire::Err(inner) => Value::Err(Box::new(inner.into_value())),
            Wire::Vec(items) => Value::vec(items.into_iter().map(Wire::into_value).collect()),
            Wire::Ary(class, fields) => Value::ary_owned(Ary {
                class,
                fields: fields
                    .into_iter()
                    .map(|(name, item)| (name, item.into_value()))
                    .collect(),
            }),
            Wire::Map(entries) => Value::map(
                entries
                    .into_iter()
                    .map(|(key, item)| (key.into_value(), item.into_value()))
                    .collect(),
            ),
        }
    }
}

enum Shared {
    Mutex(Arc<SyncMutex>),
    Atomic(Arc<SyncAtomic>),
    Channel(Arc<SyncChannel>),
}

/// 注册表分片数：不同名字落在不同锁上，减少跨线程争用。
const SHARDS: usize = 16;

fn registry() -> &'static [Mutex<HashMap<String, Shared>>; SHARDS] {
    static REGISTRY: OnceLock<[Mutex<HashMap<String, Shared>>; SHARDS]> = OnceLock::new();
    REGISTRY.get_or_init(|| std::array::from_fn(|_| Mutex::new(HashMap::new())))
}

fn shard_of(name: &str) -> usize {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    (hasher.finish() as usize) % SHARDS
}

fn lock_shard(name: &str) -> MutexGuard<'static, HashMap<String, Shared>> {
    registry()[shard_of(name)]
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn ensure_mutex(name: &str) -> Result<Arc<SyncMutex>, String> {
    let mut map = lock_shard(name);
    match map.get(name) {
        Some(Shared::Mutex(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => {
            let value = Arc::new(SyncMutex::default());
            map.insert(name.to_string(), Shared::Mutex(Arc::clone(&value)));
            Ok(value)
        }
    }
}

fn ensure_atomic(name: &str, initial: i64) -> Result<Arc<SyncAtomic>, String> {
    let mut map = lock_shard(name);
    match map.get(name) {
        Some(Shared::Atomic(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => {
            let value = Arc::new(SyncAtomic {
                value: AtomicI64::new(initial),
            });
            map.insert(name.to_string(), Shared::Atomic(Arc::clone(&value)));
            Ok(value)
        }
    }
}

fn ensure_channel(name: &str, capacity: usize) -> Result<Arc<SyncChannel>, String> {
    let mut map = lock_shard(name);
    match map.get(name) {
        Some(Shared::Channel(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => {
            let value = Arc::new(SyncChannel::new(capacity));
            map.insert(name.to_string(), Shared::Channel(Arc::clone(&value)));
            Ok(value)
        }
    }
}

fn kind_conflict(name: &str) -> String {
    format!(
        "sync name '{}' is already used by a different primitive",
        name
    )
}

fn handle_mutex(name: &str) -> Result<Arc<SyncMutex>, String> {
    match lock_shard(name).get(name) {
        Some(Shared::Mutex(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => Err(format!("unknown sync.mutex '{}'", name)),
    }
}

fn handle_atomic(name: &str) -> Result<Arc<SyncAtomic>, String> {
    match lock_shard(name).get(name) {
        Some(Shared::Atomic(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => Err(format!("unknown sync.atomic '{}'", name)),
    }
}

fn handle_channel(name: &str) -> Result<Arc<SyncChannel>, String> {
    match lock_shard(name).get(name) {
        Some(Shared::Channel(value)) => Ok(Arc::clone(value)),
        Some(_) => Err(kind_conflict(name)),
        None => Err(format!("unknown sync.channel '{}'", name)),
    }
}

#[derive(Default)]
pub(crate) struct SyncMutex {
    locked: Mutex<bool>,
    cv: Condvar,
}

impl SyncMutex {
    fn try_lock(&self) -> bool {
        let mut locked = self.locked.lock().unwrap_or_else(|error| error.into_inner());
        if *locked {
            false
        } else {
            *locked = true;
            true
        }
    }

    /// 等待解锁最多 `slice`；已经被释放时立即返回，不空转。
    fn wait_slice(&self, slice: Duration) {
        let locked = self.locked.lock().unwrap_or_else(|error| error.into_inner());
        if !*locked {
            return;
        }
        let _ = self
            .cv
            .wait_timeout(locked, slice)
            .unwrap_or_else(|error| error.into_inner());
    }

    fn unlock(&self) {
        let mut locked = self.locked.lock().unwrap_or_else(|error| error.into_inner());
        *locked = false;
        self.cv.notify_one();
    }

    fn is_locked(&self) -> bool {
        *self.locked.lock().unwrap_or_else(|error| error.into_inner())
    }
}

pub(crate) struct SyncAtomic {
    value: AtomicI64,
}

impl SyncAtomic {
    fn load(&self) -> i64 {
        self.value.load(Ordering::SeqCst)
    }

    fn store(&self, value: i64) {
        self.value.store(value, Ordering::SeqCst);
    }

    fn add(&self, delta: i64) -> i64 {
        self.value.fetch_add(delta, Ordering::SeqCst)
    }

    fn swap(&self, value: i64) -> i64 {
        self.value.swap(value, Ordering::SeqCst)
    }

    fn compare_swap(&self, expected: i64, new: i64) -> bool {
        self.value
            .compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

struct ChannelInner {
    queue: VecDeque<Wire>,
    capacity: usize,
    closed: bool,
}

pub(crate) struct SyncChannel {
    inner: Mutex<ChannelInner>,
    not_empty: Condvar,
    not_full: Condvar,
}

enum SendOutcome {
    Sent,
    Full(Wire),
    Closed,
}

enum RecvOutcome {
    Got(Wire),
    Empty,
    Closed,
}

impl SyncChannel {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(ChannelInner {
                queue: VecDeque::with_capacity(capacity.min(1024)),
                capacity,
                closed: false,
            }),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
        }
    }

    fn send_slice(&self, wire: Wire, slice: Duration) -> SendOutcome {
        let deadline = Instant::now() + slice;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let wire = wire;
        loop {
            if inner.closed {
                return SendOutcome::Closed;
            }
            if inner.queue.len() < inner.capacity {
                inner.queue.push_back(wire);
                self.not_empty.notify_one();
                return SendOutcome::Sent;
            }
            if slice.is_zero() {
                return SendOutcome::Full(wire);
            }
            let now = Instant::now();
            if now >= deadline {
                return SendOutcome::Full(wire);
            }
            let wait = (deadline - now).min(SLICE);
            let (guard, _) = self
                .not_full
                .wait_timeout(inner, wait)
                .unwrap_or_else(|error| error.into_inner());
            inner = guard;
        }
    }

    fn recv_slice(&self, slice: Duration) -> RecvOutcome {
        let deadline = Instant::now() + slice;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(wire) = inner.queue.pop_front() {
                self.not_full.notify_one();
                return RecvOutcome::Got(wire);
            }
            if inner.closed {
                return RecvOutcome::Closed;
            }
            if slice.is_zero() {
                return RecvOutcome::Empty;
            }
            let now = Instant::now();
            if now >= deadline {
                return RecvOutcome::Empty;
            }
            let wait = (deadline - now).min(SLICE);
            let (guard, _) = self
                .not_empty
                .wait_timeout(inner, wait)
                .unwrap_or_else(|error| error.into_inner());
            inner = guard;
        }
    }

    fn close(&self) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if inner.closed {
            return false;
        }
        inner.closed = true;
        self.not_empty.notify_all();
        self.not_full.notify_all();
        true
    }

    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .queue
            .len()
    }

    fn capacity(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity
    }

    fn is_closed(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed
    }
}

fn handle(class: &str, name: &str) -> Value {
    Value::ary(class, vec![("name".to_string(), Value::string(name))])
}

fn handle_name(ary: &Rc<RefCell<Ary>>) -> String {
    ary.borrow()
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("sync name must not be empty".to_string());
    }
    if name.len() > 128 {
        return Err("sync name is too long (max 128 bytes)".to_string());
    }
    Ok(())
}

impl Runtime {
    /// `std/sync`：按名字创建 / 获取进程级并发原语。
    pub(crate) fn sync_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "mutex" => {
                let key = self.string_arg(&args, 0, "sync.mutex", line)?;
                check_name(&key)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                ensure_mutex(&key).map_err(|message| {
                    crate::value::Flow::Error(self.runtime_error(message, line))
                })?;
                Ok(handle("sync.mutex", &key))
            }
            "atomic" => {
                let key = self.string_arg(&args, 0, "sync.atomic", line)?;
                check_name(&key)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                let initial = self.int_arg(&args, 1, "sync.atomic", line)?;
                ensure_atomic(&key, initial)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                Ok(handle("sync.atomic", &key))
            }
            "channel" => {
                let key = self.string_arg(&args, 0, "sync.channel", line)?;
                check_name(&key)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                let capacity = self.int_arg(&args, 1, "sync.channel", line)?;
                if capacity <= 0 || capacity > MAX_CAPACITY {
                    return self.error(
                        format!("sync.channel capacity must be 1..{}", MAX_CAPACITY),
                        line,
                    );
                }
                ensure_channel(&key, capacity as usize).map_err(|message| {
                    crate::value::Flow::Error(self.runtime_error(message, line))
                })?;
                Ok(handle("sync.channel", &key))
            }
            "remove" => {
                let key = self.string_arg(&args, 0, "sync.remove", line)?;
                Ok(Value::Bool(lock_shard(&key).remove(&key).is_some()))
            }
            "exists" => {
                let key = self.string_arg(&args, 0, "sync.exists", line)?;
                Ok(Value::Bool(lock_shard(&key).contains_key(&key)))
            }
            other => self.error(format!("std/sync has no function '{}'", other), line),
        }
    }

    pub(crate) fn sync_mutex_method(
        &mut self,
        ary: &Rc<RefCell<Ary>>,
        method: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let key = handle_name(ary);
        let mutex = handle_mutex(&key).map_err(|message| {
            crate::value::Flow::Error(self.runtime_error(message, line))
        })?;
        match method {
            "lock" => {
                self.lock_mutex(&mutex, None, line)?;
                Ok(Value::Null)
            }
            "try_lock" => Ok(Value::Bool(mutex.try_lock())),
            "lock_timeout" => {
                let ms = self.int_arg(&args, 0, "lock_timeout", line)?;
                if ms < 0 || ms > MAX_TIMEOUT_MS {
                    return self.error("lock_timeout out of range (0..86400000)", line);
                }
                let locked = self.lock_mutex(&mutex, Some(Duration::from_millis(ms as u64)), line)?;
                Ok(Value::Bool(locked))
            }
            "unlock" => {
                mutex.unlock();
                Ok(Value::Null)
            }
            "is_locked" => Ok(Value::Bool(mutex.is_locked())),
            "name" => Ok(Value::string(key)),
            other => self.error(format!("sync.mutex has no method '{}'", other), line),
        }
    }

    /// 阻塞拿锁；每 20ms 检查一次请求截止时间，超时则中断处理函数。
    fn lock_mutex(
        &mut self,
        mutex: &SyncMutex,
        timeout: Option<Duration>,
        line: u32,
    ) -> FlowResult<bool> {
        let deadline = timeout.map(|value| Instant::now() + value);
        loop {
            if mutex.try_lock() {
                return Ok(true);
            }
            self.check_deadline(line)?;
            match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(false);
                    }
                    mutex.wait_slice((deadline - now).min(SLICE));
                }
                None => mutex.wait_slice(SLICE),
            }
        }
    }

    pub(crate) fn sync_atomic_method(
        &mut self,
        ary: &Rc<RefCell<Ary>>,
        method: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let key = handle_name(ary);
        let atomic = handle_atomic(&key).map_err(|message| {
            crate::value::Flow::Error(self.runtime_error(message, line))
        })?;
        match method {
            "load" => Ok(Value::Int(atomic.load())),
            "store" => {
                let value = self.int_arg(&args, 0, "store", line)?;
                atomic.store(value);
                Ok(Value::Null)
            }
            "add" => {
                let delta = self.int_arg(&args, 0, "add", line)?;
                Ok(Value::Int(atomic.add(delta)))
            }
            "sub" => {
                let delta = self.int_arg(&args, 0, "sub", line)?;
                Ok(Value::Int(atomic.add(-delta)))
            }
            "swap" => {
                let value = self.int_arg(&args, 0, "swap", line)?;
                Ok(Value::Int(atomic.swap(value)))
            }
            "compare_swap" => {
                let expected = self.int_arg(&args, 0, "compare_swap", line)?;
                let new = self.int_arg(&args, 1, "compare_swap", line)?;
                Ok(Value::Bool(atomic.compare_swap(expected, new)))
            }
            "name" => Ok(Value::string(key)),
            other => self.error(format!("sync.atomic has no method '{}'", other), line),
        }
    }

    pub(crate) fn sync_channel_method(
        &mut self,
        ary: &Rc<RefCell<Ary>>,
        method: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let key = handle_name(ary);
        let channel = handle_channel(&key).map_err(|message| {
            crate::value::Flow::Error(self.runtime_error(message, line))
        })?;
        match method {
            "send" => {
                let value = match args.first() {
                    Some(value) => value.clone(),
                    None => return self.error("sync.channel.send expects 1 value", line),
                };
                let wire = Wire::from_value(&value)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                self.send_wire(&channel, wire, None, line)?;
                Ok(Value::Null)
            }
            "try_send" => {
                let value = match args.first() {
                    Some(value) => value.clone(),
                    None => return self.error("sync.channel.try_send expects 1 value", line),
                };
                let wire = Wire::from_value(&value)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                match channel.send_slice(wire, Duration::ZERO) {
                    SendOutcome::Sent => Ok(Value::Bool(true)),
                    SendOutcome::Full(_) | SendOutcome::Closed => Ok(Value::Bool(false)),
                }
            }
            "send_timeout" => {
                let value = match args.first() {
                    Some(value) => value.clone(),
                    None => {
                        return self.error(
                            "sync.channel.send_timeout expects (value, ms)",
                            line,
                        )
                    }
                };
                let ms = self.int_arg(&args, 1, "send_timeout", line)?;
                if ms < 0 || ms > MAX_TIMEOUT_MS {
                    return self.error("send_timeout out of range (0..86400000)", line);
                }
                let wire = Wire::from_value(&value)
                    .map_err(|message| crate::value::Flow::Error(self.runtime_error(message, line)))?;
                let leftover =
                    self.send_wire(&channel, wire, Some(Duration::from_millis(ms as u64)), line)?;
                Ok(Value::Bool(leftover.is_none()))
            }
            "recv" => match self.recv_wire(&channel, None, line)? {
                Some(wire) => Ok(Value::Some(Box::new(wire.into_value()))),
                None => Ok(Value::None),
            },
            "try_recv" => match channel.recv_slice(Duration::ZERO) {
                RecvOutcome::Got(wire) => Ok(Value::Some(Box::new(wire.into_value()))),
                RecvOutcome::Empty | RecvOutcome::Closed => Ok(Value::None),
            },
            "recv_timeout" => {
                let ms = self.int_arg(&args, 0, "recv_timeout", line)?;
                if ms < 0 || ms > MAX_TIMEOUT_MS {
                    return self.error("recv_timeout out of range (0..86400000)", line);
                }
                match self.recv_wire(&channel, Some(Duration::from_millis(ms as u64)), line)? {
                    Some(wire) => Ok(Value::Some(Box::new(wire.into_value()))),
                    None => Ok(Value::None),
                }
            }
            "close" => {
                channel.close();
                Ok(Value::Null)
            }
            "is_closed" => Ok(Value::Bool(channel.is_closed())),
            "len" => Ok(Value::Int(channel.len() as i64)),
            "capacity" => Ok(Value::Int(channel.capacity() as i64)),
            "name" => Ok(Value::string(key)),
            other => self.error(format!("sync.channel has no method '{}'", other), line),
        }
    }

    /// 发送：返回 `Some(wire)` 表示超时（值未发出），`None` 表示已发送。
    fn send_wire(
        &mut self,
        channel: &SyncChannel,
        wire: Wire,
        timeout: Option<Duration>,
        line: u32,
    ) -> FlowResult<Option<Wire>> {
        let deadline = timeout.map(|value| Instant::now() + value);
        let mut wire = wire;
        loop {
            let slice = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(Some(wire));
                    }
                    (deadline - now).min(SLICE)
                }
                None => SLICE,
            };
            match channel.send_slice(wire, slice) {
                SendOutcome::Sent => return Ok(None),
                SendOutcome::Closed => return self.error("sync.channel is closed", line),
                SendOutcome::Full(value) => wire = value,
            }
            self.check_deadline(line)?;
        }
    }

    fn recv_wire(
        &mut self,
        channel: &SyncChannel,
        timeout: Option<Duration>,
        line: u32,
    ) -> FlowResult<Option<Wire>> {
        let deadline = timeout.map(|value| Instant::now() + value);
        loop {
            let slice = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(None);
                    }
                    (deadline - now).min(SLICE)
                }
                None => SLICE,
            };
            match channel.recv_slice(slice) {
                RecvOutcome::Got(wire) => return Ok(Some(wire)),
                RecvOutcome::Closed => return Ok(None),
                RecvOutcome::Empty => {}
            }
            self.check_deadline(line)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn mutex_excludes_threads() {
        let mutex = Arc::new(SyncMutex::default());
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let mutex = Arc::clone(&mutex);
            let inside = Arc::clone(&inside);
            let peak = Arc::clone(&peak);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    while !mutex.try_lock() {
                        mutex.wait_slice(Duration::from_millis(1));
                    }
                    let current = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    inside.fetch_sub(1, Ordering::SeqCst);
                    mutex.unlock();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn channel_hands_off_between_threads() {
        let channel = Arc::new(SyncChannel::new(4));
        let producer = Arc::clone(&channel);
        let handle = std::thread::spawn(move || {
            for i in 0..100 {
                assert!(matches!(
                    producer.send_slice(Wire::Int(i), Duration::from_secs(1)),
                    SendOutcome::Sent
                ));
            }
            producer.close();
        });
        let mut received = Vec::new();
        loop {
            match channel.recv_slice(Duration::from_secs(1)) {
                RecvOutcome::Got(Wire::Int(value)) => received.push(value),
                RecvOutcome::Got(_) => panic!("unexpected payload type"),
                RecvOutcome::Empty => panic!("timed out before close"),
                RecvOutcome::Closed => break,
            }
        }
        handle.join().unwrap();
        assert_eq!(received, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn channel_timeout_returns_empty() {
        let channel = SyncChannel::new(1);
        assert!(matches!(
            channel.recv_slice(Duration::from_millis(10)),
            RecvOutcome::Empty
        ));
        assert!(matches!(
            channel.send_slice(Wire::Null, Duration::ZERO),
            SendOutcome::Sent
        ));
        assert!(matches!(
            channel.send_slice(Wire::Null, Duration::from_millis(10)),
            SendOutcome::Full(_)
        ));
        assert!(matches!(
            channel.recv_slice(Duration::ZERO),
            RecvOutcome::Got(Wire::Null)
        ));
    }

    #[test]
    fn wire_preserves_value_shape() {
        let value = Value::ary(
            "demo",
            vec![
                ("id".to_string(), Value::Int(7)),
                (
                    "items".to_string(),
                    Value::vec(vec![Value::string("a"), Value::Float(1.5)]),
                ),
            ],
        );
        let wire = Wire::from_value(&value).unwrap();
        let back = wire.into_value();
        let ary = match &back {
            Value::Ary(ary) => ary.borrow(),
            other => panic!("expected ary, got {}", crate::value::format_value(other)),
        };
        assert_eq!(ary.class, "demo");
        assert!(matches!(ary.get("id"), Some(Value::Int(7))));
    }

    #[test]
    fn wire_rejects_functions() {
        let value = Value::func_rc(Rc::new(crate::value::Function {
            name: "f".to_string(),
            kind: crate::value::FuncKind::Native {
                arity: 0,
                body: Rc::new(
                    |_host: &mut Runtime, _args: Vec<Value>, _line: u32| Ok(Value::Null),
                ),
            },
        }));
        assert!(Wire::from_value(&value).is_err());
    }
}

//! 进程级缓存：TTL + LRU，值为字符串（由调用方自行 JSON 编码）。
//!
//! 用 `HashMap` + `BTreeMap<序号, key>` 组成 LRU：读取时把序号抬到最新（O(log n)），
//! 满了淘汰序号最小的一项，避免线性扫描整个表。
//! 缓存是进程级共享的（跨工作线程），因此只存 `String`，不存 Rip `Value`。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

const DEFAULT_MAX_ENTRIES: usize = 4096;

struct Entry {
    value: String,
    /// 过期时间（毫秒时间戳）；0 表示永不过期。
    expires_ms: i64,
    seq: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// 访问序号 → key，用于 O(log n) 找到最久未使用的键。
    order: BTreeMap<u64, String>,
    next_seq: u64,
    max_entries: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

fn state() -> &'static Mutex<Inner> {
    static CACHE: OnceLock<Mutex<Inner>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Inner {
            max_entries: DEFAULT_MAX_ENTRIES,
            ..Inner::default()
        })
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_millis() as i64)
        .unwrap_or(0)
}

/// 写入缓存；`ttl_ms <= 0` 表示永不过期。
pub fn set(key: &str, value: String, ttl_ms: i64) {
    let Ok(mut inner) = state().lock() else {
        return;
    };
    let expires_ms = if ttl_ms > 0 {
        now_ms().saturating_add(ttl_ms)
    } else {
        0
    };
    if let Some(previous) = inner.entries.get(key).map(|entry| entry.seq) {
        inner.order.remove(&previous);
    }
    inner.next_seq += 1;
    let seq = inner.next_seq;
    inner.entries.insert(
        key.to_string(),
        Entry {
            value,
            expires_ms,
            seq,
        },
    );
    inner.order.insert(seq, key.to_string());
    // 超限时淘汰最久未使用的条目（先淘汰已过期的，避免挤掉热数据）。
    while inner.entries.len() > inner.max_entries {
        if !evict_expired(&mut inner) {
            let Some((_, oldest)) = inner.order.pop_first() else {
                break;
            };
            inner.entries.remove(&oldest);
            inner.evictions += 1;
        }
    }
}

/// 读取缓存；命中会刷新 LRU 位置。返回 `None` 表示未命中或已过期。
pub fn get(key: &str) -> Option<String> {
    let Ok(mut inner) = state().lock() else {
        return None;
    };
    let expired = match inner.entries.get(key) {
        Some(entry) => entry.expires_ms > 0 && entry.expires_ms <= now_ms(),
        None => {
            inner.misses += 1;
            return None;
        }
    };
    if expired {
        if let Some(entry) = inner.entries.remove(key) {
            inner.order.remove(&entry.seq);
        }
        inner.misses += 1;
        return None;
    }
    inner.hits += 1;
    inner.next_seq += 1;
    let seq = inner.next_seq;
    let (old_seq, value) = {
        let entry = inner.entries.get_mut(key).expect("checked above");
        let old_seq = entry.seq;
        entry.seq = seq;
        (old_seq, entry.value.clone())
    };
    inner.order.remove(&old_seq);
    inner.order.insert(seq, key.to_string());
    Some(value)
}

pub fn delete(key: &str) -> bool {
    let Ok(mut inner) = state().lock() else {
        return false;
    };
    match inner.entries.remove(key) {
        Some(entry) => {
            inner.order.remove(&entry.seq);
            true
        }
        None => false,
    }
}

pub fn clear() {
    if let Ok(mut inner) = state().lock() {
        inner.entries.clear();
        inner.order.clear();
    }
}

pub fn len() -> usize {
    state()
        .lock()
        .map(|inner| inner.entries.len())
        .unwrap_or(0)
}

/// 设置容量上限（16..=1_000_000），返回设置后的值。
pub fn configure(max_entries: usize) -> usize {
    let max_entries = max_entries.clamp(16, 1_000_000);
    if let Ok(mut inner) = state().lock() {
        inner.max_entries = max_entries;
        while inner.entries.len() > inner.max_entries {
            if !evict_expired(&mut inner) {
                let Some((_, oldest)) = inner.order.pop_first() else {
                    break;
                };
                inner.entries.remove(&oldest);
                inner.evictions += 1;
            }
        }
    }
    max_entries
}

/// `[hits, misses, evictions, entries]`，供 `cache.stats()` 与指标使用。
pub fn snapshot() -> (u64, u64, u64, usize) {
    match state().lock() {
        Ok(inner) => (
            inner.hits,
            inner.misses,
            inner.evictions,
            inner.entries.len(),
        ),
        Err(_) => (0, 0, 0, 0),
    }
}

fn evict_expired(inner: &mut Inner) -> bool {
    let now = now_ms();
    let expired: Vec<String> = inner
        .entries
        .iter()
        .filter(|(_, entry)| entry.expires_ms > 0 && entry.expires_ms <= now)
        .map(|(key, _)| key.clone())
        .collect();
    if expired.is_empty() {
        return false;
    }
    for key in expired {
        if let Some(entry) = inner.entries.remove(&key) {
            inner.order.remove(&entry.seq);
            inner.evictions += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 进程级缓存是全局状态，测试之间必须串行执行，否则互相 clear / 淘汰。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn set_get_and_delete() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear();
        set("a", "1".to_string(), 0);
        assert_eq!(get("a").as_deref(), Some("1"));
        assert!(delete("a"));
        assert!(get("a").is_none());
    }

    #[test]
    fn expires_and_evicts() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        clear();
        configure(16);
        set("short", "x".to_string(), 1);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(get("short").is_none());

        for index in 0..20 {
            set(&format!("k{}", index), index.to_string(), 0);
        }
        // 容量上限 16：最早的键已被淘汰。
        assert!(len() <= 16);
        assert!(get("k0").is_none());
        assert!(get("k19").is_some());
    }
}

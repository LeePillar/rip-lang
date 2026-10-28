//! 持久化任务队列（SQLite 后端）。
//!
//! 语义与常见作业队列一致：`enqueue` 入队 → `reserve` 取一条并加租约 →
//! 处理成功 `ack`、失败 `fail`（按次数退避重试，超过上限进入 `failed`）。
//! 进程崩溃时未 ack 的任务由 `release_expired` 重新放回队列，保证至少一次投递。
//!
//! 连接按 DSN 复用（进程级），SQLite 以 WAL + busy_timeout 运行，
//! 多工作线程共享同一个连接，写操作天然串行。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection, OptionalExtension};

/// 失败重试上限：达到后转为 `failed`，不再自动重试。
const MAX_ATTEMPTS: i64 = 5;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_millis() as i64)
        .unwrap_or(0)
}

fn store() -> &'static Mutex<HashMap<String, Connection>> {
    static STORE: OnceLock<Mutex<HashMap<String, Connection>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程级缓存的 DSN 连接上限：DSN 由业务代码拼接时防止连接句柄无限增长。
const MAX_CACHED_DSN: usize = 32;

fn with_conn<T>(
    dsn: &str,
    action: impl FnOnce(&mut Connection) -> Result<T, String>,
) -> Result<T, String> {
    let mut store = store()
        .lock()
        .map_err(|_| "queue store is poisoned".to_string())?;
    if !store.contains_key(dsn) {
        // 超出上限时驱逐一个既有连接（SQLite 句柄，关闭即释放）。
        if store.len() >= MAX_CACHED_DSN {
            if let Some(key) = store.keys().next().cloned() {
                store.remove(&key);
            }
        }
        let conn = open(dsn)?;
        store.insert(dsn.to_string(), conn);
    }
    let conn = store.get_mut(dsn).expect("inserted above");
    action(conn)
}

fn open(dsn: &str) -> Result<Connection, String> {
    let path = crate::db::sqlite_path(dsn);
    let conn = if path == ":memory:" || path.is_empty() {
        Connection::open_in_memory()
    } else {
        Connection::open(&path)
    }
    .map_err(|error| format!("cannot open queue database: {}", error))?;
    // WAL 提升并发读性能；busy_timeout 避免瞬时锁冲突直接失败。
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rip_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            queue TEXT NOT NULL,
            payload TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'pending',
            attempts INTEGER NOT NULL DEFAULT 0,
            available_at INTEGER NOT NULL,
            reserved_until INTEGER NOT NULL DEFAULT 0,
            last_error TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rip_jobs_ready
            ON rip_jobs(queue, state, available_at);",
    )
    .map_err(|error| format!("cannot create queue table: {}", error))?;
    Ok(conn)
}

/// 入队；`delay_ms > 0` 时延迟可见。返回任务 id。
pub fn enqueue(dsn: &str, queue: &str, payload: &str, delay_ms: i64) -> Result<i64, String> {
    let now = now_ms();
    let available_at = now.saturating_add(delay_ms.max(0));
    with_conn(dsn, |conn| {
        conn.execute(
            "INSERT INTO rip_jobs (queue, payload, state, attempts, available_at, reserved_until, created_at)
             VALUES (?1, ?2, 'pending', 0, ?3, 0, ?4)",
            params![queue, payload, available_at, now],
        )
        .map_err(|error| format!("cannot enqueue job: {}", error))?;
        Ok(conn.last_insert_rowid())
    })
}

/// 取一条可执行任务并加租约；没有任务时返回 None。
pub fn reserve(
    dsn: &str,
    queue: &str,
    lease_ms: i64,
) -> Result<Option<(i64, String, i64)>, String> {
    let now = now_ms();
    let reserved_until = now.saturating_add(lease_ms.max(1000));
    with_conn(dsn, |conn| {
        let transaction = conn
            .transaction()
            .map_err(|error| format!("cannot start transaction: {}", error))?;
        let found: Option<(i64, String, i64)> = transaction
            .query_row(
                "SELECT id, payload, attempts FROM rip_jobs
                 WHERE queue = ?1 AND state = 'pending' AND available_at <= ?2
                 ORDER BY available_at, id LIMIT 1",
                params![queue, now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("cannot select job: {}", error))?;
        let Some((id, payload, attempts)) = found else {
            transaction
                .commit()
                .map_err(|error| format!("cannot commit: {}", error))?;
            return Ok(None);
        };
        transaction
            .execute(
                "UPDATE rip_jobs SET state = 'running', attempts = attempts + 1,
                        reserved_until = ?2 WHERE id = ?1",
                params![id, reserved_until],
            )
            .map_err(|error| format!("cannot reserve job: {}", error))?;
        transaction
            .commit()
            .map_err(|error| format!("cannot commit: {}", error))?;
        Ok(Some((id, payload, attempts + 1)))
    })
}

/// 确认完成：删除任务。返回是否确实删除了一行。
pub fn ack(dsn: &str, id: i64) -> Result<bool, String> {
    with_conn(dsn, |conn| {
        let deleted = conn
            .execute("DELETE FROM rip_jobs WHERE id = ?1", params![id])
            .map_err(|error| format!("cannot ack job: {}", error))?;
        Ok(deleted > 0)
    })
}

/// 标记失败：未超过重试上限时按次数指数退避，超过则转为 `failed`。
pub fn fail(dsn: &str, id: i64, message: &str) -> Result<String, String> {
    let now = now_ms();
    with_conn(dsn, |conn| {
        let attempts: Option<i64> = conn
            .query_row(
                "SELECT attempts FROM rip_jobs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("cannot read job: {}", error))?;
        let Some(attempts) = attempts else {
            return Ok("missing".to_string());
        };
        if attempts >= MAX_ATTEMPTS {
            conn.execute(
                "UPDATE rip_jobs SET state = 'failed', last_error = ?2, reserved_until = 0 WHERE id = ?1",
                params![id, message],
            )
            .map_err(|error| format!("cannot mark job failed: {}", error))?;
            return Ok("failed".to_string());
        }
        // 退避：1s、2s、4s、8s，避免失败任务立刻被再次取出。
        let backoff = 1000i64.saturating_mul(1 << (attempts - 1).clamp(0, 4));
        conn.execute(
            "UPDATE rip_jobs SET state = 'pending', available_at = ?2, reserved_until = 0, last_error = ?3
             WHERE id = ?1",
            params![id, now.saturating_add(backoff), message],
        )
        .map_err(|error| format!("cannot requeue job: {}", error))?;
        Ok("retry".to_string())
    })
}

/// 回收租约过期的任务（进程崩溃恢复）；返回放回队列的条数。
pub fn release_expired(dsn: &str) -> Result<i64, String> {
    let now = now_ms();
    with_conn(dsn, |conn| {
        let released = conn
            .execute(
                "UPDATE rip_jobs SET state = 'pending', available_at = ?1, reserved_until = 0
                 WHERE state = 'running' AND reserved_until > 0 AND reserved_until <= ?1",
                params![now],
            )
            .map_err(|error| format!("cannot release expired jobs: {}", error))?;
        Ok(released as i64)
    })
}

/// 队列统计：`[pending, running, failed]`。
pub fn stats(dsn: &str, queue: &str) -> Result<(i64, i64, i64), String> {
    with_conn(dsn, |conn| {
        let mut statement = conn
            .prepare(
                "SELECT state, COUNT(*) FROM rip_jobs WHERE queue = ?1 GROUP BY state",
            )
            .map_err(|error| format!("cannot prepare stats: {}", error))?;
        let rows = statement
            .query_map(params![queue], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| format!("cannot query stats: {}", error))?;
        let (mut pending, mut running, mut failed) = (0, 0, 0);
        for row in rows {
            let (state, count) =
                row.map_err(|error| format!("cannot read stats row: {}", error))?;
            match state.as_str() {
                "pending" => pending = count,
                "running" => running = count,
                "failed" => failed = count,
                _ => {}
            }
        }
        Ok((pending, running, failed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dsn(tag: &str) -> String {
        format!("sqlite::memory:?queue-test-{}", tag)
    }

    #[test]
    fn enqueue_reserve_ack_flow() {
        let dsn = dsn("flow");
        let id = enqueue(&dsn, "mail", "{\"to\":1}", 0).expect("enqueue");
        let job = reserve(&dsn, "mail", 5000).expect("reserve").expect("job");
        assert_eq!(job.0, id);
        assert_eq!(job.1, "{\"to\":1}");
        assert_eq!(job.2, 1);
        // 已租出的任务不会被再次取出。
        assert!(reserve(&dsn, "mail", 5000).expect("reserve").is_none());
        assert!(ack(&dsn, id).expect("ack"));
        assert!(reserve(&dsn, "mail", 5000).expect("reserve").is_none());
    }

    #[test]
    fn failed_job_retries_then_dies() {
        let dsn = dsn("retry");
        let id = enqueue(&dsn, "mail", "{}", 0).expect("enqueue");
        for attempt in 0..MAX_ATTEMPTS {
            // 退避会让任务暂时不可见：测试里直接把它拉回可执行状态。
            with_conn(&dsn, |conn| {
                conn.execute(
                    "UPDATE rip_jobs SET available_at = 0 WHERE id = ?1",
                    params![id],
                )
                .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("make visible");
            let job = reserve(&dsn, "mail", 1000).expect("reserve").expect("job");
            assert_eq!(job.0, id);
            let state = fail(&dsn, id, "boom").expect("fail");
            if attempt + 1 >= MAX_ATTEMPTS {
                assert_eq!(state, "failed");
            } else {
                assert_eq!(state, "retry");
            }
        }
        let (pending, running, failed) = stats(&dsn, "mail").expect("stats");
        assert_eq!((pending, running, failed), (0, 0, 1));
    }

    #[test]
    fn expired_lease_is_released() {
        let dsn = dsn("lease");
        let id = enqueue(&dsn, "sms", "{}", 0).expect("enqueue");
        reserve(&dsn, "sms", 1000).expect("reserve").expect("job");
        // 手动把租约改到过去，模拟进程崩溃。
        with_conn(&dsn, |conn| {
            conn.execute(
                "UPDATE rip_jobs SET reserved_until = 1 WHERE id = ?1",
                params![id],
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("update");
        assert_eq!(release_expired(&dsn).expect("release"), 1);
        assert!(reserve(&dsn, "sms", 1000).expect("reserve").is_some());
    }
}

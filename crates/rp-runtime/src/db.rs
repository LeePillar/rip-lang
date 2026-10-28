//! 数据库支持：SQLite、MySQL、PostgreSQL、SQL Server。
//!
//! 对外统一使用 `?` 作为参数占位符，内部按驱动改写成 `$1` / `@P1`；
//! 链式 ORM 生成的 SQL 会按方言选择标识符引用方式（MySQL 反引号，其余双引号）、
//! 分页语法与自增主键获取方式。

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mysql::prelude::Queryable as MySqlQueryable;
use mysql::{Conn as MySqlConn, Opts as MySqlOpts, Value as MySqlValue};
use postgres::types::ToSql as PgToSql;
use postgres::{Client as PgClient, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;
use rusqlite::types::Value as SqliteValue;
use rusqlite::Connection as SqliteConn;
use tiberius::{
    AuthMethod, Client as MsSqlClient, ColumnData as MsColumnData, Config as MsSqlConfig,
    EncryptionLevel,
};
use tokio::net::TcpStream;
use tokio::runtime::Runtime as TokioRuntime;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::rt::Runtime;
use crate::value::{deep_clone, value_to_json, Ary, FlowResult, Value};

/// 数据库方言。决定标识符引用、占位符和分页语法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    MySql,
    Postgres,
    MsSql,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Sqlite => "sqlite",
            Dialect::MySql => "mysql",
            Dialect::Postgres => "postgres",
            Dialect::MsSql => "mssql",
        }
    }

    /// 标识符引用：MySQL 用反引号，其余用双引号（SQL Server 默认开启 QUOTED_IDENTIFIER）。
    fn quote(self, part: &str) -> String {
        match self {
            Dialect::MySql => format!("`{}`", part.replace('`', "``")),
            _ => format!("\"{}\"", part.replace('"', "\"\"")),
        }
    }

    fn placeholder(self, index: usize) -> String {
        match self {
            Dialect::Sqlite | Dialect::MySql => "?".to_string(),
            Dialect::Postgres => format!("${}", index),
            Dialect::MsSql => format!("@P{}", index),
        }
    }
}

type MsSqlConn = MsSqlClient<Compat<TcpStream>>;

pub(crate) enum ConnKind {
    Sqlite(SqliteConn),
    MySql(MySqlConn),
    Postgres(PgClient),
    MsSql(MsSqlConn),
}

/// 连接池参数。默认值面向「多工作线程 + 短请求」的典型服务端场景。
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// 每个 DSN 允许同时存活的连接上限（空闲 + 借出）。
    pub max_open: usize,
    /// 每个 DSN 允许缓存的空闲连接上限。
    pub max_idle: usize,
    /// 池满时的等待上限；超时后 `db.open` 返回错误。
    pub acquire_timeout: Duration,
    /// 空闲超过该时长的连接在下次借出时关闭，避免用到被服务端断开的连接。
    pub idle_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_open: 32,
            max_idle: 8,
            acquire_timeout: Duration::from_millis(5000),
            idle_timeout: Duration::from_secs(300),
        }
    }
}

impl PoolConfig {
    /// 修正不合法组合：空闲上限不能超过存活上限，存活上限至少为 1。
    fn normalized(mut self) -> Self {
        self.max_open = self.max_open.max(1);
        self.max_idle = self.max_idle.min(self.max_open);
        self
    }
}

/// 一条已借出的连接。持有 `Permit`，连接真正关闭时把名额还给池。
pub(crate) struct DbConn {
    kind: ConnKind,
    dialect: Dialect,
    database: String,
    /// 借出凭证：`DbConn` 被丢弃时递减该 DSN 的存活计数。
    permit: Permit,
}

/// 连接名额。池进程内共享，因此借出/归还可跨线程。
struct Permit {
    /// 用 `Weak` 避免「池 → 空闲连接 → 凭证 → 池」的引用环。
    pool: std::sync::Weak<Pool>,
    /// `Arc<str>` 让归还路径只做引用计数，不再克隆 DSN 字符串。
    dsn: Arc<str>,
}

impl Permit {
    /// 解除凭证：池名额已在锁内就地归还，丢弃连接时不再触发 `Drop` 的二次加锁。
    fn disarm(&mut self) {
        self.pool = std::sync::Weak::new();
    }
}

/// 归还一个存活名额；返回是否真的释放了计数。
fn release_slot(state: &mut PoolState, dsn: &str) -> bool {
    match state.open.get_mut(dsn) {
        Some(count) => {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.open.remove(dsn);
            }
            true
        }
        None => false,
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let Some(pool) = self.pool.upgrade() else {
            return;
        };
        let freed = {
            let mut state = lock(&pool.state);
            release_slot(&mut state, &self.dsn)
        };
        if freed && pool.waiters.load(Ordering::SeqCst) > 0 {
            // 只有确实有线程在等名额时才付出唤醒成本。
            pool.ready.notify_all();
        }
    }
}

struct IdleConn {
    conn: DbConn,
    /// 最近一次归还时间，用于空闲超时淘汰。
    returned: Instant,
    /// 创建该连接的线程：SQL Server 连接依赖创建它的 tokio 运行时，不能跨线程复用。
    owner: std::thread::ThreadId,
}

struct PoolState {
    /// 按 DSN 缓存的空闲连接。
    idle: HashMap<String, Vec<IdleConn>>,
    /// 每个 DSN 的存活连接数（空闲 + 借出）；借出期间由 `Permit` 记账。
    open: HashMap<String, usize>,
}

/// 池的借出结果：复用空闲、占新名额待建、或已满需等待。
enum Take {
    Reused(DbConn),
    Fresh(Permit),
    Full,
}

/// 进程内共享的连接池：HTTP 工作线程、后台任务与 `parallel` 都从这里借连接。
///
/// 池参数放在原子变量里：读路径（每次借出/归还）零锁读取配置，
/// 只有 `configure_pool` 启动时写一次。借出合并为单次加锁的 `take`，
/// 每请求的锁竞争从 3~4 次降到 2 次（借 + 还）。
struct Pool {
    state: Mutex<PoolState>,
    ready: Condvar,
    max_open: AtomicUsize,
    max_idle: AtomicUsize,
    /// 超时参数以微秒存储，避免亚毫秒配置被截断为 0。
    acquire_timeout: AtomicU64,
    idle_timeout: AtomicU64,
    /// 正在 `wait_for` 里等名额的线程数；为 0 时归还连接不做任何唤醒。
    waiters: AtomicUsize,
}

impl Pool {
    fn new(config: PoolConfig) -> Self {
        let config = config.normalized();
        Self {
            state: Mutex::new(PoolState {
                idle: HashMap::new(),
                open: HashMap::new(),
            }),
            ready: Condvar::new(),
            max_open: AtomicUsize::new(config.max_open),
            max_idle: AtomicUsize::new(config.max_idle),
            acquire_timeout: AtomicU64::new(config.acquire_timeout.as_micros() as u64),
            idle_timeout: AtomicU64::new(config.idle_timeout.as_micros() as u64),
            waiters: AtomicUsize::new(0),
        }
    }

    fn set_config(&self, config: PoolConfig) {
        let config = config.normalized();
        self.max_open.store(config.max_open, Ordering::Relaxed);
        self.max_idle.store(config.max_idle, Ordering::Relaxed);
        self.acquire_timeout
            .store(config.acquire_timeout.as_micros() as u64, Ordering::Relaxed);
        self.idle_timeout
            .store(config.idle_timeout.as_micros() as u64, Ordering::Relaxed);
    }

    /// 单次加锁完成「复用空闲 → 占新名额 → 判定已满」三步。
    /// 过期或跨线程的 SQL Server 连接就地淘汰，其名额在同一把锁内归还，
    /// 连接对象在锁外丢弃（`Permit` 已解除，不再二次加锁）。
    fn take(self: &Arc<Self>, dsn: &str) -> Take {
        let mut stale: Vec<DbConn> = Vec::new();
        let taken = {
            let mut state = lock(&self.state);
            let idle_timeout = Duration::from_micros(self.idle_timeout.load(Ordering::Relaxed));
            let max_open = self.max_open.load(Ordering::Relaxed);
            let now = Instant::now();
            let me = std::thread::current().id();
            let mut count = state.open.get(dsn).copied().unwrap_or(0);
            let mut chosen: Option<DbConn> = None;
            if let Some(list) = state.idle.get_mut(dsn) {
                while let Some(entry) = list.pop() {
                    if stale_entry(&entry, now, idle_timeout, me) {
                        count = count.saturating_sub(1);
                        let mut conn = entry.conn;
                        conn.permit.disarm();
                        stale.push(conn);
                        continue;
                    }
                    chosen = Some(entry.conn);
                    break;
                }
                if list.is_empty() {
                    state.idle.remove(dsn);
                }
            }
            match chosen {
                Some(conn) => {
                    if !stale.is_empty() {
                        if count == 0 {
                            state.open.remove(dsn);
                        } else {
                            state.open.insert(dsn.to_string(), count);
                        }
                    }
                    Take::Reused(conn)
                }
                None => {
                    if count >= max_open {
                        if !stale.is_empty() && count == 0 {
                            state.open.remove(dsn);
                        }
                        Take::Full
                    } else {
                        state.open.insert(dsn.to_string(), count + 1);
                        drop(state);
                        Take::Fresh(Permit {
                            pool: Arc::downgrade(self),
                            dsn: Arc::from(dsn),
                        })
                    }
                }
            }
        };
        // 淘汰的连接在锁外关闭；若释放了名额且有人等待则唤醒。
        let freed = !stale.is_empty();
        drop(stale);
        if freed && self.waiters.load(Ordering::SeqCst) > 0 {
            self.ready.notify_all();
        }
        taken
    }

    /// 归还连接：有空位就缓存，否则关闭（关闭时 `Permit` 归还名额）。
    fn give_back(&self, conn: DbConn) {
        let dsn = Arc::clone(&conn.permit.dsn);
        let owner = std::thread::current().id();
        let mut keep = Some(conn);
        {
            let mut state = lock(&self.state);
            let max_idle = self.max_idle.load(Ordering::Relaxed);
            match state.idle.get_mut(dsn.as_ref()) {
                Some(list) if list.len() < max_idle => {
                    let conn = keep.take().expect("checked max_idle above");
                    list.push(IdleConn {
                        conn,
                        returned: Instant::now(),
                        owner,
                    });
                }
                None if max_idle > 0 => {
                    let conn = keep.take().expect("checked max_idle above");
                    state.idle.insert(dsn.to_string(), vec![IdleConn {
                        conn,
                        returned: Instant::now(),
                        owner,
                    }]);
                }
                _ => {}
            }
        }
        // 超额连接在锁外关闭。
        drop(keep);
        if self.waiters.load(Ordering::SeqCst) > 0 {
            self.ready.notify_one();
        }
    }

    fn wait_for(&self, timeout: Duration) {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let state = lock(&self.state);
        let _ = self.ready.wait_timeout(state, timeout);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 空闲连接是否已不可复用。SQL Server 连接绑定创建它的 tokio 运行时，
/// 换线程驱动会破坏 I/O 状态，因此跨线程一律关闭重连。
fn stale_entry(
    entry: &IdleConn,
    now: Instant,
    idle_timeout: Duration,
    me: std::thread::ThreadId,
) -> bool {
    if now.duration_since(entry.returned) > idle_timeout {
        return true;
    }
    matches!(&entry.conn.kind, ConnKind::MsSql(_)) && entry.owner != me
}

/// 加锁；中毒也继续使用（连接池失败不应让进程崩溃）。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// 全局连接池：延迟初始化，进程内唯一。
fn pool() -> &'static Arc<Pool> {
    static POOL: OnceLock<Arc<Pool>> = OnceLock::new();
    POOL.get_or_init(|| Arc::new(Pool::new(PoolConfig::default())))
}

/// 用外部配置覆盖连接池参数（`config/rip.toml` 的 `[db]` 与环境变量）。
pub fn configure_pool(config: PoolConfig) {
    pool().set_config(config);
}

/// 连接池快照：`(DSN, 存活连接数, 空闲连接数)`，按 DSN 排序。DSN 一律脱敏，
/// 避免口令随监控输出泄露。
pub fn pool_stats() -> Vec<(String, usize, usize)> {
    let pool = pool();
    let state = lock(&pool.state);
    let mut rows: Vec<(String, usize, usize)> = state
        .open
        .iter()
        .map(|(dsn, open)| {
            let idle = state.idle.get(dsn).map(|list| list.len()).unwrap_or(0);
            (redact_dsn(dsn), *open, idle)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

pub struct DbState {
    pub(crate) conns: HashMap<i64, DbConn>,
    /// 事务中的连接不归还空闲池，避免把未提交状态带给下一个调用方。
    in_tx: std::collections::HashSet<i64>,
    next_id: i64,
    tokio: Option<TokioRuntime>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            conns: HashMap::new(),
            in_tx: std::collections::HashSet::new(),
            next_id: 1,
            tokio: None,
        }
    }

    /// 归还本运行时借出的全部连接（任务复用运行时前调用，避免句柄泄漏）。
    pub(crate) fn reset(&mut self) {
        let ids: Vec<i64> = self.conns.keys().copied().collect();
        for id in ids {
            release_conn(self, id);
        }
        self.in_tx.clear();
    }
}

fn register_conn(db: &mut DbState, conn: DbConn) -> Value {
    let id = db.next_id;
    db.next_id += 1;
    let driver = conn.dialect.name();
    let database = conn.database.clone();
    db.conns.insert(id, conn);
    db.in_tx.remove(&id);
    Value::ary(
        "db.conn",
        vec![
            ("id".to_string(), Value::Int(id)),
            ("driver".to_string(), Value::string(driver)),
            ("database".to_string(), Value::string(database)),
        ],
    )
}

/// 归还连接：事务中的连接直接关闭，其余进入池的空闲列表。
fn release_conn(db: &mut DbState, id: i64) {
    let in_tx = db.in_tx.remove(&id);
    if let Some(conn) = db.conns.remove(&id) {
        if in_tx {
            // 事务未结束：连接带着未提交状态，不能被下一个调用方复用。
            drop(conn);
        } else {
            pool().give_back(conn);
        }
    }
}

struct QueryState {
    conn: i64,
    table: String,
    fields: String,
    where_sql: String,
    bindings: Vec<Value>,
    joins: String,
    order: String,
    limit: i64,
    offset: i64,
}

impl Runtime {
    pub(crate) fn db_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "open" => {
                let dsn = self.string_arg(&args, 0, name, line)?;
                if dsn.trim().is_empty() {
                    return self.error("db.open expects a non-empty DSN", line);
                }
                let dsn = dsn.trim().to_string();
                // 池优先复用空闲连接；池满时按 acquire_timeout 等待名额。
                match self.open_connection(&dsn) {
                    Ok(conn) => Ok(Value::Ok(Box::new(register_conn(&mut self.db, conn)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "driver" => match conn_id(args.first()) {
                Some(id) => Ok(Value::string(self.dialect_for(id).name())),
                None => self.error("db.driver expects a db.conn as argument 1", line),
            },
            "table" => {
                let id = match conn_id(args.first()) {
                    Some(id) => id,
                    None => return self.error("db.table expects a db.conn as argument 1", line),
                };
                let table = self.string_arg(&args, 1, name, line)?;
                if table.trim().is_empty() {
                    return self.error("table name cannot be empty", line);
                }
                Ok(make_builder(id, table.trim(), "*"))
            }
            // 一对多：按外键取子表全部行，等价于 SELECT * FROM 子表 WHERE 外键 = ?。
            "has_many" => {
                let id = match conn_id(args.first()) {
                    Some(id) => id,
                    None => return self.error("db.has_many expects a db.conn as argument 1", line),
                };
                let table = self.string_arg(&args, 1, name, line)?;
                let column = self.string_arg(&args, 2, name, line)?;
                let value = args.get(3).cloned().unwrap_or(Value::Null);
                let dialect = self.dialect_for(id);
                let (Ok(table), Ok(column)) =
                    (quote_ident(&table, dialect), quote_ident(&column, dialect))
                else {
                    return Ok(Value::Err(Box::new(Value::string(
                        "invalid table or column name".to_string(),
                    ))));
                };
                let sql = format!("SELECT * FROM {} WHERE {} = ?", table, column);
                match self.run_query(id, &sql, &[value]) {
                    Ok(rows) => Ok(Value::Ok(Box::new(Value::vec(rows)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            // 多对一：按主键 id 取父表一行。
            "belongs_to" => {
                let id = match conn_id(args.first()) {
                    Some(id) => id,
                    None => return self.error("db.belongs_to expects a db.conn as argument 1", line),
                };
                let table = self.string_arg(&args, 1, name, line)?;
                let key = args.get(2).cloned().unwrap_or(Value::Null);
                let dialect = self.dialect_for(id);
                let Ok(table) = quote_ident(&table, dialect) else {
                    return Ok(Value::Err(Box::new(Value::string(
                        "invalid table name".to_string(),
                    ))));
                };
                let sql = format!("SELECT * FROM {} WHERE id = ?", table);
                match self.run_query(id, &sql, &[key]) {
                    Ok(mut rows) => Ok(Value::Ok(Box::new(if rows.is_empty() {
                        Value::None
                    } else {
                        Value::Some(Box::new(rows.remove(0)))
                    }))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            // 多对一（自定义键）：按任意唯一列取父表一行。
            "belongs_to_by" => {
                let id = match conn_id(args.first()) {
                    Some(id) => id,
                    None => {
                        return self.error(
                            "db.belongs_to_by expects a db.conn as argument 1",
                            line,
                        )
                    }
                };
                let table = self.string_arg(&args, 1, name, line)?;
                let column = self.string_arg(&args, 2, name, line)?;
                let key = args.get(3).cloned().unwrap_or(Value::Null);
                let dialect = self.dialect_for(id);
                let (Ok(table), Ok(column)) =
                    (quote_ident(&table, dialect), quote_ident(&column, dialect))
                else {
                    return Ok(Value::Err(Box::new(Value::string(
                        "invalid table or column name".to_string(),
                    ))));
                };
                let sql = format!("SELECT * FROM {} WHERE {} = ?", table, column);
                match self.run_query(id, &sql, &[key]) {
                    Ok(mut rows) => Ok(Value::Ok(Box::new(if rows.is_empty() {
                        Value::None
                    } else {
                        Value::Some(Box::new(rows.remove(0)))
                    }))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            _ => self.error(format!("std/db has no function '{}'", name), line),
        }
    }

    pub(crate) fn db_conn_method(
        &mut self,
        ary: &Rc<RefCell<Ary>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let id = match conn_id(Some(&Value::Ary(ary.clone()))) {
            Some(id) => id,
            None => return self.error("invalid database connection", line),
        };
        match name {
            "execute" => {
                let sql = self.string_arg(&args, 0, name, line)?;
                let params = match bind_params(args.get(1)) {
                    Ok(values) => values,
                    Err(message) => return self.error(message, line),
                };
                match self.run_exec(id, &sql, &params) {
                    Ok(count) => Ok(Value::Ok(Box::new(Value::Int(count as i64)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "execute_many" => {
                let sql = self.string_arg(&args, 0, name, line)?;
                let rows = match bind_rows(args.get(1)) {
                    Ok(rows) => rows,
                    Err(message) => return self.error(message, line),
                };
                match self.run_exec_many(id, &sql, &rows) {
                    Ok(count) => Ok(Value::Ok(Box::new(Value::Int(count as i64)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "query" => {
                let sql = self.string_arg(&args, 0, name, line)?;
                let params = match bind_params(args.get(1)) {
                    Ok(values) => values,
                    Err(message) => return self.error(message, line),
                };
                match self.run_query(id, &sql, &params) {
                    Ok(rows) => Ok(Value::Ok(Box::new(Value::vec(rows)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "query_one" => {
                let sql = self.string_arg(&args, 0, name, line)?;
                let params = match bind_params(args.get(1)) {
                    Ok(values) => values,
                    Err(message) => return self.error(message, line),
                };
                match self.run_query(id, &sql, &params) {
                    Ok(mut rows) => {
                        let value = if rows.is_empty() {
                            Value::None
                        } else {
                            Value::Some(Box::new(rows.remove(0)))
                        };
                        Ok(Value::Ok(Box::new(value)))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            // JSON API 快速路径：单行直出 JSON 字符串，不构造 map。
            "row_json" => {
                let sql = self.string_arg(&args, 0, name, line)?;
                let params = match bind_params(args.get(1)) {
                    Ok(values) => values,
                    Err(message) => return self.error(message, line),
                };
                match self.db_row_json(id, &sql, &params) {
                    Ok(json) => Ok(Value::Ok(Box::new(Value::string(json)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "last_insert_id" => match self.last_insert_id(id) {
                Ok(value) => Ok(Value::Int(value)),
                Err(message) => self.error(message, line),
            },
            "begin" | "commit" | "rollback" => {
                let dialect = self.dialect_for(id);
                let sql = transaction_sql(name, dialect);
                match self.run_exec(id, sql, &[]) {
                    Ok(_) => {
                        if name == "begin" {
                            self.db.in_tx.insert(id);
                        } else {
                            self.db.in_tx.remove(&id);
                        }
                        Ok(Value::Ok(Box::new(Value::Int(0))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "close" => {
                release_conn(&mut self.db, id);
                Ok(Value::Null)
            }
            _ => self.error(format!("db.conn has no method '{}'", name), line),
        }
    }

    pub(crate) fn db_query_method(
        &mut self,
        ary: &Rc<RefCell<Ary>>,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        let builder = clone_builder(&ary.borrow());
        let conn = builder
            .get("conn")
            .and_then(|value| value.as_int())
            .unwrap_or(0);
        let dialect = self.dialect_for(conn);
        let mut builder = builder;
        match name {
            "field" => {
                let raw = self.string_arg(&args, 0, name, line)?;
                let quoted = match quote_fields(&raw, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                builder.set("fields", Value::string(quoted));
                Ok(builder_value(builder))
            }
            "where" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let op_raw = self.string_arg(&args, 1, name, line)?;
                let value = args.get(2).cloned().unwrap_or(Value::Null);
                let op = normalize_op(&op_raw);
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                if is_nullish(&value) {
                    let sql_op = match op.as_str() {
                        "=" | "is" => "IS NULL",
                        "!=" | "is not" => "IS NOT NULL",
                        _ => return self.error("null can only be used with '=' or '!='", line),
                    };
                    state.where_sql.push_str(&prefix_for(&state.where_sql));
                    state.where_sql.push_str(&quoted);
                    state.where_sql.push(' ');
                    state.where_sql.push_str(sql_op);
                } else {
                    if !allowed_op(&op) {
                        return self.error(
                            format!("unsupported operator '{}'", op_raw),
                            line,
                        );
                    }
                    state.where_sql.push_str(&prefix_for(&state.where_sql));
                    state.where_sql.push_str(&quoted);
                    state.where_sql.push(' ');
                    state.where_sql.push_str(&op);
                    state.where_sql.push_str(" ?");
                    state.bindings.push(value);
                }
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "where_in" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let items = match args.get(1) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    _ => return self.error("where_in expects a vec as argument 2", line),
                };
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                state.where_sql.push_str(&prefix_for(&state.where_sql));
                if items.is_empty() {
                    state.where_sql.push_str("1 = 0");
                } else {
                    let marks = vec!["?"; items.len()].join(", ");
                    state.where_sql.push_str(&quoted);
                    state.where_sql.push_str(" IN (");
                    state.where_sql.push_str(&marks);
                    state.where_sql.push(')');
                    state.bindings.extend(items);
                }
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "where_null" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                state.where_sql.push_str(&prefix_for(&state.where_sql));
                state.where_sql.push_str(&quoted);
                state.where_sql.push_str(" IS NULL");
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "order" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let direction = self.string_arg(&args, 1, name, line)?;
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let dir = match direction.trim().to_lowercase().as_str() {
                    "asc" => "ASC",
                    "desc" => "DESC",
                    _ => return self.error("order direction must be 'asc' or 'desc'", line),
                };
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                if !state.order.is_empty() {
                    state.order.push_str(", ");
                }
                state.order.push_str(&quoted);
                state.order.push(' ');
                state.order.push_str(dir);
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "limit" | "offset" => {
                let value = self.int_arg(&args, 0, name, line)?;
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                if name == "limit" {
                    if value <= 0 {
                        return self.error("limit must be positive", line);
                    }
                    state.limit = value;
                } else {
                    if value < 0 {
                        return self.error("offset cannot be negative", line);
                    }
                    state.offset = value;
                }
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "join" => {
                let table = self.string_arg(&args, 0, name, line)?;
                let on = self.string_arg(&args, 1, name, line)?;
                let quoted = match quote_ident(&table, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                if on.trim().is_empty() {
                    return self.error("join condition cannot be empty", line);
                }
                // join 条件是唯一以字符串进入 SQL 结构的入口；只允许标识符/数字/比较符，
                // 禁止引号与注释，避免把请求数据拼进 SQL。
                if !is_safe_join_condition(&on) {
                    return self.error(
                        "join condition may only contain identifiers, numbers, dots, comparison operators, parentheses and AND/OR",
                        line,
                    );
                }
                let mut state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                state.joins.push_str(" JOIN ");
                state.joins.push_str(&quoted);
                state.joins.push_str(" ON (");
                state.joins.push_str(on.trim());
                state.joins.push(')');
                copy_state(&mut builder, state);
                Ok(builder_value(builder))
            }
            "select" => {
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let sql = build_select(&state, &state.fields, None, true, dialect);
                self.result_rows(state.conn, &sql, &state.bindings)
            }
            "find" => {
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let sql = build_select(&state, &state.fields, Some(1), true, dialect);
                match self.run_query(state.conn, &sql, &state.bindings) {
                    Ok(mut rows) => {
                        let value = if rows.is_empty() {
                            Value::None
                        } else {
                            Value::Some(Box::new(rows.remove(0)))
                        };
                        Ok(Value::Ok(Box::new(value)))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "count" => {
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let alias = dialect.quote("count");
                let sql = build_select(
                    &state,
                    &format!("COUNT(*) AS {}", alias),
                    None,
                    false,
                    dialect,
                );
                match self.run_query(state.conn, &sql, &state.bindings) {
                    Ok(rows) => {
                        let count = rows
                            .first()
                            .and_then(|row| row_field(row, "count"))
                            .and_then(|value| value.as_int())
                            .unwrap_or(0);
                        Ok(Value::Ok(Box::new(Value::Int(count))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "value" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let alias = dialect.quote("value");
                let sql = build_select(
                    &state,
                    &format!("{} AS {}", quoted, alias),
                    Some(1),
                    false,
                    dialect,
                );
                match self.run_query(state.conn, &sql, &state.bindings) {
                    Ok(rows) => {
                        let value = rows
                            .first()
                            .and_then(|row| row_field(row, "value"))
                            .map(|value| Value::Some(Box::new(value)))
                            .unwrap_or(Value::None);
                        Ok(Value::Ok(Box::new(value)))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "column" => {
                let column = self.string_arg(&args, 0, name, line)?;
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let quoted = match quote_ident(&column, dialect) {
                    Ok(value) => value,
                    Err(message) => return self.error(message, line),
                };
                let alias = dialect.quote("value");
                let sql = build_select(
                    &state,
                    &format!("{} AS {}", quoted, alias),
                    None,
                    true,
                    dialect,
                );
                match self.run_query(state.conn, &sql, &state.bindings) {
                    Ok(rows) => {
                        let values = rows
                            .iter()
                            .filter_map(|row| row_field(row, "value"))
                            .collect();
                        Ok(Value::Ok(Box::new(Value::vec(values))))
                    }
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "insert" | "update" => {
                let values = match args.first() {
                    Some(value) => match data_entries(value) {
                        Ok(entries) => entries,
                        Err(message) => return self.error(message, line),
                    },
                    None => return self.error("insert/update expects a map", line),
                };
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let sql = if name == "insert" {
                    match build_insert(&state, &values, dialect) {
                        Ok(sql) => sql,
                        Err(message) => return self.error(message, line),
                    }
                } else {
                    match build_update(&state, &values, dialect) {
                        Ok(sql) => sql,
                        Err(message) => return self.error(message, line),
                    }
                };
                let mut params: Vec<Value> =
                    values.iter().map(|(_, value)| value.clone()).collect();
                params.extend(state.bindings.clone());
                match self.run_exec(state.conn, &sql, &params) {
                    Ok(count) => Ok(Value::Ok(Box::new(Value::Int(count as i64)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "delete" => {
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                let sql = format!(
                    "DELETE FROM {}{}",
                    match quote_ident(&state.table, dialect) {
                        Ok(value) => value,
                        Err(message) => return self.error(message, line),
                    },
                    state.where_sql
                );
                match self.run_exec(state.conn, &sql, &state.bindings) {
                    Ok(count) => Ok(Value::Ok(Box::new(Value::Int(count as i64)))),
                    Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
                }
            }
            "to_sql" => {
                let state = match query_state(&builder) {
                    Ok(state) => state,
                    Err(message) => return self.error(message, line),
                };
                Ok(Value::string(build_select(
                    &state,
                    &state.fields,
                    None,
                    true,
                    dialect,
                )))
            }
            "clone" => Ok(Value::ary_owned(builder)),
            _ => self.error(format!("db.query has no method '{}'", name), line),
        }
    }

    fn dialect_for(&self, conn_id: i64) -> Dialect {
        self.db
            .conns
            .get(&conn_id)
            .map(|conn| conn.dialect)
            .unwrap_or(Dialect::Sqlite)
    }

    /// 迁移执行器：打开一条连接并登记到当前运行时，返回连接 ID。
    pub(crate) fn open_tracked(&mut self, dsn: &str) -> Result<i64, String> {
        let conn = self.open_connection(dsn)?;
        let value = register_conn(&mut self.db, conn);
        conn_id(Some(&value)).ok_or_else(|| "internal: database connection id missing".to_string())
    }

    /// 迁移执行器：关闭登记过的连接并归还连接池。
    pub(crate) fn close_tracked(&mut self, id: i64) {
        release_conn(&mut self.db, id);
    }

    pub(crate) fn tracked_dialect(&self, id: i64) -> Dialect {
        self.dialect_for(id)
    }

    /// 按“脚本”语义执行可能包含多条语句的 SQL。
    ///
    /// SQLite / PostgreSQL 使用驱动的批量接口（能正确处理触发器、`$$` 函数体）；
    /// MySQL / SQL Server 的驱动默认不开启多语句协议，因此先按语句切分再逐条执行。
    pub(crate) fn exec_script(&mut self, id: i64, script: &str) -> Result<(), String> {
        let DbState { conns, tokio, .. } = &mut self.db;
        match conns.get_mut(&id).map(|conn| &mut conn.kind) {
            Some(ConnKind::Sqlite(conn)) => conn
                .execute_batch(script)
                .map_err(|error| format!("sqlite: {}", error)),
            Some(ConnKind::Postgres(conn)) => conn
                .batch_execute(script)
                .map_err(|error| format!("postgres: {}", error)),
            Some(ConnKind::MySql(conn)) => {
                for statement in split_sql_statements(script) {
                    conn.query_drop(statement.as_str())
                        .map_err(|error| format!("mysql: {}", error))?;
                }
                Ok(())
            }
            Some(ConnKind::MsSql(client)) => {
                let runtime = tokio
                    .as_ref()
                    .ok_or_else(|| "mssql runtime is missing".to_string())?;
                let statements = split_sql_statements(script);
                runtime.block_on(async {
                    for statement in statements {
                        client
                            .simple_query(statement)
                            .await
                            .map_err(|error| format!("mssql: {}", error))?
                            .into_results()
                            .await
                            .map_err(|error| format!("mssql: {}", error))?;
                    }
                    Ok(())
                })
            }
            None => Err("database connection is closed".to_string()),
        }
    }

    /// 从进程级连接池借一条连接：优先复用空闲连接，未达上限时新建，
    /// 池满则等待其他线程归还，超时后返回错误。
    pub(crate) fn open_connection(&mut self, dsn: &str) -> Result<DbConn, String> {
        let shared = pool();
        let acquire_timeout = Duration::from_micros(shared.acquire_timeout.load(Ordering::Relaxed));
        let deadline = Instant::now() + acquire_timeout;
        loop {
            match shared.take(dsn) {
                Take::Reused(conn) => return Ok(conn),
                // 新建失败时 `permit` 随栈帧释放，名额自动归还。
                Take::Fresh(permit) => return self.connect(dsn, permit),
                Take::Full => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "db pool exhausted for '{}' (max_open = {})",
                    redact_dsn(dsn),
                    shared.max_open.load(Ordering::Relaxed)
                ));
            }
            // 分段等待，保证超时判断及时。
            shared.wait_for((deadline - now).min(Duration::from_millis(200)));
        }
    }

    fn connect(&mut self, dsn: &str, permit: Permit) -> Result<DbConn, String> {
        let lower = dsn.to_ascii_lowercase();
        let (kind, dialect, database) = if lower.starts_with("mysql://") || lower.starts_with("mysql:") {
            let url = if lower.starts_with("mysql://") {
                dsn.to_string()
            } else {
                format!("mysql://{}", &dsn["mysql:".len()..])
            };
            let opts = MySqlOpts::from_url(&url).map_err(|error| format!("mysql dsn: {}", error))?;
            let database = opts
                .get_db_name()
                .map(|name| name.to_string())
                .unwrap_or_default();
            let conn = MySqlConn::new(opts).map_err(|error| format!("mysql open failed: {}", error))?;
            (ConnKind::MySql(conn), Dialect::MySql, database)
        } else if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
            let sslmode = parse_sslmode(dsn)?;
            let client = match sslmode {
                PgSslMode::Disable => PgClient::connect(dsn, NoTls),
                mode => PgClient::connect(dsn, MakeRustlsConnect::new(pg_rustls_config(mode))),
            }
            .map_err(|error| format!("postgres open failed: {}", error))?;
            let database = postgres_database_name(dsn);
            (ConnKind::Postgres(client), Dialect::Postgres, database)
        } else if lower.starts_with("mssql://")
            || lower.starts_with("sqlserver://")
            || lower.starts_with("jdbc:sqlserver:")
            || lower.starts_with("server=")
        {
            let (config, database) = mssql_config(dsn)?;
            let client = self.connect_mssql(config)?;
            (ConnKind::MsSql(client), Dialect::MsSql, database)
        } else {
            let path = sqlite_path(dsn);
            let conn = SqliteConn::open(&path)
                .map_err(|error| format!("sqlite open failed: {}", error))?;
            // WAL 让读不被写阻塞；NORMAL 把每事务两次 fsync 降到一次；
            // busy_timeout 让并发写等待锁而不是立刻 SQLITE_BUSY 失败。
            // `:memory:` 上这些 PRAGMA 同样安全（journal_mode 返回 memory）。
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
            let _ = conn.execute_batch("PRAGMA synchronous=NORMAL;");
            let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
            (ConnKind::Sqlite(conn), Dialect::Sqlite, path)
        };
        Ok(DbConn {
            kind,
            dialect,
            database,
            permit,
        })
    }

    fn connect_mssql(&mut self, config: MsSqlConfig) -> Result<MsSqlConn, String> {
        if self.db.tokio.is_none() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("mssql runtime: {}", error))?;
            self.db.tokio = Some(runtime);
        }
        let runtime = self.db.tokio.as_ref().expect("runtime just created");
        let addr = config.get_addr();
        runtime.block_on(async move {
            let tcp = TcpStream::connect(&addr)
                .await
                .map_err(|error| format!("mssql connect {}: {}", addr, error))?;
            tcp.set_nodelay(true)
                .map_err(|error| format!("mssql socket: {}", error))?;
            MsSqlClient::connect(config, tcp.compat_write())
                .await
                .map_err(|error| format!("mssql open failed: {}", error))
        })
    }

    fn last_insert_id(&mut self, conn_id: i64) -> Result<i64, String> {
        match self.dialect_for(conn_id) {
            Dialect::Sqlite => {
                let DbState { conns, .. } = &mut self.db;
                match conns.get_mut(&conn_id).map(|conn| &mut conn.kind) {
                    Some(ConnKind::Sqlite(conn)) => Ok(conn.last_insert_rowid()),
                    Some(_) => Err("database connection changed driver".to_string()),
                    None => Err("database connection is closed".to_string()),
                }
            }
            Dialect::MySql => {
                let DbState { conns, .. } = &mut self.db;
                match conns.get_mut(&conn_id).map(|conn| &mut conn.kind) {
                    Some(ConnKind::MySql(conn)) => Ok(conn.last_insert_id() as i64),
                    Some(_) => Err("database connection changed driver".to_string()),
                    None => Err("database connection is closed".to_string()),
                }
            }
            Dialect::Postgres => {
                let rows = self.run_query(conn_id, "SELECT lastval() AS id", &[])?;
                rows.first()
                    .and_then(|row| row_field(row, "id"))
                    .and_then(|value| value.as_int())
                    .ok_or_else(|| "postgres has no last_insert_id (no sequence used yet)".to_string())
            }
            Dialect::MsSql => {
                let rows = self.run_query(
                    conn_id,
                    "SELECT CAST(SCOPE_IDENTITY() AS BIGINT) AS id",
                    &[],
                )?;
                rows.first()
                    .and_then(|row| row_field(row, "id"))
                    .and_then(|value| value.as_int())
                    .ok_or_else(|| "mssql has no last_insert_id (no identity column used yet)".to_string())
            }
        }
    }

    fn result_rows(&mut self, conn_id: i64, sql: &str, params: &[Value]) -> FlowResult<Value> {
        match self.run_query(conn_id, sql, params) {
            Ok(rows) => Ok(Value::Ok(Box::new(Value::vec(rows)))),
            Err(message) => Ok(Value::Err(Box::new(Value::string(message)))),
        }
    }

    /// 执行写语句并记录指标（次数/耗时/失败）。
    pub(crate) fn run_exec(&mut self, conn_id: i64, sql: &str, params: &[Value]) -> Result<usize, String> {
        let started = std::time::Instant::now();
        let result = self.run_exec_driver(conn_id, sql, params);
        let failed = result.is_err();
        self.record_db_span("db.execute", started, failed);
        self.record_db_metrics(started, failed);
        result
    }

    /// 批量执行同一条语句（每组一行绑定参数），返回受影响行数之和。
    ///
    /// 不在显式事务里时自动包一层事务：要么全部成功，要么整体回滚，
    /// 顺带把 N 次提交合并成 1 次，导入类场景不再是「N 次网络往返」。
    pub(crate) fn run_exec_many(
        &mut self,
        conn_id: i64,
        sql: &str,
        rows: &[Vec<Value>],
    ) -> Result<usize, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        // 已经在显式事务里时不再开新事务，边界交给调用方的 commit / rollback。
        let nested = self.db.in_tx.contains(&conn_id);
        let dialect = self.dialect_for(conn_id);
        if !nested {
            self.run_exec(conn_id, transaction_sql("begin", dialect), &[])?;
        }
        let mut total = 0usize;
        for row in rows {
            match self.run_exec(conn_id, sql, row) {
                Ok(count) => total += count,
                Err(error) => {
                    if !nested {
                        let _ = self.run_exec(conn_id, transaction_sql("rollback", dialect), &[]);
                    }
                    return Err(error);
                }
            }
        }
        if !nested {
            if let Err(error) = self.run_exec(conn_id, transaction_sql("commit", dialect), &[]) {
                let _ = self.run_exec(conn_id, transaction_sql("rollback", dialect), &[]);
                return Err(error);
            }
        }
        Ok(total)
    }

    /// 执行查询并记录指标（次数/耗时/失败）。
    pub(crate) fn run_query(&mut self, conn_id: i64, sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
        let started = std::time::Instant::now();
        let result = self.run_query_driver(conn_id, sql, params);
        let failed = result.is_err();
        self.record_db_span("db.query", started, failed);
        self.record_db_metrics(started, failed);
        result
    }

    /// 把数据库操作挂到当前请求的 trace 上；没有请求上下文或未开启追踪时直接返回。
    fn record_db_span(&self, operation: &str, started: std::time::Instant, failed: bool) {
        if self.current_span_id.is_none() && self.current_trace_id.is_none() {
            return;
        }
        crate::trace::record_db_span(
            self.current_trace_id.as_deref(),
            self.current_span_id.as_deref(),
            operation,
            started.elapsed().as_millis() as u64,
            failed,
        );
    }

    fn record_db_metrics(&self, started: std::time::Instant, failed: bool) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        metrics
            .db_queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if failed {
            metrics
                .db_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        metrics.db_duration_ms.fetch_add(
            started.elapsed().as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn run_exec_driver(&mut self, conn_id: i64, sql: &str, params: &[Value]) -> Result<usize, String> {
        let dialect = self.dialect_for(conn_id);
        let sql = render_sql(sql, dialect);
        let DbState { conns, tokio, .. } = &mut self.db;
        match conns.get_mut(&conn_id).map(|conn| &mut conn.kind) {
            Some(ConnKind::Sqlite(conn)) => {
                let values = to_sqlite_params(params)?;
                // 同一连接上缓存已编译语句，避免每次请求重复解析/编译 SQL。
                let mut stmt = conn.prepare_cached(&sql).map_err(|error| error.to_string())?;
                stmt.execute(rusqlite::params_from_iter(values.iter()))
                    .map_err(|error| error.to_string())
            }
            Some(ConnKind::MySql(conn)) => {
                let values = to_mysql_params(params)?;
                // MySQL 的事务控制等语句不支持预处理协议，失败后改用文本协议重试。
                if let Err(error) = conn.exec_drop(&sql, values) {
                    if !is_unpreparable(&error) {
                        return Err(format!("mysql: {}", error));
                    }
                    conn.query_drop(&sql)
                        .map_err(|error| format!("mysql: {}", error))?;
                }
                Ok(conn.affected_rows() as usize)
            }
            Some(ConnKind::Postgres(conn)) => {
                let values = to_postgres_params(params)?;
                let refs: Vec<&(dyn PgToSql + Sync)> = values
                    .iter()
                    .map(|value| value.as_ref() as &(dyn PgToSql + Sync))
                    .collect();
                match conn.execute(sql.as_str(), &refs) {
                    Ok(count) => Ok(count as usize),
                    // 仅对无参数的会话控制语句回退简单协议；其余错误直接上抛。
                    // 否则语句可能已生效（如 INSERT…RETURNING 取行失败），回退会二次执行造成重复写入。
                    Err(error) => {
                        if refs.is_empty() && is_session_control(&sql) {
                            conn.batch_execute(&sql)
                                .map(|_| 0usize)
                                .map_err(|error| format!("postgres: {}", error))
                        } else {
                            Err(format!("postgres: {}", error))
                        }
                    }
                }
            }
            Some(ConnKind::MsSql(client)) => {
                let data = to_mssql_params(params)?;
                let refs: Vec<&dyn tiberius::ToSql> = data
                    .iter()
                    .map(|value| value as &dyn tiberius::ToSql)
                    .collect();
                let runtime = tokio
                    .as_ref()
                    .ok_or_else(|| "mssql runtime is missing".to_string())?;
                runtime.block_on(async {
                    let result = client
                        .execute(sql.as_str(), &refs)
                        .await
                        .map_err(|error| format!("mssql: {}", error))?;
                    Ok(result.total() as usize)
                })
            }
            None => Err("database connection is closed".to_string()),
        }
    }

    fn run_query_driver(&mut self, conn_id: i64, sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
        let dialect = self.dialect_for(conn_id);
        let sql = render_sql(sql, dialect);
        let DbState { conns, tokio, .. } = &mut self.db;
        match conns.get_mut(&conn_id).map(|conn| &mut conn.kind) {
            Some(ConnKind::Sqlite(conn)) => {
                let values = to_sqlite_params(params)?;
                let mut stmt = conn.prepare_cached(&sql).map_err(|error| error.to_string())?;
                // 列名键只构造一次，行内用 Arc 克隆，避免每单元格分配一次 String。
                let keys: Vec<Value> = stmt
                    .column_names()
                    .iter()
                    .map(|name| Value::string((*name).to_string()))
                    .collect();
                let mut rows = stmt
                    .query(rusqlite::params_from_iter(values.iter()))
                    .map_err(|error| error.to_string())?;
                let mut result = Vec::new();
                while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                    let mut entries = Vec::with_capacity(keys.len());
                    for (index, key) in keys.iter().enumerate() {
                        let value: SqliteValue =
                            row.get(index).map_err(|error| error.to_string())?;
                        entries.push((key.clone(), sqlite_to_value(value)));
                    }
                    result.push(Value::map(entries));
                }
                Ok(result)
            }
            Some(ConnKind::MySql(conn)) => {
                let values = to_mysql_params(params)?;
                let rows = conn
                    .exec_iter(&sql, values)
                    .map_err(|error| format!("mysql: {}", error))?;
                // 列名键只构造一次，行内用 Arc 克隆，避免每单元格分配一次 String。
                let keys: Vec<Value> = rows
                    .columns()
                    .as_ref()
                    .iter()
                    .map(|column| Value::string(column.name_str().to_string()))
                    .collect();
                let mut result = Vec::new();
                for row in rows {
                    let row = row.map_err(|error| format!("mysql: {}", error))?;
                    let mut entries = Vec::with_capacity(keys.len());
                    for (index, key) in keys.iter().enumerate() {
                        let value = row
                            .as_ref(index)
                            .cloned()
                            .map(mysql_to_value)
                            .unwrap_or(Value::Null);
                        entries.push((key.clone(), value));
                    }
                    result.push(Value::map(entries));
                }
                Ok(result)
            }
            Some(ConnKind::Postgres(conn)) => {
                let values = to_postgres_params(params)?;
                let refs: Vec<&(dyn PgToSql + Sync)> = values
                    .iter()
                    .map(|value| value.as_ref() as &(dyn PgToSql + Sync))
                    .collect();
                let rows = conn
                    .query(sql.as_str(), &refs)
                    .map_err(|error| format!("postgres: {}", error))?;
                // 列名与类型只解析一次：行内键用 Arc 克隆，避免每格重建 String/类型名。
                let first = rows.first();
                let (keys, types): (Vec<Value>, Vec<String>) = match first {
                    Some(row) => {
                        let keys = row
                            .columns()
                            .iter()
                            .map(|column| Value::string(column.name().to_string()))
                            .collect();
                        let types = row
                            .columns()
                            .iter()
                            .map(|column| column.type_().name().to_string())
                            .collect();
                        (keys, types)
                    }
                    None => (Vec::new(), Vec::new()),
                };
                let mut result = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut entries = Vec::with_capacity(keys.len());
                    for (index, key) in keys.iter().enumerate() {
                        let value = postgres_value(row, index, &types[index])?;
                        entries.push((key.clone(), value));
                    }
                    result.push(Value::map(entries));
                }
                Ok(result)
            }
            Some(ConnKind::MsSql(client)) => {
                let data = to_mssql_params(params)?;
                let refs: Vec<&dyn tiberius::ToSql> = data
                    .iter()
                    .map(|value| value as &dyn tiberius::ToSql)
                    .collect();
                let runtime = tokio
                    .as_ref()
                    .ok_or_else(|| "mssql runtime is missing".to_string())?;
                runtime.block_on(async {
                    let mut stream = client
                        .query(sql.as_str(), &refs)
                        .await
                        .map_err(|error| format!("mssql: {}", error))?;
                    // 列名键只构造一次，行内用 Arc 克隆，避免每格分配新 String。
                    let keys: Vec<Value> = match stream
                        .columns()
                        .await
                        .map_err(|error| format!("mssql: {}", error))?
                    {
                        Some(columns) => columns
                            .iter()
                            .map(|column| Value::string(column.name().to_string()))
                            .collect(),
                        None => Vec::new(),
                    };
                    let rows = stream
                        .into_first_result()
                        .await
                        .map_err(|error| format!("mssql: {}", error))?;
                    let mut result = Vec::with_capacity(rows.len());
                    for row in &rows {
                        let mut entries = Vec::with_capacity(keys.len());
                        for (index, key) in keys.iter().enumerate() {
                            let data = row
                                .get_column_data(index)
                                .map_err(|error| format!("mssql: {}", error))?;
                            entries.push((key.clone(), mssql_to_value(data)));
                        }
                        result.push(Value::map(entries));
                    }
                    Ok(result)
                })
            }
            None => Err("database connection is closed".to_string()),
        }
    }

    /// 单行查询并直接产出 JSON 字符串：驱动值 → JSON 字节一步到位，
    /// 跳过 `Value::map` 中间树（Rc/RefCell 分配与键克隆）。键按字典序、
    /// 重复列名保留最后一个，与 `Value::map` 序列化路径逐字节一致。
    /// 供 JSON API 热路径（如 `conn.row_json`）使用。
    pub(crate) fn db_row_json(
        &mut self,
        conn_id: i64,
        sql: &str,
        params: &[Value],
    ) -> Result<String, String> {
        let started = std::time::Instant::now();
        let result = self.db_row_json_driver(conn_id, sql, params);
        let failed = result.is_err();
        self.record_db_span("db.query", started, failed);
        self.record_db_metrics(started, failed);
        result
    }

    fn db_row_json_driver(
        &mut self,
        conn_id: i64,
        sql: &str,
        params: &[Value],
    ) -> Result<String, String> {
        let dialect = self.dialect_for(conn_id);
        let sql = render_sql(sql, dialect);
        let DbState { conns, tokio, .. } = &mut self.db;
        match conns.get_mut(&conn_id).map(|conn| &mut conn.kind) {
            Some(ConnKind::Sqlite(conn)) => {
                let values = to_sqlite_params(params)?;
                let mut stmt = conn.prepare_cached(&sql).map_err(|error| error.to_string())?;
                let names: Vec<String> = stmt
                    .column_names()
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect();
                let mut rows = stmt
                    .query(rusqlite::params_from_iter(values.iter()))
                    .map_err(|error| error.to_string())?;
                match rows.next().map_err(|error| error.to_string())? {
                    Some(row) => {
                        let mut cells = Vec::with_capacity(names.len());
                        for index in 0..names.len() {
                            let value: SqliteValue =
                                row.get(index).map_err(|error| error.to_string())?;
                            cells.push(sqlite_to_value(value));
                        }
                        Ok(row_to_json(&names, &cells))
                    }
                    None => Err("row not found".to_string()),
                }
            }
            Some(ConnKind::MySql(conn)) => {
                let values = to_mysql_params(params)?;
                let mut rows = conn
                    .exec_iter(&sql, values)
                    .map_err(|error| format!("mysql: {}", error))?;
                let names: Vec<String> = rows
                    .columns()
                    .as_ref()
                    .iter()
                    .map(|column| column.name_str().to_string())
                    .collect();
                match rows.next() {
                    Some(Ok(row)) => {
                        let mut cells = Vec::with_capacity(names.len());
                        for index in 0..names.len() {
                            let value = row
                                .as_ref(index)
                                .cloned()
                                .map(mysql_to_value)
                                .unwrap_or(Value::Null);
                            cells.push(value);
                        }
                        Ok(row_to_json(&names, &cells))
                    }
                    Some(Err(error)) => Err(format!("mysql: {}", error)),
                    None => Err("row not found".to_string()),
                }
            }
            Some(ConnKind::Postgres(conn)) => {
                let values = to_postgres_params(params)?;
                let refs: Vec<&(dyn PgToSql + Sync)> = values
                    .iter()
                    .map(|value| value.as_ref() as &(dyn PgToSql + Sync))
                    .collect();
                let rows = conn
                    .query(sql.as_str(), &refs)
                    .map_err(|error| format!("postgres: {}", error))?;
                match rows.first() {
                    Some(row) => {
                        let columns: Vec<(String, String)> = row
                            .columns()
                            .iter()
                            .map(|column| {
                                (
                                    column.name().to_string(),
                                    column.type_().name().to_string(),
                                )
                            })
                            .collect();
                        let mut cells = Vec::with_capacity(columns.len());
                        for (index, (_, ty)) in columns.iter().enumerate() {
                            cells.push(postgres_value(row, index, ty)?);
                        }
                        let names: Vec<String> =
                            columns.iter().map(|(name, _)| name.clone()).collect();
                        Ok(row_to_json(&names, &cells))
                    }
                    None => Err("row not found".to_string()),
                }
            }
            Some(ConnKind::MsSql(client)) => {
                let data = to_mssql_params(params)?;
                let refs: Vec<&dyn tiberius::ToSql> = data
                    .iter()
                    .map(|value| value as &dyn tiberius::ToSql)
                    .collect();
                let runtime = tokio
                    .as_ref()
                    .ok_or_else(|| "mssql runtime is missing".to_string())?;
                runtime.block_on(async {
                    let mut stream = client
                        .query(sql.as_str(), &refs)
                        .await
                        .map_err(|error| format!("mssql: {}", error))?;
                    let names: Vec<String> = match stream
                        .columns()
                        .await
                        .map_err(|error| format!("mssql: {}", error))?
                    {
                        Some(columns) => columns
                            .iter()
                            .map(|column| column.name().to_string())
                            .collect(),
                        None => Vec::new(),
                    };
                    let rows = stream
                        .into_first_result()
                        .await
                        .map_err(|error| format!("mssql: {}", error))?;
                    match rows.first() {
                        Some(row) => {
                            let mut cells = Vec::with_capacity(names.len());
                            for index in 0..names.len() {
                                let column = row
                                    .get_column_data(index)
                                    .map_err(|error| format!("mssql: {}", error))?;
                                cells.push(mssql_to_value(column));
                            }
                            Ok(row_to_json(&names, &cells))
                        }
                        None => Err("row not found".to_string()),
                    }
                })
            }
            None => Err("database connection is closed".to_string()),
        }
    }
}

/// 行值直写 JSON 对象：键字典序、重复列名保留最后一个（与 serde_json BTreeMap 一致）。
fn row_to_json(names: &[String], cells: &[Value]) -> String {
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|a, b| names[*a].cmp(&names[*b]));
    let mut out = Vec::with_capacity(64 + names.len() * 24);
    out.push(b'{');
    let mut first = true;
    for position in 0..order.len() {
        // 同名键只在最后一次出现处写出。
        if order
            .get(position + 1)
            .is_some_and(|&next| names[next] == names[order[position]])
        {
            continue;
        }
        if !first {
            out.push(b',');
        }
        first = false;
        let index = order[position];
        crate::value::write_json_string(&names[index], &mut out);
        out.push(b':');
        crate::value::value_write_json(&cells[index], &mut out);
    }
    out.push(b'}');
    String::from_utf8(out).unwrap_or_default()
}

/// MySQL 对 `START TRANSACTION` 等语句返回 1295，表示该语句不支持预处理协议。
fn is_unpreparable(error: &mysql::Error) -> bool {
    matches!(error, mysql::Error::MySqlError(inner) if inner.code == 1295)
}

/// 把一段 SQL 脚本按末尾分号切分为独立语句。
///
/// 单引号、双引号、反引号内的分号不切分；`--`、`#` 行注释与 `/* */`
/// 块注释原样保留在语句里（服务端会忽略），只用来避免在注释内误切。
fn split_sql_statements(script: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = script.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            current.push(ch);
            if ch == active {
                if chars.peek() == Some(&active) {
                    current.push(chars.next().expect("peeked char"));
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => {
                quote = Some(ch);
                current.push(ch);
            }
            '-' if chars.peek() == Some(&'-') => {
                current.push(ch);
                current.push(chars.next().expect("peeked char"));
                for next in chars.by_ref() {
                    current.push(next);
                    if next == '\n' {
                        break;
                    }
                }
            }
            '#' => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        current.push(next);
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                current.push(ch);
                current.push(chars.next().expect("peeked char"));
                let mut prev = '\0';
                for next in chars.by_ref() {
                    current.push(next);
                    if prev == '*' && next == '/' {
                        break;
                    }
                    prev = next;
                }
            }
            ';' => push_statement(&mut statements, &mut current),
            _ => current.push(ch),
        }
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let statement = current.trim();
    if !statement.is_empty() && !is_comment_only(statement) {
        statements.push(statement.to_string());
    }
    current.clear();
}

/// 判断一段文本是否只有注释与空白（切分后可能残留文件尾部注释）。
fn is_comment_only(statement: &str) -> bool {
    let mut rest = statement;
    loop {
        let text = rest.trim_start();
        if let Some(after) = text.strip_prefix("--") {
            rest = after.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
            continue;
        }
        if let Some(after) = text.strip_prefix('#') {
            rest = after.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
            continue;
        }
        if let Some(after) = text.strip_prefix("/*") {
            match after.split_once("*/") {
                Some((_, tail)) => {
                    rest = tail;
                    continue;
                }
                None => return true,
            }
        }
        return text.is_empty();
    }
}

/// `join(table, on)` 的条件白名单：标识符、数字、点、比较符、括号、算术与 AND/OR。
/// 不允许引号、分号与 SQL 注释，防止把外部输入拼进查询结构。
fn is_safe_join_condition(text: &str) -> bool {
    if text.contains("--") || text.contains("/*") || text.contains("*/") || text.contains(';') {
        return false;
    }
    text.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '_' | '.' | ' ' | '\t' | '=' | '<' | '>' | '!' | '(' | ')' | ',' | '+' | '-' | '*'
            )
    })
}

/// Postgres 会话控制类语句：扩展查询协议对它们支持不稳定，允许回退简单协议。
fn is_session_control(sql: &str) -> bool {
    let head = sql
        .trim_start()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "BEGIN"
            | "START"
            | "COMMIT"
            | "ROLLBACK"
            | "SAVEPOINT"
            | "RELEASE"
            | "SET"
            | "RESET"
            | "DISCARD"
    )
}

/// 若剩余字符以美元引用标签开头（`$` 或 `tag$`），返回完整起始定界符（如 `$$` / `$tag$`）。
fn dollar_delim(rest: &[char]) -> Option<String> {
    let end = rest.iter().position(|c| *c == '$')?;
    let tag: String = rest[..end].iter().collect();
    if tag.is_empty() {
        return Some("$$".to_string());
    }
    let mut iter = tag.chars();
    let first = iter.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !iter.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(format!("${}$", tag))
}

fn render_sql(sql: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::Sqlite | Dialect::MySql => sql.to_string(),
        Dialect::Postgres | Dialect::MsSql => {
            let mut out = String::with_capacity(sql.len() + 8);
            let mut index = 0usize;
            let mut quote: Option<char> = None;
            let chars: Vec<char> = sql.chars().collect();
            let mut i = 0usize;
            while i < chars.len() {
                let ch = chars[i];
                i += 1;
                if let Some(active) = quote {
                    out.push(ch);
                    if ch == active {
                        if chars.get(i) == Some(&active) {
                            out.push(active);
                            i += 1;
                        } else {
                            quote = None;
                        }
                    }
                    continue;
                }
                match ch {
                    '\'' | '"' => {
                        quote = Some(ch);
                        out.push(ch);
                    }
                    '-' if chars.get(i) == Some(&'-') => {
                        out.push('-');
                        out.push('-');
                        i += 1;
                        while i < chars.len() {
                            let c = chars[i];
                            out.push(c);
                            i += 1;
                            if c == '\n' {
                                break;
                            }
                        }
                    }
                    // 块注释 /* */：内容原样透传，其中的 ? 不得被改写为占位符。
                    '/' if chars.get(i) == Some(&'*') => {
                        out.push('/');
                        out.push('*');
                        i += 1;
                        while i < chars.len() {
                            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                                out.push('*');
                                out.push('/');
                                i += 2;
                                break;
                            }
                            out.push(chars[i]);
                            i += 1;
                        }
                    }
                    // 美元引用 $$…$$ / $tag$…$tag$：同上，内容原样透传。
                    '$' => {
                        if let Some(delim) = dollar_delim(&chars[i..]) {
                            let closing: Vec<char> = delim.chars().collect();
                            out.push('$');
                            for c in &closing[1..] {
                                out.push(*c);
                                i += 1;
                            }
                            let mut found: Option<usize> = None;
                            let mut j = i;
                            while j + closing.len() <= chars.len() {
                                if chars[j..j + closing.len()] == closing[..] {
                                    found = Some(j);
                                    break;
                                }
                                j += 1;
                            }
                            match found {
                                Some(pos) => {
                                    out.extend(chars[i..pos].iter());
                                    out.extend(closing.iter());
                                    i = pos + closing.len();
                                }
                                None => {
                                    out.extend(chars[i..].iter());
                                    i = chars.len();
                                }
                            }
                        } else {
                            out.push(ch);
                        }
                    }
                    '?' => {
                        index += 1;
                        out.push_str(&dialect.placeholder(index));
                    }
                    other => out.push(other),
                }
            }
            out
        }
    }
}

fn transaction_sql(name: &str, dialect: Dialect) -> &'static str {
    match (name, dialect) {
        ("begin", Dialect::MySql) => "START TRANSACTION",
        ("begin", Dialect::MsSql) => "BEGIN TRANSACTION",
        ("commit", Dialect::MsSql) => "COMMIT TRANSACTION",
        ("rollback", Dialect::MsSql) => "ROLLBACK TRANSACTION",
        ("begin", _) => "BEGIN",
        ("commit", _) => "COMMIT",
        _ => "ROLLBACK",
    }
}

/// 错误信息里隐藏 DSN 的口令：`mysql://user:secret@host/db` → `mysql://user:***@host/db`。
///
/// 连接池耗尽的错误会回显给调用方（通常直接进响应体），不能把口令带出去。
pub(crate) fn redact_dsn(dsn: &str) -> String {
    let Some(at) = dsn.find('@') else {
        return dsn.to_string();
    };
    let authority_start = dsn.find("://").map(|index| index + 3).unwrap_or(0);
    let Some(colon) = dsn[authority_start..at].rfind(':') else {
        return dsn.to_string();
    };
    let mut out = String::with_capacity(dsn.len());
    out.push_str(&dsn[..authority_start + colon]);
    out.push_str(":***");
    out.push_str(&dsn[at..]);
    out
}

pub(crate) fn sqlite_path(dsn: &str) -> String {
    let lower = dsn.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("sqlite:") {
        let path = &dsn[dsn.len() - rest.len()..];
        let path = path.strip_prefix("//").unwrap_or(path);
        // `sqlite::memory:` 可带参数（用于让不同调用方拿到独立的进程内库）。
        if path.starts_with(":memory:") {
            ":memory:".to_string()
        } else if path.is_empty() {
            ":memory:".to_string()
        } else {
            path.to_string()
        }
    } else {
        dsn.to_string()
    }
}

/// Postgres 传输加密模式（对应 DSN 中的 `sslmode` 参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PgSslMode {
    /// 明文连接（默认，与不带 sslmode 的历史行为一致）。
    Disable,
    /// 仅加密，不校验服务端证书（等价 libpq 的 require）。
    Require,
    /// 用系统根证书库校验证书链与主机名。
    VerifyCa,
    /// 同 `verify-ca`：rustls 始终校验主机名，比 libpq 的 verify-ca 更严格。
    VerifyFull,
}

/// 从 DSN 中提取 sslmode 值：兼容 URL 查询参数（`?sslmode=require`）
/// 与关键字写法（`sslmode=require`，空格分隔）。
fn pg_sslmode_value(dsn: &str) -> Option<String> {
    if let Some((_, query)) = dsn.split_once('?') {
        for pair in query.split('#').next().unwrap_or("").split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                if key.eq_ignore_ascii_case("sslmode") {
                    return Some(value.to_ascii_lowercase());
                }
            }
        }
    }
    for token in dsn.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            if key.eq_ignore_ascii_case("sslmode") {
                return Some(value.to_ascii_lowercase());
            }
        }
    }
    None
}

/// 解析 sslmode。libpq 的 `allow`/`prefer` 依赖回退明文重连，
/// 与"唯一写法"原则冲突，明确拒绝而不是静默降级。
pub(crate) fn parse_sslmode(dsn: &str) -> Result<PgSslMode, String> {
    let value = pg_sslmode_value(dsn);
    match value.as_deref().map(str::trim) {
        None | Some("") | Some("disable") => Ok(PgSslMode::Disable),
        Some("require") => Ok(PgSslMode::Require),
        Some("verify-ca") => Ok(PgSslMode::VerifyCa),
        Some("verify-full") => Ok(PgSslMode::VerifyFull),
        Some("allow") | Some("prefer") => Err(format!(
            "postgres sslmode '{}' is not supported; use disable, require, verify-ca or verify-full",
            value.unwrap_or_default()
        )),
        Some(other) => Err(format!("unknown postgres sslmode '{}'", other)),
    }
}

/// `require` 模式：只加密不校验证书。rustls 需要显式的放行验签器。
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 按 sslmode 构造 rustls 客户端配置（`Disable` 不会走到这里）。
fn pg_rustls_config(mode: PgSslMode) -> rustls::ClientConfig {
    crate::tls::install_crypto_provider();
    let config = match mode {
        PgSslMode::Require => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
            .with_no_client_auth(),
        _ => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };
    config
}

fn postgres_database_name(dsn: &str) -> String {
    let without_scheme = dsn
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(dsn);
    let after_host = without_scheme
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or("");
    after_host
        .split(['?', ';'])
        .next()
        .unwrap_or("")
        .to_string()
}

fn mssql_config(dsn: &str) -> Result<(MsSqlConfig, String), String> {
    let lower = dsn.to_ascii_lowercase();
    if lower.starts_with("mssql://") || lower.starts_with("sqlserver://") {
        let rest = &dsn[dsn.find("://").map(|i| i + 3).unwrap_or(0)..];
        let (userinfo, hostpart) = match rest.split_once('@') {
            Some((userinfo, hostpart)) => (Some(userinfo), hostpart),
            None => (None, rest),
        };
        // 分离参数段：兼容 `?k=v&k=v` 与 `;k=v` 两种写法。
        let (endpoint, params) = match hostpart.find(['?', ';']) {
            Some(index) => (&hostpart[..index], &hostpart[index + 1..]),
            None => (hostpart, ""),
        };
        let (host_port, database) = match endpoint.split_once('/') {
            Some((host_port, database)) => (host_port, database),
            None => (endpoint, ""),
        };
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| format!("invalid mssql port '{}'", port))?,
            ),
            None => (host_port, 1433),
        };
        let mut config = MsSqlConfig::new();
        config.host(host);
        config.port(port);
        if !database.is_empty() {
            config.database(database);
        }
        match userinfo {
            Some(userinfo) => {
                let (user, password) = match userinfo.split_once(':') {
                    Some(pair) => pair,
                    None => (userinfo, ""),
                };
                let user = percent_decode(user);
                let password = percent_decode(password);
                if !user.is_empty() {
                    config.authentication(AuthMethod::sql_server(user, password));
                }
            }
            None => {}
        }
        // TLS 设置：缺省与历史行为一致（强制加密 + 直接信任证书）。
        let tls = mssql_tls_settings(params)?;
        if !tls.encrypt {
            config.encryption(EncryptionLevel::Off);
        }
        if tls.trust_cert {
            config.trust_cert();
        }
        Ok((config, database.to_string()))
    } else {
        let mut raw = dsn.to_string();
        if lower.starts_with("jdbc:sqlserver:") {
            raw = raw["jdbc:".len()..].to_string();
        }
        let config = MsSqlConfig::from_ado_string(&raw)
            .map_err(|error| format!("mssql dsn: {}", error))?;
        Ok((config, ado_database(&raw)))
    }
}

/// 从 MSSQL URL 形式 DSN 参数解析出的 TLS 设置。
struct MsSqlTls {
    encrypt: bool,
    trust_cert: bool,
}

/// 解析 `encrypt`/`trustcert` 参数；两者缺省为 true，与历史行为一致。
/// 键名不区分大小写并忽略下划线，因此 JDBC 风格的 `TrustServerCertificate`
/// 同样可识别；未知参数忽略。
fn mssql_tls_settings(params: &str) -> Result<MsSqlTls, String> {
    let mut tls = MsSqlTls {
        encrypt: true,
        trust_cert: true,
    };
    for pair in params.split(['&', ';']) {
        let Some((raw_key, raw_value)) = pair.split_once('=') else {
            continue;
        };
        let key = raw_key.to_ascii_lowercase().replace('_', "");
        match key.as_str() {
            "encrypt" => tls.encrypt = parse_flag_param("encrypt", raw_value)?,
            "trustcert" | "trustservercertificate" => {
                tls.trust_cert = parse_flag_param("trustcert", raw_value)?;
            }
            _ => {}
        }
    }
    Ok(tls)
}

fn parse_flag_param(name: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!("invalid mssql {name} value '{other}'; use true or false")),
    }
}

fn ado_database(raw: &str) -> String {
    for part in raw.split(';') {
        let part = part.trim();
        let lower = part.to_ascii_lowercase();
        for key in ["database=", "initial catalog="] {
            if let Some(value) = lower.strip_prefix(key) {
                let value = &part[part.len() - value.len()..];
                return value.trim().to_string();
            }
        }
    }
    String::new()
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &raw[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn make_builder(conn: i64, table: &str, fields: &str) -> Value {
    Value::ary(
        "db.query",
        vec![
            ("conn".to_string(), Value::Int(conn)),
            ("table".to_string(), Value::string(table)),
            ("fields".to_string(), Value::string(fields)),
            ("where_sql".to_string(), Value::string("")),
            ("bindings".to_string(), Value::vec(Vec::new())),
            ("joins".to_string(), Value::string("")),
            ("order".to_string(), Value::string("")),
            ("limit".to_string(), Value::Int(0)),
            ("offset".to_string(), Value::Int(0)),
        ],
    )
}

fn builder_value(builder: Ary) -> Value {
    Value::ary(builder.class, builder.fields)
}

fn clone_builder(ary: &Ary) -> Ary {
    Ary {
        class: ary.class.clone(),
        fields: ary
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), deep_clone(value)))
            .collect(),
    }
}

fn conn_id(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Ary(ary)) => {
            let ary = ary.borrow();
            if ary.class == "db.conn" {
                ary.get("id").and_then(|value| value.as_int())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn query_state(ary: &Ary) -> Result<QueryState, String> {
    let field = |name: &str| -> Result<Value, String> {
        ary.get(name)
            .ok_or_else(|| format!("invalid db.query: missing field '{}'", name))
    };
    let text = |name: &str| -> Result<String, String> {
        field(name)?
            .as_str()
            .ok_or_else(|| format!("invalid db.query: '{}' must be string", name))
    };
    let int = |name: &str| -> Result<i64, String> {
        field(name)?
            .as_int()
            .ok_or_else(|| format!("invalid db.query: '{}' must be int", name))
    };
    let bindings = match field("bindings")? {
        Value::Vec(items) => items.borrow().clone(),
        _ => return Err("invalid db.query: 'bindings' must be vec".to_string()),
    };
    Ok(QueryState {
        conn: int("conn")?,
        table: text("table")?,
        fields: text("fields")?,
        where_sql: text("where_sql")?,
        bindings,
        joins: text("joins")?,
        order: text("order")?,
        limit: int("limit")?,
        offset: int("offset")?,
    })
}

fn copy_state(builder: &mut Ary, state: QueryState) {
    builder.set("where_sql", Value::string(state.where_sql));
    builder.set("bindings", Value::vec(state.bindings));
    builder.set("joins", Value::string(state.joins));
    builder.set("order", Value::string(state.order));
    builder.set("limit", Value::Int(state.limit));
    builder.set("offset", Value::Int(state.offset));
}

fn build_select(
    state: &QueryState,
    columns: &str,
    limit: Option<i64>,
    with_order: bool,
    dialect: Dialect,
) -> String {
    let mut sql = format!(
        "SELECT {} FROM {}",
        columns,
        quote_ident(&state.table, dialect).unwrap_or_else(|_| dialect.quote(""))
    );
    sql.push_str(&state.joins);
    sql.push_str(&state.where_sql);
    let take = limit.unwrap_or(state.limit);
    let has_order = with_order && !state.order.is_empty();
    match dialect {
        Dialect::MsSql => {
            if take > 0 || state.offset > 0 {
                sql.push_str(" ORDER BY ");
                if has_order {
                    sql.push_str(&state.order);
                } else {
                    sql.push_str("(SELECT NULL)");
                }
                if state.offset > 0 {
                    sql.push_str(&format!(" OFFSET {} ROWS", state.offset));
                } else {
                    sql.push_str(" OFFSET 0 ROWS");
                }
                if take > 0 {
                    sql.push_str(&format!(" FETCH NEXT {} ROWS ONLY", take));
                }
            } else if has_order {
                sql.push_str(" ORDER BY ");
                sql.push_str(&state.order);
            }
        }
        _ => {
            if has_order {
                sql.push_str(" ORDER BY ");
                sql.push_str(&state.order);
            }
            if take > 0 {
                sql.push_str(&format!(" LIMIT {}", take));
                if state.offset > 0 {
                    sql.push_str(&format!(" OFFSET {}", state.offset));
                }
            } else if state.offset > 0 {
                match dialect {
                    Dialect::Sqlite => sql.push_str(&format!(" LIMIT -1 OFFSET {}", state.offset)),
                    _ => sql.push_str(&format!(" OFFSET {}", state.offset)),
                }
            }
        }
    }
    sql
}

fn build_insert(
    state: &QueryState,
    values: &[(String, Value)],
    dialect: Dialect,
) -> Result<String, String> {
    if values.is_empty() {
        return Err("insert requires at least one field".to_string());
    }
    let table = quote_ident(&state.table, dialect)?;
    let mut columns = Vec::new();
    for (name, _) in values {
        columns.push(quote_ident(name, dialect)?);
    }
    let marks = vec!["?"; values.len()].join(", ");
    Ok(format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table,
        columns.join(", "),
        marks
    ))
}

fn build_update(
    state: &QueryState,
    values: &[(String, Value)],
    dialect: Dialect,
) -> Result<String, String> {
    if values.is_empty() {
        return Err("update requires at least one field".to_string());
    }
    let table = quote_ident(&state.table, dialect)?;
    let mut assigns = Vec::new();
    for (name, _) in values {
        assigns.push(format!("{} = ?", quote_ident(name, dialect)?));
    }
    Ok(format!(
        "UPDATE {} SET {}{}",
        table,
        assigns.join(", "),
        state.where_sql
    ))
}

fn quote_ident(raw: &str, dialect: Dialect) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("identifier cannot be empty".to_string());
    }
    let mut parts = Vec::new();
    for part in raw.split('.') {
        let part = part.trim();
        if part.is_empty() || !is_identifier(part) {
            return Err(format!("invalid identifier '{}'", raw));
        }
        parts.push(dialect.quote(part));
    }
    Ok(parts.join("."))
}

fn quote_fields(raw: &str, dialect: Dialect) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok("*".to_string());
    }
    if raw == "*" {
        return Ok("*".to_string());
    }
    let mut fields = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        let lower = part.to_lowercase();
        if let Some(index) = lower.find(" as ") {
            let column = part[..index].trim();
            let alias = part[index + 4..].trim();
            fields.push(format!(
                "{} AS {}",
                quote_ident(column, dialect)?,
                quote_ident(alias, dialect)?
            ));
        } else {
            fields.push(quote_ident(part, dialect)?);
        }
    }
    Ok(fields.join(", "))
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn data_entries(value: &Value) -> Result<Vec<(String, Value)>, String> {
    match value {
        Value::Map(map) => {
            let map = map.borrow();
            let mut entries = Vec::new();
            for (key, value) in &map.entries {
                let name = key
                    .as_str()
                    .ok_or_else(|| "field names must be strings".to_string())?;
                if !is_identifier(&name) {
                    return Err(format!("invalid field name '{}'", name));
                }
                entries.push((name, value.clone()));
            }
            Ok(entries)
        }
        Value::Ary(ary) => {
            let ary = ary.borrow();
            let mut entries = Vec::new();
            for (name, value) in &ary.fields {
                if !is_identifier(name) {
                    return Err(format!("invalid field name '{}'", name));
                }
                entries.push((name.clone(), value.clone()));
            }
            Ok(entries)
        }
        _ => Err("insert/update expects a map or ary as argument".to_string()),
    }
}

fn bind_params(value: Option<&Value>) -> Result<Vec<Value>, String> {
    match value {
        None | Some(Value::None) => Ok(Vec::new()),
        Some(Value::Vec(items)) => Ok(items.borrow().clone()),
        Some(Value::Ary(ary)) => Ok(ary
            .borrow()
            .fields
            .iter()
            .map(|(_, value)| value.clone())
            .collect()),
        Some(Value::Map(map)) => Ok(map
            .borrow()
            .entries
            .iter()
            .map(|(_, value)| value.clone())
            .collect()),
        Some(_) => Err("parameters must be a vec, ary or map".to_string()),
    }
}

/// 批量执行的参数：`vec<vec<any>>`，外层每个元素是一组绑定参数。
fn bind_rows(value: Option<&Value>) -> Result<Vec<Vec<Value>>, String> {
    let items = match value {
        None | Some(Value::None) => return Ok(Vec::new()),
        Some(Value::Vec(items)) => items,
        Some(_) => return Err("execute_many rows must be vec<vec<any>>".to_string()),
    };
    let items = items.borrow();
    let mut rows = Vec::with_capacity(items.len());
    for item in items.iter() {
        let row = match item {
            Value::Vec(cells) => cells.borrow().clone(),
            Value::Ary(ary) => ary
                .borrow()
                .fields
                .iter()
                .map(|(_, value)| value.clone())
                .collect(),
            Value::Map(map) => map
                .borrow()
                .entries
                .iter()
                .map(|(_, value)| value.clone())
                .collect(),
            _ => return Err("execute_many rows must be vec<vec<any>>".to_string()),
        };
        rows.push(row);
    }
    Ok(rows)
}

fn is_nullish(value: &Value) -> bool {
    matches!(value, Value::Null | Value::None)
}

fn normalize_op(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "=" | "==" => "=".to_string(),
        "!=" | "<>" => "!=".to_string(),
        ">" => ">".to_string(),
        ">=" => ">=".to_string(),
        "<" => "<".to_string(),
        "<=" => "<=".to_string(),
        "like" => "LIKE".to_string(),
        "not like" => "NOT LIKE".to_string(),
        "is" => "is".to_string(),
        "is not" => "is not".to_string(),
        _ => "INVALID".to_string(),
    }
}

fn allowed_op(op: &str) -> bool {
    matches!(
        op,
        "=" | "!=" | ">" | ">=" | "<" | "<=" | "LIKE" | "NOT LIKE"
    )
}

fn prefix_for(where_sql: &str) -> &'static str {
    if where_sql.trim().is_empty() {
        " WHERE "
    } else {
        " AND "
    }
}

fn to_sqlite_params(params: &[Value]) -> Result<Vec<SqliteValue>, String> {
    params.iter().map(value_to_sqlite).collect()
}

/// 无符号整数绑定到 SQL 的 64 位有符号整数列；超出范围直接报错，避免静默变成负数。
fn uint_to_sql(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("u64 value {} exceeds SQL BIGINT range", value))
}

fn value_to_sqlite(value: &Value) -> Result<SqliteValue, String> {
    match value {
        Value::Null | Value::None => Ok(SqliteValue::Null),
        Value::Bool(value) => Ok(SqliteValue::Integer(if *value { 1 } else { 0 })),
        Value::Int(value) => Ok(SqliteValue::Integer(*value)),
        Value::UInt(value) => Ok(SqliteValue::Integer(uint_to_sql(*value)?)),
        Value::Float(value) => Ok(SqliteValue::Real(*value)),
        Value::Str(value) => Ok(SqliteValue::Text((**value).clone())),
        Value::Char(value) => Ok(SqliteValue::Text(value.clone())),
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => value_to_sqlite(inner),
        Value::Vec(_) | Value::Map(_) | Value::Ary(_) => Ok(SqliteValue::Text(json_text(value)?)),
        Value::Func(_) => Err("cannot bind a function to SQL".to_string()),
        Value::Task(_) => Err("cannot bind a task to SQL".to_string()),
    }
}

fn sqlite_to_value(value: SqliteValue) -> Value {
    match value {
        SqliteValue::Null => Value::Null,
        SqliteValue::Integer(value) => Value::Int(value),
        SqliteValue::Real(value) => Value::Float(value),
        SqliteValue::Text(value) => Value::string(value),
        SqliteValue::Blob(value) => bytes_to_value(&value),
    }
}

fn to_mysql_params(params: &[Value]) -> Result<Vec<MySqlValue>, String> {
    params.iter().map(mysql_param).collect()
}

fn mysql_param(value: &Value) -> Result<MySqlValue, String> {
    match value {
        Value::Null | Value::None => Ok(MySqlValue::NULL),
        Value::Bool(value) => Ok(MySqlValue::Int(if *value { 1 } else { 0 })),
        Value::Int(value) => Ok(MySqlValue::Int(*value)),
        Value::UInt(value) => Ok(MySqlValue::Int(uint_to_sql(*value)?)),
        Value::Float(value) => Ok(MySqlValue::Double(*value)),
        Value::Str(value) => Ok(MySqlValue::Bytes(value.as_bytes().to_vec())),
        Value::Char(value) => Ok(MySqlValue::Bytes(value.as_bytes().to_vec())),
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => mysql_param(inner),
        Value::Vec(_) | Value::Map(_) | Value::Ary(_) => {
            Ok(MySqlValue::Bytes(json_text(value)?.into_bytes()))
        }
        Value::Func(_) => Err("cannot bind a function to SQL".to_string()),
        Value::Task(_) => Err("cannot bind a task to SQL".to_string()),
    }
}

fn mysql_to_value(value: MySqlValue) -> Value {
    match value {
        MySqlValue::NULL => Value::Null,
        MySqlValue::Int(value) => Value::Int(value),
        MySqlValue::UInt(value) => Value::Int(value as i64),
        MySqlValue::Float(value) => Value::Float(value as f64),
        MySqlValue::Double(value) => Value::Float(value),
        MySqlValue::Bytes(value) => Value::string(String::from_utf8_lossy(&value).to_string()),
        MySqlValue::Date(year, month, day, hour, minute, second, micro) => Value::string(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}",
            year, month, day, hour, minute, second, micro
        )),
        MySqlValue::Time(negative, days, hours, minutes, seconds, micro) => Value::string(format!(
            "{}{:03}:{:02}:{:02}.{:06}",
            if negative { "-" } else { "" },
            days * 24 + hours as u32,
            minutes,
            seconds,
            micro
        )),
    }
}

fn to_postgres_params(params: &[Value]) -> Result<Vec<Box<dyn PgToSql + Sync>>, String> {
    params.iter().map(postgres_param).collect()
}

fn postgres_param(value: &Value) -> Result<Box<dyn PgToSql + Sync>, String> {
    match value {
        Value::Null | Value::None => Ok(Box::new(None::<String>)),
        Value::Bool(value) => Ok(Box::new(*value)),
        Value::Int(value) => Ok(Box::new(*value)),
        Value::UInt(value) => Ok(Box::new(uint_to_sql(*value)?)),
        Value::Float(value) => Ok(Box::new(*value)),
        Value::Str(value) => Ok(Box::new((**value).clone())),
        Value::Char(value) => Ok(Box::new(value.clone())),
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => postgres_param(inner),
        Value::Vec(_) | Value::Map(_) | Value::Ary(_) => Ok(Box::new(json_text(value)?)),
        Value::Func(_) => Err("cannot bind a function to SQL".to_string()),
        Value::Task(_) => Err("cannot bind a task to SQL".to_string()),
    }
}

fn postgres_value(row: &postgres::Row, index: usize, ty: &str) -> Result<Value, String> {
    let mismatch = |error: postgres::Error| {
        format!(
            "postgres column {} ({}) cannot be read: {}",
            index, ty, error
        )
    };
    match ty {
        "int2" => row
            .try_get::<usize, i16>(index)
            .map(|v| Value::Int(v as i64))
            .map_err(mismatch),
        "int4" => row
            .try_get::<usize, i32>(index)
            .map(|v| Value::Int(v as i64))
            .map_err(mismatch),
        "int8" => row
            .try_get::<usize, i64>(index)
            .map(Value::Int)
            .map_err(mismatch),
        "float4" => row
            .try_get::<usize, f32>(index)
            .map(|v| Value::Float(v as f64))
            .map_err(mismatch),
        "float8" | "numeric" => row
            .try_get::<usize, f64>(index)
            .map(Value::Float)
            .map_err(mismatch),
        "bool" => row
            .try_get::<usize, bool>(index)
            .map(Value::Bool)
            .map_err(mismatch),
        "bytea" => row
            .try_get::<usize, Vec<u8>>(index)
            .map(|v| bytes_to_value(&v))
            .map_err(mismatch),
        "timestamp" | "timestamptz" => row
            .try_get::<usize, SystemTime>(index)
            .map(|time| Value::Int(unix_seconds(time)))
            .map_err(mismatch),
        _ => row
            .try_get::<usize, String>(index)
            .map(Value::string)
            .map_err(|error| {
                format!(
                    "postgres column {} ({}) cannot be read as text; cast it in SQL: {}",
                    index, ty, error
                )
            }),
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(_) => 0,
    }
}

fn to_mssql_params(params: &[Value]) -> Result<Vec<MsColumnData<'static>>, String> {
    params.iter().map(mssql_param).collect()
}

fn mssql_param(value: &Value) -> Result<MsColumnData<'static>, String> {
    match value {
        Value::Null | Value::None => Ok(MsColumnData::String(None)),
        Value::Bool(value) => Ok(MsColumnData::Bit(Some(*value))),
        Value::Int(value) => Ok(MsColumnData::I64(Some(*value))),
        Value::UInt(value) => Ok(MsColumnData::I64(Some(uint_to_sql(*value)?))),
        Value::Float(value) => Ok(MsColumnData::F64(Some(*value))),
        Value::Str(value) => Ok(MsColumnData::String(Some(Cow::Owned((**value).clone())))),
        Value::Char(value) => Ok(MsColumnData::String(Some(Cow::Owned(value.clone())))),
        Value::Some(inner) | Value::Ok(inner) | Value::Err(inner) => mssql_param(inner),
        Value::Vec(_) | Value::Map(_) | Value::Ary(_) => Ok(MsColumnData::String(Some(
            Cow::Owned(json_text(value)?),
        ))),
        Value::Func(_) => Err("cannot bind a function to SQL".to_string()),
        Value::Task(_) => Err("cannot bind a task to SQL".to_string()),
    }
}

fn mssql_to_value(data: &MsColumnData<'_>) -> Value {
    match data {
        MsColumnData::U8(value) => int_or_null(value.map(|v| v as i64)),
        MsColumnData::I16(value) => int_or_null(value.map(|v| v as i64)),
        MsColumnData::I32(value) => int_or_null(value.map(|v| v as i64)),
        MsColumnData::I64(value) => int_or_null(*value),
        MsColumnData::F32(value) => float_or_null(value.map(|v| v as f64)),
        MsColumnData::F64(value) => float_or_null(*value),
        MsColumnData::Bit(value) => match value {
            Some(value) => Value::Bool(*value),
            None => Value::Null,
        },
        MsColumnData::String(value) => match value {
            Some(value) => Value::string(value.to_string()),
            None => Value::Null,
        },
        MsColumnData::Guid(value) => match value {
            Some(value) => Value::string(value.to_string()),
            None => Value::Null,
        },
        MsColumnData::Binary(value) => match value {
            Some(value) => bytes_to_value(value),
            None => Value::Null,
        },
        MsColumnData::Numeric(value) => match value {
            Some(value) => {
                let text = value.to_string();
                text.parse::<f64>()
                    .map(Value::Float)
                    .unwrap_or_else(|_| Value::string(text))
            }
            None => Value::Null,
        },
        MsColumnData::Xml(value) => match value {
            Some(value) => Value::string(format!("{:?}", value)),
            None => Value::Null,
        },
        MsColumnData::DateTime(value) => match value {
            Some(value) => {
                let seconds = (value.days() as i64 - 25_567) * 86_400
                    + (value.seconds_fragments() as i64 / 300);
                Value::Int(seconds)
            }
            None => Value::Null,
        },
        MsColumnData::SmallDateTime(value) => match value {
            Some(value) => {
                let seconds = (value.days() as i64 - 25_567) * 86_400
                    + (value.seconds_fragments() as i64 / 300);
                Value::Int(seconds)
            }
            None => Value::Null,
        },
        other => Value::string(format!("{:?}", other)),
    }
}

fn int_or_null(value: Option<i64>) -> Value {
    match value {
        Some(value) => Value::Int(value),
        None => Value::Null,
    }
}

fn float_or_null(value: Option<f64>) -> Value {
    match value {
        Some(value) => Value::Float(value),
        None => Value::Null,
    }
}

fn bytes_to_value(bytes: &[u8]) -> Value {
    Value::vec(
        bytes
            .iter()
            .map(|byte| Value::UInt(*byte as u64))
            .collect(),
    )
}

fn json_text(value: &Value) -> Result<String, String> {
    serde_json::to_string(&value_to_json(value)).map_err(|error| error.to_string())
}

pub(crate) fn row_field(row: &Value, name: &str) -> Option<Value> {
    match row {
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .find(|(key, _)| key.as_str().as_deref() == Some(name))
            .map(|(_, value)| value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_postgres_sslmode() {
        assert_eq!(
            parse_sslmode("postgres://u:p@h/db").unwrap(),
            PgSslMode::Disable
        );
        assert_eq!(
            parse_sslmode("postgres://u:p@h/db?sslmode=require").unwrap(),
            PgSslMode::Require
        );
        assert_eq!(
            parse_sslmode("postgres://u:p@h/db?sslmode=verify-ca&connect_timeout=3").unwrap(),
            PgSslMode::VerifyCa
        );
        assert_eq!(
            parse_sslmode("postgres://u:p@h/db?sslmode=VERIFY-FULL").unwrap(),
            PgSslMode::VerifyFull
        );
        assert_eq!(
            parse_sslmode("host=h dbname=d user=u sslmode=require").unwrap(),
            PgSslMode::Require
        );
        assert_eq!(
            parse_sslmode("postgres://u:p@h/db?sslmode=disable").unwrap(),
            PgSslMode::Disable
        );
        assert!(parse_sslmode("postgres://u:p@h/db?sslmode=prefer").is_err());
        assert!(parse_sslmode("postgres://u:p@h/db?sslmode=allow").is_err());
        assert!(parse_sslmode("postgres://u:p@h/db?sslmode=bogus").is_err());
    }

    fn state() -> QueryState {
        QueryState {
            conn: 1,
            table: "users".to_string(),
            fields: "*".to_string(),
            where_sql: String::new(),
            bindings: Vec::new(),
            joins: String::new(),
            order: String::new(),
            limit: 0,
            offset: 0,
        }
    }

    #[test]
    fn splits_sql_scripts_outside_quotes_and_comments() {
        let script = "-- 头部注释\nCREATE TABLE a (id INTEGER);\n\nINSERT INTO a VALUES ('x;y');\nINSERT INTO a VALUES (\"p;q\");\n/* 块注释 ; */\nUPDATE a SET id = 1 -- 行尾注释 ;\n;\n";
        let statements = split_sql_statements(script);
        assert_eq!(statements.len(), 4, "{:?}", statements);
        assert!(statements[0].contains("CREATE TABLE a"));
        assert!(statements[1].contains("'x;y'"));
        assert!(statements[2].contains("\"p;q\""));
        assert!(statements[3].contains("UPDATE a SET id = 1"));
    }

    #[test]
    fn drops_comment_only_statements() {
        assert!(split_sql_statements("-- only a comment\n").is_empty());
        assert!(split_sql_statements("/* only a comment */").is_empty());
        assert_eq!(split_sql_statements("SELECT 1;").len(), 1);
    }

    #[test]
    fn rewrites_placeholders_by_dialect() {
        assert_eq!(
            render_sql("SELECT * FROM t WHERE a = ? AND b = ?", Dialect::Sqlite),
            "SELECT * FROM t WHERE a = ? AND b = ?"
        );
        assert_eq!(
            render_sql("SELECT * FROM t WHERE a = ? AND b = ?", Dialect::MySql),
            "SELECT * FROM t WHERE a = ? AND b = ?"
        );
        assert_eq!(
            render_sql("SELECT * FROM t WHERE a = ? AND b = ?", Dialect::Postgres),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
        assert_eq!(
            render_sql("SELECT * FROM t WHERE a = ? AND b = ?", Dialect::MsSql),
            "SELECT * FROM t WHERE a = @P1 AND b = @P2"
        );
    }

    #[test]
    fn keeps_question_marks_inside_literals_and_comments() {
        let sql = "SELECT '?' AS q -- ?\n FROM t WHERE a = ?";
        assert_eq!(
            render_sql(sql, Dialect::Postgres),
            "SELECT '?' AS q -- ?\n FROM t WHERE a = $1"
        );
        // 块注释与美元引用中的 ? 不得被改写。
        let block = "SELECT /* why? */ a FROM t WHERE b = ?";
        assert_eq!(
            render_sql(block, Dialect::Postgres),
            "SELECT /* why? */ a FROM t WHERE b = $1"
        );
        let dollar = "SELECT $$ what? $$ AS d, $tag$ how? $tag$ AS h WHERE x = ?";
        assert_eq!(
            render_sql(dollar, Dialect::Postgres),
            "SELECT $$ what? $$ AS d, $tag$ how? $tag$ AS h WHERE x = $1"
        );
    }

    #[test]
    fn quotes_identifiers_per_dialect() {
        assert_eq!(quote_ident("users", Dialect::Sqlite).unwrap(), "\"users\"");
        assert_eq!(quote_ident("users", Dialect::MySql).unwrap(), "`users`");
        assert_eq!(quote_ident("app.users", Dialect::Postgres).unwrap(), "\"app\".\"users\"");
        assert_eq!(quote_ident("users", Dialect::MsSql).unwrap(), "\"users\"");
        assert!(quote_ident("users; DROP TABLE x", Dialect::Sqlite).is_err());
    }

    #[test]
    fn quotes_field_list_with_aliases() {
        assert_eq!(
            quote_fields("id, name as n", Dialect::MySql).unwrap(),
            "`id`, `name` AS `n`"
        );
        assert_eq!(quote_fields("*", Dialect::Sqlite).unwrap(), "*");
    }

    #[test]
    fn builds_select_with_pagination_per_dialect() {
        let mut query = state();
        query.where_sql = " WHERE \"id\" = ?".to_string();
        query.order = "\"id\" DESC".to_string();
        query.limit = 10;
        query.offset = 5;

        assert_eq!(
            build_select(&query, "*", None, true, Dialect::Sqlite),
            "SELECT * FROM \"users\" WHERE \"id\" = ? ORDER BY \"id\" DESC LIMIT 10 OFFSET 5"
        );
        assert_eq!(
            build_select(&query, "*", None, true, Dialect::Postgres),
            "SELECT * FROM \"users\" WHERE \"id\" = ? ORDER BY \"id\" DESC LIMIT 10 OFFSET 5"
        );
        assert_eq!(
            build_select(&query, "*", None, true, Dialect::MsSql),
            "SELECT * FROM \"users\" WHERE \"id\" = ? ORDER BY \"id\" DESC OFFSET 5 ROWS FETCH NEXT 10 ROWS ONLY"
        );
    }

    #[test]
    fn mssql_needs_order_by_for_offset() {
        let mut query = state();
        query.offset = 20;
        assert_eq!(
            build_select(&query, "*", None, true, Dialect::MsSql),
            "SELECT * FROM \"users\" ORDER BY (SELECT NULL) OFFSET 20 ROWS"
        );
    }

    #[test]
    fn builds_insert_and_update() {
        let query = state();
        let values = vec![
            ("name".to_string(), Value::string("Alice")),
            ("email".to_string(), Value::string("a@x.com")),
        ];
        assert_eq!(
            build_insert(&query, &values, Dialect::MySql).unwrap(),
            "INSERT INTO `users` (`name`, `email`) VALUES (?, ?)"
        );
        assert_eq!(
            build_update(&query, &values, Dialect::MsSql).unwrap(),
            "UPDATE \"users\" SET \"name\" = ?, \"email\" = ?"
        );
    }

    #[test]
    fn parses_sqlite_dsn() {
        assert_eq!(sqlite_path("storage/app.db"), "storage/app.db");
        assert_eq!(sqlite_path("sqlite:storage/app.db"), "storage/app.db");
        assert_eq!(sqlite_path("sqlite://storage/app.db"), "storage/app.db");
        assert_eq!(sqlite_path("sqlite:///tmp/app.db"), "/tmp/app.db");
        assert_eq!(sqlite_path("sqlite::memory:"), ":memory:");
    }

    #[test]
    fn parses_postgres_database_name() {
        assert_eq!(
            postgres_database_name("postgres://u:p@localhost:5432/app?sslmode=disable"),
            "app"
        );
        assert_eq!(postgres_database_name("postgresql://localhost/app"), "app");
        assert_eq!(postgres_database_name("host=localhost user=postgres"), "");
    }

    #[test]
    fn parses_mssql_dsn() {
        let (config, database) =
            mssql_config("mssql://sa:pass%40word@127.0.0.1:1433/app").unwrap();
        assert_eq!(database, "app");
        assert_eq!(config.get_addr(), "127.0.0.1:1433");

        let (config, database) = mssql_config(
            "Server=db.local,1434;Database=app2;User Id=sa;Password=secret;TrustServerCertificate=true",
        )
        .unwrap();
        assert_eq!(database, "app2");
        assert!(config.get_addr().starts_with("db.local:1434"), "{}", config.get_addr());
    }

    #[test]
    fn parses_mssql_tls_params() {
        let tls = mssql_tls_settings("").unwrap();
        assert!(tls.encrypt, "default keeps encryption on");
        assert!(tls.trust_cert, "default keeps trust_cert on");
        assert!(!mssql_tls_settings("encrypt=false").unwrap().encrypt);
        let tls = mssql_tls_settings("TrustServerCertificate=FALSE;encrypt=on").unwrap();
        assert!(tls.encrypt);
        assert!(!tls.trust_cert);
        assert!(mssql_tls_settings("encrypt=maybe").is_err());
        // 参数从 URL 中剥离，不会泄漏进主机名或库名。
        let (config, database) =
            mssql_config("mssql://sa:p@h:1433/app?encrypt=false;trustcert=false").unwrap();
        assert_eq!(database, "app");
        assert_eq!(config.get_addr(), "h:1433");
    }

    #[test]
    fn transaction_sql_is_dialect_aware() {
        assert_eq!(transaction_sql("begin", Dialect::Sqlite), "BEGIN");
        assert_eq!(transaction_sql("begin", Dialect::MySql), "START TRANSACTION");
        assert_eq!(transaction_sql("begin", Dialect::MsSql), "BEGIN TRANSACTION");
        assert_eq!(transaction_sql("commit", Dialect::MsSql), "COMMIT TRANSACTION");
        assert_eq!(transaction_sql("rollback", Dialect::MsSql), "ROLLBACK TRANSACTION");
        assert_eq!(transaction_sql("rollback", Dialect::Postgres), "ROLLBACK");
    }

    #[test]
    fn rejects_unsafe_identifiers() {
        assert!(quote_ident("name or 1=1", Dialect::Sqlite).is_err());
        assert!(quote_ident("", Dialect::Sqlite).is_err());
        assert!(quote_fields("id, (select 1)", Dialect::Sqlite).is_err());
    }

    /// 造一条本地连接；直接占名额，不经过空闲复用，便于测试池的记账分支。
    fn sqlite_conn(pool: &Arc<Pool>, dsn: &str) -> DbConn {
        let mut state = lock(&pool.state);
        let count = state.open.get(dsn).copied().unwrap_or(0);
        state.open.insert(dsn.to_string(), count + 1);
        drop(state);
        let permit = Permit {
            pool: Arc::downgrade(pool),
            dsn: Arc::from(dsn),
        };
        DbConn {
            kind: ConnKind::Sqlite(SqliteConn::open_in_memory().unwrap()),
            dialect: Dialect::Sqlite,
            database: ":memory:".to_string(),
            permit,
        }
    }

    #[test]
    fn reuses_returned_connection() {
        let pool = Arc::new(Pool::new(PoolConfig::default()));
        let conn = sqlite_conn(&pool, "test://reuse");
        pool.give_back(conn);
        let reused = match pool.take("test://reuse") {
            Take::Reused(conn) => conn,
            _ => panic!("connection should be reused"),
        };
        assert_eq!(reused.dialect, Dialect::Sqlite);
        // 借出后存活计数仍为 1，归还后仍可复用。
        {
            let state = lock(&pool.state);
            assert_eq!(state.open.get("test://reuse").copied(), Some(1));
        }
        drop(reused);
    }

    #[test]
    fn closes_connection_beyond_max_idle() {
        let pool = Arc::new(Pool::new(PoolConfig {
            max_open: 4,
            max_idle: 1,
            ..PoolConfig::default()
        }));
        pool.give_back(sqlite_conn(&pool, "test://idle"));
        pool.give_back(sqlite_conn(&pool, "test://idle"));
        let state = lock(&pool.state);
        assert_eq!(state.idle.get("test://idle").map(|list| list.len()), Some(1));
        // 超额的连接被关闭，名额随之释放。
        assert_eq!(state.open.get("test://idle").copied(), Some(1));
    }

    #[test]
    fn take_respects_max_open_and_releases_on_drop() {
        let pool = Arc::new(Pool::new(PoolConfig {
            max_open: 1,
            max_idle: 1,
            ..PoolConfig::default()
        }));
        let permit = match pool.take("test://limit") {
            Take::Fresh(permit) => permit,
            _ => panic!("first slot"),
        };
        assert!(matches!(pool.take("test://limit"), Take::Full), "max_open must cap");
        drop(permit);
        assert!(
            matches!(pool.take("test://limit"), Take::Fresh(_)),
            "dropped conn frees a slot"
        );
    }

    #[test]
    fn drops_stale_idle_connection() {
        let pool = Arc::new(Pool::new(PoolConfig {
            max_open: 2,
            max_idle: 2,
            idle_timeout: Duration::ZERO,
            ..PoolConfig::default()
        }));
        pool.give_back(sqlite_conn(&pool, "test://stale"));
        std::thread::sleep(Duration::from_millis(2));
        // 过期空闲连接被淘汰并新建：返回 Fresh 而非 Reused，且名额记账不重复递减。
        let taken = pool.take("test://stale");
        assert!(matches!(taken, Take::Fresh(_)), "stale idle conn must be evicted");
        let state = lock(&pool.state);
        assert_eq!(state.open.get("test://stale").copied(), Some(1));
        assert!(state.idle.get("test://stale").is_none()
            || state.idle.get("test://stale").map(|l| l.is_empty()).unwrap_or(false));
    }

    #[test]
    fn normalizes_pool_limits() {
        let config = PoolConfig {
            max_open: 0,
            max_idle: 99,
            ..PoolConfig::default()
        }
        .normalized();
        assert_eq!(config.max_open, 1);
        assert_eq!(config.max_idle, 1);
    }

    /// `parallel` 与 HTTP 工作线程要求连接池可跨线程共享。
    #[test]
    fn pool_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<Pool>>();

        let pool = Arc::new(Pool::new(PoolConfig::default()));
        let worker = Arc::clone(&pool);
        std::thread::spawn(move || {
            let conn = sqlite_conn(&worker, "test://threads");
            worker.give_back(conn);
        })
        .join()
        .unwrap();

        let reused = match pool.take("test://threads") {
            Take::Reused(conn) => conn,
            _ => panic!("idle connection should be reusable from another thread"),
        };
        assert_eq!(reused.dialect, Dialect::Sqlite);
    }

    /// `row_to_json` 直出必须与「map 值树 → 快速序列化」路径逐字节一致。
    #[test]
    fn row_to_json_matches_map_path() {
        let names = vec![
            "updated_ms".to_string(),
            "id".to_string(),
            "name".to_string(),
        ];
        let cells = vec![Value::Null, Value::Int(7), Value::string("u\"1")];
        let direct = row_to_json(&names, &cells);
        let map = Value::map(
            names
                .iter()
                .zip(&cells)
                .map(|(name, value)| (Value::string(name.clone()), value.clone()))
                .collect(),
        );
        assert_eq!(direct, crate::value::value_to_json_string(&map));
        // 重复列名保留最后一个（与 serde_json BTreeMap 覆盖语义一致）。
        let dup_names = vec!["a".to_string(), "a".to_string()];
        let dup_cells = vec![Value::Int(1), Value::Int(2)];
        assert_eq!(row_to_json(&dup_names, &dup_cells), "{\"a\":2}");
        // 空行：合法的空对象。
        assert_eq!(row_to_json(&[], &[]), "{}");
    }

    #[test]
    fn bind_rows_accepts_vec_of_vec() {
        let value = Value::vec(vec![
            Value::vec(vec![Value::Int(1), Value::string("a")]),
            Value::vec(vec![Value::Int(2), Value::string("b")]),
        ]);
        let rows = bind_rows(Some(&value)).expect("vec<vec<any>> 应当可用");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert!(matches!(rows[1][0], Value::Int(2)));
    }

    #[test]
    fn bind_rows_rejects_scalars_and_empty() {
        assert!(bind_rows(None).expect("缺省表示没有行").is_empty());
        assert!(bind_rows(Some(&Value::vec(Vec::new())))
            .expect("空 vec 合法")
            .is_empty());
        let mixed = Value::vec(vec![Value::Int(1)]);
        assert!(bind_rows(Some(&mixed)).is_err(), "标量行必须报错");
        assert!(bind_rows(Some(&Value::Int(1))).is_err());
    }

    #[test]
    fn redacts_credentials_in_dsn() {
        assert_eq!(
            redact_dsn("mysql://root:123456@127.0.0.1:3306/ripbench"),
            "mysql://root:***@127.0.0.1:3306/ripbench"
        );
        // 没有口令的 DSN 原样返回。
        assert_eq!(redact_dsn("sqlite:e:/tmp/bench.db"), "sqlite:e:/tmp/bench.db");
        assert_eq!(redact_dsn("mysql://root@localhost/db"), "mysql://root@localhost/db");
    }
}

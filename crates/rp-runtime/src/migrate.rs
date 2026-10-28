//! 数据库迁移执行器。
//!
//! 迁移脚本放在项目的 `migrations/` 目录，按 `<版本>_<名称>.sql` 命名；
//! 回滚脚本同名加 `.down`（`<版本>_<名称>.down.sql`）。已执行的版本记录在
//! `rip_schema_migrations` 表里，同时保存脚本校验和，脚本被改动后再次
//! `up` 会直接报错而不是悄悄跳过。
//!
//! 每条迁移在自己的事务里执行：脚本成功后才写入版本行，任何一步失败都整体
//! 回滚。MySQL 的 DDL 会隐式提交，这一点在文档里注明。

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::db::{row_field, Dialect};
use crate::rt::{NativeModule, Runtime};
use crate::value::Value;

/// 版本表名。所有方言共用同一个名字，便于跨库排查。
pub const VERSION_TABLE: &str = "rip_schema_migrations";

const INSERT_VERSION_SQL: &str = "INSERT INTO rip_schema_migrations \
    (version, name, checksum, applied_at, duration_ms) VALUES (?, ?, ?, ?, ?)";
const DELETE_VERSION_SQL: &str = "DELETE FROM rip_schema_migrations WHERE version = ?";
const SELECT_VERSIONS_SQL: &str = "SELECT version, name, checksum, applied_at \
    FROM rip_schema_migrations ORDER BY version";

/// 迁移方向。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// `up` / `down` 的选项。
#[derive(Clone, Default)]
pub struct Options {
    /// 最多执行多少条；`up` 缺省全部，`down` 缺省 1 条。
    pub step: Option<usize>,
    /// 目标版本：`up` 只应用到该版本（含），`down` 回滚到该版本之后（该版本保留）。
    pub to: Option<i64>,
    /// 只打印计划，不写数据库。
    pub dry_run: bool,
    /// 忽略已应用脚本的校验和不一致（默认报错）。
    pub allow_modified: bool,
}

/// `status` 的一行：磁盘上的迁移 + 数据库里的执行记录。
#[derive(Debug)]
pub struct StatusEntry {
    pub version: i64,
    pub name: String,
    pub applied_at: Option<String>,
    /// 已应用但脚本内容变了（或脚本文件已删除）时为 false。
    pub checksum_ok: bool,
    /// 数据库有记录、磁盘上找不到对应脚本。
    pub missing: bool,
    pub has_down: bool,
}

#[derive(Debug)]
pub struct StatusReport {
    pub dialect: String,
    pub directory: PathBuf,
    pub entries: Vec<StatusEntry>,
    pub applied: usize,
    pub pending: usize,
}

#[derive(Debug)]
pub struct StepReport {
    pub version: i64,
    pub name: String,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct RunReport {
    pub dialect: String,
    pub directory: PathBuf,
    pub direction: Direction,
    pub executed: Vec<StepReport>,
    /// 执行后（dry-run 时为“计划执行后”）已应用的迁移数量。
    pub applied: usize,
    /// 执行后仍未应用的迁移数量。
    pub pending: usize,
    pub dry_run: bool,
}

#[derive(Debug)]
struct Migration {
    version: i64,
    name: String,
    up: PathBuf,
    down: Option<PathBuf>,
    checksum: String,
}

struct AppliedRow {
    version: i64,
    name: String,
    checksum: String,
    applied_at: String,
}

/// 迁移会话：独立的运行时 + 一条独占连接，全部驱动复用 `std/db` 的实现。
struct Session {
    runtime: Runtime,
    conn: i64,
}

impl Session {
    fn open(dsn: &str) -> Result<Session, String> {
        let mut runtime = Runtime::new_native(NativeModule::default());
        let conn = runtime.open_tracked(dsn)?;
        Ok(Session { runtime, conn })
    }

    fn dialect(&self) -> Dialect {
        self.runtime.tracked_dialect(self.conn)
    }

    fn script(&mut self, sql: &str) -> Result<(), String> {
        self.runtime.exec_script(self.conn, sql)
    }

    fn exec(&mut self, sql: &str, params: &[Value]) -> Result<usize, String> {
        self.runtime.run_exec(self.conn, sql, params)
    }

    fn query(&mut self, sql: &str) -> Result<Vec<Value>, String> {
        self.runtime.run_query(self.conn, sql, &[])
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.runtime.close_tracked(self.conn);
    }
}

/// 只读查询：当前数据库已应用/未应用的迁移。
pub fn status(dsn: &str, dir: &Path) -> Result<StatusReport, String> {
    let mut session = Session::open(dsn)?;
    let dialect = session.dialect();
    let migrations = discover(dir, dialect.name())?;
    ensure_table(&mut session, dialect)?;
    let applied = applied_rows(&mut session)?;
    let applied_map: BTreeMap<i64, &AppliedRow> =
        applied.iter().map(|row| (row.version, row)).collect();

    let mut entries = Vec::with_capacity(migrations.len() + applied.len());
    for migration in &migrations {
        match applied_map.get(&migration.version) {
            Some(row) => entries.push(StatusEntry {
                version: migration.version,
                name: row.name.clone(),
                applied_at: Some(row.applied_at.clone()),
                checksum_ok: row.checksum == migration.checksum,
                missing: false,
                has_down: migration.down.is_some(),
            }),
            None => entries.push(StatusEntry {
                version: migration.version,
                name: migration.name.clone(),
                applied_at: None,
                checksum_ok: true,
                missing: false,
                has_down: migration.down.is_some(),
            }),
        }
    }
    // 数据库里有、磁盘上没有的版本：保留展示，提示脚本丢失。
    for row in &applied {
        if !migrations.iter().any(|item| item.version == row.version) {
            entries.push(StatusEntry {
                version: row.version,
                name: row.name.clone(),
                applied_at: Some(row.applied_at.clone()),
                checksum_ok: false,
                missing: true,
                has_down: false,
            });
        }
    }
    entries.sort_by_key(|entry| entry.version);
    let applied_count = entries
        .iter()
        .filter(|entry| entry.applied_at.is_some())
        .count();
    let pending = entries.len() - applied_count;
    Ok(StatusReport {
        dialect: dialect.name().to_string(),
        directory: dir.to_path_buf(),
        entries,
        applied: applied_count,
        pending,
    })
}

/// 应用未执行的迁移（按版本升序）。
pub fn up(dsn: &str, dir: &Path, options: &Options) -> Result<RunReport, String> {
    let mut session = Session::open(dsn)?;
    let dialect = session.dialect();
    let migrations = discover(dir, dialect.name())?;
    ensure_table(&mut session, dialect)?;
    let applied = applied_rows(&mut session)?;
    verify_checksums(&migrations, &applied, options.allow_modified)?;

    let applied_versions: HashSet<i64> = applied.iter().map(|row| row.version).collect();
    let mut pending: Vec<&Migration> = migrations
        .iter()
        .filter(|migration| !applied_versions.contains(&migration.version))
        .collect();
    if let Some(to) = options.to {
        pending.retain(|migration| migration.version <= to);
    }
    if let Some(step) = options.step {
        pending.truncate(step);
    }

    let mut executed = Vec::with_capacity(pending.len());
    for migration in &pending {
        let started = Instant::now();
        if !options.dry_run {
            let script = read_script(&migration.up)?;
            let version = migration.version;
            let name = migration.name.clone();
            let checksum = migration.checksum.clone();
            with_transaction(&mut session, dialect, |session| {
                session.script(&script)?;
                let elapsed = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
                session.exec(
                    INSERT_VERSION_SQL,
                    &[
                        Value::Int(version),
                        Value::string(name.clone()),
                        Value::string(checksum.clone()),
                        Value::string(now_text()),
                        Value::Int(elapsed),
                    ],
                )?;
                Ok(())
            })
            .map_err(|error| {
                format!(
                    "migration {} '{}' ({}) failed: {}",
                    migration.version,
                    migration.name,
                    migration.up.display(),
                    error
                )
            })?;
        }
        executed.push(StepReport {
            version: migration.version,
            name: migration.name.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    let applied_files = migrations
        .iter()
        .filter(|migration| applied_versions.contains(&migration.version))
        .count();
    let applied_now = applied_files + executed.len();
    Ok(RunReport {
        dialect: dialect.name().to_string(),
        directory: dir.to_path_buf(),
        direction: Direction::Up,
        executed,
        applied: applied_now,
        pending: migrations.len() - applied_now,
        dry_run: options.dry_run,
    })
}

/// 回滚已执行的迁移（按版本降序）。缺省回滚 1 条；给了 `--to` 则回滚到该版本之后。
pub fn down(dsn: &str, dir: &Path, options: &Options) -> Result<RunReport, String> {
    let mut session = Session::open(dsn)?;
    let dialect = session.dialect();
    let migrations = discover(dir, dialect.name())?;
    ensure_table(&mut session, dialect)?;
    let applied = applied_rows(&mut session)?;

    let mut targets: Vec<&AppliedRow> = applied.iter().rev().collect();
    if let Some(to) = options.to {
        targets.retain(|row| row.version > to);
    }
    let limit = match options.step {
        Some(step) => Some(step),
        // 未给 --step 时：`down` 默认只回滚一条；指定 `--to` 时回滚到目标版本。
        None if options.to.is_some() => None,
        None => Some(1),
    };
    if let Some(limit) = limit {
        targets.truncate(limit);
    }

    let mut executed = Vec::with_capacity(targets.len());
    for row in &targets {
        let migration = migrations
            .iter()
            .find(|migration| migration.version == row.version)
            .ok_or_else(|| {
                format!(
                    "cannot roll back {} '{}': migration file is missing",
                    row.version, row.name
                )
            })?;
        let down_path = migration.down.as_ref().ok_or_else(|| {
            format!(
                "cannot roll back {} '{}': no down script (expected '{}')",
                row.version,
                row.name,
                expected_down_name(&migration.up)
            )
        })?;
        let started = Instant::now();
        if !options.dry_run {
            let script = read_script(down_path)?;
            let version = row.version;
            with_transaction(&mut session, dialect, |session| {
                session.script(&script)?;
                session.exec(DELETE_VERSION_SQL, &[Value::Int(version)])?;
                Ok(())
            })
            .map_err(|error| {
                format!(
                    "rollback {} '{}' ({}) failed: {}",
                    row.version,
                    row.name,
                    down_path.display(),
                    error
                )
            })?;
        }
        executed.push(StepReport {
            version: row.version,
            name: row.name.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    let applied_now = applied.len() - executed.len();
    Ok(RunReport {
        dialect: dialect.name().to_string(),
        directory: dir.to_path_buf(),
        direction: Direction::Down,
        executed,
        applied: applied_now,
        pending: migrations.len().saturating_sub(applied_now),
        dry_run: options.dry_run,
    })
}

fn verify_checksums(
    migrations: &[Migration],
    applied: &[AppliedRow],
    allow_modified: bool,
) -> Result<(), String> {
    if allow_modified {
        return Ok(());
    }
    let disk: BTreeMap<i64, &Migration> =
        migrations.iter().map(|item| (item.version, item)).collect();
    for row in applied {
        let Some(migration) = disk.get(&row.version) else {
            continue;
        };
        if migration.checksum != row.checksum {
            return Err(format!(
                "migration {} '{}' was modified after it was applied (checksum mismatch); \
                 revert the change or pass --allow-modified",
                row.version, migration.name
            ));
        }
    }
    Ok(())
}

/// 读取迁移目录。文件名为 `<版本>_<名称>[.<方言>].sql`，回滚脚本再加 `.down`。
fn discover(dir: &Path, dialect: &str) -> Result<Vec<Migration>, String> {
    let entries = fs::read_dir(dir).map_err(|error| {
        format!(
            "cannot read migration directory '{}': {}",
            dir.display(),
            error
        )
    })?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read migration entry: {}", error))?;
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();

    let mut ups: BTreeMap<i64, (PathBuf, String)> = BTreeMap::new();
    let mut downs: BTreeMap<i64, PathBuf> = BTreeMap::new();
    for path in files {
        if path.extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let stem = file_name.trim_end_matches(".sql");
        let (base, is_down) = match stem.strip_suffix(".down") {
            Some(base) => (base, true),
            None => (stem, false),
        };
        let (version_text, raw_name) = base.split_once('_').ok_or_else(|| {
            format!(
                "migration file '{}' must be named <version>_<name>.sql",
                file_name
            )
        })?;
        let version = version_text.parse::<i64>().map_err(|_| {
            format!(
                "migration file '{}' has an invalid version '{}' (must be a positive integer)",
                file_name, version_text
            )
        })?;
        if version <= 0 {
            return Err(format!(
                "migration file '{}' has an invalid version '{}' (must be a positive integer)",
                file_name, version_text
            ));
        }
        let (name, scoped) = split_dialect_suffix(raw_name);
        if let Some(scope) = scoped {
            if scope != dialect {
                continue;
            }
        }
        if name.is_empty() {
            return Err(format!("migration file '{}' has an empty name", file_name));
        }
        if is_down {
            if let Some(previous) = downs.insert(version, path.clone()) {
                return Err(format!(
                    "duplicate down migration version {}: '{}' and '{}'",
                    version,
                    previous.display(),
                    path.display()
                ));
            }
        } else if let Some((previous, _)) = ups.get(&version) {
            return Err(format!(
                "duplicate migration version {}: '{}' and '{}'",
                version,
                previous.display(),
                path.display()
            ));
        } else {
            ups.insert(version, (path, name));
        }
    }

    for version in downs.keys() {
        if !ups.contains_key(version) {
            return Err(format!(
                "down migration {} has no matching up script (<version>_<name>.sql)",
                version
            ));
        }
    }

    let mut migrations = Vec::with_capacity(ups.len());
    for (version, (up, name)) in ups {
        let checksum = checksum_of(&up)?;
        migrations.push(Migration {
            version,
            name,
            up,
            down: downs.remove(&version),
            checksum,
        });
    }
    Ok(migrations)
}

/// 名称末尾的 `.<方言>` 只在该方言下参与迁移（例如 `0001_init.mysql.sql`）。
fn split_dialect_suffix(name: &str) -> (String, Option<&str>) {
    if let Some((base, suffix)) = name.rsplit_once('.') {
        if matches!(suffix, "sqlite" | "mysql" | "postgres" | "mssql") {
            return (base.to_string(), Some(suffix));
        }
    }
    (name.to_string(), None)
}

/// 由升级脚本文件名推导回滚脚本名，用于缺失时的提示。
fn expected_down_name(up: &Path) -> String {
    let name = up
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    format!("{}.down.sql", name.trim_end_matches(".sql"))
}

fn checksum_of(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read '{}': {}", path.display(), error))?;
    let digest = Sha256::digest(&bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        let _ = write!(text, "{:02x}", byte);
    }
    Ok(text)
}

fn read_script(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "cannot read migration script '{}': {}",
            path.display(),
            error
        )
    })?;
    // Windows 编辑器常写入 UTF-8 BOM；BOM 会让部分驱动把首条语句解析失败。
    Ok(match text.strip_prefix('\u{feff}') {
        Some(text) => text.to_string(),
        None => text,
    })
}

fn now_text() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn ensure_table(session: &mut Session, dialect: Dialect) -> Result<(), String> {
    let sql = match dialect {
        Dialect::Sqlite => format!(
            "CREATE TABLE IF NOT EXISTS {} (version INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             checksum TEXT NOT NULL, applied_at TEXT NOT NULL, duration_ms INTEGER NOT NULL)",
            VERSION_TABLE
        ),
        Dialect::MySql => format!(
            "CREATE TABLE IF NOT EXISTS {} (version BIGINT PRIMARY KEY, name VARCHAR(255) NOT NULL, \
             checksum VARCHAR(64) NOT NULL, applied_at VARCHAR(32) NOT NULL, \
             duration_ms BIGINT NOT NULL) ENGINE=InnoDB",
            VERSION_TABLE
        ),
        Dialect::Postgres => format!(
            "CREATE TABLE IF NOT EXISTS {} (version BIGINT PRIMARY KEY, name TEXT NOT NULL, \
             checksum TEXT NOT NULL, applied_at TEXT NOT NULL, duration_ms BIGINT NOT NULL)",
            VERSION_TABLE
        ),
        Dialect::MsSql => format!(
            "IF OBJECT_ID('{0}', 'U') IS NULL CREATE TABLE {0} (version BIGINT PRIMARY KEY, \
             name NVARCHAR(255) NOT NULL, checksum VARCHAR(64) NOT NULL, \
             applied_at VARCHAR(32) NOT NULL, duration_ms BIGINT NOT NULL)",
            VERSION_TABLE
        ),
    };
    session.script(&sql)
}

fn applied_rows(session: &mut Session) -> Result<Vec<AppliedRow>, String> {
    let rows = session.query(SELECT_VERSIONS_SQL)?;
    let mut applied = Vec::with_capacity(rows.len());
    for row in rows {
        let version = row_field(&row, "version")
            .and_then(|value| value.as_int())
            .ok_or_else(|| format!("{} has a row without a numeric version", VERSION_TABLE))?;
        applied.push(AppliedRow {
            version,
            name: row_field(&row, "name")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            checksum: row_field(&row, "checksum")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            applied_at: row_field(&row, "applied_at")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
        });
    }
    Ok(applied)
}

fn with_transaction<F>(session: &mut Session, dialect: Dialect, body: F) -> Result<(), String>
where
    F: FnOnce(&mut Session) -> Result<(), String>,
{
    session.script(begin_sql(dialect))?;
    match body(session) {
        Ok(()) => session.script(commit_sql(dialect)),
        Err(error) => {
            let _ = session.script(rollback_sql(dialect));
            Err(error)
        }
    }
}

fn begin_sql(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => "BEGIN IMMEDIATE",
        Dialect::MySql => "START TRANSACTION",
        Dialect::Postgres => "BEGIN",
        Dialect::MsSql => "BEGIN TRANSACTION",
    }
}

fn commit_sql(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::MySql => "COMMIT",
        Dialect::MsSql => "COMMIT TRANSACTION",
        _ => "COMMIT",
    }
}

fn rollback_sql(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::MySql => "ROLLBACK",
        Dialect::MsSql => "ROLLBACK TRANSACTION",
        _ => "ROLLBACK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rip_migrate_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn discovers_versions_and_down_scripts() {
        let dir = temp_dir("discover");
        write(&dir, "0001_init.sql", "CREATE TABLE a (id INTEGER);\n");
        write(&dir, "0001_init.down.sql", "DROP TABLE a;\n");
        write(&dir, "0002_add_index.sql", "CREATE INDEX i ON a (id);\n");
        write(&dir, "notes.txt", "ignored");
        let migrations = discover(&dir, "sqlite").unwrap();
        assert_eq!(migrations.len(), 2);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[0].name, "init");
        assert!(migrations[0].down.is_some());
        assert!(migrations[1].down.is_none());
        assert_eq!(migrations[0].checksum.len(), 64);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_duplicate_versions() {
        let dir = temp_dir("duplicate");
        write(&dir, "0001_a.sql", "SELECT 1;");
        write(&dir, "1_b.sql", "SELECT 2;");
        let error = discover(&dir, "sqlite").unwrap_err();
        assert!(error.contains("duplicate migration version 1"), "{}", error);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_dialect_specific_scripts() {
        let dir = temp_dir("dialect");
        write(&dir, "0001_init.sql", "SELECT 1;");
        write(&dir, "0002_mysql_only.mysql.sql", "SELECT 2;");
        write(&dir, "0003_pg_only.postgres.sql", "SELECT 3;");
        let sqlite = discover(&dir, "sqlite").unwrap();
        assert_eq!(sqlite.len(), 1);
        let mysql = discover(&dir, "mysql").unwrap();
        assert_eq!(mysql.len(), 2);
        assert_eq!(mysql[1].name, "mysql_only");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_down_without_up() {
        let dir = temp_dir("orphan");
        write(&dir, "0001_init.down.sql", "SELECT 1;");
        let error = discover(&dir, "sqlite").unwrap_err();
        assert!(error.contains("has no matching up script"), "{}", error);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_up_down_and_status_against_sqlite() {
        let dir = temp_dir("sqlite_up");
        write(
            &dir,
            "0001_init.sql",
            "-- 建表\nCREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL);\n",
        );
        write(&dir, "0001_init.down.sql", "DROP TABLE items;\n");
        write(
            &dir,
            "0002_seed.sql",
            "INSERT INTO items (id, name) VALUES (1, 'a');\n",
        );

        let db = dir.join("app.db");
        let dsn = format!("sqlite:{}", db.display());

        let report = up(&dsn, &dir, &Options::default()).unwrap();
        assert_eq!(report.executed.len(), 2, "both migrations apply");
        assert_eq!(report.direction, Direction::Up);
        assert_eq!(report.pending, 0);

        // 再跑一次是幂等的。
        let again = up(&dsn, &dir, &Options::default()).unwrap();
        assert!(again.executed.is_empty());

        let state = status(&dsn, &dir).unwrap();
        assert_eq!(state.applied, 2);
        assert_eq!(state.pending, 0);
        assert!(state.entries.iter().all(|entry| entry.applied_at.is_some()));

        // 缺省回滚一条（0002 没有 down 脚本时必须报错，先补上）。
        write(&dir, "0002_seed.down.sql", "DELETE FROM items;\n");
        let rolled = down(&dsn, &dir, &Options::default()).unwrap();
        assert_eq!(rolled.executed.len(), 1);
        assert_eq!(rolled.executed[0].version, 2);
        assert_eq!(rolled.applied, 1);

        let state = status(&dsn, &dir).unwrap();
        assert_eq!(state.applied, 1);
        assert_eq!(state.pending, 1);

        // --to 0 回滚到空库。
        let options = Options {
            to: Some(0),
            ..Options::default()
        };
        let rolled = down(&dsn, &dir, &options).unwrap();
        assert_eq!(rolled.executed.len(), 1);
        assert_eq!(rolled.applied, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = temp_dir("dry_run");
        write(&dir, "0001_init.sql", "CREATE TABLE dry (id INTEGER);\n");
        let db = dir.join("app.db");
        let dsn = format!("sqlite:{}", db.display());
        let options = Options {
            dry_run: true,
            ..Options::default()
        };
        let report = up(&dsn, &dir, &options).unwrap();
        assert_eq!(report.executed.len(), 1);
        assert!(report.dry_run);
        let state = status(&dsn, &dir).unwrap();
        assert_eq!(state.applied, 0);
        assert_eq!(state.pending, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_modified_migration_scripts() {
        let dir = temp_dir("checksum");
        write(&dir, "0001_init.sql", "CREATE TABLE c1 (id INTEGER);\n");
        let db = dir.join("app.db");
        let dsn = format!("sqlite:{}", db.display());
        up(&dsn, &dir, &Options::default()).unwrap();

        write(&dir, "0001_init.sql", "CREATE TABLE c1 (id INTEGER, name TEXT);\n");
        let error = up(&dsn, &dir, &Options::default()).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{}", error);

        let options = Options {
            allow_modified: true,
            ..Options::default()
        };
        assert!(up(&dsn, &dir, &options).unwrap().executed.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rolls_back_failed_migration_as_a_whole() {
        let dir = temp_dir("rollback");
        write(
            &dir,
            "0001_broken.sql",
            "CREATE TABLE partial (id INTEGER);\nINSERT INTO missing_table VALUES (1);\n",
        );
        let db = dir.join("app.db");
        let dsn = format!("sqlite:{}", db.display());
        let error = up(&dsn, &dir, &Options::default()).unwrap_err();
        assert!(error.contains("0001"), "{}", error);

        let state = status(&dsn, &dir).unwrap();
        assert_eq!(state.applied, 0, "failed migration must not be recorded");
        // 事务回滚后表不应该存在。
        let mut session = Session::open(&dsn).unwrap();
        let rows = session
            .query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'partial'")
            .unwrap();
        assert!(rows.is_empty(), "partial table should be rolled back");
        let _ = fs::remove_dir_all(&dir);
    }
}

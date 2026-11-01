//! SQLite 连接与建表（weimo-ocr 风格：内嵌 bootstrap SQL，不用 sqlx migrate）。

use anyhow::{Context, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};

const BOOTSTRAP_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
  task_id     TEXT PRIMARY KEY,
  user        TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'queued',
  items_count INTEGER NOT NULL DEFAULT 0,
  error_json  TEXT,
  created_ms  INTEGER NOT NULL,
  started_ms  INTEGER,
  finished_ms INTEGER,
  expires_ms  INTEGER
);

CREATE TABLE IF NOT EXISTS dimensions (
  uid           TEXT NOT NULL,
  category      TEXT NOT NULL,
  dim_status    TEXT NOT NULL,
  state         TEXT NOT NULL DEFAULT 'idle',
  fetched_count INTEGER NOT NULL DEFAULT 0,
  total         INTEGER,
  next_start    INTEGER NOT NULL DEFAULT 0,
  fetched_ms    INTEGER,
  attempts      INTEGER NOT NULL DEFAULT 0,
  next_sync_ms  INTEGER,
  last_error    TEXT,
  updated_ms    INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (uid, category, dim_status)
);

CREATE TABLE IF NOT EXISTS task_dims (
  task_id    TEXT NOT NULL,
  uid        TEXT NOT NULL,
  category   TEXT NOT NULL,
  dim_status TEXT NOT NULL,
  state      TEXT NOT NULL DEFAULT 'waiting',
  items      INTEGER NOT NULL DEFAULT 0,
  error      TEXT,
  PRIMARY KEY (task_id, category, dim_status),
  FOREIGN KEY (task_id) REFERENCES tasks(task_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS records (
  uid         TEXT NOT NULL,
  category    TEXT NOT NULL,
  dim_status  TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  title       TEXT NOT NULL DEFAULT '',
  alt_title   TEXT NOT NULL DEFAULT '',
  my_rating   INTEGER,
  date        TEXT NOT NULL DEFAULT '',
  comment     TEXT NOT NULL DEFAULT '',
  subject_url TEXT NOT NULL DEFAULT '',
  cover       TEXT NOT NULL DEFAULT '',
  comment_id  TEXT NOT NULL DEFAULT '',
  intro       TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (uid, category, dim_status, subject_id)
);

CREATE TABLE IF NOT EXISTS users (
  uid        TEXT PRIMARY KEY,
  username   TEXT NOT NULL DEFAULT '',
  avatar     TEXT NOT NULL DEFAULT '',
  updated_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS subscriptions (
  uid          TEXT PRIMARY KEY,
  enabled      INTEGER NOT NULL DEFAULT 1,
  rss_interval_secs  INTEGER NOT NULL,
  full_interval_secs INTEGER NOT NULL,
  next_rss_ms  INTEGER NOT NULL,
  next_full_ms INTEGER NOT NULL,
  last_rss_ms  INTEGER,
  last_full_ms INTEGER,
  last_guid    TEXT NOT NULL DEFAULT '',
  rss_attempts INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  created_ms   INTEGER NOT NULL,
  updated_ms   INTEGER NOT NULL DEFAULT 0
);

-- 旧库的文件导出元数据表（数据现已直接入库，无迁移、直接丢弃）
DROP TABLE IF EXISTS task_files;

-- 条目结构化元信息缓存（rexxar 富集；主键不含 uid：跨用户共享一份，
-- 详见《条目元信息富集方案.md》）。raw_json 留底 rexxar 原始响应，
-- 映射规则修订无需重新请求豆瓣。
CREATE TABLE IF NOT EXISTS subjects (
  category       TEXT NOT NULL,
  subject_id     TEXT NOT NULL,
  title          TEXT NOT NULL DEFAULT '',
  original_title TEXT NOT NULL DEFAULT '',
  year           INTEGER,
  aka_json       TEXT NOT NULL DEFAULT '[]',
  url            TEXT NOT NULL DEFAULT '',
  cover_url      TEXT NOT NULL DEFAULT '',
  rating_value   REAL,
  rating_count   INTEGER,
  meta_json      TEXT NOT NULL DEFAULT '{}',
  raw_json       TEXT NOT NULL DEFAULT '',
  status         TEXT NOT NULL DEFAULT 'ok',
  source         TEXT NOT NULL DEFAULT 'rexxar',
  attempts       INTEGER NOT NULL DEFAULT 0,
  next_retry_ms  INTEGER,
  fetched_ms     INTEGER NOT NULL,
  PRIMARY KEY (category, subject_id)
);

-- 富集队列（与 dimensions 同构的"DB 即队列"）；主键保证不同用户
-- 触发同一条目只排一个 job。
CREATE TABLE IF NOT EXISTS subject_jobs (
  category    TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'queued',
  attempts    INTEGER NOT NULL DEFAULT 0,
  next_run_ms INTEGER,
  priority    INTEGER NOT NULL DEFAULT 1,
  created_ms  INTEGER NOT NULL,
  updated_ms  INTEGER NOT NULL,
  last_error  TEXT,
  PRIMARY KEY (category, subject_id)
);

CREATE INDEX IF NOT EXISTS idx_task_dims_dim ON task_dims(uid, category, dim_status);
CREATE INDEX IF NOT EXISTS idx_dimensions_due ON dimensions(state, next_sync_ms);
CREATE INDEX IF NOT EXISTS idx_records_dim_date ON records(uid, category, dim_status, date DESC);
CREATE INDEX IF NOT EXISTS idx_records_subject ON records(category, subject_id);
CREATE INDEX IF NOT EXISTS idx_subjects_retry ON subjects(status, next_retry_ms);
CREATE INDEX IF NOT EXISTS idx_subject_jobs_due ON subject_jobs(state, next_run_ms);

CREATE TABLE IF NOT EXISTS app_preferences (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- 中心查询的短租约只约束普通抓取，不占用豆瓣重试次数或退避时间。
CREATE TABLE IF NOT EXISTS subject_cache_checks (
  category TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  next_check_ms INTEGER NOT NULL DEFAULT 0,
  hold_until_ms INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (category, subject_id)
);
"#;

pub async fn connect(db_path: &std::path::Path) -> Result<SqlitePool> {
    // WAL 使只读缓存查询不阻塞网页同步的提交；模式随数据库持久化。
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("连接数据库失败: {}", db_path.display()))?;

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .context("启用外键失败")?;

    for statement in BOOTSTRAP_SCHEMA
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .with_context(|| format!("建表失败: {statement}"))?;
    }
    ensure_column(&pool, "tasks", "source", "TEXT NOT NULL DEFAULT 'manual'").await?;
    // 获取方式标记：rexxar = 权威富集；intro = 重试耗尽后的 records.intro
    // 合成兜底行（无豆瓣评分，可经 refresh / requeue stale 升级回 rexxar）
    ensure_column(
        &pool,
        "subjects",
        "source",
        "TEXT NOT NULL DEFAULT 'rexxar'",
    )
    .await?;
    ensure_column(&pool, "dimensions", "staging", "INTEGER NOT NULL DEFAULT 0").await?;
    // A separate durable staging table keeps the last published snapshot readable.
    sqlx::query("CREATE TABLE IF NOT EXISTS record_staging AS SELECT * FROM records WHERE 0")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS idx_record_staging_key ON record_staging(uid, category, dim_status, subject_id)")
        .execute(&pool).await?;
    init_cache_versions(&pool).await?;
    Ok(pool)
}

/// 幂等加列（CREATE TABLE IF NOT EXISTS 不会更新已有表结构）
async fn ensure_column(pool: &SqlitePool, table: &str, column: &str, ddl: &str) -> Result<()> {
    let sql = format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'");
    let has: i64 = sqlx::query_scalar(&sql)
        .fetch_one(pool)
        .await
        .with_context(|| format!("检查 {table}.{column} 列失败"))?;
    if has == 0 {
        let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}");
        sqlx::query(&alter)
            .execute(pool)
            .await
            .with_context(|| format!("加列失败: {alter}"))?;
    }
    Ok(())
}

/// Versions are updated in the same SQLite transaction as the owning data, including
/// RSS writes. The database identity also invalidates browser caches after replacement.
async fn init_cache_versions(pool: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS cache_versions (scope TEXT PRIMARY KEY, revision INTEGER NOT NULL DEFAULT 0)")
        .execute(pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS cache_identity (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL)")
        .execute(pool).await?;
    sqlx::query("INSERT OR IGNORE INTO cache_identity VALUES (1, lower(hex(randomblob(16))))")
        .execute(pool)
        .await?;
    // Content equality matters: re-observing the same profile or record is not a change.
    for (table, scope, fields) in [
        (
            "records",
            "{row}.uid || ':' || {row}.category || ':' || {row}.dim_status",
            vec![
                "title",
                "alt_title",
                "my_rating",
                "date",
                "comment",
                "subject_url",
                "cover",
                "comment_id",
                "intro",
            ],
        ),
        (
            "users",
            "'profile:' || {row}.uid",
            vec!["username", "avatar"],
        ),
        (
            "subjects",
            "'subjects:' || {row}.category",
            vec![
                "title",
                "original_title",
                "year",
                "aka_json",
                "url",
                "cover_url",
                "rating_value",
                "rating_count",
                "meta_json",
                "source",
                "status",
            ],
        ),
        // Publish/availability changes matter even for an empty collection. A refresh
        // keeps fetched_ms until publication; queued/running transitions do not invalidate.
        (
            "dimensions",
            "{row}.uid || ':' || {row}.category || ':' || {row}.dim_status",
            vec!["fetched_ms"],
        ),
    ] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let row = if event == "DELETE" { "OLD" } else { "NEW" };
            let scope = scope.replace("{row}", row);
            let condition = if event == "UPDATE" && table == "dimensions" {
                "WHEN (OLD.fetched_ms IS NULL) != (NEW.fetched_ms IS NULL)".to_string()
            } else if event == "UPDATE" {
                format!(
                    "WHEN {}",
                    fields
                        .iter()
                        .map(|f| format!("OLD.{f} IS NOT NEW.{f}"))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            } else {
                String::new()
            };
            let sql = format!("CREATE TRIGGER IF NOT EXISTS cache_{table}_{event} AFTER {event} ON {table} {condition}
                BEGIN INSERT INTO cache_versions(scope, revision) VALUES ({scope}, 1)
                ON CONFLICT(scope) DO UPDATE SET revision=revision+1; END");
            sqlx::query(&sql).execute(pool).await?;
        }
    }
    Ok(())
}

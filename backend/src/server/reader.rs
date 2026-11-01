//! Local read-only queries shared by the MCP executable and other library consumers.
//! No AppState, network clients, workers, schema initialization or filesystem exports.

use anyhow::{bail, ensure, Result};
use chrono::NaiveDate;
use serde_json::{json, Value};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{db, dims, subject};
use crate::crawler::{Category, Record, Status};

const MAX_CANDIDATES: usize = 50_000;
pub const FACETS: &[&str] = &[
    "genres",
    "countries",
    "directors",
    "press",
    "authors",
    "translators",
    "singers",
];

#[derive(Clone, Default)]
pub struct MarkFilter {
    pub uid: String,
    pub category: Option<String>,
    pub status: Option<String>,
    pub query: Option<String>,
    pub my_rating_min: Option<u8>,
    pub my_rating_max: Option<u8>,
    pub marked_from: Option<String>,
    pub marked_to: Option<String>,
    pub release_year_min: Option<i64>,
    pub release_year_max: Option<i64>,
    /// Exact facet values, AND across filters; metadata uses the same intro fallback as the UI.
    pub facets: Vec<(String, String)>,
}

impl MarkFilter {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.uid.trim().is_empty(),
            "uid 不能为空；请先调用 list_users"
        );
        if let Some(c) = &self.category {
            ensure!(
                Category::from_key(c).is_some(),
                "category 须为 movie/book/music"
            );
        }
        if let Some(s) = &self.status {
            ensure!(Status::from_key(s).is_some(), "status 须为 collect/do/wish");
        }
        for rating in [self.my_rating_min, self.my_rating_max]
            .into_iter()
            .flatten()
        {
            ensure!(
                (1..=5).contains(&rating),
                "我的评分须为 1–5 星，豆瓣公共评分为另一字段"
            );
        }
        ensure!(
            !matches!((self.my_rating_min, self.my_rating_max), (Some(a), Some(b)) if a > b),
            "评分下限不能大于上限"
        );
        for date in [&self.marked_from, &self.marked_to].into_iter().flatten() {
            let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")?;
            ensure!(
                parsed.format("%Y-%m-%d").to_string() == *date,
                "日期须为 YYYY-MM-DD"
            );
        }
        ensure!(
            !matches!((&self.marked_from, &self.marked_to), (Some(a), Some(b)) if a > b),
            "日期下限不能大于上限"
        );
        for year in [self.release_year_min, self.release_year_max]
            .into_iter()
            .flatten()
        {
            ensure!((1..=9999).contains(&year), "作品年份须为 1–9999");
        }
        ensure!(
            !matches!((self.release_year_min, self.release_year_max), (Some(a), Some(b)) if a > b),
            "作品年份下限不能大于上限"
        );
        ensure!(self.facets.len() <= 12, "最多指定 12 个维度条件");
        for (name, value) in &self.facets {
            ensure!(
                FACETS.contains(&name.as_str()) && !value.is_empty(),
                "非法维度条件；支持 {}",
                FACETS.join("/")
            );
        }
        ensure!(
            self.query.as_ref().is_none_or(|q| q.chars().count() <= 200),
            "关键词最多 200 字符"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct Reader {
    pool: SqlitePool,
    allowed_uids: Option<Arc<BTreeSet<String>>>,
}

struct Selection {
    items: Vec<Value>,
    sync: Vec<Value>,
}

impl Reader {
    pub async fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            pool: db::connect_read_only(path).await?,
            allowed_uids: None,
        })
    }

    /// Restrict all reader entry points, including discovery and subject lookup.
    /// Empty scope is rejected; it must never mean unrestricted access.
    pub fn restrict_to(mut self, uids: Vec<String>) -> Result<Self> {
        ensure!(
            !uids.is_empty() && uids.len() <= 100,
            "开放用户范围须为 1–100 个 uid"
        );
        ensure!(
            uids.iter()
                .all(|u| !u.trim().is_empty() && u.len() <= 200 && u.trim() == u),
            "开放 uid 无效"
        );
        let uids: BTreeSet<String> = uids.into_iter().collect();
        if let Some(existing) = &self.allowed_uids {
            ensure!(uids.is_subset(existing), "不能扩大已有查询权限");
        }
        self.allowed_uids = Some(Arc::new(uids));
        Ok(self)
    }

    fn check_uid(&self, uid: &str) -> Result<()> {
        ensure!(
            self.allowed_uids
                .as_ref()
                .is_none_or(|uids| uids.contains(uid)),
            "该 uid 不在服务开放范围内；请调用 list_users 查看可用用户"
        );
        Ok(())
    }

    fn scope_sql(&self, query: &mut QueryBuilder<'_, Sqlite>, column: &str) {
        if let Some(uids) = &self.allowed_uids {
            query.push(" AND ").push(column).push(" IN (");
            let mut values = query.separated(",");
            for uid in uids.iter() {
                values.push_bind(uid.clone());
            }
            query.push(")");
        }
    }

    pub async fn list_users(&self, limit: usize, offset: usize) -> Result<Value> {
        validate_page(limit, offset)?;
        // One SQL statement gives count and page a consistent snapshot, including an empty page.
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH all_uids(uid) AS (
            SELECT uid FROM users UNION SELECT uid FROM records
            UNION SELECT uid FROM subscriptions UNION SELECT user FROM tasks
        ), uids AS (SELECT uid FROM all_uids WHERE 1=1",
        );
        self.scope_sql(&mut query, "uid");
        query.push("), page AS (
            SELECT u.uid, us.username,
                (SELECT COUNT(*) FROM records r WHERE r.uid=u.uid) AS records,
                (SELECT MAX(fetched_ms) FROM dimensions d WHERE d.uid=u.uid) AS synced_ms
            FROM uids u LEFT JOIN users us ON us.uid=u.uid ORDER BY u.uid LIMIT ")
            .push_bind(limit as i64).push(" OFFSET ").push_bind(offset as i64)
            .push(") SELECT page.*, (SELECT COUNT(*) FROM uids) AS total FROM (SELECT 1) LEFT JOIN page ON 1=1");
        let rows = query.build().fetch_all(&self.pool).await?;
        let total: i64 = rows[0].get("total");
        let items: Vec<Value> = rows.iter().filter_map(|r| {
            let uid: Option<String> = r.get("uid");
            uid.map(|uid| json!({"uid":uid, "username":r.get::<Option<String>,_>("username"),
                "records":r.get::<i64,_>("records"), "last_synced_at":timestamp(r.get("synced_ms"))}))
        }).collect();
        Ok(page(items, total as usize, limit, offset))
    }

    pub async fn get_subject(&self, category: &str, subject_id: &str) -> Result<Value> {
        ensure!(
            Category::from_key(category).is_some(),
            "category 须为 movie/book/music"
        );
        ensure!(!subject_id.is_empty(), "subject_id 不能为空");
        if self.allowed_uids.is_some() {
            let mut query = QueryBuilder::<Sqlite>::new("SELECT EXISTS(SELECT 1 FROM records r
                JOIN dimensions d ON d.uid=r.uid AND d.category=r.category AND d.dim_status=r.dim_status
                WHERE (d.state IN ('ok','running') OR d.fetched_ms IS NOT NULL) AND r.category=");
            query
                .push_bind(category.to_owned())
                .push(" AND r.subject_id=")
                .push_bind(subject_id.to_owned());
            self.scope_sql(&mut query, "r.uid");
            query.push(")");
            let allowed: bool = query.build_query_scalar().fetch_one(&self.pool).await?;
            ensure!(allowed, "条目不在开放范围内或尚无同步数据");
        }
        let mut view =
            subject::get_subject_view_from_pool(&self.pool, category, subject_id).await?;
        remove_cover(&mut view);
        Ok(json!({"subject":view, "local_only":true}))
    }

    pub async fn search_marks(
        &self,
        filter: &MarkFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Value> {
        validate_page(limit, offset)?;
        let selected = self.select(filter).await?;
        let total = selected.items.len();
        let items = selected
            .items
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        let mut result = page(items, total, limit, offset);
        result["sync"] = json!(selected.sync);
        Ok(result)
    }

    pub async fn get_stats(&self, filter: &MarkFilter) -> Result<Value> {
        let selected = self.select(filter).await?;
        let mut categories = BTreeMap::from([
            ("movie".to_owned(), 0),
            ("book".to_owned(), 0),
            ("music".to_owned(), 0),
        ]);
        let mut statuses = BTreeMap::new();
        let mut ratings = BTreeMap::new();
        let mut years = BTreeMap::new();
        for item in &selected.items {
            increment(
                &mut categories,
                item["category"].as_str().unwrap_or_default(),
            );
            increment(&mut statuses, item["status"].as_str().unwrap_or_default());
            let rating = item["my_rating"]
                .as_u64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unrated".into());
            increment(&mut ratings, &rating);
            let date = item["marked_at"].as_str().unwrap_or_default();
            let year = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()
                .map(|d| d.format("%Y").to_string())
                .unwrap_or_else(|| "unknown".into());
            increment(&mut years, &year);
        }
        Ok(
            json!({"total":selected.items.len(), "by_category":categories, "by_status":statuses,
            "my_rating_distribution":ratings, "by_marked_year":years, "sync":selected.sync, "local_only":true}),
        )
    }

    pub async fn list_facets(
        &self,
        filter: &MarkFilter,
        facet: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Value> {
        validate_page(limit, offset)?;
        ensure!(
            FACETS.contains(&facet),
            "非法维度；支持 {}",
            FACETS.join("/")
        );
        let selected = self.select(filter).await?;
        let mut counts = BTreeMap::new();
        for item in &selected.items {
            let values: BTreeSet<&str> = item["facets"][facet]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            for value in values {
                increment(&mut counts, value);
            }
        }
        let mut counts: Vec<_> = counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let total = counts.len();
        let values = counts
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(value, count)| json!({"value":value,"count":count}))
            .collect();
        let mut result = page(values, total, limit, offset);
        result["facet"] = json!(facet);
        result["matched_marks"] = json!(selected.items.len());
        result["sync"] = json!(selected.sync);
        Ok(result)
    }

    async fn select(&self, filter: &MarkFilter) -> Result<Selection> {
        self.check_uid(&filter.uid)?;
        filter.validate()?;
        // A short read transaction pins metadata and marks to the same committed snapshot.
        // Finish all DB work before formatting or returning data to a slow MCP client.
        let mut tx = self.pool.begin().await?;
        let dimensions = sqlx::query("SELECT category, dim_status, state, fetched_ms FROM dimensions WHERE uid=? ORDER BY category, dim_status")
            .bind(&filter.uid).fetch_all(&mut *tx).await?;
        let sync: Vec<Value> = dimensions.iter().filter(|r| {
            filter.category.as_ref().is_none_or(|c| c == r.get::<&str,_>("category")) &&
            filter.status.as_ref().is_none_or(|s| s == r.get::<&str,_>("dim_status"))
        }).map(|r| json!({"category":r.get::<&str,_>("category"), "status":r.get::<&str,_>("dim_status"),
            "state":r.get::<&str,_>("state"), "last_synced_at":timestamp(r.get("fetched_ms"))})).collect();
        if sync.is_empty() {
            bail!("指定用户/类别/状态尚无同步数据；请先在 App 同步，或调用 list_users 确认 uid");
        }
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT
            r.category, r.dim_status, r.subject_id, r.title AS mark_title, r.alt_title, r.my_rating,
            r.date, r.comment, r.subject_url, r.intro,
            s.subject_id AS enriched_id, s.title, s.original_title, s.year, s.aka_json,
            s.cover_url, s.rating_value, s.rating_count, s.meta_json, s.status, s.fetched_ms, s.url, s.source
            FROM records r JOIN dimensions d ON d.uid=r.uid AND d.category=r.category AND d.dim_status=r.dim_status
            LEFT JOIN subjects s ON s.category=r.category AND s.subject_id=r.subject_id
            WHERE (d.state IN ('ok','running') OR d.fetched_ms IS NOT NULL) AND r.uid=");
        q.push_bind(&filter.uid);
        for (column, value) in [
            ("r.category", &filter.category),
            ("r.dim_status", &filter.status),
        ] {
            if let Some(v) = value {
                q.push(" AND ").push(column).push("=").push_bind(v);
            }
        }
        if let Some(v) = filter.my_rating_min {
            q.push(" AND r.my_rating>=").push_bind(i64::from(v));
        }
        if let Some(v) = filter.my_rating_max {
            q.push(" AND r.my_rating<=").push_bind(i64::from(v));
        }
        if let Some(v) = &filter.marked_from {
            q.push(" AND r.date>=").push_bind(v);
        }
        if let Some(v) = &filter.marked_to {
            q.push(" AND r.date<>'' AND r.date<=").push_bind(v);
        }
        if let Some(v) = filter.query.as_ref().filter(|s| !s.is_empty()) {
            q.push(" AND instr(lower(r.title || char(10) || r.alt_title || char(10) || r.comment), lower(").push_bind(v).push("))>0");
        }
        q.push(" ORDER BY r.date DESC, r.category, r.dim_status, r.subject_id LIMIT ")
            .push_bind((MAX_CANDIDATES + 1) as i64);
        let rows = q.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        ensure!(
            rows.len() <= MAX_CANDIDATES,
            "候选标记超过 {MAX_CANDIDATES} 条，请缩小类别、状态、标记日期或关键词范围"
        );
        let mut items = Vec::new();
        for r in rows {
            let category: String = r.get("category");
            let Some(category_enum) = Category::from_key(&category) else {
                continue;
            };
            let record = Record {
                category: category.clone(),
                status: r.get("dim_status"),
                subject_id: r.get("subject_id"),
                title: r.get("mark_title"),
                alt_title: r.get("alt_title"),
                my_rating: r.get::<Option<i64>, _>("my_rating").map(|v| v as u8),
                date: r.get("date"),
                comment: r.get("comment"),
                subject_url: r.get("subject_url"),
                intro: r.get("intro"),
                cover: String::new(),
                comment_id: String::new(),
            };
            let mut view = if r.get::<Option<String>, _>("enriched_id").is_some() {
                subject::subject_view(&category, &record.subject_id, &r)
            } else {
                subject::intro_fallback_view(&record)
            };
            let year = view["year"].as_i64();
            if filter
                .release_year_min
                .is_some_and(|min| year.is_none_or(|y| y < min))
                || filter
                    .release_year_max
                    .is_some_and(|max| year.is_none_or(|y| y > max))
            {
                continue;
            }
            let facets = dims::project_dims(category_enum, &view);
            if !filter.facets.iter().all(|(key, value)| {
                facets[key]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(value)))
            }) {
                continue;
            }
            remove_cover(&mut view);
            // Search returns a compact subject summary; get_subject supplies the full metadata.
            items.push(json!({"category":category, "status":record.status, "subject_id":record.subject_id,
                "title":record.title, "alt_title":record.alt_title, "my_rating":record.my_rating,
                "marked_at":record.date, "comment":record.comment, "url":record.subject_url,
                "release_year":view["year"], "douban_rating":view["rating"], "metadata_source":view["source"],
                "metadata_fetched_at":view["fetched_at"], "facets":facets}));
        }
        Ok(Selection { items, sync })
    }
}

fn timestamp(ms: Option<i64>) -> Option<String> {
    ms.and_then(chrono::DateTime::from_timestamp_millis)
        .map(|d| d.to_rfc3339())
}
fn remove_cover(view: &mut Value) {
    if let Some(o) = view.as_object_mut() {
        o.remove("cover");
    }
}
fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_owned()).or_default() += 1;
}
fn validate_page(limit: usize, offset: usize) -> Result<()> {
    ensure!((1..=100).contains(&limit), "limit 须为 1–100");
    ensure!(offset <= 1_000_000, "offset 须为 0–1000000");
    Ok(())
}
fn page(items: Vec<Value>, total: usize, limit: usize, offset: usize) -> Value {
    let next = offset.saturating_add(items.len());
    json!({"items":items, "total":total, "limit":limit, "offset":offset,
        "next_offset":if next < total {Some(next)} else {None}, "local_only":true})
}

//! 条目维度数据（标签树的纯数据源）——按维度投影每条标记的元信息
//! （条目维度走 dims 对象，标记级字段 my_rating 平铺在 item 顶层），
//! 不做任何聚合/过滤/排序计数/长尾合并等展示决策（那些归前端
//! facet-engine）。数据合成口径与列表页一致：subjects 已富集用
//! rexxar meta，未富集回退 intro 启发式（load_views 共用管线）。
//! items 按 date DESC, category, subject_id 稳定序输出——前端保序筛出当页 id 集，
//! marks 端点按 ids 顺序返回当页数据，两侧顺序权威在前端。

use anyhow::Result;
use serde_json::{json, Map, Value};
use sqlx::Row;

use crate::crawler::{Category, Record, Status};

use super::covers::cover_proxy_url;
use super::error::AppResult;
use super::state::AppState;
use super::subject;

/// 读该维度全部标记行，与 read_marks 同序（date DESC, category, subject_id）——
/// 顺序是 dims 端点与前端分页切片的排序契约
async fn load_all_records(
    state: &AppState,
    uid: &str,
    category: Category,
    status: Status,
) -> Result<Vec<Record>> {
    let rows = sqlx::query(
        "SELECT subject_id, title, alt_title, my_rating, date, comment,
                subject_url, cover, comment_id, intro
         FROM records
         WHERE uid = ? AND category = ? AND dim_status = ?
         ORDER BY date DESC, category, subject_id",
    )
    .bind(uid)
    .bind(category.key())
    .bind(status.key())
    .fetch_all(&state.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Record {
            category: category.key().to_string(),
            status: status.key().to_string(),
            title: r.get("title"),
            alt_title: r.get("alt_title"),
            my_rating: r.get::<Option<i64>, _>("my_rating").map(|v| v as u8),
            date: r.get("date"),
            comment: r.get("comment"),
            subject_url: r.get("subject_url"),
            subject_id: r.get("subject_id"),
            cover: cover_proxy_url(&r.get::<String, _>("cover")),
            comment_id: r.get("comment_id"),
            intro: r.get("intro"),
        })
        .collect())
}

/// 全量条目视图：subjects 批查（已富集用 rexxar，否则 intro 合成），
/// 按 400 一段分批以避开 SQLite 绑定参数上限
async fn load_views(
    state: &AppState,
    category: Category,
    records: &[Record],
) -> Result<Vec<Value>> {
    let mut views = Vec::with_capacity(records.len());
    for chunk in records.chunks(400) {
        views.extend(subject::load_subject_views(state, category.key(), chunk).await?);
    }
    Ok(views)
}

/// 条目视图 → 维度投影：只取各维度的原始字段（数组原文/布尔/年份），
/// 不派生（decade、is_tv 中文化由前端做）、不清洗展示（空值原样，
/// 前端自行跳过）。维度数组在视图的类别节点下（view[category]），
/// year 在顶层（rexxar 与 intro 兜底两侧行状一致）。
pub(super) fn project_dims(category: Category, view: &Value) -> Value {
    let node = view.get(category.key()).cloned().unwrap_or(json!({}));
    let mut dims = Map::new();
    for key in [
        "genres",
        "countries",
        "directors",
        "press",
        "authors",
        "translators",
        "singers",
    ] {
        if let Some(arr) = node.get(key).and_then(Value::as_array) {
            dims.insert(key.to_string(), Value::Array(arr.clone()));
        }
    }
    // is_tv 为 null（intro 兜底）时缺省——前端映射「未分类」
    if let Some(is_tv) = node.get("is_tv").and_then(Value::as_bool) {
        dims.insert("is_tv".to_string(), Value::Bool(is_tv));
    }
    // year 为 null 时缺省——前端 decade 派生跳过
    if let Some(year) = view.get("year").and_then(Value::as_i64) {
        dims.insert("year".to_string(), json!(year));
    }
    Value::Object(dims)
}

/// GET /users/{uid}/subject-dims：该维度全部条目的维度数据（标签树/
/// 筛选的唯一数据源）。维度未同步与 marks 同语义返回
/// DIMENSION_NOT_CACHED。
pub async fn subject_dims(
    state: &AppState,
    uid: &str,
    category: Category,
    status: Status,
) -> AppResult<Value> {
    super::service::ensure_dimension_cached(state, uid, category, status).await?;
    let records = load_all_records(state, uid, category, status).await?;
    let views = load_views(state, category, &records).await?;
    let items: Vec<Value> = records
        .iter()
        .zip(&views)
        .map(|(record, view)| {
            json!({
                "id": record.subject_id,
                "date": record.date,
                // 标记级字段（用户自己的评分，非条目维度）：前端「全部
                // 标记」导航子节点按它计数，随维度投影一并下发
                "my_rating": record.my_rating,
                // 富集就绪标记（source=rexxar；intro 兜底未就绪）：前端
                // 侧边栏「已拉取详情 x/y」进度在投影上统计——富集逐条
                // 落库 bump subjects revision → dims 版本变 → 投影重拉，
                // 进度随既有版本流自动更新，零额外请求
                "enriched": view.get("source") == Some(&json!("rexxar")),
                "dims": project_dims(category, view),
            })
        })
        .collect();
    Ok(json!({
        "user": uid,
        "category": category.key(),
        "status": status.key(),
        "total": items.len(),
        "items": items,
    }))
}

/// marks 按 id 集取数（ids 模式）：WHERE IN 查询后按传入顺序重排——
/// 前端 dims 保序筛出的当页 id 集是顺序权威。返回 (标记行, 命中数)。
/// ids 由路由层校验非空且限量。
pub async fn read_marks_by_ids(
    state: &AppState,
    uid: &str,
    category: Category,
    status: Status,
    ids: &[String],
) -> AppResult<(Vec<Record>, usize)> {
    use super::error::AppError;
    super::service::ensure_dimension_cached(state, uid, category, status).await?;
    let fetched = service_read_by_ids(state, uid, category, status, ids).await?;
    // 按传入顺序重排（IN 查询不保序）
    let by_id: std::collections::HashMap<String, Record> = fetched
        .into_iter()
        .map(|r| (r.subject_id.clone(), r))
        .collect();
    let items: Vec<Record> = ids.iter().filter_map(|id| by_id.get(id).cloned()).collect();
    if items.len() != ids.len() {
        return Err(AppError::InvalidArgument(
            "ids 中存在不属于该维度的条目".to_string(),
        ));
    }
    let total = items.len();
    Ok((items, total))
}

async fn service_read_by_ids(
    state: &AppState,
    uid: &str,
    category: Category,
    status: Status,
    ids: &[String],
) -> Result<Vec<Record>> {
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT subject_id, title, alt_title, my_rating, date, comment,
                subject_url, cover, comment_id, intro
         FROM records
         WHERE uid = ? AND category = ? AND dim_status = ?
           AND subject_id IN ({placeholders})"
    );
    let mut q = sqlx::query(&sql)
        .bind(uid)
        .bind(category.key())
        .bind(status.key());
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(&state.pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| Record {
            category: category.key().to_string(),
            status: status.key().to_string(),
            title: r.get("title"),
            alt_title: r.get("alt_title"),
            my_rating: r.get::<Option<i64>, _>("my_rating").map(|v| v as u8),
            date: r.get("date"),
            comment: r.get("comment"),
            subject_url: r.get("subject_url"),
            subject_id: r.get("subject_id"),
            cover: cover_proxy_url(&r.get::<String, _>("cover")),
            comment_id: r.get("comment_id"),
            intro: r.get("intro"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_dims_copies_raw_fields() {
        let view = json!({
            "title": "浮城谜事",
            "year": 2012,
            "movie": {
                "is_tv": false,
                "genres": ["剧情", "犯罪"],
                "countries": ["中国大陆", "法国"],
                "directors": ["娄烨"],
                "actors": ["郝蕾", "秦昊"],           // 非维度字段不投影
                "intro": "……长文本……",                // 展示字段不投影
            },
        });
        let dims = project_dims(Category::Movie, &view);
        assert_eq!(dims["genres"], json!(["剧情", "犯罪"]));
        assert_eq!(dims["countries"], json!(["中国大陆", "法国"]));
        assert_eq!(dims["directors"], json!(["娄烨"]));
        assert_eq!(dims["is_tv"], json!(false));
        assert_eq!(dims["year"], json!(2012));
        assert!(dims.get("actors").is_none());
        assert!(dims.get("intro").is_none());
        // intro 兜底视图（稀疏字段）：缺字段缺省、year null 缺省
        let sparse = json!({"year": null, "movie": {"genres": ["剧情"]}});
        let dims = project_dims(Category::Movie, &sparse);
        assert_eq!(dims["genres"], json!(["剧情"]));
        assert!(dims.get("is_tv").is_none());
        assert!(dims.get("year").is_none());
    }
}

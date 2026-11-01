//! 服务器集成测试——weimo 范式：tower oneshot 直接打 Router，进程内完成，
//! 不起端口、不碰真实豆瓣（经 FakeFetcher 注入脱敏后的真实页面 fixture）。

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;
use weimo_douban::crawler::{
    parse_feed, parse_items, parse_profile, parse_total, Category, Record, Status,
};
use weimo_douban::server::{
    config::ServerConfig,
    db,
    fetcher::{DoubanFetcher, FetchError},
    routes::build_app,
    service,
    state::AppState,
    subject, worker,
};

// --------------------------------------------------------------------------- //
// 基础设施
// --------------------------------------------------------------------------- //

fn test_config(dir: &TempDir, token: &str) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".to_string(),
        data_dir: dir.path().to_path_buf(),
        token: token.to_string(),
        douban_cookie: String::new(),
        cors_origins: String::new(),
        cache_ttl_secs: 86_400,
        crawl_min_interval_secs: 0.0, // 测试不限速
        circuit_threshold: 5,
        circuit_cooldown_secs: 600,
        task_retention_days: 30,
        queue_limit: 100,
        sub_rss_interval_secs: 600,
        sub_full_interval_secs: 86_400,
        cover_concurrency: 4,
        subject_enrich: true,
        subject_ttl_secs: 2_592_000,
        subject_min_interval_secs: 0.0, // 测试不限速
        subject_category_cooldown_secs: 300,
        subject_cache_serve: false,
        subject_cache_url: None,
        desktop_app: false,
        // oneshot 无真实连接，IP 回退到 loopback 且全测试共享；调大避免互扰
        rate_per_minute: 10_000_000,
    }
}

async fn setup() -> (TempDir, AppState) {
    setup_with_token("").await
}

async fn setup_with_token(token: &str) -> (TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(&dir, token);
    let pool = db::connect(&config.db_path()).await.unwrap();
    let state = AppState::new(config, pool);
    (dir, state)
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap()
}

/// 按 URL（域名 + start 偏移）返回对应的脱敏真实页面
fn fixture_fetcher() -> FakeFetcher<impl Fn(&str) -> Result<String, FetchError>> {
    FakeFetcher(|url: &str| {
        if url.contains("/feed/people/") {
            return Ok(fixture("feed.xml"));
        }
        if url.contains("m.douban.com/rexxar") {
            // 已知条目回真实 fixture，其余按类别合成最小合法响应
            let id = url.rsplit('/').next().unwrap();
            return Ok(match id {
                "1292052" => fixture("rexxar_movie.json"),
                "38386470" => fixture("rexxar_book.json"),
                "3221720" => fixture("rexxar_music.json"),
                _ => {
                    let title = format!("T-{id}");
                    format!(
                        r#"{{"title": "{title}", "pubdate": ["1999-1"],
                              "rating": {{"value": 8.0, "count": 10}}}}"#
                    )
                }
            });
        }
        let start: usize = url
            .split("start=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let page = start / 15 + 1;
        if url.contains("music.douban.com") {
            Ok(fixture(&format!("music_collect_p{page}.html")))
        } else if url.contains("book.douban.com") {
            Ok(fixture(&format!("book_collect_p{page}.html")))
        } else {
            Err(FetchError::NotFound)
        }
    })
}

struct FakeFetcher<F>(F);

impl<F> DoubanFetcher for FakeFetcher<F>
where
    F: Fn(&str) -> Result<String, FetchError> + Send + Sync,
{
    fn fetch_page(
        &self,
        url: &str,
        _referer: &str,
    ) -> impl std::future::Future<Output = Result<String, FetchError>> + Send {
        let out = (self.0)(url);
        async move { out }
    }

    fn fetch_subject(
        &self,
        category: &str,
        subject_id: &str,
    ) -> impl std::future::Future<Output = Result<Value, FetchError>> + Send {
        let out = (self.0)(&subject::rexxar_url(category, subject_id)).and_then(|text| {
            serde_json::from_str(&text).map_err(|_| FetchError::Suspicious(text.len()))
        });
        async move { out }
    }
}

/// 直插一条标记记录（不含维度行；富集测试只关心 records → 队列）
async fn seed_record(state: &AppState, uid: &str, category: &str, subject_id: &str) {
    sqlx::query(
        "INSERT OR IGNORE INTO records (uid, category, dim_status, subject_id, title, date)
         VALUES (?, ?, 'collect', ?, ?, '2024-01-01')",
    )
    .bind(uid)
    .bind(category)
    .bind(subject_id)
    .bind(format!("t-{subject_id}"))
    .execute(&state.pool)
    .await
    .unwrap();
}

async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

/// 驱动 worker 直到没有到期维度（每页含 ≤1s 抖动休眠，页数少耗时可忽略）
async fn drive(state: &AppState, fetcher: &impl DoubanFetcher) {
    for _ in 0..100 {
        if !worker::process_next_due(state, fetcher).await.unwrap() {
            break;
        }
    }
}

async fn post_sync(state: &AppState, body: Value) -> (StatusCode, Value) {
    let app = build_app(state.clone());
    request(
        app,
        Method::POST,
        "/api/v1/syncs",
        Some(body.to_string()),
        None,
    )
    .await
}

async fn request(
    app: Router,
    method: Method,
    uri: &str,
    body: Option<String>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// 预置一份"已成功缓存"的维度（记录直插 records 表，与 worker 写入路径一致）
async fn seed_ok_dimension(state: &AppState, category: Category, records: Vec<Record>) {
    let now = ServerConfig::now_ms();
    let page_size = weimo_douban::crawler::PAGE_SIZE as i64;
    let len = records.len() as i64;
    service::upsert_records(&state.pool, "wenjun30", category.key(), "collect", &records)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO dimensions (uid, category, dim_status, state, fetched_count,
                                 total, next_start, fetched_ms, updated_ms)
         VALUES ('wenjun30', ?, 'collect', 'ok', ?, ?, ?, ?, ?)",
    )
    .bind(category.key())
    .bind(len)
    .bind(len)
    .bind((len + page_size - 1) / page_size * page_size)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await
    .unwrap();
}

async fn get_task_view(state: &AppState, task_id: &str) -> Value {
    serde_json::to_value(service::get_task(state, task_id).await.unwrap()).unwrap()
}

// --------------------------------------------------------------------------- //
// 解析器（fixture 锁定豆瓣两代页面结构，改版时测试第一时间红）
// --------------------------------------------------------------------------- //

#[test]
fn parser_music_old_structure() {
    let items = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    assert_eq!(items.len(), 15);
    // 音乐短评在无 class 的 <li> 里；被点过「有用」的短评 li 内含
    // <span class="color_gray">N 有用</span>，正文取 li 的直接文本子节点
    assert_eq!(items.iter().filter(|r| !r.comment.is_empty()).count(), 15);
    assert!(items
        .iter()
        .all(|r| r.comment.is_empty() || r.comment.contains("测试短评")));
    assert!(items.iter().all(|r| !r.comment.contains("有用")));
    let bb = items.iter().find(|r| r.title == "Beethoven Blues").unwrap();
    assert_eq!(bb.comment, "（测试短评占位文本）");
    let tt = items.iter().find(|r| r.title == "Tony Takitani").unwrap();
    assert_eq!(tt.comment, "（测试短评占位文本）");
    let first = &items[0];
    assert_eq!(first.title, "歌之女");
    assert_eq!(first.my_rating, Some(5));
    assert!(first
        .subject_url
        .starts_with("https://music.douban.com/subject/"));
    assert!(!first.subject_id.is_empty());
    assert!(first.cover.starts_with("https://img"));
    assert_eq!(parse_total(&fixture("music_collect_p1.html")), Some(29));
}

#[test]
fn parser_book_new_structure() {
    let items = parse_items(
        &fixture("book_collect_p1.html"),
        Category::Book,
        Status::Collect,
    );
    assert_eq!(items.len(), 15);
    // 图书为 li.subject-item 新结构，副书名在标题链接内的 span
    let fy = items.iter().find(|r| r.title == "浮生六记").unwrap();
    assert_eq!(fy.alt_title, "人间三梦：古典文库系列3");
    assert!(fy.intro.contains("广西师范大学出版社"));
    assert_eq!(parse_total(&fixture("book_collect_p4.html")), Some(55));
}

#[test]
fn parser_user_profile_from_both_structures() {
    // 图书/电影：#db-usr-profile .pic img（src 头像 + alt 用户名）
    let book = parse_profile(&fixture("book_collect_p1.html")).unwrap();
    assert_eq!(book.username, "芝麻");
    assert_eq!(
        book.avatar,
        "https://img2.doubanio.com/icon/u189180823-11.jpg"
    );
    // 音乐：.music-user-profile img.avatar + .username
    let music = parse_profile(&fixture("music_collect_p1.html")).unwrap();
    assert_eq!(music.username, "芝麻");
    assert_eq!(
        music.avatar,
        "https://img2.doubanio.com/icon/u189180823-11.jpg"
    );
    // 两种结构都不在的页面返回 None
    assert!(parse_profile("<html><body>empty</body></html>").is_none());
}

// --------------------------------------------------------------------------- //
// 任务生命周期（oneshot 打 Router）
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn cached_dimension_creates_completed_task_instantly() {
    let (_dir, state) = setup().await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records).await;

    let (code, v) = post_sync(
        &state,
        json!({"user": "wenjun30", "categories": ["music"], "wait_timeout": 10}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "全缓存命中应秒回终态: {v}");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["cached_dimensions"], 1);
    assert_eq!(v["created_dimensions"], 0);
    assert_eq!(v["items_count"], 15);
    assert_eq!(v["percent"], 100);

    // 结果汇总（数据本体经 /users/{uid}/marks 浏览）
    let task_id = v["task_id"].as_str().unwrap().to_string();
    let app = build_app(state.clone());
    let (code, result) = request(
        app,
        Method::GET,
        &format!("/api/v1/syncs/{task_id}/result"),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(result["summary"]["total"], 15);
    assert_eq!(result["summary"]["with_comment"], 15);
    let dims = result["dimensions"].as_array().unwrap();
    assert_eq!(dims.len(), 1);
    assert_eq!(dims[0]["category"], "music");
    assert_eq!(dims[0]["items"], 15);
}

#[tokio::test]
async fn worker_crawls_book_dimension_to_completion() {
    let (_dir, state) = setup().await;
    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["book"]})).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let task_id = v["task_id"].as_str().unwrap().to_string();

    drive(&state, &fixture_fetcher()).await;

    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "completed");
    assert_eq!(view["items_count"], 55);
    assert_eq!(view["percent"], 100);
    let p = &view["progress"][0];
    assert_eq!(p["next_start"], 60);
    assert_eq!(p["page"], 4);
    assert_eq!(p["pages_total"], 4);

    // 抓取完成后数据已入库，marks 可分页读取
    let app = build_app(state.clone());
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=book&status=collect&page=1&page_size=50",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 55);
    assert_eq!(v["items"].as_array().unwrap().len(), 50);
}

#[tokio::test]
async fn worker_upserts_user_profile_and_endpoint_serves_it() {
    let (_dir, state) = setup().await;
    // 同步前资料端点 404
    let app = build_app(state.clone());
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/profile",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "USER_NOT_FOUND");

    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["book"]})).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    drive(&state, &fixture_fetcher()).await;

    // 抓取任一页后资料已随页入库；头像改写为本地代理端点（豆瓣头像域直连 418）
    let app = build_app(state.clone());
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/profile",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["uid"], "wenjun30");
    assert_eq!(v["username"], "芝麻");
    assert_eq!(
        v["avatar"],
        "/api/v1/covers?u=https%3A%2F%2Fimg2.doubanio.com%2Ficon%2Fu189180823-11.jpg"
    );
}

#[tokio::test]
async fn verify_user_fetches_page_upserts_profile_then_short_circuits() {
    let (_dir, state) = setup().await;
    // 抓取路径：本地未入库 → 抓 movie/collect 第 1 页，解析资料块入库
    let fetcher = FakeFetcher(|url: &str| {
        assert!(url.contains("/people/zhuma/collect?start=0"), "{url}");
        Ok(fixture("music_collect_p1.html"))
    });
    let view = service::verify_user(&state, &fetcher, "zhuma").await.unwrap();
    assert_eq!(view.uid, "zhuma");
    assert_eq!(view.username, "芝麻");
    assert_eq!(
        view.avatar,
        "/api/v1/covers?u=https%3A%2F%2Fimg2.doubanio.com%2Ficon%2Fu189180823-11.jpg"
    );

    // 本地捷径：再测同 uid 零外呼（计数 fetcher 证明未被打到）直接返回
    let calls = Arc::new(AtomicU32::new(0));
    let counting = {
        let calls = calls.clone();
        FakeFetcher(move |_url: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fixture("music_collect_p1.html"))
        })
    };
    let again = service::verify_user(&state, &counting, "zhuma").await.unwrap();
    assert_eq!(again.username, "芝麻");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verify_user_maps_not_found_and_enforces_cooldown() {
    let (_dir, state) = setup().await;
    // 豆瓣 404 → PAGE_NOT_FOUND（区别于本地未同步的 USER_NOT_FOUND）
    let missing = FakeFetcher(|_url: &str| Err(FetchError::NotFound));
    let err = service::verify_user(&state, &missing, "nobody").await.unwrap_err();
    assert_eq!(err.code(), "PAGE_NOT_FOUND");

    // 失败也记冷却：换成功 fetcher 立即重测仍被拒（本地无行、冷却未过）
    assert!(state.verify_cooldown_remaining_ms("nobody") > 0);
    let ok = FakeFetcher(|_url: &str| Ok(fixture("music_collect_p1.html")));
    let err = service::verify_user(&state, &ok, "nobody").await.unwrap_err();
    assert_eq!(err.code(), "TOO_MANY_REQUESTS");
}

#[tokio::test]
async fn verify_user_blocked_or_profileless_maps_upstream() {
    let (_dir, state) = setup().await;
    // 限流/超时类瞬态失败 → DOUBAN_UNAVAILABLE
    let blocked = FakeFetcher(|_url: &str| Err(FetchError::Blocked(403)));
    let err = service::verify_user(&state, &blocked, "limited").await.unwrap_err();
    assert_eq!(err.code(), "DOUBAN_UNAVAILABLE");

    // 页面抓到但无资料块（改版/登录墙）：同为上游故障，不误报 ID 错误
    let blank = FakeFetcher(|_url: &str| Ok("<html><body>empty</body></html>".into()));
    let err = service::verify_user(&state, &blank, "blankpage").await.unwrap_err();
    assert_eq!(err.code(), "DOUBAN_UNAVAILABLE");
}

#[tokio::test]
async fn worker_crawls_music_with_dedup_and_percent_progress() {
    let (_dir, state) = setup().await;
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    // 抓完第一页后中断检查中间进度
    assert!(worker::process_next_due(&state, &fixture_fetcher())
        .await
        .unwrap());
    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "running");
    let p = &view["progress"][0];
    assert_eq!(p["state"], "running");
    assert_eq!(p["fetched"], 15);
    assert_eq!(p["total"], 29);
    assert_eq!(p["percent"], 51);
    assert_eq!(p["page"], 1);
    assert_eq!(p["pages_total"], 2);

    drive(&state, &fixture_fetcher()).await;
    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "completed");
    assert_eq!(view["items_count"], 28); // 28/29，缺 1 条为豆瓣隐藏条目
    assert_eq!(view["percent"], 100);
}

/// 首发同步直写 records（staging=0）：无已发布数据的新维度抓取期间
/// running 态的已落库部分即可见——marks/stats/all 渐进返回，每页落库
/// 即 bump 版本（前端版本轮询感知）；完成即 ok 且暂存表无行。
/// 对照：有已发布数据的刷新仍走两阶段发布（见
/// refresh_keeps_published_snapshot_until_success_and_after_failure）。
#[tokio::test]
async fn first_sync_direct_write_streams_progressively() {
    let (_dir, state) = setup().await;
    let version_before = weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
        .await
        .unwrap()
        .music;
    post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;

    // 新维度无已发布数据 → 直写取向
    assert_eq!(
        count(
            &state,
            "SELECT staging FROM dimensions WHERE category='music'"
        )
        .await,
        0
    );

    // 抓完第一页：running 态的部分数据即可见
    assert!(worker::process_next_due(&state, &fixture_fetcher())
        .await
        .unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM dimensions WHERE category='music' AND state='running'"
        )
        .await,
        1
    );
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 20)
            .await
            .unwrap()
            .total,
        15
    );
    // 版本随页落库 bump：前端轮询 cache-version 即可感知渐进数据
    assert_ne!(
        version_before,
        weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
            .await
            .unwrap()
            .music
    );
    let app = build_app(state.clone());
    let (_, stats) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/stats",
        None,
        None,
    )
    .await;
    assert_eq!(stats["totals"]["music"], 15);
    let (_, all) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect",
        None,
        None,
    )
    .await;
    assert_eq!(all["total"], 15);

    // 完成：直写维度的记录数与暂存表清洁
    drive(&state, &fixture_fetcher()).await;
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 200)
            .await
            .unwrap()
            .total,
        28
    );
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM dimensions WHERE category='music' AND state='ok' AND staging=0"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM record_staging").await,
        0
    );
}

/// 首发失败重试仍直写：error 维度无已发布数据，重新入队保留 staging=0
/// （冲突不更新标志），续爬直写至完成。
#[tokio::test]
async fn first_sync_error_then_resume_stays_direct_write() {
    let (_dir, state) = setup().await;
    post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    // 连续失败耗尽重试 → 维度 error，records 无行
    drive(&state, &FakeFetcher(|_: &str| Err(FetchError::NotFound))).await;
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM dimensions WHERE state='error'"
        )
        .await,
        1
    );
    assert_eq!(count(&state, "SELECT COUNT(*) FROM records").await, 0);

    post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    assert_eq!(
        count(
            &state,
            "SELECT staging FROM dimensions WHERE category='music'"
        )
        .await,
        0
    );
    drive(&state, &fixture_fetcher()).await;
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 200)
            .await
            .unwrap()
            .total,
        28
    );
}

#[tokio::test]
async fn single_flight_links_two_tasks_to_same_dimension() {
    let (_dir, state) = setup().await;
    let (_, a) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let (_, b) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let a_id = a["task_id"].as_str().unwrap().to_string();
    let b_id = b["task_id"].as_str().unwrap().to_string();

    drive(&state, &fixture_fetcher()).await;

    // 两个任务都应完成且条数一致（共享同一次抓取）
    for id in [&a_id, &b_id] {
        let view = get_task_view(&state, id).await;
        assert_eq!(view["status"], "completed", "task {id}");
        assert_eq!(view["items_count"], 28);
    }
}

#[tokio::test]
async fn not_found_marks_task_failed() {
    let (_dir, state) = setup().await;
    let fetcher = FakeFetcher(|_url: &str| Err(FetchError::NotFound));
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["movie"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    drive(&state, &fetcher).await;

    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "failed");
    assert_eq!(view["error"]["code"], "PAGE_NOT_FOUND");
    assert_eq!(view["progress"][0]["state"], "error");
    assert!(view["percent"].is_null());
}

#[tokio::test]
async fn retry_exhaustion_fails_dimension_and_trips_circuit() {
    let (_dir, state) = setup().await;
    let fetcher = FakeFetcher(|_url: &str| Err(FetchError::Blocked(403)));
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["movie"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    // 重试带退避（next_sync_ms 推后），测试中强制置为到期以驱动下一次尝试
    for attempt in 1..=5 {
        sqlx::query("UPDATE dimensions SET next_sync_ms = 0")
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(
            worker::process_next_due(&state, &fetcher).await.unwrap(),
            "第 {attempt} 次应处理"
        );
    }

    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "failed");
    assert_eq!(view["error"]["code"], "DOUBAN_BLOCKED");

    // 连续失败达到阈值，熔断开启
    assert!(state.circuit_remaining_ms() > 0);

    // 维度 last_error 记录了错误码
    let last_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM dimensions WHERE category = 'movie'")
            .fetch_optional(&state.pool)
            .await
            .unwrap()
            .flatten();
    assert!(last_error.unwrap().contains("DOUBAN_BLOCKED"));
}

// --------------------------------------------------------------------------- //
// 鉴权 / 校验 / marks / 删除
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn auth_required_when_token_configured() {
    let (_dir, state) = setup_with_token("s3cret").await;
    let app = build_app(state.clone());

    let (code, v) = request(app.clone(), Method::GET, "/api/v1/server/info", None, None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "UNAUTHORIZED");

    let (code, _) = request(
        app.clone(),
        Method::GET,
        "/api/v1/server/info",
        None,
        Some("wrong"),
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    let (code, _) = request(
        app,
        Method::GET,
        "/api/v1/server/info",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn invalid_arguments_return_400_envelope() {
    let (_dir, state) = setup().await;
    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["game"]})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");

    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "wait_timeout": 99})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");

    // formats 已随文件导出移除，出现未知字段应 400
    let (code, v) = post_sync(
        &state,
        json!({"user": "wenjun30", "categories": ["music"], "formats": ["csv"]}),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");

    // 非法 JSON body
    let app = build_app(state.clone());
    let (code, v) = request(
        app,
        Method::POST,
        "/api/v1/syncs",
        Some("{bad json".into()),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn user_self_sync_creates_task_and_joins_in_flight() {
    let (_dir, state) = setup().await;
    let app = build_app(state.clone());

    // 首次触发：无 body，固定全类别 collect，任务 queued（202）
    let (code, v) = request(
        app.clone(),
        Method::POST,
        "/api/v1/users/wenjun30/syncs",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED);
    assert_eq!(v["user"], "wenjun30");
    assert_eq!(v["status"], "queued");
    assert_eq!(v["created_dimensions"], 3);
    let first_task = v["task_id"].as_str().unwrap().to_string();

    // 重复触发：维度仍 queued → single-flight 并入（新任务、维度 waiting，不重置抓取）
    let (code, v) = request(
        app.clone(),
        Method::POST,
        "/api/v1/users/wenjun30/syncs",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED);
    assert_ne!(v["task_id"].as_str().unwrap(), first_task);

    // 非法 uid：400 参数错误
    let (code, v) = request(
        app,
        Method::POST,
        "/api/v1/users/bad%20uid/syncs",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn marks_pagination_and_not_cached() {
    let (_dir, state) = setup().await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records.clone()).await;

    let app = build_app(state.clone());
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=music&status=collect&page=2&page_size=5",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 15);
    assert_eq!(v["fresh"], true);
    // 第 2 页（每页 5 条）是按日期倒序的第 6–10 条，而非从头偏移 page-1
    let mut sorted = records.clone();
    sorted.sort_by(|a, b| b.date.cmp(&a.date));
    assert_eq!(v["items"].as_array().unwrap().len(), 5);
    assert_eq!(v["items"][0]["title"], sorted[5].title);
    assert_eq!(v["items"][4]["title"], sorted[9].title);

    // 未缓存维度
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=book&status=collect",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "DIMENSION_NOT_CACHED");
}

/// 直插一条最小记录（评分筛选测试用）：comment_id 兼作主键去重键
fn rated_record(subject_id: &str, comment_id: &str, rating: Option<u8>, date: &str) -> Record {
    Record {
        title: format!("T-{subject_id}"),
        subject_id: subject_id.to_string(),
        comment_id: comment_id.to_string(),
        my_rating: rating,
        date: date.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn all_marks_rating_filter_and_validation() {
    let (_dir, state) = setup().await;
    // 跨 movie/book 两类别：五星×2（跨类别）、四星×1、三星×1、未评×1
    // ——验证合并视图按 my_rating 过滤时计数与条目同口径、跨类别按
    // 日期归并；lte 区间档（三分及以下）命中 1-3 星且未评不入档
    seed_ok_dimension(
        &state,
        Category::Movie,
        vec![
            rated_record("m1", "c1", Some(5), "2026-01-03"),
            rated_record("m2", "c2", Some(4), "2026-01-02"),
            rated_record("m3", "c5", Some(3), "2026-01-05"),
        ],
    )
    .await;
    seed_ok_dimension(
        &state,
        Category::Book,
        vec![
            rated_record("b1", "c3", Some(5), "2026-01-04"),
            rated_record("b2", "c4", None, "2026-01-01"),
        ],
    )
    .await;

    let app = build_app(state.clone());
    // 无筛选：5 条
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 5);

    // rating=5：跨类别命中 2 条，日期倒序（book 更新在前），未评不入档
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect&rating=5",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 2);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["subject_id"], "b1");
    assert_eq!(items[1]["subject_id"], "m1");
    assert_eq!(items[0]["my_rating"], 5);

    // rating=4：单条
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect&rating=4",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 1);
    assert_eq!(v["items"][0]["subject_id"], "m2");

    // rating=lte3（三分及以下）：仅三星命中，四/五星与未评均不入档
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect&rating=lte3",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 1);
    assert_eq!(v["items"][0]["subject_id"], "m3");
    assert_eq!(v["items"][0]["my_rating"], 3);

    // rating=1：无命中返回空页（total=0 而非 404）
    let (code, v) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=all&status=collect&rating=1",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 0);
    assert_eq!(v["items"].as_array().unwrap().len(), 0);

    // 非法星级/区间与单类别携带 rating 均 400
    for uri in [
        "rating=6",
        "rating=abc",
        "rating=lte0",
        "rating=lte9",
        "rating=lte",
    ] {
        let (code, v) = request(
            app.clone(),
            Method::GET,
            &format!("/api/v1/users/wenjun30/marks?category=all&status=collect&{uri}"),
            None,
            None,
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{uri}: {v}");
        assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
    }
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=movie&status=collect&rating=5",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn subject_dims_carries_my_rating_per_item() {
    let (_dir, state) = setup().await;
    // 标记级评分随维度投影平铺在 item 顶层（未评为 null）——前端
    // 「全部标记」导航子节点按它跨类别计数
    seed_ok_dimension(
        &state,
        Category::Movie,
        vec![
            rated_record("m1", "c1", Some(5), "2026-01-03"),
            rated_record("m2", "c2", None, "2026-01-02"),
        ],
    )
    .await;
    // m1 已富集（subjects 有 rexxar 行）、m2 走 intro 兜底：enriched
    // 标记两态——前端侧边栏「已拉取详情 x/y」进度据此统计
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, meta_json, source, status, fetched_ms)
         VALUES ('movie', 'm1', 'T-m1', '{}', 'rexxar', 'ok', 0)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let app = build_app(state);
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/subject-dims?category=movie&status=collect",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 2);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items[0]["id"], "m1");
    assert_eq!(items[0]["my_rating"], 5);
    assert_eq!(items[0]["enriched"], true);
    assert_eq!(items[1]["id"], "m2");
    assert_eq!(items[1]["my_rating"], Value::Null);
    assert_eq!(items[1]["enriched"], false);
}

// --------------------------------------------------------------------------- //
// 封面代理（cache-then-serve）：URL 改写 + 白名单 + 磁盘命中 + 鉴权
// --------------------------------------------------------------------------- //

/// 返回原始字节与响应头（封面是二进制，不能走 JSON 版 request()）
async fn request_bytes(app: Router, uri: &str) -> (StatusCode, Vec<u8>, header::HeaderMap) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes, headers)
}

const IMG3_COVER: &str =
    "https%3A%2F%2Fimg3.doubanio.com%2Fview%2Fphoto%2Fs_ratio_poster%2Fpublic%2Fp2935834896.webp";

#[tokio::test]
async fn marks_cover_rewritten_to_proxy() {
    let (_dir, state) = setup().await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records).await;

    let app = build_app(state);
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=music&status=collect&page_size=1",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    // 浏览器直连 img2/img3 被 Referer 防盗链 403，响应里必须是本地代理端点
    let cover = v["items"][0]["cover"].as_str().unwrap();
    assert!(
        cover.starts_with("/api/v1/covers?u=https%3A%2F%2Fimg"),
        "实际值: {cover}"
    );
}

#[tokio::test]
async fn cover_serves_from_disk_cache() {
    let (dir, state) = setup().await;
    // 预置磁盘缓存：命中路径全程不发起网络请求
    let covers_dir = dir.path().join("covers");
    std::fs::create_dir_all(&covers_dir).unwrap();
    let seeded = b"fake-webp-bytes".to_vec();
    std::fs::write(covers_dir.join("p2935834896.webp"), &seeded).unwrap();

    let app = build_app(state);
    let (code, bytes, headers) =
        request_bytes(app, &format!("/api/v1/covers?u={IMG3_COVER}")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(bytes, seeded);
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/webp");
    assert!(
        headers
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("immutable"),
        "应允许浏览器长缓存"
    );
}

#[tokio::test]
async fn cover_rejects_non_whitelisted_url() {
    let (_dir, state) = setup().await;
    let app = build_app(state);
    for bad in [
        "https%3A%2F%2Fevil.com%2Fview%2Fphoto%2Fp1.webp",
        "https%3A%2F%2Fimg3.doubanio.com%2Fsecret%2Fp1.webp",
        "not%20a%20url",
    ] {
        let (code, v) = request(
            app.clone(),
            Method::GET,
            &format!("/api/v1/covers?u={bad}"),
            None,
            None,
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "应拒绝: {bad}");
        assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
    }
}

#[tokio::test]
async fn cover_requires_token_when_configured() {
    let (dir, state) = setup_with_token("secret").await;
    let covers_dir = dir.path().join("covers");
    std::fs::create_dir_all(&covers_dir).unwrap();
    std::fs::write(covers_dir.join("p2935834896.webp"), b"x").unwrap();

    let app = build_app(state);
    // 无 token
    let (code, _, _) = request_bytes(app.clone(), &format!("/api/v1/covers?u={IMG3_COVER}")).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    // img 标签带不了 Header，?token= 查询参数兜底必须可用
    let (code, _, _) =
        request_bytes(app, &format!("/api/v1/covers?u={IMG3_COVER}&token=secret")).await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn delete_task_keeps_records_and_returns_404() {
    let (_dir, state) = setup().await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records).await;
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    let app = build_app(state.clone());
    let (code, _) = request(
        app.clone(),
        Method::DELETE,
        &format!("/api/v1/syncs/{task_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);

    let (code, v) = request(
        app.clone(),
        Method::GET,
        &format!("/api/v1/syncs/{task_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "TASK_NOT_FOUND");

    // 数据本体与任务生命周期解耦：任务删除后仍可浏览
    let (code, v) = request(
        app,
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=music&status=collect&page_size=5",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["total"], 15);
}

#[tokio::test]
async fn rate_allow_fixed_window() {
    let (dir, mut state) = setup().await;
    // setup 的限流很大；单独构造一个 limit=2 的 state（IP 用独立地址避免共享表互扰）
    state.config.rate_per_minute = 2;
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    assert!(state.rate_allow(ip));
    assert!(state.rate_allow(ip));
    assert!(!state.rate_allow(ip), "同 IP 同窗口第三次应拒绝");
    let other: IpAddr = "203.0.113.8".parse().unwrap();
    assert!(state.rate_allow(other), "不同 IP 不受影响");
    drop(dir);
}

// --------------------------------------------------------------------------- //
// SSE 进度推流
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn sse_emits_done_for_terminal_task() {
    let (_dir, state) = setup().await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records).await;
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/syncs/{task_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
    // 终态任务：推 done 事件后流关闭，body 可完整读取
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("event: done"), "body: {body}");
    assert!(body.contains("\"status\":\"completed\""), "body: {body}");
}

#[tokio::test]
async fn sse_follows_crawl_to_completion() {
    let (_dir, state) = setup().await;
    let (_, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    let task_id = v["task_id"].as_str().unwrap().to_string();

    // 先建立 SSE 连接（订阅在响应返回前已建立），再驱动抓取；
    // broadcast 缓冲保证期间的通知不丢，最终 done 事件使流正常关闭
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/syncs/{task_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    drive(&state, &fixture_fetcher()).await;

    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("event: progress"), "body: {body}");
    assert!(body.contains("event: done"), "body: {body}");
    assert!(body.contains("\"items_count\":28"), "body: {body}");
}

#[tokio::test]
async fn sse_auth_via_query_param_for_eventsource() {
    let (_dir, state) = setup_with_token("s3cret").await;
    let records = parse_items(
        &fixture("music_collect_p1.html"),
        Category::Music,
        Status::Collect,
    );
    seed_ok_dimension(&state, Category::Music, records).await;

    // 创建任务需带鉴权头
    let app = build_app(state.clone());
    let (code, v) = request(
        app.clone(),
        Method::POST,
        "/api/v1/syncs",
        Some(json!({"user": "wenjun30", "categories": ["music"]}).to_string()),
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let task_id = v["task_id"].as_str().unwrap().to_string();

    // EventSource 无法设置 Header → ?token= 兜底
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/syncs/{task_id}/events?token=s3cret"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 错误 token 仍拒绝
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/syncs/{task_id}/events?token=wrong"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --------------------------------------------------------------------------- //
// 订阅同步：RSS 解析 / CRUD / 增量 tick / auto 全量
// --------------------------------------------------------------------------- //

#[test]
fn parser_feed_structure() {
    let items = parse_feed(&fixture("feed.xml"));
    assert_eq!(items.len(), 3);

    let a = &items[0];
    assert_eq!(a.record.category, "movie");
    assert_eq!(a.record.status, "看过");
    assert_eq!(a.status_key, "collect");
    assert_eq!(a.record.title, "生日快乐");
    assert_eq!(a.record.alt_title, "Alt Title One");
    assert_eq!(a.record.my_rating, Some(5));
    assert_eq!(a.record.comment, "（测试短评占位一）");
    assert_eq!(a.record.subject_id, "37322586");
    assert_eq!(a.record.comment_id, "4975285140");
    assert_eq!(a.guid, "4975285140");
    assert_eq!(a.record.date, "2026-10-26"); // pubDate GMT +8 后取日期
    assert!(a.record.cover.starts_with("https://img"));

    let b = &items[1];
    assert_eq!(b.record.title, "再见UFO");
    assert_eq!(b.record.my_rating, Some(4)); // "推荐: 推荐" → 4 星
    assert_eq!(b.record.date, "2026-08-25");

    let c = &items[2];
    assert_eq!(c.record.category, "book");
    assert_eq!(c.status_key, "wish");
    assert_eq!(c.record.my_rating, None); // 无评分段
}

#[test]
fn parser_feed_tolerates_garbage() {
    assert!(parse_feed("").is_empty());
    assert!(parse_feed("not xml < at all").is_empty());
    // 截断的 XML：已完整解析的 item 仍返回
    let xml = fixture("feed.xml");
    let cut = &xml[..xml.find("<item>").unwrap() + 800];
    assert!(!parse_feed(cut).is_empty() || parse_feed(cut).len() <= 1);
}

#[tokio::test]
async fn subscription_crud_and_auth() {
    let (dir, state) = setup_with_token("s3cret").await;
    let app = build_app(state.clone());

    let (code, _) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subscriptions",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    let (code, body) = request(
        app.clone(),
        Method::PUT,
        "/api/v1/subscriptions/wenjun30",
        Some(json!({"rss_interval_secs": 300, "full_interval_secs": 7200}).to_string()),
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["uid"], "wenjun30");
    assert_eq!(body["rss_interval_secs"], 300);
    assert_eq!(body["full_interval_secs"], 7200);
    assert!(body["next_rss_at"].is_string());

    let (code, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subscriptions",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["subscriptions"].as_array().unwrap().len(), 1);

    let (code, _) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subscriptions/wenjun30",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::OK);

    // 间隔下限校验：rss < 60s → 400
    let (code, _) = request(
        app.clone(),
        Method::PUT,
        "/api/v1/subscriptions/wenjun30",
        Some(json!({"rss_interval_secs": 10}).to_string()),
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // 非法 uid → 400
    let (code, _) = request(
        app.clone(),
        Method::PUT,
        "/api/v1/subscriptions/not%20a%20id",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    let (code, _) = request(
        app.clone(),
        Method::DELETE,
        "/api/v1/subscriptions/wenjun30",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = request(
        app.clone(),
        Method::DELETE,
        "/api/v1/subscriptions/wenjun30",
        None,
        Some("s3cret"),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    drop(dir);
}

/// GET /users：四表并集（users ∪ records ∪ subscriptions ∪ tasks），
/// 各来源的用户都要出现，聚合字段（订阅/计数/时间/资料）正确
#[tokio::test]
async fn users_endpoint_unifies_all_uid_sources() {
    let (dir, state) = setup().await;
    let app = build_app(state.clone());

    // wenjun30：建任务（tasks 来源）+ 直插资料与一条记录
    let (_, _) = post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    seed_record(&state, "wenjun30", "music", "1000001").await;
    sqlx::query(
        "INSERT INTO users (uid, username, avatar, updated_ms)
         VALUES ('wenjun30', '文文', 'https://img.doubanio.com/icon/u1.jpg', 1700000000000)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    // alice60：仅订阅（subscriptions 来源，无任何数据）
    let (code, _) = request(
        app.clone(),
        Method::PUT,
        "/api/v1/subscriptions/alice60",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);

    // bob80：仅有标记记录（records 来源，资料未入库）
    seed_record(&state, "bob80", "book", "1000002").await;
    seed_record(&state, "bob80", "book", "1000003").await;

    let (code, body) = request(app.clone(), Method::GET, "/api/v1/users", None, None).await;
    assert_eq!(code, StatusCode::OK, "body: {body}");
    let users = body["users"].as_array().unwrap();
    let by: std::collections::HashMap<&str, &Value> = users
        .iter()
        .map(|u| (u["uid"].as_str().unwrap(), u))
        .collect();
    assert_eq!(by.len(), 3, "三个来源各一个用户: {body}");

    let wenjun = by["wenjun30"];
    assert_eq!(wenjun["username"], "文文");
    assert_eq!(wenjun["subscribed"], false);
    assert_eq!(wenjun["records"], 1);
    assert!(wenjun["last_task_at"].is_string(), "建过任务应有时间");
    assert_eq!(
        wenjun["avatar"],
        "/api/v1/covers?u=https%3A%2F%2Fimg.doubanio.com%2Ficon%2Fu1.jpg"
    );
    assert!(wenjun["profile_updated_at"].is_string());

    // 记录数降序：bob80(2) 在 wenjun30(1) 前，alice60(0) 最后
    let uids: Vec<&str> = users.iter().map(|u| u["uid"].as_str().unwrap()).collect();
    assert_eq!(uids, vec!["bob80", "wenjun30", "alice60"]);

    let alice = by["alice60"];
    assert_eq!(alice["subscribed"], true);
    assert_eq!(alice["records"], 0);
    assert!(alice["username"].is_null(), "资料未入库应为 null");
    assert!(alice["avatar"].is_null());
    assert!(alice["last_task_at"].is_null());

    let bob = by["bob80"];
    assert_eq!(bob["records"], 2);
    assert_eq!(bob["subscribed"], false);
    drop(dir);
}

#[tokio::test]
async fn rss_tick_inserts_new_and_skips_unknown_dim() {
    let (dir, state) = setup().await;
    // seed：movie/collect 已有一条 guid=4975285140 的记录（与 feed 首条同主键 → 去重）
    seed_ok_dimension(
        &state,
        Category::Movie,
        vec![Record {
            title: "生日快乐（旧）".to_string(),
            subject_id: "37322586".to_string(),
            comment_id: "4975285140".to_string(),
            ..Default::default()
        }],
    )
    .await;

    service::upsert_subscription(&state, "wenjun30", Some(300), Some(7200))
        .await
        .unwrap();
    // 拨到期（upsert 设的是 now+300s）
    sqlx::query("UPDATE subscriptions SET next_rss_ms = 0")
        .execute(&state.pool)
        .await
        .unwrap();

    let fetcher = fixture_fetcher();
    assert!(service::subscription_tick(&state, &fetcher).await.unwrap());

    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM records
         WHERE uid='wenjun30' AND category='movie' AND dim_status='collect'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(n, 2, "feed 首条与旧记录同主键被 IGNORE，第二条新增");

    // book/wish 维度不存在 → 条目被跳过，不产生孤儿 records
    let nb: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM records
         WHERE uid='wenjun30' AND category='book' AND dim_status='wish'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(nb, 0);

    // 新记录字段来自 feed 解析
    let comment: String = sqlx::query_scalar(
        "SELECT comment FROM records WHERE uid='wenjun30' AND subject_id='30208001'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(comment, "（测试短评占位二）");

    // 维度 fetched_count 已重算；订阅节奏推进、错误清零
    let fc: i64 = sqlx::query_scalar(
        "SELECT fetched_count FROM dimensions
         WHERE uid='wenjun30' AND category='movie' AND dim_status='collect'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(fc, 2);
    let (attempts, guid): (i64, String) =
        sqlx::query_as("SELECT rss_attempts, last_guid FROM subscriptions WHERE uid='wenjun30'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!((attempts, guid.as_str()), (0, "4975285140"));
    drop(dir);
}

#[tokio::test]
async fn rss_error_backoff() {
    let (dir, state) = setup().await;
    service::upsert_subscription(&state, "wenjun30", None, None)
        .await
        .unwrap();
    sqlx::query("UPDATE subscriptions SET next_rss_ms = 0")
        .execute(&state.pool)
        .await
        .unwrap();

    let fetcher = FakeFetcher(|_url: &str| Err(FetchError::Blocked(403)));
    assert!(service::subscription_tick(&state, &fetcher).await.unwrap());

    let (attempts, err): (i64, Option<String>) =
        sqlx::query_as("SELECT rss_attempts, last_error FROM subscriptions WHERE uid='wenjun30'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(err.unwrap().contains("DOUBAN_BLOCKED"));
    drop(dir);
}

#[tokio::test]
async fn full_tick_creates_auto_task_and_recrawls() {
    let (dir, state) = setup().await;
    // seed：music/collect 已缓存两条假记录，refresh 后应被 fixture 数据替换
    seed_ok_dimension(
        &state,
        Category::Music,
        vec![
            Record {
                title: "旧记录一".to_string(),
                subject_id: "999001".to_string(),
                ..Default::default()
            },
            Record {
                title: "旧记录二".to_string(),
                subject_id: "999002".to_string(),
                ..Default::default()
            },
        ],
    )
    .await;

    service::upsert_subscription(&state, "wenjun30", None, None)
        .await
        .unwrap();
    // rss 不到期、full 立即到期
    sqlx::query("UPDATE subscriptions SET next_rss_ms = 99999999999999, next_full_ms = 0")
        .execute(&state.pool)
        .await
        .unwrap();

    let fetcher = fixture_fetcher();
    assert!(service::subscription_tick(&state, &fetcher).await.unwrap());

    let (source, status): (String, String) = sqlx::query_as("SELECT source, status FROM tasks")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!((source.as_str(), status.as_str()), ("auto", "queued"));

    // 驱动 worker 抓完（music fixture 共 4 页）
    drive(&state, &fetcher).await;
    let status: String = sqlx::query_scalar("SELECT status FROM tasks")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");

    // 旧假记录被 refresh 清掉，records 为 fixture 数据
    let old_left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM records WHERE subject_id IN ('999001','999002')")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(old_left, 0);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM records WHERE uid='wenjun30' AND category='music'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        n, 28,
        "music fixture 实际渲染 28 条（1 条隐藏，与 worker 测试一致）"
    );

    // 列表默认只看 manual（不占注意力）；?source=auto 可见
    let app = build_app(state.clone());
    let (code, body) = request(app.clone(), Method::GET, "/api/v1/syncs", None, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["tasks"].as_array().unwrap().len(), 0);
    let (code, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/syncs?source=auto",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(body["tasks"][0]["source"], "auto");
    // 非法 source 过滤值
    let (code, _) = request(
        app.clone(),
        Method::GET,
        "/api/v1/syncs?source=nope",
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // 订阅 full 节奏已推进
    let (last_full,): (Option<i64>,) =
        sqlx::query_as("SELECT last_full_ms FROM subscriptions WHERE uid='wenjun30'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(last_full.is_some());
    drop(dir);
}

#[tokio::test]
async fn full_tick_single_flight() {
    let (dir, state) = setup().await;
    seed_ok_dimension(
        &state,
        Category::Music,
        vec![Record {
            title: "旧记录".to_string(),
            subject_id: "999001".to_string(),
            ..Default::default()
        }],
    )
    .await;
    service::upsert_subscription(&state, "wenjun30", None, None)
        .await
        .unwrap();
    // 已有一个未终态 auto 任务
    sqlx::query(
        "INSERT INTO tasks (task_id, user, request_json, status, created_ms, source)
         VALUES ('exp_auto_existing', 'wenjun30', '{}', 'running', 0, 'auto')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    sqlx::query("UPDATE subscriptions SET next_rss_ms = 99999999999999, next_full_ms = 0")
        .execute(&state.pool)
        .await
        .unwrap();
    let fetcher = fixture_fetcher();
    assert!(service::subscription_tick(&state, &fetcher).await.unwrap());

    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "已有进行中的 auto 任务时不重复创建");
    // records 未被 refresh 清空
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM records")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    drop(dir);
}

// --------------------------------------------------------------------------- //
// 条目元信息富集（rexxar → subjects 跨用户缓存；方案 §5 场景走查）
// --------------------------------------------------------------------------- //

/// 核心场景：不同用户标记同一条目 → rexxar 只请求一次（集合差 + 主键去重）
#[tokio::test]
async fn enrichment_dedups_across_users() {
    let (_dir, state) = setup().await;
    seed_record(&state, "userA", "movie", "1292052").await;
    seed_record(&state, "userA", "movie", "90001").await;
    seed_record(&state, "userB", "movie", "1292052").await; // 与 A 重叠

    let calls = Arc::new(AtomicU32::new(0));
    let counter = calls.clone();
    let fetcher = FakeFetcher(move |url: &str| {
        if url.contains("m.douban.com/rexxar") {
            counter.fetch_add(1, Ordering::Relaxed);
            let id = url.rsplit('/').next().unwrap();
            return Ok(format!(
                r#"{{"title": "T-{id}", "pubdate": ["1999-1"], "rating": {{"value": 8.0, "count": 10}}}}"#
            ));
        }
        Err(FetchError::NotFound)
    });

    // 两次维度完成钩子（模拟 A、B 先后同步）
    let qa = subject::enqueue_missing_subjects(&state.pool, "userA", "movie")
        .await
        .unwrap();
    let qb = subject::enqueue_missing_subjects(&state.pool, "userB", "movie")
        .await
        .unwrap();
    assert_eq!(qa, 2, "A 首次同步：两个条目全部入队");
    assert_eq!(qb, 0, "B 同步重叠条目：排队期间零新增 job");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM subject_jobs").await, 2);

    // 驱动富集到完成
    for _ in 0..10 {
        if !subject::process_next_job(&state, &fetcher).await.unwrap() {
            break;
        }
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "rexxar 恰好请求 2 次（1292052 不因 B 重复请求）"
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM subjects WHERE status='ok'").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM subject_jobs WHERE state='ok'").await,
        2
    );

    // 已有缓存条目不再入队：集合差只看缺失（重抓属显式操作）
    let qb2 = subject::enqueue_missing_subjects(&state.pool, "userB", "movie")
        .await
        .unwrap();
    assert_eq!(qb2, 0, "库中已有的条目重复同步零入队");
    for _ in 0..10 {
        if !subject::process_next_job(&state, &fetcher).await.unwrap() {
            break;
        }
    }
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

/// rexxar movie 端点突发限流（400，实测 2026-10-28）：一次 4xx 失败即触发
/// 类别冷却——同类 job 全部推迟到冷却期后（自身退避也不早于冷却），
/// 其他类别不受影响；类别间隔 AIMD 升档
#[tokio::test]
async fn enrichment_blocked_defers_category_and_grows_pacing() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "44001").await;
    seed_record(&state, "wenjun30", "movie", "44002").await;
    seed_record(&state, "wenjun30", "book", "44003").await;
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "book")
        .await
        .unwrap();

    let fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            Err(FetchError::Blocked(400))
        } else {
            Err(FetchError::NotFound)
        }
    });
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());

    let now = ServerConfig::now_ms();
    let (attempts, next_a): (i64, Option<i64>) = sqlx::query_as(
        "SELECT attempts, next_run_ms FROM subject_jobs WHERE category='movie' AND subject_id='44001'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 1);
    let next_a = next_a.unwrap();
    assert!(
        next_a >= now + 290_000,
        "被限流的 job 自身重试不早于类别冷却（实测封锁 ~5-6 分钟）"
    );
    let next_b: Option<i64> = sqlx::query_scalar(
        "SELECT next_run_ms FROM subject_jobs WHERE category='movie' AND subject_id='44002'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(next_b.unwrap() >= now + 290_000, "同类 job 被类别冷却推迟");
    let next_c: Option<i64> = sqlx::query_scalar(
        "SELECT next_run_ms FROM subject_jobs WHERE category='book' AND subject_id='44003'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(next_c.is_none(), "其他类别不受 movie 端点封锁影响");

    // AIMD：失败一次后 movie 间隔升档（配置下限 0.5s ×3）
    assert_eq!(state.subject_interval("movie"), 1.5);
    assert_eq!(
        state.subject_interval("book"),
        0.5,
        "未受影响的类别保持下限"
    );
}

/// 重试耗尽 → intro 兜底行落库（source='intro'，无豆瓣评分，形状与 rexxar
/// 一致），job 置 ok 终结重试链；显式强制重排可升级回 rexxar 权威数据
#[tokio::test]
async fn enrichment_exhaustion_falls_back_to_intro() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "45001").await;
    sqlx::query("UPDATE records SET intro = ? WHERE subject_id = '45001'")
        .bind("2019-11-17(香港亚洲电影节) / 2020-06-11(中国香港) / 邓丽欣 / 中国香港 / 黄绮琳 / 91分钟 / 剧情 / 粤语")
        .execute(&state.pool)
        .await
        .unwrap();
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    let counter = calls.clone();
    let fetcher = FakeFetcher(move |url: &str| {
        if url.contains("m.douban.com/rexxar") {
            counter.fetch_add(1, Ordering::Relaxed);
            return Err(FetchError::Blocked(400));
        }
        Err(FetchError::NotFound)
    });
    for _ in 0..subject::MAX_ATTEMPTS {
        assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
        // 抹掉退避等待，模拟时间流逝（真实运行中由到期时间驱动）
        sqlx::query("UPDATE subject_jobs SET next_run_ms = NULL")
            .execute(&state.pool)
            .await
            .unwrap();
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        subject::MAX_ATTEMPTS as u32,
        "恰重试 5 次"
    );
    let (job_state, last_error): (String, String) = sqlx::query_as(
        "SELECT state, COALESCE(last_error,'') FROM subject_jobs WHERE subject_id='45001'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(job_state, "ok", "兜底后 job 终结为 ok");
    assert!(last_error.contains("兜底"), "降级原因可见: {last_error}");

    let (source, title, year, rating): (String, String, Option<i64>, Option<f64>) = sqlx::query_as(
        "SELECT source, title, year, rating_value FROM subjects WHERE subject_id='45001'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(source, "intro");
    assert_eq!(title, "t-45001", "标题来自 records 标记行");
    assert_eq!(year, Some(2019), "intro 解析出的年份");
    assert!(rating.is_none(), "intro 兜底无豆瓣评分");
    let meta_json: String =
        sqlx::query_scalar("SELECT meta_json FROM subjects WHERE subject_id='45001'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let meta: Value = serde_json::from_str(&meta_json).unwrap();
    assert_eq!(meta["directors"], serde_json::json!(["黄绮琳"]));
}

/// intro 兜底行的升级路径：显式强制重排（priority=0，绕过执行守卫）→
/// rexxar 成功后整行覆盖为权威数据（source='rexxar'、评分就位）
#[tokio::test]
async fn intro_fallback_row_upgrades_on_force_refresh() {
    let (_dir, state) = setup().await;
    // 直插 intro 兜底行（等同 write_intro_fallback 的产物形态）
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, cover_url, source, fetched_ms)
         VALUES ('movie', '45001', 't-45001', '', 'intro', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    subject::force_refresh(&state.pool, "movie", "45001")
        .await
        .unwrap();
    let ok_fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            Ok(
                r#"{"title": "金都", "year": "2019", "rating": {"value": 6.5, "count": 100}}"#
                    .to_string(),
            )
        } else {
            Err(FetchError::NotFound)
        }
    });
    assert!(subject::process_next_job(&state, &ok_fetcher)
        .await
        .unwrap());
    let (source, title, rating): (String, String, Option<f64>) =
        sqlx::query_as("SELECT source, title, rating_value FROM subjects WHERE subject_id='45001'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(source, "rexxar", "强制重排成功后升级为权威数据");
    assert_eq!(title, "金都");
    assert_eq!(rating, Some(6.5));
}

/// 重试耗尽但 records 无该条目（理论不可达）：维持 error 终态，不写 subjects
#[tokio::test]
async fn enrichment_exhaustion_without_record_keeps_error() {
    let (_dir, state) = setup().await;
    let now = ServerConfig::now_ms();
    sqlx::query(
        "INSERT INTO subject_jobs (category, subject_id, state, created_ms, updated_ms)
         VALUES ('movie', '46001', 'queued', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await
    .unwrap();

    let fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            Err(FetchError::Blocked(400))
        } else {
            Err(FetchError::NotFound)
        }
    });
    for _ in 0..subject::MAX_ATTEMPTS {
        assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
        sqlx::query("UPDATE subject_jobs SET next_run_ms = NULL")
            .execute(&state.pool)
            .await
            .unwrap();
    }
    let job_state: String = sqlx::query_scalar("SELECT state FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job_state, "error");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM subjects").await, 0);
}
async fn enrichment_404_is_terminal_not_found() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "41001").await;
    let queued = subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    assert_eq!(queued, 1);

    let fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            Err(FetchError::NotFound)
        } else {
            Err(FetchError::NotFound)
        }
    });
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM subject_jobs WHERE state='not_found'"
        )
        .await,
        1
    );
    assert_eq!(count(&state, "SELECT COUNT(*) FROM subjects").await, 0);
    // 立即重跑不重试（终态）
    assert!(!subject::process_next_job(&state, &fetcher).await.unwrap());
}

/// 瞬时失败：退避后重排，未到期不再拾起；计入全局熔断
#[tokio::test]
async fn enrichment_transient_error_backs_off() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "42001").await;
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();

    let fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            Err(FetchError::Blocked(403))
        } else {
            Err(FetchError::NotFound)
        }
    });
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
    let state_str: String = sqlx::query_scalar("SELECT state FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let next_run: Option<i64> = sqlx::query_scalar("SELECT next_run_ms FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(state_str, "queued");
    assert_eq!(attempts, 1);
    assert!(
        next_run.unwrap_or(0) > ServerConfig::now_ms(),
        "退避时间在未来"
    );
    // 未到期：不再拾起
    assert!(!subject::process_next_job(&state, &fetcher).await.unwrap());
    assert_eq!(state.circuit_remaining_ms() >= 0, true);
}

/// marks?expand=subject 展开缓存元信息；未富集为 null；非法 expand 值 400
#[tokio::test]
async fn marks_expand_subject_and_subjects_endpoints() {
    let (_dir, state) = setup().await;
    // 预置已缓存维度（两条标记：一个会被富集，一个不驱动富集）
    let records = vec![
        Record {
            category: "movie".into(),
            status: "collect".into(),
            title: "肖申克的救赎".into(),
            alt_title: String::new(),
            my_rating: Some(5),
            date: "2024-01-02".into(),
            comment: String::new(),
            subject_url: String::new(),
            subject_id: "1292052".into(),
            cover: String::new(),
            comment_id: String::new(),
            intro: String::new(),
        },
        Record {
            category: "movie".into(),
            status: "collect".into(),
            title: "未富集条目".into(),
            alt_title: String::new(),
            my_rating: None,
            date: "2024-01-01".into(),
            comment: String::new(),
            subject_url: String::new(),
            subject_id: "99999".into(),
            cover: String::new(),
            comment_id: String::new(),
            intro: String::new(),
        },
    ];
    seed_ok_dimension(&state, Category::Movie, records).await;

    // 99999 的 intro 留空（兜底视图各字段为空数组）；另种一条带真实 intro 的
    // 记录验证兜底解析（金都：黄绮琳 / 91分钟 / 中国香港）
    // —— 上面 records 第二条 intro 为空，这里直接改写它的 intro 再测兜底解析
    sqlx::query("UPDATE records SET intro = ? WHERE subject_id = '99999'")
        .bind("2019-11-17(香港亚洲电影节) / 2020-06-11(中国香港) / 邓丽欣 / 中国香港 / 黄绮琳 / 91分钟 / 剧情 / 粤语")
        .execute(&state.pool)
        .await
        .unwrap();
    // 只富集 1292052（真实 fixture）
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    let fetcher = fixture_fetcher();
    // 1292052 与 99999 都会入队；只驱动一次，留下 99999 未富集（走 intro 兜底）
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());

    let app = build_app(state.clone());
    // 未展开：响应不含 subject 键
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=movie&status=collect",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["items"][0].get("subject").is_none());

    // 展开：已富集条目带结构化字段，未富集条目为 null
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=movie&status=collect&expand=subject",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["subject"]["title"], "肖申克的救赎");
    assert_eq!(body["items"][0]["subject"]["movie"]["genres"][0], "剧情");
    assert_eq!(body["items"][0]["subject"]["year"], 1994);
    assert_eq!(body["items"][0]["subject"]["source"], "rexxar");
    // 未富集条目：intro 兜底视图（启发式字段、无豆瓣评分）
    assert_eq!(body["items"][1]["subject"]["source"], "intro");
    assert_eq!(body["items"][1]["subject"]["year"], 2019);
    assert_eq!(
        body["items"][1]["subject"]["movie"]["directors"],
        serde_json::json!(["黄绮琳"])
    );
    assert_eq!(
        body["items"][1]["subject"]["movie"]["durations"],
        serde_json::json!(["91分钟"])
    );
    assert_eq!(body["items"][1]["subject"]["rating"]["value"], Value::Null);

    // 非法 expand 值
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/users/wenjun30/marks?category=movie&status=collect&expand=bogus",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");

    // 单条目端点：rexxar 命中 / records 命中走 intro 兜底 / 完全未知 404
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/movie/1292052",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["original_title"], "The Shawshank Redemption");
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/movie/99999",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "intro");
    assert_eq!(body["movie"]["countries"], serde_json::json!(["中国香港"]));
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/movie/88888",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "SUBJECT_NOT_FOUND");
}

/// refresh 无视 TTL 重新排队；backfill 幂等
#[tokio::test]
async fn subject_refresh_and_backfill_endpoints() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "1292052").await;
    let app = build_app(state.clone());

    // 回填：首跑入队 1，重跑 0（幂等）
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/backfill",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued_jobs"], 1);
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/backfill",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued_jobs"], 0);

    // 驱动到完成
    let fetcher = fixture_fetcher();
    for _ in 0..10 {
        if !subject::process_next_job(&state, &fetcher).await.unwrap() {
            break;
        }
    }
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM subjects WHERE status='ok'").await,
        1
    );

    // 强制刷新：job 重排（priority=0），可再次驱动
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/movie/1292052/refresh",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], true);
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM subject_jobs WHERE state='queued' AND priority=0"
        )
        .await,
        1
    );
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
}

/// 执行守卫：库中已有条目且非显式强制（priority=0）→ 跳过，不请求豆瓣；
/// refresh 端点（priority=0）显式重抓则绕过守卫正常请求
#[tokio::test]
async fn enrichment_skips_in_db_subjects_unless_forced() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "1292052").await;
    // 直接造 subjects 行 + 普通 priority=1 的 job（模拟旧路径排队 / job 清理后被重排）
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, fetched_ms)
         VALUES ('movie', '1292052', '旧标题', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let now = ServerConfig::now_ms();
    sqlx::query(
        "INSERT INTO subject_jobs (category, subject_id, state, priority, created_ms, updated_ms)
         VALUES ('movie', '1292052', 'queued', 1, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await
    .unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    let counter = calls.clone();
    let fetcher = FakeFetcher(move |url: &str| {
        if url.contains("m.douban.com/rexxar") {
            counter.fetch_add(1, Ordering::Relaxed);
            return Ok(r#"{"title": "新标题", "pubdate": ["1999-1"]}"#.to_string());
        }
        Err(FetchError::NotFound)
    });

    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
    assert_eq!(calls.load(Ordering::Relaxed), 0, "库中已有 → 不发请求");
    let job_state: String = sqlx::query_scalar("SELECT state FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job_state, "ok", "跳过的 job 直接标记完成");
    let fetched_ms: i64 = sqlx::query_scalar("SELECT fetched_ms FROM subjects")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(fetched_ms, 1, "跳过时不得改写库中数据");

    // 入队侧同样只入缺失：records 有标记但 subjects 已有 → 零新增
    let queued = subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    assert_eq!(queued, 0);

    // 显式强制重抓（refresh 端点 → priority=0）：绕过守卫真实请求
    let app = build_app(state.clone());
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/movie/1292052/refresh",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], true);
    assert!(subject::process_next_job(&state, &fetcher).await.unwrap());
    assert_eq!(calls.load(Ordering::Relaxed), 1, "强制重抓正常请求一次");
    let title: String = sqlx::query_scalar("SELECT title FROM subjects")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(title, "新标题");
}

/// 入队语义回归：过期但仍在库的条目不自动重抓（只入缺失）
#[tokio::test]
async fn enrichment_enqueue_only_missing_even_if_stale() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "1292052").await;
    seed_record(&state, "wenjun30", "movie", "55555").await;
    // fetched_ms = 1：远早于任何 TTL，属"过期但仍在库"
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, fetched_ms)
         VALUES ('movie', '1292052', '旧数据', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    let queued = subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    assert_eq!(
        queued, 1,
        "只入缺失的 55555；在库的 1292052 即使过期也不入队"
    );
    let job_id: String = sqlx::query_scalar("SELECT subject_id FROM subject_jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job_id, "55555");
}

/// 任务管理端点：列表（过滤/搜索/计数/标题兜底）、单条删除、批量清理（中断）、重排
#[tokio::test]
async fn subject_jobs_management_endpoints() {
    let (_dir, state) = setup().await;
    let app = build_app(state.clone());
    let now = ServerConfig::now_ms();

    // 数据面：movie/1292052 已富集（过期）；movie/777 待执行；book/888 失败终态；music/999 已删除终态
    seed_record(&state, "wenjun30", "movie", "1292052").await;
    seed_record(&state, "wenjun30", "movie", "777").await;
    seed_record(&state, "wenjun30", "book", "888").await;
    seed_record(&state, "wenjun30", "music", "999").await;
    sqlx::query(
        "INSERT INTO subjects (category, subject_id, title, fetched_ms)
         VALUES ('movie', '1292052', '肖申克的救赎', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    for (cat, id, st) in [
        ("movie", "1292052", "ok"),
        ("movie", "777", "queued"),
        ("book", "888", "error"),
        ("music", "999", "not_found"),
    ] {
        sqlx::query(
            "INSERT INTO subject_jobs (category, subject_id, state, attempts, priority,
                                       created_ms, updated_ms, last_error)
             VALUES (?, ?, ?, ?, 1, ?, ?, ?)",
        )
        .bind(cat)
        .bind(id)
        .bind(st)
        .bind(if st == "error" { 5 } else { 0 })
        .bind(now)
        .bind(now)
        .bind(if st == "error" {
            Some("BLOCKED: 403")
        } else {
            None::<&str>
        })
        .execute(&state.pool)
        .await
        .unwrap();
    }

    // 列表：活动状态在前，未富集条目标题回退 records（t-777）
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 4);
    assert_eq!(body["counts_total"], 4);
    assert_eq!(body["counts"]["queued"], 1);
    assert_eq!(body["counts"]["ok"], 1);
    assert_eq!(body["counts"]["error"], 1);
    assert_eq!(body["counts"]["not_found"], 1);
    assert_eq!(body["items"][0]["subject_id"], "777", "活动状态排前");
    assert_eq!(body["items"][0]["title"], "t-777", "未富集标题回退 records");
    let enriched = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["subject_id"] == "1292052")
        .unwrap();
    assert_eq!(enriched["title"], "肖申克的救赎");
    assert_eq!(enriched["stale"], true, "fetched_ms=1 已过 TTL");
    assert_eq!(enriched["forced"], false);
    assert!(enriched["fetched_at"].is_string());

    // 状态过滤（多值）+ 类别过滤 + 标题/ID 搜索 + 非法值 400
    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs?state=queued,running",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs?category=book",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["last_error"], "BLOCKED: 403");
    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs?q=肖申克",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["subject_id"], "1292052");
    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs?q=888",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    let (status, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects/jobs?state=bogus",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");

    // 单条删除：queued 的 777（即"取消"该条目）；重复删 404
    let (status, _) = request(
        app.clone(),
        Method::DELETE,
        "/api/v1/subjects/jobs/movie/777",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = request(
        app.clone(),
        Method::DELETE,
        "/api/v1/subjects/jobs/movie/777",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "JOB_NOT_FOUND");

    // 批量清理已完成：只清 ok，其余不动
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/clear",
        Some(r#"{"states": ["ok"]}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], 1);
    assert_eq!(count(&state, "SELECT COUNT(*) FROM subject_jobs").await, 2);

    // 重排失败：error 终态 → queued、attempts 清零、priority=0（显式插队）
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/requeue",
        Some(r#"{"scope": "failed"}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"], 1);
    let (st, attempts, priority): (String, i64, i64) = sqlx::query_as(
        "SELECT state, attempts, priority FROM subject_jobs WHERE category='book' AND subject_id='888'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!((st.as_str(), attempts, priority), ("queued", 0, 0));

    // 重抓过期：subjects 中过 TTL 的 1292052（job 行刚被清理）重新入队且为强制
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/requeue",
        Some(r#"{"scope": "stale"}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["queued"], 1);
    let (st, priority): (String, i64) = sqlx::query_as(
        "SELECT state, priority FROM subject_jobs WHERE category='movie' AND subject_id='1292052'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!((st.as_str(), priority), ("queued", 0));
    // 再跑一次：已排队的不动（幂等，0 增量）
    let (_, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/requeue",
        Some(r#"{"scope": "stale"}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(body["queued"], 0, "已在排队的 job 不被重置");

    // 中断队列：清空全部待执行（book/888、movie/1292052 两个 queued）
    let (status, body) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/clear",
        Some(r#"{"states": ["queued", "running"]}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], 2);
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM subject_jobs WHERE state IN ('queued','running')"
        )
        .await,
        0
    );

    // 非法入参：空 states / 非法 scope
    let (status, _) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/clear",
        Some(r#"{"states": []}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        app.clone(),
        Method::POST,
        "/api/v1/subjects/jobs/requeue",
        Some(r#"{"scope": "everything"}"#.to_string()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// GET /subjects 列表：分页/类别过滤/标题搜索/进度计数
#[tokio::test]
async fn subjects_list_endpoint_filters_and_counts() {
    let (_dir, state) = setup().await;
    seed_record(&state, "wenjun30", "movie", "1292052").await;
    seed_record(&state, "wenjun30", "book", "38386470").await;
    // 制造一个 not_found 终态 job 进计数
    seed_record(&state, "wenjun30", "music", "41001").await;
    subject::backfill(&state.pool).await.unwrap();

    // music/41001 模拟已删除条目（404），其余走 fixture
    let fetcher = FakeFetcher(|url: &str| {
        if url.contains("m.douban.com/rexxar") {
            if url.ends_with("/41001") {
                return Err(FetchError::NotFound);
            }
            let id = url.rsplit('/').next().unwrap();
            return Ok(match id {
                "1292052" => fixture("rexxar_movie.json"),
                "38386470" => fixture("rexxar_book.json"),
                _ => format!(
                    r#"{{"title": "T-{id}", "pubdate": ["1999-1"], "rating": {{"value": 8.0, "count": 10}}}}"#
                ),
            });
        }
        Err(FetchError::NotFound)
    });
    for _ in 0..10 {
        if !subject::process_next_job(&state, &fetcher).await.unwrap() {
            break;
        }
    }

    let app = build_app(state.clone());
    // 全量列表：两条 ok
    let (status, body) = request(app.clone(), Method::GET, "/api/v1/subjects", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["counts"]["enriched"], 2);
    assert_eq!(body["counts"]["not_found"], 1, "404 job 计入 not_found");
    assert_eq!(
        body["items"][0]["url"],
        "https://movie.douban.com/subject/1292052/"
    );

    // 类别过滤 + 标题搜索
    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects?category=book",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["title"], "浮生六记");

    let (_, body) = request(
        app.clone(),
        Method::GET,
        "/api/v1/subjects?q=肖申克",
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["subject_id"], "1292052");

    // % 通配符按字面量处理
    let (_, body) = request(app.clone(), Method::GET, "/api/v1/subjects?q=%", None, None).await;
    assert_eq!(body["total"], 0, "百分号被转义，不匹配全部");
}

// --------------------------------------------------------------------------- //
// 解析哨兵：改版/登录墙空页必须显式失败，合法空集不受影响
// --------------------------------------------------------------------------- //

#[tokio::test]
async fn parse_sentinel_flags_silent_empty_page_as_error() {
    let (_dir, state) = setup().await;
    // 模拟豆瓣改版/登录墙：200 + total=55 + 页面无条目结构
    //（改版前这里会静默标 ok + 0 条，见《豆瓣源依赖性与脆弱性分析.md》§3 F1）
    let broken = "<html><body><span class=\"subject-num\">1-15&nbsp;/&nbsp;55</span></body></html>";
    let fetcher = FakeFetcher(move |_url: &str| Ok(broken.to_string()));
    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["book"]})).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let task_id = v["task_id"].as_str().unwrap().to_string();

    drive(&state, &fetcher).await;

    // 维度 error、任务 failed 且错误码可见（而非 completed + 0 条的静默缺失）
    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "failed");
    assert_eq!(view["error"]["code"], "PARSE_ERROR");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM records").await, 0);
    // 解析异常不是豆瓣拒绝服务，不计入熔断
    assert_eq!(state.circuit_remaining_ms(), 0);
}

#[tokio::test]
async fn parse_sentinel_allows_genuinely_empty_dimension() {
    let (_dir, state) = setup().await;
    // 用户真的没有标记：无 subject-num、无条目——哨兵不误伤
    let empty = "<html><body><p>还没有添加内容</p></body></html>";
    let fetcher = FakeFetcher(move |_url: &str| Ok(empty.to_string()));
    let (code, v) = post_sync(&state, json!({"user": "wenjun30", "categories": ["book"]})).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let task_id = v["task_id"].as_str().unwrap().to_string();

    drive(&state, &fetcher).await;

    let view = get_task_view(&state, &task_id).await;
    assert_eq!(view["status"], "completed");
    assert_eq!(view["items_count"], 0);
}

#[tokio::test]
async fn refresh_keeps_published_snapshot_until_success_and_after_failure() {
    let (_dir, state) = setup().await;
    seed_ok_dimension(
        &state,
        Category::Music,
        vec![Record {
            subject_id: "old".into(),
            title: "旧快照".into(),
            ..Default::default()
        }],
    )
    .await;
    let before = weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
        .await
        .unwrap()
        .music;
    post_sync(
        &state,
        json!({"user":"wenjun30", "categories":["music"], "refresh":true}),
    )
    .await;
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 20)
            .await
            .unwrap()
            .items[0]
            .subject_id,
        "old"
    );
    // A failed refresh leaves the old snapshot readable and keeps the same version.
    drive(&state, &FakeFetcher(|_: &str| Err(FetchError::NotFound))).await;
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 20)
            .await
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        before,
        weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
            .await
            .unwrap()
            .music
    );
    post_sync(&state, json!({"user":"wenjun30", "categories":["music"]})).await;
    assert!(worker::process_next_due(&state, &fixture_fetcher())
        .await
        .unwrap());
    // One staged page does not leak into any published view.
    assert_eq!(
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 20)
            .await
            .unwrap()
            .total,
        1
    );
    let (_, stats) = request(
        build_app(state.clone()),
        Method::GET,
        "/api/v1/users/wenjun30/stats",
        None,
        None,
    )
    .await;
    assert_eq!(stats["totals"]["music"], 1);
    // A repeated feed item for an old record must not resurrect it in staging.
    service::upsert_records(
        &state.pool,
        "wenjun30",
        "music",
        "collect",
        &[Record {
            subject_id: "old".into(),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    // An RSS arrival during the crawl must survive publication.
    service::upsert_records(
        &state.pool,
        "wenjun30",
        "music",
        "collect",
        &[Record {
            subject_id: "rss-new".into(),
            title: "同步期间新增".into(),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    drive(&state, &fixture_fetcher()).await;
    let result = service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 200)
        .await
        .unwrap();
    assert_eq!(result.total, 29);
    assert!(result.items.iter().any(|r| r.subject_id == "rss-new"));
    assert!(!result.items.iter().any(|r| r.subject_id == "old"));
    assert_ne!(
        before,
        weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
            .await
            .unwrap()
            .music
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM record_staging").await,
        0
    );
}

/// cache-version 的 subjects_pending：富集 job 入队（不 bump revision）
/// 也要让版本端点给出新体（ETag 掺 pending，304 不吞）——前端据此在
/// 同步终态后维持 3s 轮询，pending 归零自动停
#[tokio::test]
async fn cache_version_exposes_subjects_pending_and_etag_tracks_it() {
    let (_dir, state) = setup().await;
    post_sync(&state, json!({"user": "wenjun30", "categories": ["music"]})).await;
    drive(&state, &fixture_fetcher()).await;
    let uri = "/api/v1/users/wenjun30/cache-version?status=collect";

    // 首发完成即入队富集 job：pending 随版本端点暴露
    let (code1, headers1, v1) = conditional_get(&state, uri, None, None).await;
    assert_eq!(code1, StatusCode::OK);
    let tag1 = headers1
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        v1["subjects_pending"].as_i64().unwrap() > 0,
        "首发完成后应有富集 job 在队: {v1}"
    );

    // 无变化时保持协商缓存语义（304 空体）
    let (code304, _, _) = conditional_get(&state, uri, Some(&tag1), None).await;
    assert_eq!(code304, StatusCode::NOT_MODIFIED);

    // 清空富集队列（终态全部 job）：pending 归零，ETag 随之变化——
    // 旧 tag 再请求必须是 200 新体而非 304（入队/出队不 bump revision，
    // pending 是 ETag 的独立组成）
    sqlx::query("UPDATE subject_jobs SET state='ok', next_run_ms=NULL")
        .execute(&state.pool)
        .await
        .unwrap();
    let (code2, headers2, v2) = conditional_get(&state, uri, Some(&tag1), None).await;
    assert_eq!(code2, StatusCode::OK);
    assert_eq!(v2["subjects_pending"], 0);
    let tag2 = headers2
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(tag1, tag2, "pending 变化应改变 ETag（304 不得吞掉）");
}

#[tokio::test]
async fn paging_versions_allow_enrichment_but_reject_record_changes_and_keep_legacy_guards() {
    let (_dir, state) = setup().await;
    seed_ok_dimension(
        &state,
        Category::Movie,
        vec![Record {
            subject_id: "1292052".into(),
            ..Default::default()
        }],
    )
    .await;
    let manifest = "/api/v1/users/wenjun30/cache-version?status=collect";
    let (_, _, before) = conditional_get(&state, manifest, None, None).await;
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    subject::process_next_job(&state, &fixture_fetcher())
        .await
        .unwrap();
    let (_, _, enriched) = conditional_get(&state, manifest, None, None).await;
    assert_ne!(before["movie"], enriched["movie"]);
    for category in ["movie", "all"] {
        let field = format!("{category}_marks");
        assert_eq!(before[&field], enriched[&field]);
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("category", category)
            .append_pair("status", "collect")
            .append_pair("expand", "subject")
            .append_pair("cache_version", before[category].as_str().unwrap())
            .append_pair("paging_version", before[&field].as_str().unwrap())
            .finish();
        let (code, _, body) = conditional_get(
            &state,
            &format!("/api/v1/users/wenjun30/marks?{params}"),
            None,
            None,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["items"][0]["subject"]["source"], "rexxar");
        // Removing the new guard exercises an old client against the new server.
        let legacy = params.split("&paging_version=").next().unwrap();
        assert_eq!(
            conditional_get(
                &state,
                &format!("/api/v1/users/wenjun30/marks?{legacy}"),
                None,
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    let dims_params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("category", "movie")
        .append_pair("status", "collect")
        .append_pair("cache_version", before["movie"].as_str().unwrap())
        .append_pair("paging_version", before["movie_marks"].as_str().unwrap())
        .finish();
    assert_eq!(
        conditional_get(
            &state,
            &format!("/api/v1/users/wenjun30/subject-dims?{dims_params}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT,
        "dims must keep its full content guard"
    );
    sqlx::query("UPDATE records SET date='2026-11-01' WHERE uid='wenjun30'")
        .execute(&state.pool)
        .await
        .unwrap();
    for category in ["movie", "all"] {
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("category", category)
            .append_pair("status", "collect")
            .append_pair(
                "paging_version",
                before[format!("{category}_marks")].as_str().unwrap(),
            )
            .finish();
        assert_eq!(
            conditional_get(
                &state,
                &format!("/api/v1/users/wenjun30/marks?{params}"),
                None,
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
}

#[tokio::test]
async fn paging_version_checks_record_publication_after_the_read() {
    // A deterministic handler publishes a change between middleware's two
    // checks. This exercises the post-read race without timing-dependent sleeps.
    for change_records in [false, true] {
        let (_dir, state) = setup().await;
        seed_ok_dimension(
            &state,
            Category::Music,
            vec![Record {
                subject_id: "1".into(),
                ..Default::default()
            }],
        )
        .await;
        let before = weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
            .await
            .unwrap();
        let pool = state.pool.clone();
        let app = Router::new()
            .route("/users/{uid}/marks", axum::routing::get(move || async move {
                let sql = if change_records {
                    "UPDATE records SET date='2026-11-01' WHERE uid='wenjun30'"
                } else {
                    "INSERT INTO cache_versions(scope,revision) VALUES ('subjects:music',1) ON CONFLICT(scope) DO UPDATE SET revision=revision+1"
                };
                sqlx::query(sql).execute(&pool).await.unwrap();
                axum::Json(json!({"items": []}))
            }))
            .layer(axum::middleware::from_fn_with_state(state, weimo_douban::server::cache::middleware));
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("category", "music")
            .append_pair("status", "collect")
            .append_pair("cache_version", &before.music)
            .append_pair("paging_version", &before.music_marks)
            .finish();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/users/wenjun30/marks?{params}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if change_records {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            }
        );
    }
}

#[tokio::test]
async fn manifest_schema_change_does_not_reuse_a_legacy_etag() {
    use std::hash::{Hash, Hasher};
    let (_dir, state) = setup().await;
    let uri = "/api/v1/users/wenjun30/cache-version?status=collect";
    let (_, _, before) = conditional_get(&state, uri, None, None).await;
    // Axum's nested middleware sees the /users path.
    for path in [uri, "/users/wenjun30/cache-version?status=collect"] {
        let legacy_key = format!(
            "{}:{}:{}:{}",
            path,
            before["all"].as_str().unwrap(),
            before["profile"].as_str().unwrap(),
            before["subjects_pending"].as_i64().unwrap()
        );
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        legacy_key.hash(&mut hash);
        let legacy_tag = format!("W/\"{:x}\"", hash.finish());
        let (code, headers, body) = conditional_get(&state, uri, Some(&legacy_tag), None).await;
        assert_eq!(code, StatusCode::OK);
        assert!(body["music_marks"].is_string());
        assert_eq!(
            conditional_get(&state, uri, headers[header::ETAG].to_str().ok(), None)
                .await
                .0,
            StatusCode::NOT_MODIFIED
        );
    }
}

#[tokio::test]
async fn unchanged_sync_keeps_content_version_and_stable_order() {
    let (_dir, state) = setup().await;
    post_sync(&state, json!({"user":"wenjun30", "categories":["music"]})).await;
    drive(&state, &fixture_fetcher()).await;
    let before = weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
        .await
        .unwrap()
        .music;
    let ids = service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 200)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|r| r.subject_id)
        .collect::<Vec<_>>();
    post_sync(
        &state,
        json!({"user":"wenjun30", "categories":["music"], "refresh":true}),
    )
    .await;
    drive(&state, &fixture_fetcher()).await;
    assert_eq!(
        before,
        weimo_douban::server::cache::versions(&state, "wenjun30", "collect")
            .await
            .unwrap()
            .music
    );
    assert_eq!(
        ids,
        service::read_marks(&state, "wenjun30", Category::Music, Status::Collect, 1, 200)
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|r| r.subject_id)
            .collect::<Vec<_>>()
    );
}

async fn conditional_get(
    state: &AppState,
    uri: &str,
    tag: Option<&str>,
    token: Option<&str>,
) -> (StatusCode, header::HeaderMap, Value) {
    let mut req = Request::builder().uri(uri);
    if let Some(tag) = tag {
        req = req.header(header::IF_NONE_MATCH, tag);
    }
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = build_app(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn versions_and_projection_etags_follow_rss_enrichment_and_profile_changes() {
    let (_dir, state) = setup().await;
    seed_ok_dimension(
        &state,
        Category::Movie,
        vec![Record {
            subject_id: "1292052".into(),
            ..Default::default()
        }],
    )
    .await;
    let uri = "/api/v1/users/wenjun30/cache-version?status=collect";
    assert_eq!(
        conditional_get(
            &state,
            "/api/v1/users/wenjun30/cache-version?status=invalid",
            None,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, headers, versions) = conditional_get(&state, uri, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-cache");
    let tag = headers[header::ETAG].to_str().unwrap();
    assert_eq!(
        conditional_get(&state, uri, Some(tag), None).await.0,
        StatusCode::NOT_MODIFIED
    );
    let dims_uri = "/api/v1/users/wenjun30/subject-dims?category=movie&status=collect";
    let (_, headers, _) = conditional_get(&state, dims_uri, None, None).await;
    let dims_tag = headers[header::ETAG].to_str().unwrap();
    assert_eq!(
        conditional_get(&state, dims_uri, Some(dims_tag), None)
            .await
            .0,
        StatusCode::NOT_MODIFIED
    );
    subject::enqueue_missing_subjects(&state.pool, "wenjun30", "movie")
        .await
        .unwrap();
    subject::process_next_job(&state, &fixture_fetcher())
        .await
        .unwrap();
    let (_, _, enriched) = conditional_get(&state, uri, Some(tag), None).await;
    assert_ne!(versions["movie"], enriched["movie"]);
    assert_eq!(versions["stats"], enriched["stats"]);
    assert_eq!(versions["book"], enriched["book"]);
    assert_eq!(
        conditional_get(&state, dims_uri, Some(dims_tag), None)
            .await
            .0,
        StatusCode::OK
    );
    service::upsert_subscription(&state, "wenjun30", Some(300), Some(7200))
        .await
        .unwrap();
    sqlx::query("UPDATE subscriptions SET next_rss_ms=0")
        .execute(&state.pool)
        .await
        .unwrap();
    service::subscription_tick(&state, &fixture_fetcher())
        .await
        .unwrap();
    let (_, _, rss) = conditional_get(&state, uri, None, None).await;
    assert_ne!(rss["stats"], enriched["stats"]);
    assert!(
        count(
            &state,
            "SELECT COUNT(*) FROM subject_jobs WHERE state='queued'"
        )
        .await
            > 0,
        "RSS 新增立即排入富集"
    );
    let profile = weimo_douban::crawler::UserProfile {
        username: "新名字".into(),
        avatar: String::new(),
    };
    service::upsert_user_profile(&state.pool, "wenjun30", &profile)
        .await
        .unwrap();
    let (_, _, changed) = conditional_get(&state, uri, None, None).await;
    assert_ne!(rss["profile"], changed["profile"]);
    service::upsert_user_profile(&state.pool, "wenjun30", &profile)
        .await
        .unwrap();
    assert_eq!(
        conditional_get(&state, uri, None, None).await.2["profile"],
        changed["profile"]
    );
    // Old-page requests cannot be combined with a newer collection snapshot.
    let obsolete = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("category", "movie")
        .append_pair("status", "collect")
        .append_pair("cache_version", versions["movie"].as_str().unwrap())
        .finish();
    assert_eq!(
        conditional_get(
            &state,
            &format!("/api/v1/users/wenjun30/marks?{obsolete}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn conditional_cache_does_not_bypass_auth_or_cache_missing_dimensions() {
    let (_dir, state) = setup_with_token("secret").await;
    let uri = "/api/v1/users/wenjun30/cache-version";
    let (_, headers, _) = conditional_get(&state, uri, None, Some("secret")).await;
    assert_eq!(
        conditional_get(
            &state,
            uri,
            Some(headers[header::ETAG].to_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let missing = "/api/v1/users/wenjun30/subject-dims?category=book&status=collect";
    assert_eq!(
        conditional_get(&state, missing, Some("*"), Some("secret"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    seed_ok_dimension(&state, Category::Book, vec![]).await;
    assert_eq!(
        conditional_get(&state, missing, None, Some("secret"))
            .await
            .0,
        StatusCode::OK
    );
    // Reopening a database preserves versions (and initializes schema idempotently).
    let before = conditional_get(&state, uri, None, Some("secret")).await.2;
    let pool = db::connect(&state.config.db_path()).await.unwrap();
    let reopened = AppState::new(state.config.clone(), pool);
    assert_eq!(
        before,
        conditional_get(&reopened, uri, None, Some("secret"))
            .await
            .2
    );
}

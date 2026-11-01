use serde_json::json;
use tempfile::TempDir;
use weimo_douban::server::{
    db,
    reader::{MarkFilter, Reader},
};

async fn fixture() -> (TempDir, sqlx::SqlitePool, Reader) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("douban_export.db");
    let writer = db::connect(&path).await.unwrap();
    for sql in [
        "INSERT INTO users(uid,username) VALUES('alice','Alice')",
        "INSERT INTO subscriptions(uid,rss_interval_secs,full_interval_secs,next_rss_ms,next_full_ms,created_ms) VALUES('only-sub',3600,86400,0,0,0)",
        "INSERT INTO dimensions(uid,category,dim_status,state,fetched_ms) VALUES('alice','movie','collect','running',1700000000000),('alice','book','wish','ok',1700000000000),('alice','music','collect','error',NULL)",
        "INSERT INTO records(uid,category,dim_status,subject_id,title,my_rating,date,comment,intro) VALUES
            ('alice','movie','collect','1','Old film',5,'2026-04-01','100% 好看',''),
            ('alice','movie','collect','2','New film',3,'2025-12-01','another',''),
            ('alice','movie','collect','3','Fallback',NULL,'2026-03-01','','1999-01-01(美国) / 演员甲 / 美国 / 导演甲 / 100分钟 / 剧情 / 英语'),
            ('alice','book','wish','1','Book',NULL,'2026-01-01','',''),
            ('alice','music','collect','bad','unpublished',5,'2026-01-01','','')",
        "INSERT INTO subjects(category,subject_id,title,year,rating_value,meta_json,fetched_ms) VALUES
            ('movie','1','Old film',1999,8.8,'{\"directors\":[\"导演甲\",\"导演甲\"],\"genres\":[\"剧情\"]}',1700000000000),
            ('movie','2','New film',2026,9.1,'{\"directors\":[\"导演乙\"],\"genres\":[\"喜剧\"]}',1700000000000)",
        "INSERT INTO record_staging SELECT * FROM records WHERE subject_id='1' AND category='movie'",
        "UPDATE record_staging SET title='not published yet'",
    ] { sqlx::query(sql).execute(&writer).await.unwrap(); }
    let reader = Reader::open(&path).await.unwrap();
    (dir, writer, reader)
}
fn filter() -> MarkFilter {
    MarkFilter {
        uid: "alice".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn read_only_rejects_writes_and_missing_or_incompatible_databases() {
    let (dir, writer, _) = fixture().await;
    let path = dir.path().join("douban_export.db");
    let pool = db::connect_read_only(&path).await.unwrap();
    assert!(sqlx::query("DELETE FROM records")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("CREATE TABLE surprise(id)")
        .execute(&pool)
        .await
        .is_err());
    assert!(Reader::open(&dir.path().join("missing.db")).await.is_err());
    assert!(!dir.path().join("missing.db").exists());
    pool.close().await;
    writer.close().await;
    // Also opens after the writing process closes and WAL auxiliaries may be cleaned up.
    assert!(Reader::open(&path).await.is_ok());
    let other = tempfile::tempdir().unwrap();
    let legacy = other.path().join("legacy.db");
    let old = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&legacy)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE records(uid TEXT)")
        .execute(&old)
        .await
        .unwrap();
    assert!(Reader::open(&legacy).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .fetch_one(&old)
        .await
        .unwrap();
    assert_eq!(count, 1, "reader must not bootstrap or migrate");
}

#[tokio::test]
async fn filters_distinguish_mark_date_release_year_and_personal_rating() {
    let (_dir, _writer, reader) = fixture().await;
    let f = MarkFilter {
        category: Some("movie".into()),
        my_rating_min: Some(5),
        marked_from: Some("2026-01-01".into()),
        ..filter()
    };
    let result = reader.search_marks(&f, 20, 0).await.unwrap();
    assert_eq!(result["total"], 1);
    assert_eq!(result["items"][0]["subject_id"], "1");
    assert_eq!(result["items"][0]["release_year"], 1999);
    assert_eq!(result["items"][0]["my_rating"], 5);
    assert_eq!(result["items"][0]["douban_rating"]["value"], 8.8);
    let f = MarkFilter {
        release_year_min: Some(2026),
        ..filter()
    };
    assert_eq!(
        reader.search_marks(&f, 20, 0).await.unwrap()["items"][0]["subject_id"],
        "2"
    );
    let f = MarkFilter {
        query: Some("%".into()),
        ..filter()
    };
    assert_eq!(
        reader.search_marks(&f, 20, 0).await.unwrap()["total"],
        1,
        "literal search, not LIKE wildcard"
    );
    let f = MarkFilter {
        query: Some("' OR 1=1 --".into()),
        ..filter()
    };
    assert_eq!(reader.search_marks(&f, 20, 0).await.unwrap()["total"], 0);
    assert!(reader
        .search_marks(
            &MarkFilter {
                marked_from: Some("2026-02-30".into()),
                ..filter()
            },
            20,
            0
        )
        .await
        .is_err());
    assert!(reader
        .search_marks(
            &MarkFilter {
                my_rating_min: Some(6),
                ..filter()
            },
            20,
            0
        )
        .await
        .is_err());
    assert!(reader.search_marks(&filter(), 101, 0).await.is_err());
}

#[tokio::test]
async fn pagination_facets_and_stats_share_published_data_and_fallbacks() {
    let (_dir, _writer, reader) = fixture().await;
    let all = reader.search_marks(&filter(), 100, 0).await.unwrap();
    assert_eq!(all["total"], 4); // excludes failed first sync and staging
    assert_eq!(all["items"][0]["title"], "Old film");
    let first = reader.search_marks(&filter(), 2, 0).await.unwrap();
    let second = reader.search_marks(&filter(), 2, 2).await.unwrap();
    assert_eq!(first["next_offset"], 2);
    assert!(second["next_offset"].is_null());
    assert_eq!(
        [
            first["items"].as_array().unwrap().clone(),
            second["items"].as_array().unwrap().clone()
        ]
        .concat(),
        *all["items"].as_array().unwrap()
    );
    let f = MarkFilter {
        category: Some("movie".into()),
        facets: vec![("directors".into(), "导演甲".into())],
        ..filter()
    };
    let found = reader.search_marks(&f, 20, 0).await.unwrap();
    assert_eq!(found["total"], 2);
    assert_eq!(found["items"][1]["metadata_source"], "intro");
    assert_eq!(found["items"][1]["release_year"], 1999);
    assert_eq!(
        reader.list_facets(&f, "directors", 20, 0).await.unwrap()["items"],
        json!([{"value":"导演甲","count":2}])
    );
    let stats = reader.get_stats(&filter()).await.unwrap();
    assert_eq!(stats["total"], 4);
    assert_eq!(stats["my_rating_distribution"]["unrated"], 2);
    assert_eq!(stats["by_marked_year"]["2026"], 3);
    let subject = reader.get_subject("movie", "3").await.unwrap();
    assert_eq!(subject["subject"]["source"], "intro");
    assert!(subject["subject"].get("cover").is_none());
    let users = reader.list_users(20, 0).await.unwrap();
    assert_eq!(users["total"], 2);
    assert!(reader.list_users(20, 20).await.unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn reader_observes_commits_while_writer_is_alive_without_seeing_uncommitted_rows() {
    let (_dir, writer, reader) = fixture().await;
    let mut tx = writer.begin().await.unwrap();
    sqlx::query(
        "UPDATE records SET title='committed title' WHERE subject_id='1' AND category='movie'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let f = MarkFilter {
        query: Some("committed title".into()),
        ..filter()
    };
    assert_eq!(reader.search_marks(&f, 20, 0).await.unwrap()["total"], 0);
    tx.commit().await.unwrap();
    assert_eq!(reader.search_marks(&f, 20, 0).await.unwrap()["total"], 1);
}

use rmcp::{
    model::{CallToolRequestParams, ClientConfig},
    transport::TokioChildProcess,
    ServiceExt,
};
use serde_json::{json, Value};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use weimo_douban::server::db;

const BIN: &str = env!("CARGO_BIN_EXE_weimo-douban-mcp");

async fn database() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("MCP 测试 ")
        .tempdir()
        .unwrap();
    let pool = db::connect(&dir.path().join("douban_export.db"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO dimensions(uid,category,dim_status,state,fetched_ms) VALUES('alice','movie','collect','ok',1700000000000)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO records(uid,category,dim_status,subject_id,title,my_rating,date) VALUES('alice','movie','collect','123','测试电影',5,'2026-04-01')").execute(&pool).await.unwrap();
    pool.close().await;
    dir
}

#[tokio::test]
async fn real_stdio_client_discovers_and_calls_all_tools() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let dir = database().await;
        let mut command = Command::new(BIN);
        command
            .arg("--data-dir")
            .arg(dir.path())
            .env("RUST_LOG", "debug");
        let client = ClientConfig::default()
            .serve(TokioChildProcess::new(command).unwrap())
            .await
            .unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 5);
        for tool in &tools {
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(true)
            );
        }
        let search = tools.iter().find(|t| t.name == "search_marks").unwrap();
        let schema = serde_json::to_value(&search.input_schema).unwrap();
        assert!(schema["properties"].get("uid").is_some());
        assert!(schema["properties"].get("marked_from").is_some());
        assert!(schema["properties"].get("limit").is_some());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("uid")));
        for (name, args) in [
            ("list_users", json!({})),
            ("search_marks", json!({"uid":"alice","my_rating_min":5})),
            (
                "get_subject",
                json!({"category":"movie","subject_id":"123"}),
            ),
            ("get_stats", json!({"uid":"alice"})),
            ("list_facets", json!({"uid":"alice","facet":"genres"})),
        ] {
            let response = client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args.as_object().unwrap().clone()),
                )
                .await
                .unwrap();
            assert_eq!(response.is_error, Some(false), "{name}: {response:?}");
            assert!(response.structured_content.is_some());
            assert!(!response.content.is_empty());
        }
        let bad = client
            .call_tool(
                CallToolRequestParams::new("search_marks").with_arguments(
                    json!({"uid":"alice","my_rating_min":6})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(bad.is_error, Some(true));
        client.cancel().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn legacy_protocol_and_eof_work_without_stdout_log_noise() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let dir = database().await;
        let mut child = Command::new(BIN).arg("--data-dir").arg(dir.path())
            .env("RUST_LOG","debug").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"legacy-test","version":"1"}}});
        input.write_all(format!("{init}\n").as_bytes()).await.unwrap();
        let reply: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply["result"]["protocolVersion"],"2025-11-25");
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n").await.unwrap();
        let reply: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply["result"]["tools"].as_array().unwrap().len(),5);
        drop(input);
        assert!(child.wait().await.unwrap().success());
    }).await.unwrap();
}

#[tokio::test]
async fn missing_database_errors_on_stderr_and_config_generation_is_side_effect_free() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not created");
    let output = Command::new(BIN)
        .arg("--data-dir")
        .arg(&missing)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("尚未初始化"));
    assert!(!missing.exists());
    let output = Command::new(BIN)
        .args(["--print-config", "--data-dir"])
        .arg(&missing)
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        config["mcpServers"]["weimo-douban"]["args"][1],
        missing.to_string_lossy().as_ref()
    );
    assert!(!missing.exists());
}

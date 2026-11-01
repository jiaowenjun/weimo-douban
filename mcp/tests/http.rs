#![cfg(feature = "http")]
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use weimo_douban::server::{db, reader::Reader};
use weimo_douban_mcp::{
    http::{router, HttpConfig, MAX_REQUEST_BYTES},
    DoubanMcp,
};

const TOKEN: &str = "test-only-credential-with-at-least-32-bytes";
struct Server {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    url: String,
    stop: CancellationToken,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
async fn server(burst: u32) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("douban_export.db");
    let pool = db::connect(&path).await.unwrap();
    sqlx::query("INSERT INTO dimensions(uid,category,dim_status,state,fetched_ms) VALUES('alice','movie','collect','ok',1700000000000),('bob','movie','collect','ok',1700000000000)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO records(uid,category,dim_status,subject_id,title,my_rating,date) VALUES('alice','movie','collect','123','测试电影',5,'2026-04-01'),('bob','movie','collect','456','Not allowed',4,'2025-01-01')").execute(&pool).await.unwrap();
    let mcp = DoubanMcp::remote(Reader::open(&path).await.unwrap(), vec!["alice".into()]).unwrap();
    let stop = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(
        mcp,
        HttpConfig {
            token: TOKEN.into(),
            allowed_hosts: vec![address.to_string()],
            allowed_origins: vec![],
            requests_per_minute: 60,
            burst,
            max_concurrent_requests: 2,
        },
        stop.child_token(),
    )
    .unwrap();
    let ct = stop.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(ct.cancelled_owned())
            .await
            .unwrap();
    });
    Server {
        _dir: dir,
        pool,
        url: format!("http://{address}/mcp"),
        stop,
    }
}
fn post(server: &Server) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(&server.url)
        .bearer_auth(TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
}
fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})
}

#[tokio::test]
async fn official_clients_support_legacy_initialize_and_stateless_discovery() {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let server = server(60).await;
        for modern in [false, true] {
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(server.url.clone())
                    .auth_header(TOKEN),
            );
            let client = if modern {
                ClientConfig::default()
                    .serve_with_lifecycle(
                        transport,
                        ClientLifecycleMode::Discover {
                            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                        },
                    )
                    .await
                    .unwrap()
            } else {
                ClientConfig::default().serve(transport).await.unwrap()
            };
            let tools = client.list_all_tools().await.unwrap();
            assert_eq!(tools.len(), 5);
            for (name, args) in [
                ("list_users", json!({})),
                (
                    "search_marks",
                    json!({"uid":"alice","category":"movie","marked_from":"2026-01-01","limit":1}),
                ),
                ("get_stats", json!({"uid":"alice","category":"movie"})),
                (
                    "get_subject",
                    json!({"category":"movie","subject_id":"123"}),
                ),
                ("list_facets", json!({"uid":"alice","facet":"genres"})),
            ] {
                let result = client
                    .call_tool(
                        CallToolRequestParams::new(name)
                            .with_arguments(args.as_object().unwrap().clone()),
                    )
                    .await
                    .unwrap();
                assert_eq!(result.is_error, Some(false), "{name}: {result:?}");
                let data = result.structured_content.unwrap();
                assert_eq!(data["data_source"], "server_cache");
                assert_eq!(data["fetches_upstream"], false);
                assert_eq!(data["read_only"], true);
                assert!(data.get("local_only").is_none());
                if name == "list_users" {
                    assert_eq!(data["total"], 1);
                    assert_eq!(data["items"][0]["uid"], "alice");
                }
                if name == "search_marks" {
                    assert_eq!(data["total"], 1);
                }
            }
            client.cancel().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn http_auth_scope_origin_host_body_and_argument_errors_are_preserved() {
    let server = server(60).await;
    for credential in [None, Some("wrong-token"), Some("public-web-token")] {
        let mut request = post(&server).build().unwrap();
        request.headers_mut().remove("Authorization");
        if let Some(token) = credential {
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        }
        request.url_mut().set_query(Some(&format!("token={TOKEN}")));
        let response = reqwest::Client::new().execute(request).await.unwrap();
        assert_eq!(response.status(), 401);
        assert!(response.headers().contains_key("www-authenticate"));
        assert!(!response.text().await.unwrap().contains("alice"));
    }
    let init: Value = post(&server).json(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"legacy-http-test","version":"1"}}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    assert_eq!(
        post(&server)
            .header("Origin", "https://evil.invalid")
            .json(&list)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        post(&server)
            .header("Host", "evil.invalid")
            .json(&list)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        post(&server)
            .header("Content-Type", "application/json")
            .body("x".repeat(MAX_REQUEST_BYTES + 1))
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    for (name, args) in [
        ("search_marks", json!({"uid":"bob"})),
        ("get_stats", json!({"uid":"bob"})),
        ("list_facets", json!({"uid":"bob","facet":"genres"})),
        (
            "get_subject",
            json!({"category":"movie","subject_id":"456"}),
        ),
    ] {
        let response: Value = post(&server)
            .json(&call(name, args))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(!response.to_string().contains("Not allowed"));
    }
    let missing: Value = post(&server)
        .json(&call("search_marks", json!({})))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        missing.get("error").is_some() || missing["result"]["isError"] == true,
        "{missing}"
    );
    let response = reqwest::Client::new()
        .post(format!("{}/", server.url))
        .bearer_auth(TOKEN)
        .header("Accept", "application/json, text/event-stream")
        .json(&list)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(!response.headers().contains_key("location"));
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn rate_and_result_size_limits_return_explicit_errors() {
    let limited = server(1).await;
    let request = call("get_stats", json!({"uid":"alice"}));
    assert_eq!(
        post(&limited).json(&request).send().await.unwrap().status(),
        200
    );
    let response = post(&limited).json(&request).send().await.unwrap();
    assert_eq!(response.status(), 429);
    assert!(response.headers().contains_key("retry-after"));
    let server = server(10).await;
    sqlx::query("UPDATE records SET comment=? WHERE uid='alice'")
        .bind("长".repeat(100_000))
        .execute(&server.pool)
        .await
        .unwrap();
    let response: Value = post(&server)
        .json(&call("search_marks", json!({"uid":"alice"})))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(response.to_string().contains("256 KiB"));
}

#[tokio::test]
async fn cli_refuses_missing_scope_token_and_public_listener() {
    let bin = env!("CARGO_BIN_EXE_weimo-douban-mcp-http");
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["--data-dir", dir.path().to_str().unwrap()],
        vec![
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--token-file",
            "/missing",
            "--allowed-uids",
            "alice",
            "--allowed-hosts",
            "example.com",
            "--bind",
            "0.0.0.0:8021",
        ],
    ] {
        let output = tokio::process::Command::new(bin)
            .env_clear()
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

//! `/douban/` 前端静态资源——frontend/web 构建产物（vite base=/douban/）在
//! 编译期经 rust-embed 进二进制（与 app/ Tauri 版同款装配）：dev 与打包后
//! 行为一致，运行期零路径解析，App 即整栈原子快照（API 与静态资源同版本，
//! 不依赖磁盘上的 dist）。

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../frontend/web/dist"]
struct WebDist;

pub fn router() -> Router {
    Router::new()
        .route("/", any(serve_asset))
        .route("/{*path}", any(serve_asset))
}

/// HashRouter SPA：非空路径精确取资源文件；空路径或未命中回落 index.html
/// （Hash 路由下浏览器只会请求 / 与 /assets/...，回落仅作兜底）
async fn serve_asset(uri: Uri) -> Response {
    let path = uri.path().trim_matches('/');
    let (data, name) = match (!path.is_empty())
        .then(|| WebDist::get(path))
        .flatten()
    {
        Some(file) => (file.data, path.to_string()),
        None => match WebDist::get("index.html") {
            Some(file) => (file.data, "index.html".to_owned()),
            None => {
                return (StatusCode::NOT_FOUND, "前端资源未嵌入（dist 未构建？）").into_response()
            }
        },
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    (StatusCode::OK, [(header::CONTENT_TYPE, mime.as_ref())], data).into_response()
}

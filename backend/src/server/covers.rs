//! 封面代理与磁盘缓存——cache-then-serve。
//!
//! 豆瓣图片 CDN 各子域名 Referer 防盗链策略不一致：浏览器直连时 img9 放行、
//! img2/img3 一律 403（2026-10 实测），导致前端封面大量灰块。本模块在服务端
//! 带豆瓣 Referer 有限重试并落盘 `data/covers/`，之后永久本地化：
//! 存量记录首次访问即自动"回填"，浏览不再外呼豆瓣、离线可用。
//!
//! 回源存在偶发瞬时失败（网络错误/空响应/限流类状态码），采取有限重试：
//! 仅对可重试类别、在总预算内、按随机退避（429/503 尊重 Retry-After）再试，
//! 每次失败均落 tracing 日志（图片标识、失败类型、上游状态、排队与请求
//! 耗时），便于事后确认真实失败原因。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::Semaphore;

use super::error::AppError;
use crate::crawler::UA;

/// 允许回源的路径前缀（库内三类豆瓣图：电影海报 /view/photo/、书音封面
/// /view/subject/、用户头像 /icon/——浏览器直连头像域 418，必须代理）
const ALLOWED_PATH_PREFIXES: [&str; 3] = ["/view/photo/", "/view/subject/", "/icon/"];

/// 回源时伪装的豆瓣来源页（实测 www/movie 域 Referer 对全部 img 子域名均放行）
const ORIGIN_REFERER: &str = "https://www.douban.com/";

/// 回源总尝试次数（初次 + 2 次重试）
const MAX_ATTEMPTS: u32 = 3;
/// 回源阶段总预算：与单次请求超时一致，重试不得把最坏等待成倍放大。
/// 预算从拿到回源并发许可后起算（本地排队不占用）。
const ORIGIN_BUDGET: Duration = Duration::from_secs(15);
/// 预算内发起下一次尝试所需的最小剩余时间（不足则放弃重试）
const MIN_ATTEMPT_SLICE: Duration = Duration::from_secs(2);
/// 基础退避下限（毫秒），按尝试次数线性放大；叠加 0..BACKOFF_JITTER_MS
/// 随机抖动，错开同批失败封面的再次回源时机
const BACKOFF_BASE_MS: u64 = 300;
const BACKOFF_JITTER_MS: u64 = 500;
#[derive(Clone)]
pub struct CoverService {
    root: PathBuf,
    client: reqwest::Client,
    /// 回源并发上限——豆瓣 CDN 对快速连发敏感（实测连续请求时偶发空响应），
    /// 限额排队避免一页 20 张封面同时回源触发限流
    origin_slots: Arc<Semaphore>,
    tmp_seq: Arc<AtomicU64>,
}

pub struct Cover {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

impl CoverService {
    pub fn new(data_dir: &Path, concurrency: u32) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("build reqwest client");
        Self {
            root: data_dir.join("covers"),
            client,
            origin_slots: Arc::new(Semaphore::new(concurrency.max(1) as usize)),
            tmp_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 取封面：磁盘命中直接返回；未命中则限流回源（有限重试）、落盘后返回。
    pub async fn get(&self, url: &str) -> Result<Cover, AppError> {
        let (url, filename, content_type) = parse_cover_url(url)?;
        let path = self.root.join(&filename);
        if let Some(bytes) = read_nonempty(&path).await {
            return Ok(Cover {
                bytes,
                content_type,
            });
        }

        let queue_start = Instant::now();
        let _permit = self.origin_slots.acquire().await.expect("semaphore 未关闭");
        let queued_for = queue_start.elapsed();
        // 拿到许可后复查：并发同图时后到者直接命中先到者落盘的结果
        if let Some(bytes) = read_nonempty(&path).await {
            return Ok(Cover {
                bytes,
                content_type,
            });
        }

        let bytes = self.fetch_with_retry(&url, &filename, queued_for).await?;
        self.save(&filename, &bytes).await?;
        Ok(Cover {
            bytes,
            content_type,
        })
    }

    /// 回源单次尝试：带豆瓣 Referer 请求并做内容防御（非图片/空响应均算失败）。
    async fn fetch_once(&self, url: &str) -> Result<Vec<u8>, FetchFailure> {
        let resp = self
            .client
            .get(url)
            .header(reqwest::header::REFERER, ORIGIN_REFERER)
            .send()
            .await
            .map_err(|e| FetchFailure::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            // 仅限流相关状态可能携带有意义的 Retry-After，其余不解析
            let retry_after = if matches!(status.as_u16(), 429 | 503) {
                resp.headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after)
            } else {
                None
            };
            return Err(FetchFailure::Status {
                status,
                retry_after,
            });
        }
        // 防御错误页落盘：只接受声明为图片的响应
        let upstream_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !upstream_type.starts_with("image/") {
            return Err(FetchFailure::NotImage(upstream_type));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| FetchFailure::Network(e.to_string()))?
            .to_vec();
        if bytes.is_empty() {
            return Err(FetchFailure::Empty);
        }
        Ok(bytes)
    }

    /// 回源调度：总预算内对可重试失败按退避重试，每次失败落结构化日志。
    async fn fetch_with_retry(
        &self,
        url: &str,
        filename: &str,
        queued_for: Duration,
    ) -> Result<Vec<u8>, AppError> {
        let origin_start = Instant::now();
        let mut attempt: u32 = 1;
        loop {
            let req_start = Instant::now();
            // 把单次请求也限制在剩余预算内；reqwest 自身的 15s timeout
            // 不能保证重试链的总耗时不超过 ORIGIN_BUDGET。
            let remaining = ORIGIN_BUDGET.saturating_sub(origin_start.elapsed());
            let fetch_result = match tokio::time::timeout(remaining, self.fetch_once(url)).await {
                Err(_) => Err(FetchFailure::Network("回源总预算超时".to_string())),
                Ok(result) => result,
            };
            match fetch_result {
                Ok(bytes) => {
                    if attempt > 1 {
                        tracing::info!(
                            file = filename,
                            attempt,
                            queued_ms = queued_for.as_millis() as u64,
                            total_ms = origin_start.elapsed().as_millis() as u64,
                            "封面回源重试后成功"
                        );
                    } else {
                        tracing::debug!(
                            file = filename,
                            queued_ms = queued_for.as_millis() as u64,
                            request_ms = req_start.elapsed().as_millis() as u64,
                            "封面回源成功"
                        );
                    }
                    return Ok(bytes);
                }
                Err(failure) => {
                    let request_ms = req_start.elapsed().as_millis() as u64;
                    let jitter = rand::thread_rng().gen_range(0..BACKOFF_JITTER_MS);
                    let delay = retry_delay(&failure, attempt, origin_start.elapsed(), jitter);
                    tracing::warn!(
                        file = filename,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        fail = failure.kind(),
                        detail = %failure.detail(),
                        queued_ms = queued_for.as_millis() as u64,
                        request_ms,
                        retry_in_ms = delay.map(|d| d.as_millis() as u64),
                        "封面回源失败"
                    );
                    let Some(delay) = delay else {
                        return Err(failure.into_upstream());
                    };
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    /// 落盘：写临时文件后原子改名，避免并发读到半截内容
    async fn save(&self, filename: &str, bytes: &[u8]) -> Result<(), AppError> {
        tokio::fs::create_dir_all(&self.root).await?;
        let tmp = self.root.join(format!(
            "{filename}.tmp-{}",
            self.tmp_seq.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::write(&tmp, bytes).await?;
        if tokio::fs::rename(&tmp, self.root.join(filename))
            .await
            .is_err()
        {
            // 并发下他人已抢先改名，本份内容一致，丢弃即可
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        Ok(())
    }
}

/// 单次回源失败（内部分类，用于重试决策与日志；对外统一转 CoverUpstream）
enum FetchFailure {
    /// 网络/超时错误（reqwest 传输层，含响应体读取中断）
    Network(String),
    /// 非 2xx 上游状态；429/503 附带解析出的 Retry-After
    Status {
        status: reqwest::StatusCode,
        retry_after: Option<Duration>,
    },
    /// 响应声明非 image/*（典型为防盗链/风控 HTML 错误页——确定性响应，重试无意义）
    NotImage(String),
    /// 成功状态但响应体为空
    Empty,
}

impl FetchFailure {
    /// 失败类型短名（日志 fail 字段）
    fn kind(&self) -> &'static str {
        match self {
            FetchFailure::Network(_) => "network",
            FetchFailure::Status { .. } => "http_status",
            FetchFailure::NotImage(_) => "not_image",
            FetchFailure::Empty => "empty_body",
        }
    }

    /// 失败详情（日志 detail 字段：状态码或错误描述）
    fn detail(&self) -> String {
        match self {
            FetchFailure::Network(e) => e.clone(),
            FetchFailure::Status { status, .. } => format!("HTTP {}", status.as_u16()),
            FetchFailure::NotImage(t) => format!("content-type: {t}"),
            FetchFailure::Empty => "0 bytes".to_string(),
        }
    }

    /// 是否值得重试：网络错误/空响应/限流类 5xx 与 429 为瞬时性失败；
    /// 403/418 等防盗链与错误页是确定性响应，重试无意义
    fn retryable(&self) -> bool {
        match self {
            FetchFailure::Network(_) | FetchFailure::Empty => true,
            FetchFailure::Status { status, .. } => {
                matches!(status.as_u16(), 429 | 502 | 503 | 504)
            }
            FetchFailure::NotImage(_) => false,
        }
    }

    fn into_upstream(self) -> AppError {
        match self {
            FetchFailure::Network(e) => AppError::CoverUpstream(format!("网络错误: {e}")),
            FetchFailure::Status { status, .. } => {
                AppError::CoverUpstream(format!("HTTP {}", status.as_u16()))
            }
            FetchFailure::NotImage(t) => AppError::CoverUpstream(format!("非图片响应: {t}")),
            FetchFailure::Empty => AppError::CoverUpstream("空响应".to_string()),
        }
    }
}

/// 解析 Retry-After 响应头（仅支持秒数形式；HTTP-date 形式不解析，
/// 视作无指示回落普通退避）。保留 0 和超长值，让调度器根据实际剩余
/// 预算决定是否能够等待；不能把超长值降级成短退避。
fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// 重试决策：第 attempt 次（1 起）失败后，返回到下一次尝试的等待时间；
/// None 表示不再重试。普通退避按尝试次数线性放大并叠加 jitter 抖动、
/// 不超过预算余量；Retry-After 则尊重上游指示——预算内等不起时放弃
/// （而不是缩短等待强行重试）。jitter 注入而非内部随机，便于测试。
fn retry_delay(
    failure: &FetchFailure,
    attempt: u32,
    elapsed: Duration,
    jitter: u64,
) -> Option<Duration> {
    if attempt >= MAX_ATTEMPTS || !failure.retryable() {
        return None;
    }
    let remaining = ORIGIN_BUDGET.checked_sub(elapsed)?;
    if remaining < MIN_ATTEMPT_SLICE {
        return None;
    }
    let affordable = remaining - MIN_ATTEMPT_SLICE;
    let wanted = match failure {
        FetchFailure::Status {
            retry_after: Some(ra),
            ..
        } => {
            if *ra > affordable {
                return None;
            }
            *ra
        }
        _ => {
            Duration::from_millis(BACKOFF_BASE_MS * u64::from(attempt) + jitter % BACKOFF_JITTER_MS)
        }
    };
    Some(wanted.min(affordable))
}

/// 校验封面 URL 并解析出（规范化 URL、缓存文件名、Content-Type）。
/// 白名单必须收紧：本端点本质是代理，放任意 URL 会变成开放中继。
pub fn parse_cover_url(url: &str) -> Result<(String, String, &'static str), AppError> {
    let bad = |msg: &str| AppError::InvalidArgument(format!("非法封面 URL（{msg}）"));

    let parsed = url::Url::parse(url).map_err(|_| bad("无法解析"))?;
    if parsed.scheme() != "https" {
        return Err(bad("仅允许 https"));
    }
    let Some(host) = parsed.host_str() else {
        return Err(bad("缺少主机名"));
    };
    if !host.ends_with(".doubanio.com") {
        return Err(bad("仅允许 doubanio.com 图片域"));
    }
    let path = parsed.path();
    if !ALLOWED_PATH_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return Err(bad("路径前缀不在白名单"));
    }
    let filename = path.rsplit('/').next().unwrap_or_default();
    let Some(content_type) = content_type_for(filename) else {
        return Err(bad("文件名或扩展名非法"));
    };
    Ok((parsed.to_string(), filename.to_string(), content_type))
}

/// 缓存文件名合法性：非空、不以点开头、仅字母数字与 `-_`、扩展名受限。
/// 同时天然排除路径分隔符与 `..` 穿越。
fn content_type_for(filename: &str) -> Option<&'static str> {
    let (stem, ext) = filename.rsplit_once('.')?;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    match ext.to_ascii_lowercase().as_str() {
        "webp" => Some("image/webp"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

async fn read_nonempty(path: &Path) -> Option<Vec<u8>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// 豆瓣「无封面」兜底图判定：收藏页对无封面条目返回 asset.doubanio.com
/// 上的静态占位图（现知 4 种：book-default-lpic.gif / movie_default_small.png /
/// tv_default_small.png / music-default-lpic.*.gif），真实封面与头像都在
/// img*.doubanio.com。按 host 精确匹配——封面位出现该域即兜底图，天然覆盖
/// 未来新增变体；不放宽到任意 doubanio 域，避免误吞未来真实封面域。
fn is_douban_default_cover(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "asset.doubanio.com"))
        .unwrap_or(false)
}

/// 把库里的豆瓣封面 URL 改写为本地代理端点；空值原样返回，豆瓣无封面
/// 兜底图归一为空串（前端按无封面渲染 CSS 占位区，不代理豆瓣占位图——
/// 其路径 /cuphead/ 不在白名单，代理只会确定性 400）。
/// 相对路径，前端经自身反向代理（dev 下 vite proxy）转发到本服务。
pub fn cover_proxy_url(cover: &str) -> String {
    if cover.is_empty() || is_douban_default_cover(cover) {
        return String::new();
    }
    format!("/api/v1/covers?u={}", encode_query_component(cover))
}

/// 查询参数百分号编码（保留 RFC 3986 unreserved 字符）
pub fn encode_query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_known_cover_shapes() {
        let (url, name, ct) = parse_cover_url(
            "https://img3.doubanio.com/view/photo/s_ratio_poster/public/p2935834896.webp",
        )
        .unwrap();
        assert_eq!(name, "p2935834896.webp");
        assert_eq!(ct, "image/webp");
        assert_eq!(
            url.as_str(),
            "https://img3.doubanio.com/view/photo/s_ratio_poster/public/p2935834896.webp"
        );

        let (_, name, ct) =
            parse_cover_url("https://img2.doubanio.com/view/subject/s/public/s27018761.jpg")
                .unwrap();
        assert_eq!(name, "s27018761.jpg");
        assert_eq!(ct, "image/jpeg");
        // 用户头像（文件名带版本号后缀，stem 校验允许 `-`）
        let (_, name, ct) =
            parse_cover_url("https://img2.doubanio.com/icon/u189180823-11.jpg").unwrap();
        assert_eq!(name, "u189180823-11.jpg");
        assert_eq!(ct, "image/jpeg");
    }

    #[test]
    fn parse_rejects_non_whitelisted() {
        // 非豆瓣域（开放中继风险）
        assert!(parse_cover_url("https://evil.com/view/photo/p1.webp").is_err());
        // http 明文
        assert!(parse_cover_url("http://img3.doubanio.com/view/photo/p1.webp").is_err());
        // 路径前缀不在白名单
        assert!(parse_cover_url("https://img3.doubanio.com/secret/p1.webp").is_err());
        assert!(parse_cover_url("https://img3.doubanio.com/img/music/p1.webp").is_err());
        // 路径穿越 / 无扩展名 / 非法扩展名 / 点开头
        assert!(parse_cover_url("https://img3.doubanio.com/view/photo/../../etc/passwd").is_err());
        assert!(parse_cover_url("https://img3.doubanio.com/view/photo/public/p1").is_err());
        assert!(parse_cover_url("https://img3.doubanio.com/view/photo/public/p1.html").is_err());
        assert!(
            parse_cover_url("https://img3.doubanio.com/view/photo/public/.hidden.webp").is_err()
        );
        // 无法解析
        assert!(parse_cover_url("not a url").is_err());
    }

    #[test]
    fn proxy_url_and_encoding() {
        assert_eq!(cover_proxy_url(""), "");
        assert_eq!(
            cover_proxy_url("https://img3.doubanio.com/view/photo/s/public/p1.webp"),
            "/api/v1/covers?u=https%3A%2F%2Fimg3.doubanio.com%2Fview%2Fphoto%2Fs%2Fpublic%2Fp1.webp"
        );
        assert_eq!(encode_query_component("aZ09-._~"), "aZ09-._~");
    }

    #[test]
    fn proxy_url_empty_for_douban_default_covers() {
        // 豆瓣无封面兜底图（asset.doubanio.com 静态资源域，线上实测 4 种形态）
        // 归一为空串，前端按无封面渲染 CSS 占位区
        for u in [
            "https://asset.doubanio.com/cuphead/book-static/pics/book-default-lpic.gif",
            "https://asset.doubanio.com/cuphead/movie-static/pics/movie_default_small.png",
            "https://asset.doubanio.com/cuphead/movie-static/pics/tv_default_small.png",
            "https://asset.doubanio.com/cuphead/vendors/pics/music-default-lpic.eac158170058.gif",
        ] {
            assert_eq!(cover_proxy_url(u), "", "兜底图应归一为空: {u}");
        }
        // 真实封面与头像（img*.doubanio.com）不受影响
        assert!(cover_proxy_url("https://img2.doubanio.com/view/subject/s/public/s27018761.jpg")
            .starts_with("/api/v1/covers?u="));
        assert!(cover_proxy_url("https://img9.doubanio.com/icon/u145680993-334.webp")
            .starts_with("/api/v1/covers?u="));
    }

    fn status_failure(code: u16, retry_after: Option<Duration>) -> FetchFailure {
        FetchFailure::Status {
            status: reqwest::StatusCode::from_u16(code).expect("合法状态码"),
            retry_after,
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(" 10 "), Some(Duration::from_secs(10)));
        assert_eq!(parse_retry_after("60"), Some(Duration::from_secs(60)));
        // 0 秒表示立即重试；超长值保留，由预算决策层放弃；
        // HTTP-date 形式 / 垃圾串无法按本模块策略解析
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("61"), Some(Duration::from_secs(61)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn retry_delay_skips_non_retryable() {
        // 防盗链 403 与错误页是确定性响应，重试无意义
        let not_image = FetchFailure::NotImage("text/html".to_string());
        assert_eq!(retry_delay(&not_image, 1, Duration::ZERO, 100), None);
        let forbidden = status_failure(403, None);
        assert_eq!(retry_delay(&forbidden, 1, Duration::ZERO, 100), None);
        // 尝试次数用尽
        let network = FetchFailure::Network("timeout".to_string());
        assert_eq!(
            retry_delay(&network, MAX_ATTEMPTS, Duration::ZERO, 100),
            None
        );
    }

    #[test]
    fn retry_delay_backoff_shape() {
        // 普通退避：基础 × 尝试次数 + 抖动
        let network = FetchFailure::Network("connect error".to_string());
        assert_eq!(
            retry_delay(&network, 1, Duration::ZERO, 200),
            Some(Duration::from_millis(300 + 200))
        );
        assert_eq!(
            retry_delay(&network, 2, Duration::ZERO, 499),
            Some(Duration::from_millis(600 + 499))
        );
        // 502/504 无 Retry-After 指示时同样走普通退避
        assert_eq!(
            retry_delay(&status_failure(502, None), 1, Duration::ZERO, 0),
            Some(Duration::from_millis(300))
        );
    }

    #[test]
    fn retry_delay_respects_budget() {
        let network = FetchFailure::Network("timeout".to_string());
        // 剩余不足最小尝试切片：放弃
        assert_eq!(retry_delay(&network, 1, Duration::from_secs(14), 100), None);
        // 已超出总预算：放弃
        assert_eq!(retry_delay(&network, 1, Duration::from_secs(20), 100), None);
        // 剩余恰为最小切片：可试但无退避空间（0ms 立即重试）
        assert_eq!(
            retry_delay(&network, 1, Duration::from_secs(13), 100),
            Some(Duration::ZERO)
        );
        // 退避 clamp 到预算余量（elapsed 12.5s → 剩余 2.5s，扣除最小切片后
        // 可支配 0.5s，普通退避 799ms 被压到 500ms）
        assert_eq!(
            retry_delay(&network, 1, Duration::from_millis(12_500), 499),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn retry_delay_respects_retry_after() {
        // 预算等得起：按上游指示等待
        assert_eq!(
            retry_delay(
                &status_failure(429, Some(Duration::from_secs(5))),
                1,
                Duration::ZERO,
                100
            ),
            Some(Duration::from_secs(5))
        );
        // 预算等不起上游要求：放弃而不是缩短等待强行重试
        // （elapsed 10s → 剩余 5s，扣除最小切片后可支配 3s < 5s）
        assert_eq!(
            retry_delay(
                &status_failure(503, Some(Duration::from_secs(5))),
                1,
                Duration::from_secs(10),
                100
            ),
            None
        );
        // 超长 Retry-After 也必须放弃，不能退化为普通的 300ms 退避
        assert_eq!(
            retry_delay(
                &status_failure(503, Some(Duration::from_secs(61))),
                1,
                Duration::ZERO,
                100
            ),
            None
        );
        // Retry-After: 0 表示允许立即重试
        assert_eq!(
            retry_delay(
                &status_failure(429, Some(Duration::ZERO)),
                1,
                Duration::ZERO,
                100
            ),
            Some(Duration::ZERO)
        );
    }
}

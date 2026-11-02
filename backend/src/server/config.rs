//! 服务器配置——全部经 clap 参数 + 环境变量（weimo 惯例）。

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "weimo-douban-server",
    about = "免登录豆瓣书影音同步服务器（抓取入库 + API 查询）"
)]
pub struct ServerConfig {
    /// 监听地址
    #[arg(long, env = "SYNC_SERVER_BIND", default_value = "0.0.0.0:8080")]
    pub bind: String,

    /// 数据目录（SQLite 数据库）
    #[arg(long, env = "SYNC_SERVER_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,

    /// Bearer Token（留空则不启用鉴权）
    #[arg(long, env = "SYNC_SERVER_TOKEN", default_value = "")]
    pub token: String,

    /// 豆瓣登录 Cookie（如 'dbcl2="xxx"'；留空则免登录抓取，隐藏条目不可见）
    #[arg(long, env = "SYNC_DOUBAN_COOKIE", default_value = "")]
    pub douban_cookie: String,

    /// 允许的 CORS 来源（逗号分隔，留空关闭）
    #[arg(long, env = "SYNC_SERVER_CORS_ORIGINS", default_value = "")]
    pub cors_origins: String,

    /// 缓存 TTL（秒）
    #[arg(long, env = "SYNC_CACHE_TTL_SECS", default_value_t = 86_400)]
    pub cache_ttl_secs: i64,

    /// 豆瓣抓取最小间隔（秒）
    #[arg(long, env = "SYNC_CRAWL_MIN_INTERVAL_SECS", default_value_t = 2.0)]
    pub crawl_min_interval_secs: f64,

    /// 熔断阈值（连续失败维度次数）
    #[arg(long, env = "SYNC_CIRCUIT_THRESHOLD", default_value_t = 5)]
    pub circuit_threshold: u32,

    /// 熔断冷却（秒）
    #[arg(long, env = "SYNC_CIRCUIT_COOLDOWN_SECS", default_value_t = 600)]
    pub circuit_cooldown_secs: i64,

    /// 任务记录保留天数（records 表数据不随任务过期）
    #[arg(long, env = "SYNC_TASK_RETENTION_DAYS", default_value_t = 30)]
    pub task_retention_days: i64,

    /// 队列长度上限（排队中的维度数）
    #[arg(long, env = "SYNC_QUEUE_LIMIT", default_value_t = 100)]
    pub queue_limit: i64,

    /// 每分钟每 IP 请求上限。默认须覆盖首发同步高峰：3s 版本轮询 +
    /// 每页落库/每条富集 bump 触发 dims/marks/stats 全家重拉，实测峰
    /// 值 ~140/min（封面端点豁免不计），60 会把合法用户同步期间打出
    /// 429——前端已把 429 归瞬态，这里再留 2 倍余量
    #[arg(long, env = "SYNC_RATE_PER_MINUTE", default_value_t = 300)]
    pub rate_per_minute: u32,

    /// 订阅默认 RSS 增量轮询间隔（秒），创建订阅时未指定则用此值
    #[arg(long, env = "SYNC_SUB_RSS_INTERVAL_SECS", default_value_t = 3_600)]
    pub sub_rss_interval_secs: i64,

    /// 订阅默认全量同步间隔（秒）
    #[arg(long, env = "SYNC_SUB_FULL_INTERVAL_SECS", default_value_t = 86_400)]
    pub sub_full_interval_secs: i64,

    /// 封面代理并发回源上限
    #[arg(long, env = "SYNC_COVER_CONCURRENCY", default_value_t = 4)]
    pub cover_concurrency: u32,

    /// 条目元信息富集总开关（rexxar 结构化详情；关闭则行为与旧版一致）
    #[arg(long, env = "SYNC_SUBJECT_ENRICH", default_value_t = true)]
    pub subject_enrich: bool,

    /// 条目过期提示阈值（秒）；仅供人工 stale 重排筛选，不自动重抓
    #[arg(long, env = "SYNC_SUBJECT_TTL_SECS", default_value_t = 2_592_000)]
    pub subject_ttl_secs: i64,

    /// rexxar 相邻请求最小间隔（秒），与抓取间隔取较大值生效
    #[arg(long, env = "SYNC_SUBJECT_MIN_INTERVAL_SECS", default_value_t = 3.0)]
    pub subject_min_interval_secs: f64,

    /// movie 独立间隔下限；未设置时沿用通用值，45–60s 仅作为对照实验候选
    #[arg(long, env = "SYNC_SUBJECT_MOVIE_MIN_INTERVAL_SECS")]
    pub subject_movie_min_interval_secs: Option<f64>,

    /// movie 无有效历史时的初始间隔；未设置时使用该类别下限
    #[arg(long, env = "SYNC_SUBJECT_MOVIE_INITIAL_INTERVAL_SECS")]
    pub subject_movie_initial_interval_secs: Option<f64>,

    /// 持久化间隔的有效期；超期重新使用初始值，但仍保留尚未结束的冷却
    #[arg(long, env = "SYNC_SUBJECT_PACING_TTL_SECS", default_value_t = 86_400)]
    pub subject_pacing_ttl_secs: i64,

    /// 实验选项：冷却后首个请求仍被拒绝时递增冷却，默认关闭
    #[arg(long, env = "SYNC_SUBJECT_ADAPTIVE_COOLDOWN", default_value_t = false)]
    pub subject_adaptive_cooldown: bool,

    /// rexxar 单端点（category 维度）限流后的类别冷却（秒）。实测 movie 端点
    /// 连发 4-8 个请求即 400 封锁约 5-6 分钟（404 同样累积触发），封锁期间
    /// 该类别全部富集 job 推迟到冷却期后，music/book/维度抓取不受影响
    #[arg(
        long,
        env = "SYNC_SUBJECT_CATEGORY_COOLDOWN_SECS",
        default_value_t = 300
    )]
    pub subject_category_cooldown_secs: i64,

    /// 提供公共条目缓存（独立只读连接、独立限流；默认不开放）
    #[arg(long, env = "SYNC_SUBJECT_CACHE_SERVE", default_value_t = false)]
    pub subject_cache_serve: bool,

    /// 可选中心缓存的完整 batch URL；空字符串关闭，服务器默认不使用
    #[arg(long, env = "SYNC_SUBJECT_CACHE_URL")]
    pub subject_cache_url: Option<String>,

    /// 仅桌面入口设置，允许本机持久化缓存偏好；不接受远程 CLI 开启
    #[arg(skip)]
    pub desktop_app: bool,
}

impl ServerConfig {
    pub fn subject_floor(&self, category: &str) -> f64 {
        let value = if category == "movie" {
            self.subject_movie_min_interval_secs
                .unwrap_or(self.subject_min_interval_secs)
        } else {
            self.subject_min_interval_secs
        };
        if value.is_finite() {
            value.clamp(0.5, 90.0)
        } else {
            3.0
        }
    }

    pub fn subject_initial(&self, category: &str) -> f64 {
        let floor = self.subject_floor(category);
        let initial = if category == "movie" {
            self.subject_movie_initial_interval_secs.unwrap_or(floor)
        } else {
            floor
        };
        if initial.is_finite() {
            initial.clamp(floor, 90.0)
        } else {
            floor
        }
    }

    pub fn configure_desktop(&mut self) {
        self.desktop_app = true;
        self.subject_cache_serve = false;
        self.subject_cache_url
            .get_or_insert_with(|| "https://weimo.ink/douban/api/v1/subjects/batch".to_owned());
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("douban_export.db")
    }

    /// 现在的 epoch 毫秒
    pub fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

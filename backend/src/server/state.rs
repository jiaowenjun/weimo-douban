//! 应用状态——配置、连接池、worker 唤醒通道、熔断计数、客户端限流。

use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, watch};

use super::config::ServerConfig;
use super::covers::CoverService;

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub pool: SqlitePool,
    pub covers: CoverService,
    pub projections: Arc<tokio::sync::Mutex<super::cache::ProjectionCache>>,
    pub subject_cache: Option<Arc<super::subject_cache::CacheServer>>,
    pub subject_cache_client: Option<Arc<super::subject_cache_client::CacheClient>>,
    worker_wakeup_tx: watch::Sender<u64>,
    /// 维度进度变更通知（载荷为 task_id），SSE 订阅端按 task_id 过滤
    progress_tx: broadcast::Sender<String>,
    /// 连续失败次数（豆瓣侧）——Arc 以便各 clone 共享熔断状态
    consecutive_failures: Arc<AtomicU32>,
    /// 熔断截止时间（epoch ms）
    blocked_until_ms: Arc<AtomicI64>,
    /// rexxar 各端点（category 维度）的自适应请求间隔秒数（AIMD，见下）。
    /// 实例级（非进程级 static）：并行测试的 AppState 互不污染
    subject_pacing: Arc<Mutex<HashMap<String, f64>>>,
    /// verify 端点按 uid 的上次尝试时间（epoch ms）——只约束豆瓣抓取
    /// 路径（本地捷径命中不受限），失败也记录，堵无效 ID 反复重试
    verify_attempts: Arc<Mutex<HashMap<String, i64>>>,
    rate_table: &'static Mutex<HashMap<IpAddr, (u64, u32)>>,
}

impl AppState {
    pub fn new(config: ServerConfig, pool: SqlitePool) -> Self {
        let (worker_wakeup_tx, _rx) = watch::channel(0);
        let (progress_tx, _) = broadcast::channel(256);
        static RATE_TABLE: OnceLock<Mutex<HashMap<IpAddr, (u64, u32)>>> = OnceLock::new();
        Self {
            subject_cache: config
                .subject_cache_serve
                .then(|| Arc::new(super::subject_cache::CacheServer::new(&config.db_path()))),
            subject_cache_client: super::subject_cache_client::CacheClient::from_config(&config)
                .map(Arc::new),
            config: config.clone(),
            pool,
            projections: Arc::new(tokio::sync::Mutex::new(Default::default())),
            covers: CoverService::new(&config.data_dir, config.cover_concurrency),
            worker_wakeup_tx,
            progress_tx,
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            blocked_until_ms: Arc::new(AtomicI64::new(0)),
            subject_pacing: Arc::new(Mutex::new(HashMap::new())),
            verify_attempts: Arc::new(Mutex::new(HashMap::new())),
            rate_table: RATE_TABLE.get_or_init(|| Mutex::new(HashMap::new())),
        }
    }

    pub fn worker_wakeup_receiver(&self) -> watch::Receiver<u64> {
        self.worker_wakeup_tx.subscribe()
    }

    pub fn wake_worker(&self) {
        self.worker_wakeup_tx
            .send_modify(|v| *v = v.wrapping_add(1));
    }

    /// 广播某任务进度有更新（无订阅者时发送被忽略，无副作用）
    pub fn notify_progress(&self, task_id: &str) {
        let _ = self.progress_tx.send(task_id.to_string());
    }

    pub fn subscribe_progress(&self) -> broadcast::Receiver<String> {
        self.progress_tx.subscribe()
    }

    // ---- 熔断 ----

    pub fn record_crawl_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    /// 返回是否触发熔断
    pub fn record_crawl_failure(&self) -> bool {
        let n = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= self.config.circuit_threshold {
            let until = ServerConfig::now_ms() + self.config.circuit_cooldown_secs * 1000;
            self.blocked_until_ms.store(until, Ordering::Relaxed);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            tracing::error!(
                consecutive_failures = n,
                cooldown_secs = self.config.circuit_cooldown_secs,
                "豆瓣侧连续失败，熔断开启"
            );
            return true;
        }
        false
    }

    /// 熔断剩余毫秒（0 = 未熔断）
    pub fn circuit_remaining_ms(&self) -> i64 {
        let until = self.blocked_until_ms.load(Ordering::Relaxed);
        (until - ServerConfig::now_ms()).max(0)
    }

    // ---- rexxar 类别自适应限速（AIMD） ----
    // 豆瓣 rexxar 对单端点（category 维度）有独立的 IP 级突发限流：
    // movie 端点 ~4s 间隔连发 4-8 个即 400 封锁约 5-6 分钟（404 同样累积），
    // music/book 在 3-4s 间隔下长跑无恙（2026-10-28 实测，详见
    // 《豆瓣源依赖性与脆弱性分析.md》）。固定间隔无法同时适配三类端点，
    // 按类别 AIMD：被限流 ×3（上限 90s）、成功 ×0.9（下限 = 配置最小间隔）。
    // 进程内存态，重启回到配置默认值重新自适应。

    /// 富集成功后收缩该类别间隔（×0.9，下限为配置最小间隔），返回新值
    pub fn subject_interval_after_success(&self, category: &str) -> f64 {
        let floor = self.config.subject_min_interval_secs.max(0.5);
        let mut table = self.subject_pacing.lock().unwrap();
        let cur = table.get(category).copied().unwrap_or(floor);
        let next = (cur * 0.9).max(floor);
        table.insert(category.to_string(), next);
        next
    }

    /// 富集被限流（4xx）后放大该类别间隔（×3，上限 90s），返回新值
    pub fn subject_interval_after_block(&self, category: &str) -> f64 {
        let floor = self.config.subject_min_interval_secs.max(0.5);
        let mut table = self.subject_pacing.lock().unwrap();
        let cur = table.get(category).copied().unwrap_or(floor);
        let next = (cur * 3.0).min(90.0);
        table.insert(category.to_string(), next);
        next
    }

    /// 当前类别间隔（缺省 = 配置最小间隔；只读，供测试断言）
    pub fn subject_interval(&self, category: &str) -> f64 {
        let floor = self.config.subject_min_interval_secs.max(0.5);
        self.subject_pacing
            .lock()
            .unwrap()
            .get(category)
            .copied()
            .unwrap_or(floor)
    }

    // ---- verify 端点冷却（防豆瓣侧滥用） ----

    /// 同 uid 两次 verify 尝试（豆瓣抓取路径）的最小间隔
    pub const VERIFY_COOLDOWN_MS: i64 = 10_000;

    /// 该 uid verify 冷却剩余毫秒（0 = 可尝试）。本地捷径命中时不查此项
    pub fn verify_cooldown_remaining_ms(&self, uid: &str) -> i64 {
        let last = *self.verify_attempts.lock().unwrap().get(uid).unwrap_or(&0);
        (last + Self::VERIFY_COOLDOWN_MS - ServerConfig::now_ms()).max(0)
    }

    /// 记录一次 verify 尝试（无论成败）
    pub fn record_verify_attempt(&self, uid: &str) {
        self.verify_attempts
            .lock()
            .unwrap()
            .insert(uid.to_string(), ServerConfig::now_ms());
    }

    // ---- 客户端限流（固定窗口） ----

    pub fn rate_allow(&self, ip: IpAddr) -> bool {
        let mut table = self.rate_table.lock().unwrap();
        let minute = (ServerConfig::now_ms() / 60_000) as u64;
        let entry = table.entry(ip).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 1);
            return true;
        }
        if entry.1 >= self.config.rate_per_minute {
            return false;
        }
        entry.1 += 1;
        true
    }
}

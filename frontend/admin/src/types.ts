// 与 src/server/service.rs 的响应结构一一对应

export type TaskStatus = 'queued' | 'running' | 'completed' | 'partial' | 'failed';

/** GET /syncs 列表项（摘要字段） */
export interface TaskSummary {
  url: string;
  task_id: string;
  user: string;
  status: TaskStatus;
  items_count: number;
  created_at: string;
  /** manual = 手动创建；auto = 订阅全量同步自动创建（默认列表不展示） */
  source: string;
}

export interface TaskListResponse {
  tasks: TaskSummary[];
  next_cursor: number | null;
  has_more: boolean;
}

/** GET /syncs/{id} 与 POST /syncs 响应 */
export interface ProgressEntry {
  category: string;
  status: string;
  state: 'queued' | 'running' | 'done' | 'error' | string;
  fetched: number;
  total: number | null;
  /** 0-100；done 恒 100，未知总数/出错为 null，进行中封顶 99 */
  percent: number | null;
  next_start: number;
  page: number;
  pages_total: number | null;
}

export interface TaskError {
  code: string;
  message: string;
  failed_dimensions?: { category: string; status: string }[];
}

export interface TaskView {
  task_id: string;
  user: string;
  status: TaskStatus;
  request: { categories: string[]; statuses: string[]; refresh: boolean };
  progress: ProgressEntry[];
  items_count: number;
  error: TaskError | null;
  percent: number | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  expires_at: string | null;
  result_url: string;
}

export interface CreateTaskResponse extends TaskView {
  cached_dimensions?: number;
  created_dimensions?: number;
}

/** 任务结果中一个完成维度的汇总 */
export interface ResultDim {
  category: string;
  status: string;
  items: number;
  with_comment: number;
}

export interface ResultView {
  task_id: string;
  user: string;
  summary: {
    total: number;
    with_comment: number;
    by_category_status: Record<string, number>;
  };
  dimensions: ResultDim[];
  expires_at: string | null;
}

/** rexxar 富集的类别专属元信息（键随 category 为 movie/book/music） */
export interface SubjectMetaMovie {
  directors: string[];
  actors: string[];
  genres: string[];
  countries: string[];
  languages: string[];
  durations: string[];
  pubdate: string[];
  is_tv: boolean | null;
  episodes_count: number | null;
  intro: string;
}

export interface SubjectMetaBook {
  authors: string[];
  translators: string[];
  press: string[];
  pubdate: string[];
  pages: string[];
  price: string[];
  subtitle: string;
  series: string;
  intro: string;
}

export interface SubjectMetaMusic {
  singers: string[];
  genres: string[];
  media: string[];
  discs: string[];
  pubdate: string[];
  publisher: string[];
  version: string[];
  songs: string[];
  intro: string;
}

/** GET /subjects/{category}/{subject_id}（rexxar 富集缓存，跨用户共享） */
export interface Subject {
  category: 'movie' | 'book' | 'music';
  subject_id: string;
  title: string;
  original_title: string;
  aka: string[];
  year: number | null;
  rating: { value: number | null; count: number | null };
  cover: string;
  url: string;
  status: string;
  /** intro 兜底视图无抓取时间 */
  fetched_at: string | null;
  /** rexxar = 富集缓存；intro = 未富集时的启发式解析兜底（无豆瓣评分） */
  source?: 'rexxar' | 'intro';
  movie?: SubjectMetaMovie;
  book?: SubjectMetaBook;
  music?: SubjectMetaMusic;
}

/** GET /subjects 列表响应 */
export interface SubjectListResponse {
  items: Subject[];
  total: number;
  page: number;
  page_size: number;
  counts: {
    enriched: number;
    pending: number;
    not_found: number;
    failed: number;
    /** intro 兜底行数（重试耗尽降级；旧版服务器无此字段） */
    intro_fallback?: number;
  };
}

/** 富集 job 状态：queued/running 活动态，ok/error/not_found 终态 */
export type SubjectJobState = 'queued' | 'running' | 'ok' | 'error' | 'not_found';

/** GET /subjects/jobs 列表项（富集任务管理） */
export interface SubjectJob {
  category: 'movie' | 'book' | 'music';
  subject_id: string;
  /** 已富集取 subjects.title，否则回退任一用户标记行标题；两者皆无为 null */
  title: string | null;
  /** 本地封面代理相对路径（img 标签需经 resolveApiUrl 处理），无则为空串 */
  cover: string;
  state: SubjectJobState;
  attempts: number;
  /** true = 显式强制重抓（单条 refresh / 批量重排），插队执行且绕过"已在库跳过"守卫 */
  forced: boolean;
  next_run_at: string | null;
  created_at: string;
  updated_at: string;
  last_error: string | null;
  /** subjects 已富集时的抓取时间；未富集为 null */
  fetched_at: string | null;
  /** 库中数据已过 TTL（仅提示，重抓需显式操作） */
  stale: boolean;
}

/** GET /subjects/jobs 列表响应 */
export interface SubjectJobListResponse {
  items: SubjectJob[];
  total: number;
  page: number;
  page_size: number;
  counts: Record<SubjectJobState, number>;
  counts_total: number;
}

/** Record 的 12 字段（expand=subject 时附 subject 字段） */
export interface RecordItem {
  subject?: Subject | null;
  category: string;
  status: string;
  title: string;
  alt_title: string;
  my_rating: number | null;
  date: string;
  comment: string;
  subject_url: string;
  subject_id: string;
  cover: string;
  comment_id: string;
  intro: string;
}

export interface MarksResponse {
  user: string;
  category: string;
  status: string;
  cached_at: string;
  fresh: boolean;
  total: number;
  page: number;
  page_size: number;
  items: RecordItem[];
}

export interface ServerInfo {
  name: string;
  version: string;
  crawl: {
    min_interval_seconds: number;
    circuit: {
      threshold: number;
      cooldown_seconds: number;
      open: boolean;
      remaining_ms: number;
    };
  };
  cache: {
    ttl_seconds: number;
    retention_task_days: number;
  };
  queue: { queued: number; running: number };
  subscriptions: { count: number; enabled: number };
  auth_enabled: boolean;
  cookie_enabled: boolean;
}

export interface CreateTaskBody {
  user: string;
  categories: string[];
  statuses: string[];
  refresh: boolean;
}

/** 订阅同步（RSS 短周期增量 + 长周期全量 auto 任务） */
export interface Subscription {
  uid: string;
  enabled: boolean;
  rss_interval_secs: number;
  full_interval_secs: number;
  next_rss_at: string | null;
  next_full_at: string | null;
  last_rss_at: string | null;
  last_full_at: string | null;
  last_error: string | null;
  dimensions: number;
  records: number;
  created_at: string;
}

export interface SubscriptionListResponse {
  subscriptions: Subscription[];
}

/** GET /users/{uid}/profile——收藏页顶部资料块随抓取顺带入库；uid 未同步过时 404 */
export interface UserProfile {
  uid: string;
  username: string;
  /** 本地封面代理相对路径（/api/v1/covers?u=...），img 标签需经 resolveApiUrl 处理 */
  avatar: string;
}

/** GET /users 列表项（全用户管理：四表并集，按记录数降序） */
export interface UserSummary {
  uid: string;
  /** users 表资料；未入库或抓到空值为 null（回退 @uid 与默认头像） */
  username: string | null;
  /** 本地封面代理相对路径，需经 resolveApiUrl 处理 */
  avatar: string | null;
  /** 已订阅（enabled） */
  subscribed: boolean;
  records: number;
  dimensions: number;
  last_task_at: string | null;
  last_sync_at: string | null;
  profile_updated_at: string | null;
}

export interface UserListResponse {
  users: UserSummary[];
}

export interface EnrichmentCategory {
  category: string;
  paused: boolean;
  category_paused: boolean;
  global_paused: boolean;
  blocked_until_ms: number;
  next_request_ms: number;
  interval_secs: number | null;
  floor_secs: number;
  pending: number;
  running: number;
  first_waiting: number;
  retry_waiting: number;
  retry_due: number;
  retry_wait_ms: number | null;
  dispatch_delay_ms: number | null;
  errors: number;
  not_found: number;
  history: { outcome: string; http_status: number | null; recovered: boolean; count: number }[];
  last_hour: { attempts: number; successes: number; http_400: number };
}
export interface EnrichmentOverview {
  enabled: boolean;
  now_ms: number;
  history_since_ms: number | null;
  categories: EnrichmentCategory[];
  coverage: {
    uid: string;
    category: string;
    total: number;
    enriched: number;
    fallback: number;
    missing: number;
    queued: number;
    unqueued: number;
    subscribed: boolean;
    next_full_ms: number | null;
  }[];
  operations: {
    id: string;
    kind: string;
    category: string;
    created_ms: number;
    restored_ms: number | null;
    counts: {
      deleted?: number;
      retrying?: number;
      running?: number;
      restored?: number;
      queued?: number;
      in_flight?: number;
    };
  }[];
}

export interface EnrichmentAttempt {
  id: number;
  category: string;
  subject_id: string;
  attempt: number;
  started_ms: number;
  finished_ms: number | null;
  outcome: string;
  error_code: string | null;
  http_status: number | null;
  error: string | null;
  recovered: boolean;
}
export interface EnrichmentHistory {
  items: EnrichmentAttempt[];
  total: number;
  page: number;
  page_size: number;
}

/** 后端 API 客户端——web 站点首次接后端（用户资料）。
 *  开发期经 Vite proxy（/api → 127.0.0.1:8080），部署时同源反向代理即可。 */

import { MARKS_STATUS } from './navigation'
import type { CacheVersions } from './cache-version'
import { queryClient } from '../query-client'

/** 部署形态 API 基址：生产经 VITE_API_BASE_URL 注入（如 /douban/api/v1），
 *  dev 回退 /api/v1 走 Vite proxy（同 weimo 前端的构建期约定） */
const API_BASE = import.meta.env.VITE_API_BASE_URL ?? '/api/v1'
/** 可选 Bearer Token（开启服务端鉴权时经 VITE_API_TOKEN 注入；公开读站点
 *  防匿名滥用写端点用，不承担保密语义） */
const API_TOKEN = import.meta.env.VITE_API_TOKEN ?? ''

const authHeaders: Record<string, string> = API_TOKEN
  ? { Authorization: `Bearer ${API_TOKEN}` }
  : {}

/** 把后端返回的相对 API 路径（/api/v1/covers?u=...）重写为部署前缀下的
 *  可请求地址；鉴权开启时 img 标签带不了 Header，追加 ?token= 兜底（服务端支持） */
export function resolveApiUrl(path: string): string {
  if (!path || !path.startsWith('/api/v1')) return path
  let url = API_BASE + path.slice('/api/v1'.length)
  if (API_TOKEN) {
    url += (url.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(API_TOKEN)
  }
  return url
}

export async function fetchCacheVersions(uid: string): Promise<CacheVersions> {
  const response = await fetch(`${API_BASE}/users/${encodeURIComponent(uid)}/cache-version?status=${MARKS_STATUS}`, { headers: authHeaders })
  if (!response.ok) throw new Error(`版本检查失败（HTTP ${response.status}）`)
  const data = await response.json()
  if (!data || !['profile', 'stats', 'movie', 'book', 'music', 'all'].every((key) => typeof data[key] === 'string')) {
    throw new Error('版本数据格式不正确')
  }
  return data as CacheVersions
}

export type DoubanProfile = {
  uid: string
  username: string
  /** 本地封面代理相对路径（/api/v1/covers?u=...），豆瓣头像直连会被防盗链拦（418） */
  avatar: string
}

/** 版本化数据查询失败：kind 区分「未同步」（端点 404，数据从未抓取——
 *  可触发首发同步）、「版本已变」（409，数据在取版本与取数据之间被
 *  重新发布——瞬态，版本查询失效后按新版本 key 自动重拉，不应作为
 *  失败展示）、「限流」（429，服务端固定分钟窗口——同步高峰请求密
 *  集时的瞬态，窗口翻页自动恢复，同 409 不作失败展示）与一般失败
 *  （网络/服务端错误） */
export class DoubanDataQueryError extends Error {
  kind: 'not-cached' | 'version-stale' | 'rate-limited' | 'error'
  constructor(kind: 'not-cached' | 'version-stale' | 'rate-limited' | 'error', message: string) {
    super(message)
    this.kind = kind
  }
}

/** Request a published revision; failures stay outside the persistent query cache.
 *  404（维度未同步）返回 null 由查询层转 not-cached 错误，其余失败抛错。 */
async function fetchVersionedJson<T>(uid: string, endpoint: string, params: URLSearchParams, version?: string): Promise<T | null> {
  if (version && version !== 'unversioned' && version !== 'pending') params.set('cache_version', version)
  try {
    const response = await fetch(`${API_BASE}/users/${encodeURIComponent(uid)}/${endpoint}?${params}`, { headers: authHeaders })
    if (!response.ok) {
      // 409 = 取版本与取数据之间数据被重新发布（富集写库会持续触发）：
      // 失效版本查询后按新版本 key 自动重拉，属瞬态不作失败展示
      if (response.status === 409) {
        void queryClient.invalidateQueries({ queryKey: ['douban', uid, 'versions'] })
        throw new DoubanDataQueryError('version-stale', '数据已更新，正在重新加载')
      }
      // 429 = 服务端固定分钟窗口限流（首发同步高峰请求密集时可达）：
      // 窗口翻页自动恢复，按瞬态处理不作失败展示
      if (response.status === 429) {
        throw new DoubanDataQueryError('rate-limited', '请求过于频繁，稍候自动重试')
      }
      // 404 = 该维度从未同步过（合法空态，调用方区分处理）
      if (response.status === 404) return null
      throw new DoubanDataQueryError('error', `请求失败（HTTP ${response.status}）`)
    }
    return await response.json() as T
  } catch (error) {
    if (error instanceof DoubanDataQueryError) throw error
    throw new DoubanDataQueryError('error', '网络错误，请稍后重试')
  }
}

/** 用户资料；失败返回 null，由查询层保留旧数据并提供重试。 */
export function fetchDoubanProfile(uid: string, version?: string): Promise<DoubanProfile | null> {
  return fetchVersionedJson(uid, 'profile', new URLSearchParams(), version)
}

// --------------------------------------------------------------------------- //
// /users/{uid}/marks
// --------------------------------------------------------------------------- //

/** 条目结构化元信息（?expand=subject 时随每条标记返回；形状与后端
 *  subject_view 一致）。source: rexxar = 富集缓存（有豆瓣评分）；
 *  intro = 未富集时由 records.intro 启发式合成的兜底（字段可能稀疏） */
export type DoubanSubject = {
  category: string
  subject_id: string
  title: string
  original_title: string
  aka: string[]
  year: number | null
  rating: { value: number | null; count: number | null }
  /** 本地封面代理相对路径（rexxar 中图，比标记行小图清晰） */
  cover: string
  url: string
  status: string
  fetched_at: string | null
  source: 'rexxar' | 'intro'
  movie?: {
    directors: string[]
    actors: string[]
    genres: string[]
    countries: string[]
    languages: string[]
    durations: string[]
    pubdate: string[]
    is_tv: boolean | null
    episodes_count: number | null
  }
  book?: {
    authors: string[]
    translators: string[]
    press: string[]
    pubdate: string[]
    pages: string[]
    subtitle: string
    series: string
  }
  music?: {
    singers: string[]
    genres: string[]
    media: string[]
    discs: string[]
    pubdate: string[]
    publisher: string[]
    version: string[]
    songs: string[]
  }
}

/** 一条书影音标记（字段名与后端 Record 序列化一致，snake_case） */
export type DoubanMark = {
  category: string
  status: string
  title: string
  alt_title: string
  /** 我的评分 1-5 星；未评过为 null */
  my_rating: number | null
  /** 标记日期（YYYY-MM-DD） */
  date: string
  comment: string
  subject_url: string
  subject_id: string
  /** 本地封面代理相对路径（/api/v1/covers?u=...），空串表示无封面 */
  cover: string
  comment_id: string
  intro: string
  /** 条目结构化元信息（expand=subject）；旧版响应无此字段 */
  subject?: DoubanSubject | null
}

export type DoubanMarksPage = {
  user: string
  category: string
  status: string
  /** 上次抓取时间（RFC3339） */
  cached_at: string
  fresh: boolean
  total: number
  page: number
  page_size: number
  items: DoubanMark[]
}

export type MarksResult =
  | { ok: true; data: DoubanMarksPage }
  | { ok: false; kind: 'not-cached' | 'version-stale' | 'rate-limited' | 'error'; message: string }

/** 拉取某维度（category+status）的标记页；只读缓存，不触发抓取。
 *  category 另接受 'all'：书影音三类别按日期归并的合并视图
 *  （后端任一类别已同步即放行，不支持 facet 过滤；rating 为可选的
 *  我的评分筛选键 'N'/'lteN'，仅 all 视图支持）。
 *  附 expand=subject：每条标记带条目结构化元信息（已富集用 rexxar 数据，
 *  未富集回退 intro 启发式合成，旧版后端不认该参数）。
 *  ids 为可选的当页条目 id 集合（前端 dims 保序筛出的切片，顺序权威
 *  在前端）：提供时后端按传入顺序返回，忽略 page/page_size。
 *  404 DIMENSION_NOT_CACHED（该用户该维度尚未同步过）单列为 not-cached，
 *  其余失败（网络/限流/鉴权等）归为 error，均不抛错 */
export async function fetchDoubanMarks(
  uid: string,
  category: string,
  page = 1,
  pageSize = 20,
  ids?: readonly string[] | null,
  rating?: string | null,
  version?: string,
): Promise<MarksResult> {
  const params = new URLSearchParams({
    category,
    status: MARKS_STATUS,
    page: String(ids?.length ? 1 : page),
    page_size: String(ids?.length ? ids.length : pageSize),
    expand: 'subject',
  })
  if (version && version !== 'unversioned' && version !== 'pending') params.set('cache_version', version)
  if (ids?.length) {
    params.set('ids', ids.join(','))
  }
  if (rating) {
    params.set('rating', String(rating))
  }
  try {
    const res = await fetch(
      `${API_BASE}/users/${encodeURIComponent(uid)}/marks?${params}`,
      { headers: authHeaders },
    )
    if (res.ok) {
      return { ok: true, data: (await res.json()) as DoubanMarksPage }
    }
    let kind: 'not-cached' | 'version-stale' | 'rate-limited' | 'error' = 'error'
    let message = `请求失败（HTTP ${res.status}）`
    try {
      const body = await res.json()
      if (body?.error?.code === 'DIMENSION_NOT_CACHED') kind = 'not-cached'
      // 409：取版本与取数据之间被重新发布（瞬态），失效版本查询按新
      // 版本 key 自动重拉，调用方不作失败展示
      if (body?.error?.code === 'CACHE_VERSION_CHANGED') {
        kind = 'version-stale'
        void queryClient.invalidateQueries({ queryKey: ['douban', uid, 'versions'] })
      }
      // 429：固定分钟窗口限流（同步高峰瞬态），窗口翻页自愈，同 409
      // 不作失败展示
      if (body?.error?.code === 'TOO_MANY_REQUESTS' || res.status === 429) {
        kind = 'rate-limited'
        message = '请求过于频繁，稍候自动重试'
      }
      if (body?.error?.message) message = body.error.message
    } catch {
      // 非 JSON 错误体，保留默认 message
    }
    return { ok: false, kind, message }
  } catch {
    return { ok: false, kind: 'error', message: '网络错误，请稍后重试' }
  }
}

// --------------------------------------------------------------------------- //
// /users/{uid}/subject-dims（条目维度数据，标签树纯数据源）
// --------------------------------------------------------------------------- //

/** 单条目的维度投影（后端只投影原始字段：数组原文/布尔/年份；派生与
 *  聚合在前端 facet-engine 做——year→decade 为前端语义，is_tv 布尔
 *  原样透传当前未消费）。字段随类别出现，缺省即无值 */
export type DoubanDimsItem = {
  id: string
  /** 标记日期（YYYY-MM-DD）；items 按 date DESC 稳定序输出——排序契约 */
  date: string
  /** 我的评分 1-5 星（标记级字段平铺在 item 顶层，未评为 null；旧版
   *  响应无此字段）——「全部标记」导航子节点按它跨类别计数 */
  my_rating?: number | null
  /** 富集就绪（source=rexxar；intro 兜底 false；旧后端无此字段）——
   *  侧边栏「已拉取详情 x/y」进度在投影上统计，随版本流自动刷新 */
  enriched?: boolean
  dims: {
    year?: number
    is_tv?: boolean
    genres?: string[]
    countries?: string[]
    directors?: string[]
    press?: string[]
    authors?: string[]
    translators?: string[]
    singers?: string[]
  }
}

export type DoubanDims = {
  user: string
  category: string
  status: string
  total: number
  items: DoubanDimsItem[]
}

/** 拉取某维度的全部条目维度数据（轻量：每条 ~200B，聚合/筛选全在前
 *  端）；维度未同步或请求失败返回 null——树该分支静默隐藏 */
export function fetchDoubanSubjectDims(uid: string, category: string, version?: string): Promise<DoubanDims | null> {
  return fetchVersionedJson(uid, 'subject-dims', new URLSearchParams({ category, status: MARKS_STATUS }), version)
}

// --------------------------------------------------------------------------- //
// /users/{uid}/syncs（自助首发同步）
// --------------------------------------------------------------------------- //

/** 首发同步任务视图（自助触发端点返回，字段与 admin /syncs 的 TaskView
 *  一致，这里只消费状态做展示） */
export type DoubanSyncTask = {
  task_id: string
  user: string
  status: string
}

/** 同步任务视图（GET /syncs/{task_id} 轮询用）：只声明前端消费的字段 */
export type DoubanSyncTaskView = {
  task_id: string
  user: string
  status: string
  /** 任务总进度百分比（0-100）；completed 恒为 100，未终态封顶 99，无法判定为 null */
  percent: number | null
  /** 任务累计条目数（各成功维度 items 之和）；终态横幅「共 N 条」数据源 */
  items_count: number
}

/** 触发自助首发同步：POST /users/{uid}/syncs——服务端固定全类别 collect、
 *  不 refresh，重复触发由后端 create_task 内建兜底（新鲜缓存直接 done、
 *  queued/running 并入 single-flight、队列上限拒绝），不会重复抓取。
 *  失败返回 null 不抛错（前端回落手动引导文案） */
export async function triggerDoubanFirstSync(
  uid: string,
): Promise<DoubanSyncTask | null> {
  try {
    const res = await fetch(
      `${API_BASE}/users/${encodeURIComponent(uid)}/syncs`,
      { headers: authHeaders, method: 'POST' },
    )
    if (!res.ok) return null
    return (await res.json()) as DoubanSyncTask
  } catch {
    return null
  }
}

/** 轮询首发同步任务（GET /syncs/{task_id}）：status 到终态
 *  （completed/partial/failed）即同步结束，前端停轮询收起横幅；
 *  失败抛错由调用方容错（下一 tick 重试） */
export async function fetchDoubanSyncTask(taskId: string): Promise<DoubanSyncTaskView> {
  const res = await fetch(
    `${API_BASE}/syncs/${encodeURIComponent(taskId)}`,
    { headers: authHeaders },
  )
  if (!res.ok) throw new Error(`同步任务查询失败（HTTP ${res.status}）`)
  return (await res.json()) as DoubanSyncTaskView
}

// --------------------------------------------------------------------------- //
// /users/{uid}/verify（登录框「验证 ID」按钮）
// --------------------------------------------------------------------------- //

/** verify 失败语义：not-found=ID 不对、unavailable=豆瓣侧故障、
 *  rate-limited=同 uid 冷却、error=网络/服务端一般失败 */
export type DoubanVerifyErrorKind = 'not-found' | 'unavailable' | 'rate-limited' | 'error'

export type DoubanVerifyResult =
  | { ok: true; data: DoubanProfile }
  | { ok: false; kind: DoubanVerifyErrorKind; message: string }

/** 验证豆瓣 ID（POST /users/{uid}/verify）：后端本地已入库直接返回
 *  （零豆瓣请求，曾同步/曾验证过的 uid 毫秒级命中），否则抓一页收藏页
 *  验证、资料入库后返回。错误按 code 分类供文案分派，不抛错 */
export async function verifyDoubanUser(uid: string): Promise<DoubanVerifyResult> {
  try {
    const res = await fetch(
      `${API_BASE}/users/${encodeURIComponent(uid)}/verify`,
      { headers: authHeaders, method: 'POST' },
    )
    if (res.ok) {
      return { ok: true, data: (await res.json()) as DoubanProfile }
    }
    let kind: DoubanVerifyErrorKind = 'error'
    let message = `请求失败（HTTP ${res.status}）`
    try {
      const body = await res.json()
      if (body?.error?.code === 'PAGE_NOT_FOUND') kind = 'not-found'
      if (body?.error?.code === 'DOUBAN_UNAVAILABLE') kind = 'unavailable'
      if (body?.error?.code === 'TOO_MANY_REQUESTS') kind = 'rate-limited'
      if (body?.error?.message) message = body.error.message
    } catch {
      // 非 JSON 错误体，保留默认 message
    }
    return { ok: false, kind, message }
  } catch {
    return { ok: false, kind: 'error', message: '网络错误，请稍后重试' }
  }
}

// --------------------------------------------------------------------------- //
// /users/{uid}/stats
// --------------------------------------------------------------------------- //

/** 按日计数（date 为 YYYY-MM-DD），形状即 Heatmap 的 dailyCounts */
export type DoubanDailyCount = {
  date: string
  count: number
}

/** 侧边栏统计聚合；totals 三类恒出现（未同步的类别为 0） */
export type DoubanStats = {
  user: string
  status: string
  totals: { movie: number; book: number; music: number }
  daily_counts: DoubanDailyCount[]
}

/** 拉取标记统计（按类别计数 + 按日计数，只读缓存）；请求失败返回
 *  null——侧边栏统计区静默隐藏，不打断站点主流程 */
export function fetchDoubanStats(uid: string, version?: string): Promise<DoubanStats | null> {
  return fetchVersionedJson(uid, 'stats', new URLSearchParams({ status: MARKS_STATUS }), version)
}

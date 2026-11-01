import { useContext, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { ReactNode } from 'react'
import { Navigate, useParams, useSearchParams } from 'react-router'

import { TextButton } from 'weimo-ui-core/components/text-button'

import type { DoubanMark } from '../api'
import {
  DoubanDataQueryError,
  fetchDoubanSyncTask,
  triggerDoubanFirstSync,
} from '../api'
import { useDoubanUser } from '../douban-user'
import { CacheVersionContext } from '../cache-version'
import {
  doubanQueryKeys,
  fetchDoubanMarksPage,
  MarksQueryError,
  MARKS_PAGE_SIZE,
  useDoubanDimsQuery,
  useDoubanMarksPageQuery,
} from '../douban-queries'
import { filterIds } from '../facet-engine'
import { facetPathKey, facetValueLabel, parseFacetPath } from '../facet-model'
import { MarkCard } from '../mark-card'
import {
  CATEGORY_PAST_VERBS,
  CATEGORY_UNIT_NOUNS,
  DEFAULT_MARKS_PATH,
  parseRatingParam,
  ratingFilterLabel,
  resolveMarksView,
} from '../navigation'
import { queryClient } from '../../query-client'
import './marks-page.css'

const PAGE_SIZE = MARKS_PAGE_SIZE

/** 首发同步会话级状态（跨导航存活）：uid → 进度。triggering=POST 进行
 *  中；syncing=任务运行中（携带 task_id 供轮询，percent 随轮询回填）；
 *  done=任务到终态（completed/partial），完成横幅「共 N 条」展示中，
 *  2.4s 后自动转 dismissed；dismissed=横幅已收起（含 failed 终态，静
 *  默收起不打扰）；failed=触发失败（回落手动指引文案）。重复触发本身
 *  无害（后端 single-flight 并入运行中任务），会话级去重只为避免每次
 *  导航到未同步视图都发一次 POST；刷新页面后 Map 清空重触发亦安全 */
type FirstSync =
  | { phase: 'triggering' }
  | { phase: 'syncing'; taskId: string; percent?: number }
  | { phase: 'done'; items: number }
  | { phase: 'dismissed' }
  | { phase: 'failed' }

/** Map 是模块级可变源，组件须经 useSyncExternalStore 订阅读取——render
 *  期直读会被 React Compiler 按 uid 记忆化，异步回填后重渲染拿的是
 *  缓存值（横幅/引导文案不切换的根因） */
const firstSyncState = new Map<string, FirstSync>()
const firstSyncListeners = new Set<() => void>()

/** 运行中任务的跨 reload 恢复源：直写渐进后维度一入队 marks 即 200
 *  （可能空列表），not-cached 触发窗口随之消失——刷新/导航回来的新
 *  页面实例须靠它恢复 syncing 态（带 taskId 即可重启轮询，终态自然
 *  收敛）；sessionStorage 的会话级语义与 Map 天然同生命周期 */
const FIRST_SYNC_TASK_KEY = 'weimo-douban:first-sync-task'

function persistFirstSyncTask(uid: string, taskId: string | null) {
  try {
    if (taskId) sessionStorage.setItem(FIRST_SYNC_TASK_KEY, JSON.stringify({ uid, taskId }))
    else sessionStorage.removeItem(FIRST_SYNC_TASK_KEY)
  } catch {
    // 存储不可用仅丢失跨刷新恢复，会话内链路不受影响
  }
}

function restoreFirstSyncTask() {
  try {
    const raw = sessionStorage.getItem(FIRST_SYNC_TASK_KEY)
    if (!raw) return
    const parsed = JSON.parse(raw) as { uid?: unknown; taskId?: unknown }
    if (typeof parsed.uid === 'string' && typeof parsed.taskId === 'string') {
      firstSyncState.set(parsed.uid, { phase: 'syncing', taskId: parsed.taskId })
    }
  } catch {
    // 畸形存储按无恢复源处理
  }
}
restoreFirstSyncTask()

function setFirstSync(uid: string, value: FirstSync) {
  firstSyncState.set(uid, value)
  persistFirstSyncTask(uid, value.phase === 'syncing' ? value.taskId : null)
  for (const notify of firstSyncListeners) notify()
}

function subscribeFirstSync(notify: () => void) {
  firstSyncListeners.add(notify)
  return () => firstSyncListeners.delete(notify)
}

/** 后端任务终态（TERMINAL_STATUSES 同款）：到任一终态即停止轮询 */
const SYNC_TASK_TERMINAL = new Set(['completed', 'partial', 'failed'])

/** RFC3339 → YYYY-MM-DD（cached_at 只取日期粒度即可） */
function formatDate(iso: string): string {
  return iso ? iso.slice(0, 10) : ''
}

/** 标记列表页：读登录用户的缓存标记（GET /users/{uid}/marks），只读不触发抓取。
 *  数据经 react-query 缓存（同 biji 模式）：首页查询与加载更多的每页
 *  各占独立 key，类别/筛选切回来命中缓存零请求；失败保留已有成功数据，
 *  「重试」走 refetch 强制重发。分面筛选状态在 URL ?facet=dim=value
 *  ——侧边栏筛选树写入、本页读取驱动请求；类别路由切换不带参数，
 *  天然清除筛选 */
export function MarksPage() {
  const { category } = useParams()
  const view = resolveMarksView(category)
  const { uid, profile, openLogin } = useDoubanUser()
  const [searchParams] = useSearchParams()

  const marksCategory = view?.category
  // 分面筛选路径（可多步 AND，可清除）：解析自 URL——仅真实类别有
  // 维度白名单，all 合并视图与手改非法参数一律按无筛选处理（非法步起
  // 截断保留合法前缀，后端仍会兜底校验）
  const realCategory =
    marksCategory !== undefined && marksCategory !== 'all'
      ? marksCategory
      : undefined
  const facetPath = realCategory
    ? parseFacetPath(realCategory, searchParams.get('facet'))
    : []
  // 我的评分筛选（侧边栏「全部标记」子节点写入）：仅 all 合并视图有
  // 此参数，非法值按无筛选处理（后端仍会兜底校验）
  const rating =
    marksCategory === 'all' ? parseRatingParam(searchParams.get('rating')) : undefined
  // 占位文案展示名：直接用页面标题（三类别统一「动词过的类别名」）
  const displayTitle = view?.pageTitle ?? ''

  // 维度投影与侧边栏共用缓存（同 key 去重）；筛选命中 id 集由前端
  // facet-engine 算——无筛选为 undefined，筛选中数据未就绪为 null
  // （列表保持 loading）
  const dimsQuery = useDoubanDimsQuery(uid, realCategory ?? null)
  const matchedIds: string[] | null | undefined = !facetPath.length
    ? undefined
    : dimsQuery.data && realCategory
      ? filterIds(realCategory, dimsQuery.data.items, facetPath)
      : null
  const facetKey = facetPathKey(facetPath)

  // 首页标记查询：enabled 保持旧门控（无筛选立即发；有筛选等 dims
  // 就绪算出命中集再发）；竞态防护由 react-query 内建（按 key 取消旧
  // 观察），不再需要手写 seqRef
  const marksQuery = useDoubanMarksPageQuery({
    uid,
    category: marksCategory ?? null,
    page: 1,
    ids: matchedIds?.slice(0, PAGE_SIZE),
    rating,
    enabled: uid !== null && marksCategory !== undefined && matchedIds !== null &&
      matchedIds?.length !== 0 && !dimsQuery.isPlaceholderData,
  })

  // 加载更多累积页（page ≥ 2 的追加数据）：列表身份变化（uid/类别/
  // 筛选/评分/版本）时按 key 比对清空（render 期调整，模式同旧 snapshot）
  const listKey =
    uid && marksCategory
      ? `${uid}|${marksCategory}|${facetKey}|${rating ?? ''}|${marksQuery.version}`
      : null
  const [moreSnapshot, setMoreSnapshot] = useState<{
    key: string | null
    pages: DoubanMark[][]
    loadingMore: boolean
    moreError: string | null
  }>({ key: null, pages: [], loadingMore: false, moreError: null })
  if (moreSnapshot.key !== listKey) {
    setMoreSnapshot({
      key: listKey,
      pages: [],
      loadingMore: false,
      moreError: null,
    })
  }

  // 列表态由「首页查询结果 + 追加页」派生，不再持有独立状态机
  const first = matchedIds?.length === 0
    ? { ok: true as const, data: { items: [], total: 0, cached_at: '' } }
    : marksQuery.data
  const state:
    | { phase: 'loading' }
    | { phase: 'not-cached' }
    | { phase: 'error'; message: string }
    | {
        phase: 'list'
        items: DoubanMark[]
        total: number
        loadedPages: number
        cachedAt: string
      } = !first
    ? marksQuery.error
      ? marksQuery.error instanceof MarksQueryError
        ? marksQuery.error.kind === 'not-cached'
          ? { phase: 'not-cached' }
          : // 409 版本已变是瞬态（富集写库持续触发）：失效版本查询后按新
            // 版本 key 自动重拉，这里保持 loading 而非报错；withCachedFallback
            // 有旧版本数据时 data 已被填充、不会走到该分支
            marksQuery.error.kind === 'version-stale'
          ? { phase: 'loading' }
          : { phase: 'error', message: marksQuery.error.message }
        : { phase: 'error', message: marksQuery.error.message }
      : dimsQuery.isError && matchedIds === null
        ? // 筛选态下 dims 未就绪的门控错误：404 未同步归入 not-cached
          // （首登自动同步对其生效——带筛选落地 /movie 的用户先撞到的
          // 就是这条路）；409 同为瞬态保持 loading，
          // 其余才算加载失败
          dimsQuery.error instanceof DoubanDataQueryError &&
            dimsQuery.error.kind === 'not-cached'
          ? { phase: 'not-cached' }
          : dimsQuery.error instanceof DoubanDataQueryError &&
              dimsQuery.error.kind === 'version-stale'
            ? { phase: 'loading' }
            : { phase: 'error', message: dimsQuery.error.message }
        : { phase: 'loading' }
    : {
          phase: 'list',
          items: [...first.data.items, ...moreSnapshot.pages.flat()],
          // ids 模式后端 total 是当页片数，命中总数以前端筛选结果为准
          total: matchedIds ? matchedIds.length : first.data.total,
          loadedPages: 1 + moreSnapshot.pages.length,
          cachedAt: first.data.cached_at,
        }

  // 未填 ID 进入内容页：自动弹一次豆瓣 ID 对话框。ref 守卫保证每个页面
  // 实例只弹一次——关掉后仍可点占位区「输入豆瓣 ID」手动打开，类别切换
  // （同实例不重挂）、清除 ID、StrictMode 双跑都不重弹，避免反复打扰
  const autoLoginPromptedRef = useRef(false)
  useEffect(() => {
    if (autoLoginPromptedRef.current) return
    autoLoginPromptedRef.current = true
    if (!uid) openLogin()
  }, [uid, openLogin])

  // 首发同步：数据未同步（not-cached）时自动触发一次自助全量同步，
  // 成功后转 syncing（携带 task_id）；进度经 useSyncExternalStore 订阅
  // 会话级 Map（uid 天然隔离，无切号重置）；触发失败本会话不自动重试
  // （刷新页面重走一次）
  const syncPhase = useSyncExternalStore(
    subscribeFirstSync,
    () => (uid ? firstSyncState.get(uid) : undefined),
    () => undefined,
  )
  useEffect(() => {
    if (state.phase !== 'not-cached' || !uid) return
    if (firstSyncState.has(uid)) return
    setFirstSync(uid, { phase: 'triggering' })
    void triggerDoubanFirstSync(uid).then((task) => {
      setFirstSync(
        uid,
        task ? { phase: 'syncing', taskId: task.task_id } : { phase: 'failed' },
      )
    })
  }, [state.phase, uid])

  // 同步中双轮询（3s）：失效版本查询让首发直写的每页落库尽快可见
  // （revision bump → 版本 key 变化 → 数据查询自动重拉；60s 常规轮询
  // 兜底），同时查任务终态——到终态停轮询收起横幅；终态后富集未清
  // （versions.subjects_pending>0，旧后端无字段按 0）仍维持 3s 版本轮
  // 询——富集逐条落库（评分/年份回填）持续可见，pending 归零自动停。
  // 轮询失败静默由下一 tick 重试（会话级短生命周期，任务过期清理等
  // 场景无谓报错）；存活条件用布尔（pending 数值每条富集都变，作依赖
  // 会反复重启 interval）
  const syncTaskId = syncPhase?.phase === 'syncing' ? syncPhase.taskId : null
  const versions = useContext(CacheVersionContext)
  const keepPolling = syncTaskId !== null || (versions?.subjects_pending ?? 0) > 0
  useEffect(() => {
    if (!keepPolling || !uid) return
    let stopped = false
    const timer = setInterval(() => {
      if (stopped) return
      void queryClient.invalidateQueries({
        queryKey: [...doubanQueryKeys.uid(uid), 'versions'],
      })
      if (!syncTaskId) return
      void fetchDoubanSyncTask(syncTaskId)
        .then((task) => {
          if (stopped) return
          const current = firstSyncState.get(uid)
          if (SYNC_TASK_TERMINAL.has(task.status)) {
            if (current?.phase === 'syncing') {
              // 终态时刻维度数据已全部定格：立即补一次版本失效，让最后
              // 几页落库尽快拉全（完成横幅报「共 N 条」时列表尽量对齐）
              void queryClient.invalidateQueries({
                queryKey: [...doubanQueryKeys.uid(uid), 'versions'],
              })
              // completed/partial 给完成句号（items_count=已获取条数）；
              // failed 静默收起——无数据走手动指引，有部分数据也不庆祝
              setFirstSync(
                uid,
                task.status === 'failed'
                  ? { phase: 'dismissed' }
                  : { phase: 'done', items: Math.max(0, task.items_count) },
              )
            }
          } else if (
            current?.phase === 'syncing' &&
            current.percent !== (task.percent ?? undefined)
          ) {
            setFirstSync(uid, { ...current, percent: task.percent ?? undefined })
          }
        })
        .catch(() => {})
    }, 3_000)
    return () => {
      stopped = true
      clearInterval(timer)
    }
  }, [keepPolling, syncTaskId, uid])

  // 完成横幅的收起时序：全显 ~2s 后随 CSS 动画淡出（共 2.4s），动画
  // 播完卸载元素进入会话终态；items 是 done 态的稳定标识（effect 只在
  // 进入/离开 done 时重跑，StrictMode 双跑被 cleanup 清理，幂等）
  const doneItems = syncPhase?.phase === 'done' ? syncPhase.items : null
  useEffect(() => {
    if (doneItems === null || !uid) return
    const timer = setTimeout(() => setFirstSync(uid, { phase: 'dismissed' }), 2_400)
    return () => clearTimeout(timer)
  }, [doneItems, uid])

  async function loadMore() {
    if (
      !uid ||
      !marksCategory ||
      state.phase !== 'list' ||
      moreSnapshot.loadingMore ||
      marksQuery.isPlaceholderData
    ) {
      return
    }
    const capturedKey = listKey
    const nextPage = state.loadedPages + 1
    setMoreSnapshot((s) =>
      s.key === capturedKey ? { ...s, loadingMore: true, moreError: null } : s,
    )
    const result = await fetchDoubanMarksPage({
      uid,
      category: marksCategory,
      page: nextPage,
      ids: matchedIds?.slice((nextPage - 1) * PAGE_SIZE, nextPage * PAGE_SIZE),
      rating,
      version: marksQuery.version,
    })
    setMoreSnapshot((s) => {
      // 期间类别/筛选已切换：丢弃迟到页（对齐旧 seqRef 竞态防护）
      if (s.key !== capturedKey) return s
      if (result.ok) {
        return {
          ...s,
          pages: [...s.pages, result.data.items],
          loadingMore: false,
          moreError: null,
        }
      }
      return { ...s, loadingMore: false, moreError: result.message }
    })
  }

  if (!view) {
    return <Navigate replace to={DEFAULT_MARKS_PATH} />
  }

  let content: ReactNode
  if (!uid) {
    content = (
      <div className="marks-page__placeholder">
        <p>
          {view.category === 'all'
            ? '输入豆瓣 ID，展示你的书影音标记'
            : `输入豆瓣 ID，展示你的${displayTitle}记录`}
        </p>
        <TextButton onClick={openLogin} type="button">
          输入豆瓣 ID
        </TextButton>
      </div>
    )
  } else if (state.phase === 'loading') {
    content = <div className="marks-page__placeholder">加载中…</div>
  } else if (state.phase === 'not-cached') {
    // 首发同步引导（首批数据到达前）：触发中/同步中给自动同步引导，
    // 触发失败回落手动指引；首批页落库后转列表态，同步横幅接管进度展示
    content =
      syncPhase?.phase === 'triggering' || syncPhase?.phase === 'syncing' ? (
        <div className="marks-page__placeholder">
          <p>
            首次使用：正在从豆瓣同步 {profile?.username || uid} 的标记。
          </p>
          <p className="marks-page__hint">
            已同步的条目会陆续自动展示，无需手动刷新。
          </p>
        </div>
      ) : (
        <div className="marks-page__placeholder">
          <p>用户 {profile?.username || uid} 的数据还没有同步过。</p>
          <p className="marks-page__hint">
            {syncPhase?.phase === 'failed'
              ? '自动同步发起失败（服务不可达或请求被限流），请稍后刷新页面重试，或在管理端手动发起同步。'
              : `本站只读缓存，请先在服务端发起一次同步，完成后刷新本页即可看到${displayTitle}的记录。`}
          </p>
        </div>
      )
  } else if (state.phase === 'error') {
    content = (
      <div className="marks-page__placeholder">
        <p>加载失败：{state.message}</p>
        <TextButton
          onClick={() => {
            if (matchedIds === null || dimsQuery.isPlaceholderData) void dimsQuery.refetch()
            else void marksQuery.refetch()
          }}
          type="button"
          variant="ghost"
        >
          重试
        </TextButton>
      </div>
    )
  } else {
    const hasMore = state.items.length < state.total
    const syncedDate = formatDate(state.cachedAt)
    // meta 行名词：三类别统一量词名词（…部影视/…本书/…张专辑）；
    // all 合并视图为「已标记 N 条书影音」，评分筛选时后缀追加
    // 「· 我的N星标记」
    content = (
      <>
        {syncPhase?.phase === 'syncing' ? (
          <p className="marks-page__syncing">
            {`正在从豆瓣同步其余标记${syncPhase.percent != null ? `（${syncPhase.percent}%）` : ''}，已同步部分即时展示`}
          </p>
        ) : null}
        {syncPhase?.phase === 'done' ? (
          <p className="marks-page__syncing marks-page__syncing--done">
            {`同步完成，共 ${syncPhase.items} 条`}
          </p>
        ) : null}
        <p className="marks-page__meta">
          {profile?.username || uid}
          {realCategory
            ? ` ${CATEGORY_PAST_VERBS[realCategory]} ${state.total} ${CATEGORY_UNIT_NOUNS[realCategory]}`
            : ` 已标记 ${state.total} 条书影音`}
          {facetPath.length > 0 && realCategory
            ? ` · ${facetPath
                .map((step) => facetValueLabel(realCategory, step.dim, step.value))
                .join(' / ')}`
            : ''}
          {rating ? ` · ${ratingFilterLabel(rating)}` : ''}
          {syncedDate ? ` · 同步于 ${syncedDate}` : ''}
        </p>
        <ul className="marks-list">
          {state.items.map((mark) => (
            <MarkCard key={mark.comment_id || mark.subject_id} mark={mark} />
          ))}
        </ul>
        {hasMore ? (
          <div className="marks-page__more">
            {moreSnapshot.moreError ? (
              <span className="marks-page__more-error">
                {moreSnapshot.moreError}
              </span>
            ) : null}
            <TextButton
              disabled={moreSnapshot.loadingMore || marksQuery.isPlaceholderData}
              onClick={loadMore}
              type="button"
            >
              {moreSnapshot.loadingMore
                ? '加载中…'
                : `加载更多（${state.items.length}/${state.total}）`}
            </TextButton>
          </div>
        ) : null}
      </>
    )
  }

  return (
    <section className="marks-page">
      {content}
    </section>
  )
}

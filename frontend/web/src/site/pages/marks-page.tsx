import { useEffect, useRef } from 'react'
import type { ReactNode } from 'react'
import { Navigate, useParams, useSearchParams } from 'react-router'

import { TextButton } from 'weimo-ui-core/components/text-button'

import { DoubanDataQueryError } from '../api'
import { useDoubanUser } from '../douban-user'
import { triggerFirstSyncOnce, useFirstSyncPhase } from '../first-sync'
import { useDoubanDimsQuery } from '../douban-queries'
import { useMarksList } from '../use-marks-list'
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
import './marks-page.css'

/** RFC3339 → YYYY-MM-DD（cached_at 只取日期粒度即可） */
function formatDate(iso: string): string {
  return iso ? iso.slice(0, 10) : ''
}

/** 标记列表页：读登录用户的缓存标记（GET /users/{uid}/marks），只读不触发抓取。
 *  全部已加载页订阅同一查询缓存；分页版本保护记录顺序，内容版本仅
 *  后台刷新详情。筛选集合变化时整体切换，失败保留完整旧列表。
 *  分面筛选状态在 URL ?facet=dim=value
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

  const list = useMarksList({
    uid,
    category: marksCategory ?? null,
    facetKey,
    rating,
    // Unfiltered paging is independent of dims readiness. Filtered replacements
    // wait for a current projection; the controller retains the complete old list.
    ids: facetPath.length && dimsQuery.isPlaceholderData ? null : matchedIds,
  })
  const dimsUnavailable = facetPath.length > 0 && (matchedIds === null || dimsQuery.isPlaceholderData)
  const dimsError = dimsUnavailable ? dimsQuery.error : null
  const transientDimsError = dimsError instanceof DoubanDataQueryError &&
    (dimsError.kind === 'version-stale' || dimsError.kind === 'rate-limited')
  const state = list.phase === 'loading' && dimsError
    ? dimsError instanceof DoubanDataQueryError && dimsError.kind === 'not-cached'
      ? { ...list, phase: 'not-cached' as const }
      : transientDimsError
        ? list
        : { ...list, phase: 'error' as const, error: dimsError.message }
    : list.phase === 'list' && dimsError && !transientDimsError
      ? { ...list, error: dimsError.message }
      : list
  const retry = () => {
    if (dimsUnavailable) void dimsQuery.refetch()
    list.retry()
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

  // 首发同步：数据未同步（not-cached）时自动触发一次自助全量同步（幂
  // 等：会话级守卫在后端 single-flight 之外再挡重复 POST）；进度展示
  // 与生命周期轮询已上移——灵动岛通知挂 App 层（TopBarSyncNotice）、
  // 轮询桥挂 DoubanUserProvider（FirstSyncPollingBridge），本页只消费
  // 状态驱动引导文案
  const syncPhase = useFirstSyncPhase(uid)
  useEffect(() => {
    if (state.phase !== 'not-cached' || !uid) return
    triggerFirstSyncOnce(uid)
  }, [state.phase, uid])

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
    // 触发失败回落手动指引；首批页落库后转列表态，进度/完成句号由顶栏
    // 灵动岛通知展示（TopBarSyncNotice，全局可见）
    content =
      syncPhase?.phase === 'triggering' || syncPhase?.phase === 'syncing' ? (
        <div className="marks-page__placeholder">
          <p>
            首次使用：正在从豆瓣同步 {profile?.username || uid} 的标记。
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
        <p>加载失败：{state.error}</p>
        <TextButton
          onClick={retry}
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
        {hasMore || state.error ? (
          <div className="marks-page__more">
            {state.error ? (
              <span className="marks-page__more-error">
                {state.error}
              </span>
            ) : null}
            <TextButton
              disabled={!state.error && (list.loadingMore || !list.canLoadMore)}
              onClick={state.error ? retry : list.loadMore}
              type="button"
            >
              {!state.error && list.loadingMore
                ? '加载中…'
                : state.error ? '重试' : `加载更多（${state.items.length}/${state.total}）`}
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

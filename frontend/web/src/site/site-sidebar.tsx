import { useMemo, useState } from 'react'
import { BookOpen, Clapperboard, Grid2X2, Music } from 'lucide-react'
import { useLocation, useNavigate, useSearchParams } from 'react-router'
import { TextButton } from 'weimo-ui-core/components/text-button'
import { TagTree, type TagTreeNode } from 'weimo-ui-tagtree/components/tag-tree'

import type { DoubanDims } from './api'
import { DoubanDataQueryError } from './api'
import { useDoubanDimsQuery } from './douban-queries'
import { useDoubanUser } from './douban-user'
import { FacetTree } from './facet-tree'
import {
  facetPathKey,
  parseFacetPath,
  type FacetStep,
} from './facet-model'
import { SidebarHeader } from './sidebar-header'
import { SidebarStats } from './sidebar-stats'
import {
  ALL_MARKS_TAG,
  isMarksCategory,
  MARKS_CATEGORIES,
  NAV_RATING_STEPS,
  parseRatingParam,
  ratingKeyMatches,
  ratingNavTag,
  resolveMarksView,
  type MarksCategory,
  type NavTarget,
  type RatingKey,
} from './navigation'
import './site-sidebar.css'

/** 未登录零态筛选树（模块级静态：结构对齐 FacetTree 常驻树一级——
 *  三类别根同构，图标同款、计数全 0、无子级）：侧边栏结构常驻不因
 *  未登录/清除 ID 塌掉；点行跳对应类别。tag 无需匹配真实树语义
 *  （零态无选中/展开联动） */
const EMPTY_FACET_NODES: TagTreeNode[] = [
  {
    tag: 'movie',
    label: '影视',
    icon: <Clapperboard aria-hidden="true" />,
    itemCount: 0,
  },
  {
    tag: 'book',
    label: '图书',
    icon: <BookOpen aria-hidden="true" />,
    itemCount: 0,
  },
  {
    tag: 'music',
    label: '音乐',
    icon: <Music aria-hidden="true" />,
    itemCount: 0,
  },
]

/** 站点侧边栏：整体接管模板 sidebar 插槽，布局对齐 weimo-biji 的
 *  biji-side-bar——身份栏固定在顶，滚动主体依次为标记统计区
 *  （StatGroup/Heatmap）→ 导航区（全部标记，下挂我的评分子节点）→
 *  分面筛选树（三类别同构：类别根→分组值叶→主维度值叶三层；类别根
 *  带图标，点行=跳该类别清筛选兼任导航）。
 *  筛选路径经 URL ?facet=dim=value;… 与列表页共享（侧边栏写、列表页
 *  读）；评分筛选走 ?rating=N（仅全部标记视图）；类别切换天然清除
 *  筛选。
 *  onClose 供窄屏抽屉在选中后收起 */
export function SiteSidebar({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const [searchParams, setSearchParams] = useSearchParams()
  const { uid } = useDoubanUser()
  const category = resolveMarksView(pathname.split('/')[1])?.category
  // 分面仅真实类别有维度（all 合并视图无分面）
  const realCategory = category !== undefined && category !== ALL_MARKS_TAG
    ? category
    : undefined
  const path = realCategory
    ? parseFacetPath(realCategory, searchParams.get('facet'))
    : []
  // 我的评分筛选（仅 all 合并视图有此参数；非法值按无筛选处理）
  const rating =
    category === ALL_MARKS_TAG
      ? parseRatingParam(searchParams.get('rating'))
      : undefined

  // 条目维度数据：三类别并行查询（轻量投影，聚合/长尾合并/条件计数
  // 全部由前端 facet-engine 本地算），与列表页共用同一缓存（版本进
  // query key，版本不变零请求）；失败为 error 态（下方重试行给出口），
  // 某类别无数据该根不出现
  const movieDims = useDoubanDimsQuery(uid, 'movie')
  const bookDims = useDoubanDimsQuery(uid, 'book')
  const musicDims = useDoubanDimsQuery(uid, 'music')
  // 引用钉住（仅数据真正变化才换新对象）——FacetTree 内部
  // useMemo([dimsByCategory]) 据此不再被任意重渲染（URL 变化、60s
  // 版本轮询）击穿而全量重跑 buildTree 聚合；React Compiler 通常会
  // 自动 memo，这里显式声明意图（与 facet-tree 手写 useMemo 同风格）
  const dimsData = useMemo<Partial<Record<MarksCategory, DoubanDims>>>(
    () => ({
      movie: movieDims.data,
      book: bookDims.data,
      music: musicDims.data,
    }),
    [movieDims.data, bookDims.data, musicDims.data],
  )

  // 三类别查询的 settle 观测：全部 isPending（含版本清单未就绪的
  // disabled 阶段）= 筛选区加载占位；重试行仅计真错误——404 未同步
  // （等首发同步完成）与 409 版本已变（瞬态：富集写库会持续触发版本
  // 抖动，版本查询失效后按新 key 自动重拉，一轮即追上）都不算失败
  const dimsQueries = { movie: movieDims, book: bookDims, music: musicDims }
  const dimsLoading = MARKS_CATEGORIES.every((c) => dimsQueries[c].isPending)
  const dimsRealError = (q: typeof movieDims) =>
    q.isError &&
    (!(q.error instanceof DoubanDataQueryError) || q.error.kind === 'error')
  const dimsError = MARKS_CATEGORIES.some((c) => dimsRealError(dimsQueries[c]))
  // 三类别全为 404 未同步（首发同步进行中/未发起）：给同步提示而非
  // 渲染空树——「暂无分类数据」会被误读为永久空态
  const dimsNotCached = MARKS_CATEGORIES.every(
    (c) =>
      dimsQueries[c].isError &&
      dimsQueries[c].error instanceof DoubanDataQueryError &&
      dimsQueries[c].error.kind === 'not-cached',
  )
  const countsReady =
    uid === null || MARKS_CATEGORIES.every((c) => !dimsQueries[c].isPending)

  // 富集进度「已拉取详情 x/y」：直接在 dims 投影上统计（enriched 随
  // 投影下发）——富集逐条落库 bump subjects revision → dims 版本变 →
  // pending 驱动的 3s 版本轮询让投影自动重拉，进度零额外请求实时刷
  // 新；投影须全部条目带 enriched 字段才统计（旧后端/持久化缓存的旧
  // 响应无字段——新旧混合期分子分母口径错位，宁可不显示，待投影整
  // 体重拉自愈），全部就绪后进度语义完成即收起（与灵动岛完成收起同
  // 哲学）
  const enrichmentProgress = useMemo(() => {
    const items = MARKS_CATEGORIES.flatMap((c) => dimsData[c]?.items ?? [])
    if (
      items.length === 0 ||
      !items.every((item) => item.enriched !== undefined)
    ) {
      return null
    }
    const enriched = items.filter((item) => item.enriched).length
    return enriched < items.length ? { enriched, total: items.length } : null
  }, [dimsData])

  // 导航区节点：全部标记（书影音合并视图入口，图标对齐 biji-side-bar
  // 全部笔记的 Grid2X2；类别导航由筛选树一级根兼任，不在此重复），其
  // 下挂我的评分快捷筛选子节点（点击设 ?rating=，见 handleRatingSelect）。
  // 行尾计数随 dims 数据挂：全部标记=三类别条目总数（dims 全量投影
  // length 求和，与筛选树类别根/统计区同源——失败/未同步的类别无数据
  // 计 0，合并视图本就只含已同步类别）；评分子节点=键命中数
  // （ratingKeyMatches，精确星级 === 比较与后端口径一致，my_rating 随
  // dims item 平铺下发），计数为 0 的档位整行隐藏（如一星差评常空，
  // 空入口无导航价值；URL 直达 ?rating=N 不受影响——selectedTag 对
  // 不在树内的 tag 仅不高亮，列表页自身仍渲染筛选结果）。三类别查询
  // 全部 settle 才显示（error 也算 settle），避免加载中途先亮部分和
  // 再跳变；settle 前 0 档未知先全显，settle 后随计数一起消失
  // useMemo 与 dimsData 同理钉住引用，跨类别过滤计数不随无关重渲染重跑
  const navNodes = useMemo<TagTreeNode[]>(
    () => {
      const navItems = MARKS_CATEGORIES.flatMap(
        (c) => dimsData[c]?.items ?? [],
      )
      const ratingCount = (key: RatingKey) =>
        navItems.filter((item) => ratingKeyMatches(key, item.my_rating)).length
      return [
        {
          tag: ALL_MARKS_TAG,
          label: '全部标记',
          icon: <Grid2X2 aria-hidden="true" />,
          itemCount: countsReady ? navItems.length : undefined,
          children: NAV_RATING_STEPS.flatMap(({ key, label }) => {
            const count = countsReady ? ratingCount(key) : undefined
            return count === 0
              ? []
              : [{ tag: ratingNavTag(key), label, itemCount: count }]
          }),
        },
      ]
    },
    [countsReady, dimsData],
  )

  // 展开态：类别切换时重置为「当前类别根展开」；用户手动展开的
  // 其他类别在同一类别会话内保留。选中叶子到维度的链路不必记录
  // ——TagTree 对选中祖先自动展开
  const [expandedTags, setExpandedTags] = useState<string[]>([])
  const [expandedCategory, setExpandedCategory] = useState<
    NavTarget | undefined | null
  >(null)
  if (category !== expandedCategory) {
    setExpandedCategory(category)
    // 类别切换重置展开态：展开类别根（三类别同构）；all 无根可展
    setExpandedTags(
      category !== undefined && category !== ALL_MARKS_TAG
        ? [category]
        : [],
    )
  }

  /** 筛选路径统一处理：目标=当前类别时改查询参数即生效（空路径即清除
   *  筛选——再点取消与一级根点击重置都走这里）；目标为其他类别或
   *  「全部标记」跳路由，带路径一步落位、导航节点跳纯类别路由 */
  function handlePathSelect(target: NavTarget, next: FacetStep[]) {
    if (target === category) {
      setSearchParams(next.length ? { facet: facetPathKey(next) } : {})
    } else {
      navigate(
        next.length
          ? `/${target}?facet=${encodeURIComponent(facetPathKey(next))}`
          : `/${target}`,
      )
    }
    // 筛选切换后列表整体重排变短，视口回顶两类路径（改参数/跳路由）
    // 行为一致，避免同视图切换后悬在半空位置
    window.scrollTo({ top: 0, behavior: 'smooth' })
    onClose()
  }

  /** 「全部标记」评分子节点：目标必为 all——已在 all 视图改 ?rating=
   *  即生效（再点同评分取消，父行点击经 handlePathSelect 空路径全清），
   *  在其他类别则跳路由携带 */
  function handleRatingSelect(key: RatingKey) {
    if (category === ALL_MARKS_TAG) {
      setSearchParams(rating === key ? {} : { rating: key })
    } else {
      navigate(`/${ALL_MARKS_TAG}?rating=${key}`)
    }
    // 回顶口径同 handlePathSelect
    window.scrollTo({ top: 0, behavior: 'smooth' })
    onClose()
  }

  /** 筛选区重试出口：只重发真错误类别（瞬态错误重试同 key 无意义——
   *  409 要等版本重键、404 要等同步发布；版本轮询 60s 内也会自动
   *  refetch error 态查询，重试只是立即恢复的手动出口） */
  function retryFailedDims() {
    for (const c of MARKS_CATEGORIES) {
      if (dimsRealError(dimsQueries[c])) void dimsQueries[c].refetch()
    }
  }

  // 筛选树区状态：未填 ID 渲染零态树（结构常驻计数全 0，侧边栏不因
  // 清除 ID 塌掉；登录入口在身份栏与内容页占位区）；三类别查询全部
  // 加载中给占位行——TagTree 默认空态「暂无标签」会把加载期误表达为
  // 空态；任一类别失败给轻量重试行（见 retryFailedDims），已就绪的
  // 类别根照常渐次出现
  const facetBody = !uid ? (
    <TagTree
      nodes={EMPTY_FACET_NODES}
      onSelect={(tag) => {
        // 零态无子级无筛选：点行只跳对应类别
        if (isMarksCategory(tag)) handlePathSelect(tag, [])
      }}
      variant="no-action"
    />
  ) : dimsLoading ? (
    <p className="site-sidebar__status" role="status">
      筛选加载中…
    </p>
  ) : dimsNotCached ? (
    <p className="site-sidebar__status" role="status">
      数据同步后将显示分类筛选
    </p>
  ) : (
    <>
      {dimsError ? (
        <div className="site-sidebar__failed">
          <span>筛选数据暂不可用</span>
          <TextButton onClick={retryFailedDims} type="button" variant="ghost">
            重试
          </TextButton>
        </div>
      ) : null}
      <FacetTree
        activeCategory={category}
        dimsByCategory={dimsData}
        expandedTags={expandedTags}
        onExpandedTagsChange={setExpandedTags}
        onPathSelect={handlePathSelect}
        path={path}
      />
    </>
  )
  const facetSection = (
    <section aria-label="分面筛选" className="site-sidebar__facet">
      <div className="site-sidebar__section-hint">
        <span>分类筛选</span>
        {enrichmentProgress ? (
          <span className="site-sidebar__enrich-progress">
            {`已拉取详情 ${enrichmentProgress.enriched}/${enrichmentProgress.total}`}
          </span>
        ) : null}
      </div>
      {facetBody}
    </section>
  )

  return (
    <div className="site-sidebar">
      <SidebarHeader />
      <div className="site-sidebar__body">
        <SidebarStats />
        <nav aria-label="条目导航" className="site-sidebar__nav">
          <TagTree
            aria-label="条目导航"
            nodes={navNodes}
            onSelect={(tag) => {
              if (tag === ALL_MARKS_TAG) {
                handlePathSelect(ALL_MARKS_TAG, [])
                return
              }
              const hit = NAV_RATING_STEPS.find(
                (step) => ratingNavTag(step.key) === tag,
              )
              if (hit) handleRatingSelect(hit.key)
            }}
            // 选中指示两档：all 视图评分筛选中选中对应子节点（祖先
            // 自动展开），否则当前导航目标顶上
            selectedTag={
              category === ALL_MARKS_TAG
                ? rating
                  ? ratingNavTag(rating)
                  : ALL_MARKS_TAG
                : undefined
            }
            variant="no-action"
          />
        </nav>
        {facetSection}
      </div>
    </div>
  )
}

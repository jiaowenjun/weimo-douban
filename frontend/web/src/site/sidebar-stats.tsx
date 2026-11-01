import { TextButton } from 'weimo-ui-core/components/text-button'
import { Heatmap } from 'weimo-ui-stats/components/heatmap'
import { StatGroup, type StatGroupItem } from 'weimo-ui-stats/components/stat-group'

import { DoubanDataQueryError } from './api'
import { useDoubanStatsQuery } from './douban-queries'
import { useDoubanUser } from './douban-user'
import { CATEGORY_NOUNS, MARKS_CATEGORIES } from './navigation'
import './sidebar-stats.css'

/** 侧边栏标记统计区：StatGroup 三类计数（影视/图书/音乐——movie 合并
 *  电影+电视剧计数显示「影视」，取 /stats 端点 totals）+ Heatmap 近
 *  13 周热力图，布局对齐 weimo-biji 的 biji-side-bar__stats（header
 *  下方紧跟 stats 区）。未登录渲染零态（计数全 0、热力图空数据
 *  天然全 0 格——侧边栏结构常驻不因清除 ID 塌掉）；加载中占位行；
 *  失败且无旧数据（持久缓存/上一版本回退都没有）给轻量重试行——
 *  版本轮询自愈之外的立即出口。热力图暂仅展示——站点还没有按日期
 *  过滤的页面，格子点击无动作。数据经 react-query 缓存（版本进
 *  query key），路由来回切零请求 */
export function SidebarStats() {
  const { uid } = useDoubanUser()
  const statsQuery = useDoubanStatsQuery(uid)

  // value 保持数字类型：StatGroup 对数字经 MagicNumber 渲染（数值变动
  // 滚动到新值），String() 转字符串会退回静态透出丢动画（默认 format
  // 取整无千分位，与纯数字展示一致）
  const statItems: StatGroupItem[] = MARKS_CATEGORIES.map((category) => ({
    key: category,
    value: statsQuery.data?.totals[category] ?? 0,
    label: CATEGORY_NOUNS[category],
  }))

  // 零态：结构常驻，计数全 0 + 空 dailyCounts（热力图日期网格自渲染
  // 全 0 格）；查询本就因 uid 为空禁用，无请求无占位
  if (!uid) {
    return (
      <section aria-label="标记统计" className="sidebar-stats">
        <StatGroup items={statItems} />
        <Heatmap dailyCounts={[]} />
      </section>
    )
  }

  // 成功必带数据（requireData 抛错语义），此分支仅防御
  if (statsQuery.isSuccess && !statsQuery.data) return null

  const stats = statsQuery.data ?? null
  // 失败行仅真错误（网络/限流/服务端）：404 未同步与 409 版本已变都是
  // 瞬态（版本查询失效后按新 key 自动重拉，富集写库期间常见），保持
  // 加载占位不作失败展示
  const statsFailed =
    statsQuery.isError &&
    !stats &&
    (!(statsQuery.error instanceof DoubanDataQueryError) ||
      statsQuery.error.kind === 'error')

  return (
    <section aria-label="标记统计" className="sidebar-stats">
      {statsFailed ? (
        <div className="sidebar-stats__failed">
          <span>统计数据暂不可用</span>
          <TextButton
            onClick={() => void statsQuery.refetch()}
            type="button"
            variant="ghost"
          >
            重试
          </TextButton>
        </div>
      ) : stats ? (
        <>
          <StatGroup items={statItems} />
          <Heatmap dailyCounts={stats.daily_counts} />
        </>
      ) : (
        <p className="sidebar-stats__status" role="status">
          统计加载中…
        </p>
      )}
    </section>
  )
}

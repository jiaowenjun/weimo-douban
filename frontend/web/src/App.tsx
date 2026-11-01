import { HashRouter, Navigate, Route, Routes, useLocation, useSearchParams } from 'react-router'
import { TopBarNotice } from 'weimo-ui-core/components/top-bar-notice'
import { SiteTemplate } from 'weimo-ui-template/components/site-template'
import { TagBread } from 'weimo-ui-tagtree/components/tag-bread'

import { DoubanUserProvider } from './site/douban-user-provider'
import { useDoubanUser } from './site/douban-user'
import { dismissFirstSyncDone, useFirstSyncPhase } from './site/first-sync'
import {
  facetPathKey,
  facetValueLabel,
  parseFacetPath,
} from './site/facet-model'
import {
  CATEGORY_NOUNS,
  DEFAULT_MARKS_PATH,
  resolveMarksView,
} from './site/navigation'
import { TopBarExport } from './site/marks-export'
import { MarksPage } from './site/pages/marks-page'
import { WelcomePage } from './site/pages/welcome-page'
import { TopBarSearch } from './site/search-dialog'
import { SiteSettingsProvider } from './site/site-settings-provider'
import { SiteSidebar } from './site/site-sidebar'

/** 从 /:category 路径解析导航高亮项与顶栏标题 */
function useMarksViewFromLocation() {
  const { pathname } = useLocation()
  const [, category] = pathname.split('/')

  return resolveMarksView(category)
}

/** 顶栏前置区：真实类别页常显标签面包屑「类别名/步骤值…」（无筛选
 *  单段类别名，page 级不可点）。点中间级=截断到该步（根级点击=清空
 *  回全量）；all 返回 undefined，回落模板默认的 pageTitle 标签
 *  （pageTitle 始终驱动 document.title） */
function useFacetBreadcrumb() {
  const { pathname } = useLocation()
  const [searchParams, setSearchParams] = useSearchParams()
  const view = resolveMarksView(pathname.split('/')[1])
  if (!view || view.category === 'all') return undefined
  const category = view.category
  const path = parseFacetPath(category, searchParams.get('facet'))
  const stepLabels = path.map((step) =>
    facetValueLabel(category, step.dim, step.value),
  )
  const tag = [CATEGORY_NOUNS[category], ...stepLabels].join('/')
  if (!tag) return undefined

  return (
    <TagBread
      onSelect={(sub) => {
        // sub 为截至该级的路径，kept=保留的步骤数：首级是类别名
        // kept=0=清空（值可能含 / 的边缘场景按段数截断）
        const kept = sub.split('/').length - 1
        setSearchParams(
          kept > 0 ? { facet: facetPathKey(path.slice(0, kept)) } : {},
        )
      }}
      tag={tag}
    />
  )
}

function SiteShell() {
  const { pathname } = useLocation()
  const view = useMarksViewFromLocation()
  const topBarLeading = useFacetBreadcrumb()
  // 「动词过的类别名」三类别统一（看过的影视/读过的图书/听过的音乐），
  // 始终驱动 document.title
  const pageTitle = view?.pageTitle

  // 根路径 = 欢迎页（营销页）：自带 MiniSiteTemplate 独立壳（无侧边栏、
  // hero 标题区），不套 SiteTemplate 应用壳。分支置于全部 hooks 之后——
  // 提前 return 会令 / 与应用页两分支 hook 数不一致，违反 Rules of Hooks
  if (pathname === '/') return <WelcomePage />

  return (
    <SiteTemplate
      pageTitle={pageTitle}
      showSidebarCloseButton={false}
      // 顶栏右侧主题切换让位给搜索+导出入口（主题仍随系统 defaultTheme 同步 html.dark）
      showThemeToggle={false}
      siteName="微墨·豆瓣"
      sidebar={(api) => <SiteSidebar onClose={api.close} />}
      topBarActions={
        <>
          <TopBarSearch />
          <TopBarExport />
        </>
      }
      topBarCenter={<TopBarSyncNotice />}
      topBarLeading={topBarLeading}
    >
      <Routes>
        {/* 静态段优先于动态 /:category，react-router 按具体度排序匹配。
            设置已改为弹层对话框（SiteSettingsProvider 挂载），/settings
            仅留重定向兜底旧书签/历史记录 */}
        <Route
          element={<Navigate replace to={DEFAULT_MARKS_PATH} />}
          path="/settings"
        />
        <Route element={<MarksPage />} path="/:category" />
        <Route
          element={<Navigate replace to={DEFAULT_MARKS_PATH} />}
          path="*"
        />
      </Routes>
    </SiteTemplate>
  )
}

/** 顶栏灵动岛（TopBarNotice）：首发同步进度全局可见（欢迎页/任意页
 *  顶部常驻），短文案——syncing「同步中 N%」、done「同步完成，共 N 条」
 *  （duration 2.4s 到点经 onDismissed 收起进 dismissed，退场动画由组件
 *  播完）；其余态（triggering/failed/dismissed/未登录）children 置空
 *  即收起。轮询生命周期在 FirstSyncPollingBridge，本组件纯展示 */
function TopBarSyncNotice() {
  const { uid } = useDoubanUser()
  const phase = useFirstSyncPhase(uid)
  const text =
    phase?.phase === 'syncing'
      ? phase.percent != null
        ? `同步中 ${phase.percent}%`
        : '同步中'
      : phase?.phase === 'done'
        ? `同步完成，共 ${phase.items} 条`
        : null
  return (
    <TopBarNotice
      duration={phase?.phase === 'done' ? 2_400 : undefined}
      onDismissed={
        uid && phase?.phase === 'done'
          ? () => dismissFirstSyncDone(uid)
          : undefined
      }
    >
      {text}
    </TopBarNotice>
  )
}

function App() {
  // HashRouter 置于最外层：DoubanUserProvider 挂载的登录对话框需要
  // useNavigate（「返回」按钮回欢迎页），必须在 Router 上下文内渲染
  return (
    <HashRouter>
      <DoubanUserProvider>
        <SiteSettingsProvider>
          <SiteShell />
        </SiteSettingsProvider>
      </DoubanUserProvider>
    </HashRouter>
  )
}

export default App

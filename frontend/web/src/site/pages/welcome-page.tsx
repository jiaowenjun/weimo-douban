import { ArrowRight } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { getCardSurfaceClassName } from 'weimo-ui-core/components/card-surface'
import { FrostedIconButton } from 'weimo-ui-core/components/frosted-icon-button'
import { TextButton } from 'weimo-ui-core/components/text-button'
import { MiniSiteTemplate } from 'weimo-ui-template/components/mini-site-template'

import appIcon from './assets/app-icon.svg'
import appIconWhite from './assets/app-icon-white.svg'
import marksOverviewDark from './assets/marks-overview-dark.png'
import marksOverviewLight from './assets/marks-overview-light.png'
import './welcome-page.css'

/** 桌面 App 版本与双平台安装包直链（hero CTA 次按钮）：版本号与
 *  app/tauri.conf.json 的 version 保持同步；直链走 Gitee 国内镜像（主要
 *  用户群在大陆，GitHub 直链常不可达）——镜像由 release.sh 发版时自动
 *  同步，URL 带具体版本 tag（Gitee 无 GitHub 的 latest 别名），APP_VERSION
 *  仍是版本号唯一改点；资产名是 release.sh 统一的 ASCII 前缀（gh 上传
 *  会剥掉 CJK 文件名） */
const APP_VERSION = '0.5.2'
/** 上面的发布日期（页脚展示）：= GitHub Release v{APP_VERSION} 的发布日
 *  （当天本地日期，YYYY-MM-DD），随版本 bump 同步改——部署规范要求
 *  txy 网页端页脚与最新 App Release 的版本号+发布日期一致（见 ops/
 *  《txy部署手册.md》完整部署一节：一次完整部署 = 网页端上 txy + 发新版
 *  App，页脚是两端的对齐凭据）。计划日与实际发布日不符时以实际为准，
 *  改此处后重走 web 构建 */
const APP_RELEASE_DATE = '2026-11-01'
const APP_DMG_URL = `https://gitee.com/jiaowenjun/weimo-douban/releases/download/v${APP_VERSION}/weimo-douban_${APP_VERSION}_aarch64.dmg`
const APP_EXE_URL = `https://gitee.com/jiaowenjun/weimo-douban/releases/download/v${APP_VERSION}/weimo-douban_${APP_VERSION}_x64-setup.exe`
/** other 平台回落入口：Gitee 发行版列表页（同镜像域，大陆可达） */
const GITEE_RELEASES_URL = 'https://gitee.com/jiaowenjun/weimo-douban/releases'
const REPO_URL = 'https://github.com/jiaowenjun/weimo-douban'

/** 是否运行在 Tauri 桌面 App 壳内：Tauri 2 对壳内每个 webview（含
 *  External URL，窗口加载的是内嵌服务器的 http://127.0.0.1/douban/）在
 *  文档起始注入 window.isTauri=true，先于页面任何脚本、React 渲染时必然
 *  已就绪；普通浏览器（线上站点与本地 dev）永不定义——同一份构建产物
 *  零配置区分两端，App 内隐藏仅网页端有意义的内容（下载 CTA 与 GitHub
 *  入口，桌面端各自有更新器与源码仓库）。进程内恒定，模块级求值即可 */
const IN_TAURI_APP =
  typeof window !== 'undefined' &&
  (window as { isTauri?: boolean }).isTauri === true

/** 访客桌面平台（下载 CTA 按平台分流）：UA 串检测——Chromium 的 UA
 *  冻结刻意保留平台令牌（Windows NT / Macintosh），Safari/Firefox 未
 *  冻结，三大引擎皆可靠；不用 userAgentData（Chromium 独有且 TS DOM
 *  lib 无类型，收益配不上 cast 复杂度）。两处关键点：「Win」只出现在
 *  Windows 家族 UA；iPadOS 13+ 默认「请求桌面网站」把 UA 伪装成
 *  Macintosh，用 maxTouchPoints>1 揭穿（真 Mac 无触摸屏恒 ≤1，iPhone
 *  UA 含的是「like Mac OS X」而非 Macintosh 令牌，天然不误判）。
 *  会话内恒定，模块级求值即可（同 IN_TAURI_APP 范式）；other（Linux/
 *  移动端/未识别）暂无对应客户端，下载按钮整体不渲染（CTA 行回落单
 *  按钮），检测失败最坏结果 = 看不到下载按钮，经顶栏 GitHub 仓库的
 *  releases 页可救 */
type DownloadPlatform = 'mac' | 'windows' | 'other'
const DETECTED_PLATFORM: DownloadPlatform = (() => {
  if (typeof window === 'undefined') return 'other'
  const ua = navigator.userAgent
  if (/Win/i.test(ua)) return 'windows'
  if (/Macintosh/.test(ua) && navigator.maxTouchPoints <= 1) return 'mac'
  return 'other'
})()

/** 当前实际生效主题是否为深色：模板主题最终都同步到 html.dark 类，观测
 *  根类名即唯一事实源——MutationObserver 监听 class 变化，系统主题增删
 *  实时驱动截图换图（亮主题只显亮色截图、暗主题只显暗色截图，页面永不
 *  混排两套主题的实拍）。顶栏主题切换按钮已让位给 GitHub 仓库入口
 *  （showThemeToggle=false），本页主题只随系统 */
function useAppliedDarkTheme(): boolean {
  const [dark, setDark] = useState(() =>
    document.documentElement.classList.contains('dark'),
  )
  useEffect(() => {
    const root = document.documentElement
    const sync = () => {
      setDark(root.classList.contains('dark'))
    }
    // 订阅时先补读一次当前值（订阅即同步初值的范式）：useState 初值在
    // render 阶段读，早于模板把 html.dark 同步进 DOM 的 effect（子先于
    // 父），系统暗色冷挂载会读到 false，且 observer 挂上时唯一一次类
    // 变化已错过——亮色截图就此常驻暗色页面；本 effect 执行时模板
    // effect 必已跑完，sync() 读到正确值，effect 顺序若反转则由
    // observer 兜底，两个方向都闭合
    sync()
    const observer = new MutationObserver(sync)
    observer.observe(root, { attributes: true, attributeFilter: ['class'] })
    return () => observer.disconnect()
  }, [])
  return dark
}

/** GitHub 标记（GitHub 官方 logo 路径）：lucide-react 1.x 已移除品牌图标，
 *  内联为 SVG；fill=currentColor 随按钮文字色走，明暗主题均自适应——同
 *  AppleLogo，img 引 SVG 无法继承文字色，只能内联（口径同
 *  weimo-ui-tagtree 演示页的 GitHubIcon） */
function GitHubIcon({ size = 24 }: { size?: number }) {
  return (
    <svg
      aria-hidden
      fill="currentColor"
      height={size}
      viewBox="0 0 24 24"
      width={size}
    >
      <path d="M12 .297c-6.63 0-12 5.373-12 12c0 5.303 3.438 9.8 8.205 11.385c.6.113.82-.258.82-.577c0-.285-.01-1.04-.015-2.04c-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729c1.205.084 1.838 1.236 1.838 1.236c1.07 1.835 2.809 1.305 3.495.998c.108-.776.417-1.305.76-1.605c-2.665-.3-5.466-1.332-5.466-5.93c0-1.31.465-2.38 1.235-3.22c-.135-.303-.54-1.523.105-3.176c0 0 1.005-.322 3.3 1.23c.96-.267 1.98-.399 3-.405c1.02.006 2.04.138 3 .405c2.28-1.552 3.285-1.23 3.285-1.23c.645 1.653.24 2.873.12 3.176c.765.84 1.23 1.91 1.23 3.22c0 4.61-2.805 5.625-5.475 5.92c.42.36.81 1.096.81 2.22c0 1.606-.015 2.896-.015 3.286c0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  )
}

/** 以下三枚操作系统品牌图标（Apple / Windows / Linux Tux）取自 VS Code
 *  官网下载页（code.visualstudio.com/download）的内联 SVG 原稿：lucide
 *  无品牌标志（其 Apple 是水果造型），img 引 SVG 又无法继承文字色（黑色
 *  logo 在实心主按钮上隐形），故按页面既定口径内联；fill=currentColor
 *  随按钮文字色走，明暗主题与实心/ghost 变体均自适应 */
function AppleLogo({ size = 16 }: { size?: number }) {
  return (
    <svg
      aria-hidden
      fill="currentColor"
      height={size}
      viewBox="0 0 814 1000"
      width={size}
    >
      <path d="M788.1 340.9c-5.8 4.5-108.2 62.2-108.2 190.5 0 148.4 130.3 200.9 134.2 202.2-.6 3.2-20.7 71.9-68.7 141.9-42.8 61.6-87.5 123.1-155.5 123.1s-85.5-39.5-164-39.5c-76.5 0-103.7 40.8-165.9 40.8s-105.6-57-155.5-127C46.7 790.7 0 663 0 541.8c0-194.4 126.4-297.5 250.8-297.5 66.1 0 121.2 43.4 162.7 43.4 39.5 0 101.1-46 176.3-46 28.5 0 130.9 2.6 198.3 99.2zm-234-181.5c31.1-36.9 53.1-88.1 53.1-139.3 0-7.1-.6-14.3-1.9-20.1-50.6 1.9-110.8 33.7-147.1 75.8-28.5 32.4-55.1 83.6-55.1 135.5 0 7.8 1.3 15.6 1.9 18.1 3.2.6 8.4 1.3 13.6 1.3 45.4 0 102.5-30.4 135.5-71.3z" />
    </svg>
  )
}

function WindowsLogo({ size = 16 }: { size?: number }) {
  return (
    <svg
      aria-hidden
      fill="currentColor"
      height={size}
      viewBox="0 0 4875 4875"
      width={size}
    >
      <path d="M0 0h2311v2310H0zm2564 0h2311v2310H2564zM0 2564h2311v2311H0zm2564 0h2311v2311H2564" />
    </svg>
  )
}

/** Tux 用途：other 态（Linux/移动端/未识别）暂无客户端、按钮不渲染，
 *  组件暂不被触达，留作 Linux 客户端就绪后恢复 */
function LinuxLogo({ size = 16 }: { size?: number }) {
  return (
    <svg
      aria-hidden
      fill="currentColor"
      height={size}
      viewBox="0 0 58 58"
      width={size}
    >
      <path d="M47.6083 42.5333C47.3667 42.05 47.125 41.5667 47.125 41.0833C47.125 40.1167 46.6417 39.3917 45.9167 38.6667C45.675 38.425 45.1917 38.1833 44.95 38.1833C46.4 33.8333 44.225 29.4833 41.8083 26.3417C39.875 23.4417 36.975 21.2667 37.2167 17.4C37.2167 12.8083 37.7 4.35002 29.2417 5.07502C20.5417 5.55835 22.9583 14.5 22.7167 17.6417C22.7167 20.3 21.5083 22.9583 19.575 25.1333C19.0917 25.6167 18.6083 26.3417 18.3667 26.825C15.95 29.725 14.7417 33.5917 14.7417 37.2167C14.2583 37.7 13.775 38.1833 13.5333 38.6667C13.2917 38.9083 13.05 39.15 13.05 39.3917C12.8083 39.6333 12.325 39.875 11.8417 40.1167C10.875 40.35 10.15 40.8417 9.66667 41.8083C9.425 42.5333 9.18333 43.5 9.425 44.4667C9.66667 44.95 9.66667 45.4333 9.425 46.1583C8.94167 47.125 8.94167 48.3333 9.425 49.5417C10.15 50.5083 11.35 50.75 13.05 50.9917C14.2583 50.9917 15.7083 51.475 16.9167 51.9583C18.125 52.6833 19.575 53.1667 21.025 53.1667C21.75 53.1667 22.7167 52.925 23.4417 52.6833C24.1667 52.2 24.65 51.7167 24.8917 50.9917C25.8583 50.9917 27.3083 50.5083 29 50.5083C30.45 50.5083 31.9 50.9917 33.8333 50.75C33.8333 50.9917 33.8333 51.2333 34.075 51.475C34.5583 52.6833 35.7667 53.65 37.2167 53.8917H37.7C39.6333 53.65 41.5667 52.6833 42.775 51.2333C43.7417 50.2667 44.95 49.5417 46.1583 49.0583C47.6083 48.3333 48.575 47.85 48.8167 46.6417C49.0583 44.95 48.575 43.9833 47.6083 42.5333ZM30.9333 11.6C32.3833 11.8417 33.5917 13.05 33.35 14.5C33.35 15.225 33.1083 15.95 32.625 16.675H32.3833C31.9 16.4333 31.6583 16.4333 31.4167 16.1917C31.6583 15.95 31.6583 15.4667 31.9 14.9833C31.9 14.0167 31.4167 13.2917 30.9333 13.2917C30.2083 13.2917 29.725 14.0167 29.725 14.9833V15.225C29.4833 14.9833 29 14.9833 28.7583 14.7417V14.5C28.5167 13.2917 29.4833 11.8417 30.9333 11.6ZM30.2083 16.4333C30.45 16.675 30.9333 16.9167 31.175 16.9167C31.4167 16.9167 31.9 17.1583 32.1417 17.4C32.625 17.6417 33.1083 17.8833 33.1083 18.6083C33.1083 19.3333 32.3833 20.0583 30.9333 20.5417C30.45 20.7833 30.2083 20.7833 29.9667 21.025C29.2417 21.5083 28.5167 21.75 27.55 21.75C26.825 21.75 26.1 21.2667 25.6167 20.7833C25.375 20.5417 25.1333 20.3 24.65 20.0583C24.4083 19.8167 23.925 19.3333 23.6833 18.6083C23.6833 18.3667 23.925 18.125 24.1667 17.8833C24.8917 17.4 25.1333 17.1583 25.375 16.9167L25.6167 16.675C26.1 15.95 27.0667 15.4667 28.0333 15.4667C28.7583 15.7083 29.4833 15.95 30.2083 16.4333ZM25.1333 12.0833C26.1 12.0833 26.825 13.05 27.0667 14.7417V15.225C26.825 15.225 26.3417 15.4667 26.1 15.7083V15.225C26.1 14.5 25.6333 13.775 25.1333 14.0167C24.65 14.0167 24.4083 14.7417 24.4083 15.4667C24.4083 15.95 24.65 16.1917 24.8917 16.4333C24.8917 16.4333 24.65 16.675 24.4083 16.675C23.925 16.1917 23.4417 15.4667 23.4417 14.7417C23.4417 13.2917 24.1667 12.0833 25.1333 12.0833ZM22.7167 50.9917C21.025 51.7167 18.85 51.475 17.4 50.5083C15.95 49.7833 14.7417 49.5417 13.05 49.5417C11.8417 49.3 10.6333 49.3 10.3917 48.8167C10.15 48.3333 10.15 47.6083 10.6333 46.4C10.875 45.675 10.875 44.95 10.6333 44.225C10.3917 43.5 10.3917 43.0167 10.6333 42.2917C10.875 41.5667 11.3583 41.325 12.0833 41.0833C12.8083 40.8417 13.2917 40.6 13.775 40.1167C14.0167 39.875 14.2583 39.6334 14.5 39.15C15.225 38.1833 15.7083 37.7 16.4333 37.7C17.8833 37.9417 19.0917 40.1167 20.0583 42.2917C20.5417 43.0167 21.025 43.9833 21.75 44.7083C22.7167 45.9167 23.925 47.6083 23.925 48.575C23.925 49.7833 23.4417 50.5083 22.7167 50.9917ZM34.5583 45.675C34.5583 45.9167 34.5583 45.9167 34.3167 46.1583C31.4167 48.3333 27.55 48.575 24.4083 46.8833L22.9583 44.7083C25.1333 44.4667 24.65 41.5667 20.0583 38.6667C15.225 35.525 18.6083 29.725 20.3 27.0667C20.5417 26.825 20.5417 27.0667 19.575 29C18.85 30.45 17.4 34.075 19.3333 36.7333C19.3333 34.8 19.8167 32.8667 20.5417 30.9333C22.2333 27.7917 23.4417 24.1667 24.1667 20.5417C24.4083 20.7833 24.4083 20.7833 24.65 20.7833C24.8917 21.025 25.1333 21.2667 25.375 21.2667C25.8583 21.9917 26.825 22.2333 27.55 22.2333H27.7917C28.7583 22.2333 29.725 21.9917 30.45 21.2667C30.6917 21.025 30.9333 20.7833 31.4167 20.7833C32.1417 20.5417 32.8667 20.0583 33.5917 19.3333C34.5583 22.475 35.525 25.375 36.975 28.0333C37.9417 29.9667 38.6667 31.9 39.15 34.075C39.875 34.075 40.8417 34.3167 41.5667 34.8C43.5 35.7667 44.225 36.4917 43.9833 37.7H43.5C43.5 36.975 43.0167 36.25 41.325 35.525C39.6333 34.8 38.1833 34.8 37.7 36.4917C37.4583 36.4917 37.2167 36.7333 36.975 36.7333C35.0417 37.7 35.0417 40.35 34.8 43.0167C35.0417 43.9833 34.8 44.7083 34.5583 45.675ZM45.675 47.125C44.225 47.6083 43.0167 48.575 42.05 49.7833C41.0833 51.2333 39.3917 52.2 37.4583 51.9583C36.4917 51.9583 35.525 51.2333 35.2833 50.2667C35.0417 48.8 35.0417 47.3667 35.7667 45.9167C36.0083 44.95 36.25 44.225 36.4917 43.2583C36.7333 40.35 36.7333 38.6667 37.9417 37.9417C37.9417 39.15 38.6667 39.875 39.6333 40.3583C40.8417 40.3583 42.05 40.1167 43.0167 39.15H43.5C44.225 39.15 44.7083 39.15 45.1917 39.6333C45.675 40.1167 45.9167 40.8417 45.9167 41.325C45.9167 42.05 46.4 42.775 46.6417 43.5C47.85 44.7083 47.85 45.4333 47.85 45.675C47.6083 46.1583 46.6417 46.6417 45.675 47.125ZM23.925 18.125C23.6833 18.125 23.6833 18.125 23.6833 18.3667C23.6833 18.3667 23.6833 18.6083 23.925 18.6083C24.1667 18.6083 24.1667 18.85 24.1667 18.85C24.8917 19.8 26.1 20.3 27.55 20.5417C28.75 20.3 29.9667 20.0583 31.175 19.0917L32.625 18.3667C32.8667 18.3667 32.8667 18.125 32.8667 18.125C32.8667 17.8833 32.8667 17.8833 32.625 17.8833C32.1417 18.125 31.4167 18.3667 30.9333 18.6083C29.9333 19.3333 28.75 19.8167 27.55 19.8167C26.3417 19.8167 25.375 19.0917 24.65 18.3667C24.4083 18.3667 24.1667 18.125 23.925 18.125Z" />
    </svg>
  )
}

/** 下载 CTA 按检测平台分流的内容：mac/windows 指向各自安装包直链；
 *  other（Linux/移动端/未识别）暂无对应安装包、按钮不渲染，条目与
 *  图标留作相应客户端就绪后恢复（渲染条件见 JSX） */
const DOWNLOAD_CTA: Record<DownloadPlatform, { href: string; label: string }> =
  {
    mac: { href: APP_DMG_URL, label: 'Mac 客户端' },
    windows: { href: APP_EXE_URL, label: 'Windows 客户端' },
    other: { href: GITEE_RELEASES_URL, label: '下载客户端' },
  }

/** 各平台按钮图标（与 DOWNLOAD_CTA 同键） */
const PLATFORM_LOGO = {
  mac: AppleLogo,
  windows: WindowsLogo,
  other: LinuxLogo,
} as const

/** 欢迎页（营销页）：`/` 根路径的门面。MiniSiteTemplate 无侧边栏单页壳
 *  （hero 标题区 + 全宽内容），CTA 双按钮——客户端下载居左（更推荐）
 *  +「体验网页版」居右（App 壳内叫「开始备份」单按钮），主截图一张
 *  收尾。截图按当前生效主题成对提供（*-light/*-dark 各一套实拍，
 *  1024×768，4:3），经 import 引用随生产 base（/douban/）重写资源地址 */
export function WelcomePage() {
  const navigate = useNavigate()
  const dark = useAppliedDarkTheme()
  const { href: downloadHref, label: downloadLabel } =
    DOWNLOAD_CTA[DETECTED_PLATFORM]
  const PlatformLogo = PLATFORM_LOGO[DETECTED_PLATFORM]

  return (
    <MiniSiteTemplate
      contentClassName="welcome-page"
      headline="豆瓣书影音，一键备份"
      pageTitle="首页"
      siteName="微墨·豆瓣"
      showThemeToggle={false}
      subheadline="免登录，自托管，桌面端支持 MCP"
      topBarActions={
        // GitHub 入口仅网页端：桌面端用户从安装包获得 App、更新走内建
        // 更新器，仓库链接无意义（topBarActions 本是可选插槽，缺省即空）
        IN_TAURI_APP ? undefined : (
          <FrostedIconButton
            aria-label="GitHub 仓库"
            onClick={() => window.open(REPO_URL, '_blank', 'noopener,noreferrer')}
            tooltip="GitHub 仓库"
          >
            <GitHubIcon />
          </FrostedIconButton>
        )
      }
      topBarLeading={
        <span className="welcome-page__brand">
          {/* 黑白双稿随主题切换：亮色浓墨 / 暗色暖白，dark 来自
              useAppliedDarkTheme 对 html.dark 的观测（口径同下方主截图
              的双主题换图）；alt 置空：紧邻的品牌文字已传达名称，图标纯装饰 */}
          <img
            alt=""
            className="welcome-page__brand-icon"
            src={dark ? appIconWhite : appIcon}
          />
          微墨·豆瓣
        </span>
      }
    >
      <div className="welcome-page__cta">
        {/* 客户端下载居左（首选阅读位，客户端是更推荐的使用方式）、
         *  网页版居右；两端同为实心按钮，主次由位置表达 */}
        {/* 下载 CTA 仅网页端且平台有对应客户端：桌面端已在 App 内、
         *  other 态（Linux/移动端/未识别）暂无客户端，两态均不渲染
         *  （隐藏后剩单按钮，flex 居中布局天然成立），待相应客户端
         *  就绪再放开。内容按检测平台分流——图标/文案/链接三联动；
         *  误检测/Intel Mac 经顶栏 GitHub 仓库入口的 releases 自救。
         *  借用 text-button 组件类渲染真链接：下载需要真实 href（右键
         *  另存/新标签直下），安装包直链由浏览器下载不离开本页；无
         *  data-variant 即默认实心，与右侧网页版按钮同款 */}
        {!IN_TAURI_APP && DETECTED_PLATFORM !== 'other' && (
          <a className="text-button" href={downloadHref}>
            <PlatformLogo size={16} />
            {downloadLabel}
          </a>
        )}
        <TextButton
          onClick={() => navigate('/all')}
          type="button"
        >
          {/* 网页端叫「体验网页版」（承接「一键备份」叙事，与左侧
              下载客户端相对）；App 壳内承接同一叙事叫「开始备份」，
              点击进入标记页即登录-备份链路 */}
          {IN_TAURI_APP ? '开始备份' : '体验网页版'}
          <ArrowRight aria-hidden size={16} />
        </TextButton>
      </div>

      <figure
        className={getCardSurfaceClassName(
          'welcome-page__shot welcome-page__shot--hero',
        )}
      >
        <img
          alt="标记列表页全景：侧边栏统计与筛选树、顶栏面包屑、封面与双星评分的标记卡片"
          src={dark ? marksOverviewDark : marksOverviewLight}
        />
      </figure>

      {/* 页脚版本行：网页端 = 最新客户端 Release 的版本+发布日期（App 壳内
       *  嵌的也是同版本前端，即本 App 版本），两端天然同源——一致性由
       *  完整部署规范保证（APP_RELEASE_DATE 注释），不在运行时校验 */}
      <footer className="welcome-page__footer">
        v{APP_VERSION} · {APP_RELEASE_DATE}
      </footer>
    </MiniSiteTemplate>
  )
}

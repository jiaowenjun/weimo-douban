import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { ChevronRight, RotateCw } from 'lucide-react'

import { getCardSurfaceClassName } from 'weimo-ui-core/components/card-surface'
import { GhostIconButton } from 'weimo-ui-core/components/ghost-icon-button'
import { WdfRender, type WdfDocument } from 'weimo-ui-editor/components/wdf-render'

import type { DoubanMark, DoubanSubject } from './api'
import { resolveApiUrl } from './api'
import { formatMarkCardDateBarText } from './date-display'
import { useSiteSettings } from './site-settings'
import './mark-card.css'

type MarkCardProps = {
  mark: DoubanMark
}

/** 五星评分：底色五星 + 按比例裁切的覆盖层（两段 ★★★★★ 叠加），
 *  value 为 0–5 的小数也精确支持（豆瓣 8.5 分 /2 = 4.25 星 → 85% 宽
 *  覆盖）。与 mark-card.tsx 的 StarRating 同源实现，配色由根类名在
 *  CSS 侧区分（我的评分收敛 15% / 豆瓣评分元信息同色） */
function StarRating({
  className,
  label,
  value,
}: {
  className: string
  label: string
  value: number
}) {
  return (
    <span aria-label={label} className={className} title={label}>
      <span aria-hidden="true" className={`${className}__base`}>
        ★★★★★
      </span>
      <span
        aria-hidden="true"
        className={`${className}__fill`}
        style={{ width: `${(value / 5) * 100}%` }}
      >
        ★★★★★
      </span>
    </span>
  )
}

/** 取数组前 n 个非空项 */
function names(list: string[] | undefined, n: number): string[] {
  return (list ?? []).filter((s) => s).slice(0, n)
}

/** 标记动词：看过/读过/听过（站点 status 恒 collect，按类别推导） */
const CATEGORY_VERBS: Record<string, string> = {
  movie: '看过',
  book: '读过',
  music: '听过',
}

/** 条目元信息 → 单行摘要（与 mark-card.tsx 的 subjectMetaLine 同源） */
function subjectMetaLine(subject: DoubanSubject): string | null {
  let parts: string[]
  if (subject.category === 'movie') {
    const m = subject.movie
    parts = [
      names(m?.countries, 2).join(' '),
      names(m?.genres, 3).join(' '),
      names(m?.directors, 2).join(' '),
      names(m?.actors, 2).join(' '),
    ]
  } else if (subject.category === 'book') {
    const b = subject.book
    parts = [
      names(b?.press, 1).join(' '),
      names(b?.authors, 3).join(' '),
      names(b?.translators, 2).join(' '),
    ]
  } else {
    const m = subject.music
    parts = [names(m?.genres, 2).join(' '), names(m?.singers, 3).join(' ')]
  }
  const line = parts.filter(Boolean).join(' / ')
  return line || null
}

/** 短评 → WDF 文档：仅正文段落（无标题），无短评返回 null */
function commentDocument(mark: DoubanMark): WdfDocument | null {
  if (!mark.comment) return null
  return {
    schema: 'weimo.document',
    version: 1,
    root: {
      type: 'doc',
      content: [
        {
          type: 'paragraph',
          content: [{ type: 'text', text: mark.comment }],
        },
      ],
    },
  }
}

/** 标题行飞行动画（FLIP）的在飞状态：克隆节点 + WAAPI 动画句柄 +
 *  飞行期间被隐藏的两个真实副本（动画结束/取消时恢复） */
type Flight = {
  anim: Animation
  clone: HTMLElement
  fromEl: HTMLElement
  toEl: HTMLElement
}

/** 封面自动重试上限与逐次延迟 */
const COVER_RETRY_MAX = 2
const COVER_RETRY_DELAYS_MS = [500, 1500]

/** 封面图（含失败恢复）：加载失败自动限次延迟重试——重试请求在代理
 *  URL 外层追加 _r=N 显式改变资源地址，不依赖浏览器对相同 src 重新渲染
 *  的处理（_r 不动 u 与鉴权参数，后端 covers 端点忽略未知参数）；重试
 *  耗尽切换占位骨架与手动重试按钮，占位底色按类别分色（与无封面态
 *  同款，见 mark-card.css --empty）。调用处以 key=cover 挂载，封面源
 *  变化即整体重挂载重置；卸载清理挂起的重试定时器 */
function MarkCover({ category, cover, title }: { category: string; cover: string; title: string }) {
  const [attempt, setAttempt] = useState(0)
  const [failed, setFailed] = useState(false)
  const timerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const clearTimer = useCallback(() => {
    if (timerRef.current !== undefined) {
      clearTimeout(timerRef.current)
      timerRef.current = undefined
    }
  }, [])
  useEffect(() => clearTimer, [clearTimer])

  const reset = () => {
    clearTimer()
    setAttempt(0)
    setFailed(false)
  }

  if (failed) {
    return (
      <div
        aria-label={`${title} 封面未能加载`}
        className="mark-card__cover mark-card__cover--empty mark-card__cover--failed"
        data-category={category}
        role="group"
      >
        <span
          aria-label={`${title} 封面未能加载`}
          className="mark-card__cover-failed-image"
          role="img"
        />
        <GhostIconButton aria-label="重新加载封面" onClick={reset} size="sm" type="button">
          <RotateCw aria-hidden="true" />
        </GhostIconButton>
      </div>
    )
  }

  const baseSrc = resolveApiUrl(cover)
  return (
    <img
      alt=""
      className="mark-card__cover"
      loading="lazy"
      onError={() => {
        if (attempt >= COVER_RETRY_MAX) {
          setFailed(true)
          return
        }
        clearTimer()
        timerRef.current = setTimeout(
          () => setAttempt(attempt + 1),
          COVER_RETRY_DELAYS_MS[attempt] ?? COVER_RETRY_DELAYS_MS[COVER_RETRY_DELAYS_MS.length - 1],
        )
      }}
      src={attempt > 0 ? `${baseSrc}${baseSrc.includes('&') ? '&' : '?'}_r=${attempt}` : baseSrc}
    />
  )
}

/** 书影音标记卡片，四段式一级结构：顶部标记行（日期 看过/读过/听
    过 我的评分星级 + 右侧元信息折叠开关）/ 标题行「标题 (年份) 别
    名」（标题唯一出现处、16px/650 primary，年份/别名弱灰 sm；折叠态
    独占一行、水平对齐随站点设置（默认左对齐，可居中）、展开态居封面
    右侧信息列首行——两处渲染，由根
    data-expanded 的 CSS 切换）/ 可折叠条目信息区（封面 64px 左，文
    字列右——豆瓣评分「五星 + 数字」+ 单行元信息摘要或 intro 兜底；
    内容全空时整区与开关不渲染；初始态随站点设置，默认值变化时
    未手动拨过的已挂载卡片即时跟随，手动拨过的保留用户态）/ 评论正文区（WdfRender
    渲染短评段落，无短评整区不渲染），含 <li> 列表条目语义。
    折叠开关切换带标题行 FLIP 共享元素过渡：两态标题副本排版逐字一
    致、纵向同位，切换时「量旧位置 → 克隆 fixed 钉旧位（脱离折叠容器
    裁切与透明过渡）→ 位移到新位」让标题看起来连续移动；位移 dx 按
    首个子段锚点测（折叠态居中、展开态左对齐，盒左差≠内容位移），
    dy=0 为结构性不变量。两态任一侧换行（isSingleLineRow 行盒高阈值
    ——子段 getClientRects 因 flex 项块化测不到换行，不可用）放弃飞
    行回退 CSS 交叉淡换；prefers-reduced-motion / 无 WAAPI 直接回退；
    飞行中再点从克隆当前视觉位置续接。材质复用 card-surface；折叠实
    现同源 BaseCard metaCollapsible；交互仅折叠开关 */
export function MarkCard({ mark }: MarkCardProps) {
  // 展开态读站点设置（设置对话框可改）：挂载时按默认值初始化，默认值
  // 变化时即时跟随——但手动拨过折叠开关的卡片（metaTouched）保留用
  // 户态不被覆盖；render 期调整避免 effect 级联（口径同 LoginDialog
  // 的 prevOpen）
  const { settings } = useSiteSettings()
  const [metaExpanded, setMetaExpanded] = useState(
    () => settings.defaultMetaExpanded,
  )
  // 手动拨过折叠开关即置位：此后默认值变化不再跟随，保留用户态。用
  // state 而非 ref 承载——render 期跟随逻辑要读它（React 规则禁 render
  // 期读 ref）
  const [metaTouched, setMetaTouched] = useState(false)
  const [prevDefaultExpanded, setPrevDefaultExpanded] = useState(
    settings.defaultMetaExpanded,
  )
  if (settings.defaultMetaExpanded !== prevDefaultExpanded) {
    setPrevDefaultExpanded(settings.defaultMetaExpanded)
    if (!metaTouched) {
      setMetaExpanded(settings.defaultMetaExpanded)
    }
  }
  const cover = mark.subject?.cover || mark.cover
  const metaLine = mark.subject ? subjectMetaLine(mark.subject) : null
  const year = mark.subject?.year
  const doubanRating = mark.subject?.rating.value
  const commentDoc = commentDocument(mark)
  const hasSubjectInfo =
    Boolean(cover) || doubanRating != null || Boolean(metaLine) || Boolean(mark.intro)

  const soloTitleRef = useRef<HTMLDivElement | null>(null)
  const inlineTitleRef = useRef<HTMLDivElement | null>(null)
  // FLIP first 的跨渲染传递：切换前可见副本盒几何 + 首个子段（标题）
  // 锚点 x + 旧侧是否单行（切换后旧副本已被 0fr 折叠，须在点击时先记）
  const flipFromRef = useRef<{
    rect: DOMRect
    anchor: number
    singleLine: boolean
  } | null>(null)
  const flightRef = useRef<Flight | null>(null)

  const prefersReducedMotion = () =>
    window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false

  // 单行判定：内容自然高（scrollHeight）≤ 主标题行高 × 1.45。不能用
  // 子段 getClientRects——flex 项会被块化成块盒，内部换几行都只有一
  // 个 rect；也不能用 rect.height——折叠过渡中的副本被 0fr 压扁成高 0
  // （scrollHeight 仍反映真实行盒高）
  const isSingleLineRow = (row: HTMLElement) => {
    const title = row.querySelector('.mark-card__title')
    const cs = getComputedStyle(title ?? row)
    const lineHeight =
      parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.5
    return row.scrollHeight <= lineHeight * 1.45
  }

  const cleanupFlight = () => {
    const flight = flightRef.current
    if (!flight) return
    flightRef.current = null
    try {
      flight.anim.cancel()
    } catch {
      /* 已结束的动画 cancel 无害 */
    }
    flight.clone.remove()
    flight.fromEl.style.visibility = ''
    flight.toEl.style.visibility = ''
  }

  useEffect(() => cleanupFlight, [])

  // 首个子段（标题）的锚点 x：折叠态 solo 居中、展开态 inline 左对齐，
  // 盒内内容位置随对齐方式不同——位移 dx 须按内容锚点算（盒左对盒左
  // 在居中态不等于内容实际位移），克隆整盒平移、内容随盒走，锚点对
  // 锚点即内容连续
  const anchorLeft = (row: HTMLElement) =>
    row.firstElementChild instanceof HTMLElement
      ? row.firstElementChild.getBoundingClientRect().left
      : row.getBoundingClientRect().left

  const toggleMeta = () => {
    // 飞行中再点：从克隆当前视觉位置（含进行中的 transform）续接
    let rect: DOMRect | null = null
    let anchor = 0
    let singleLine = false
    const flight = flightRef.current
    if (flight) {
      rect = flight.clone.getBoundingClientRect()
      anchor = anchorLeft(flight.clone)
      // 旧侧单行性沿用飞行开始时的判定（克隆即旧内容）
      singleLine = true
      cleanupFlight()
    } else {
      const active = metaExpanded ? inlineTitleRef.current : soloTitleRef.current
      if (active) {
        rect = active.getBoundingClientRect()
        anchor = anchorLeft(active)
        singleLine = isSingleLineRow(active)
      }
    }
    flipFromRef.current = rect ? { rect, anchor, singleLine } : null
    setMetaTouched(true)
    setMetaExpanded((current) => !current)
  }

  useLayoutEffect(() => {
    const from = flipFromRef.current
    flipFromRef.current = null
    if (!from || !from.singleLine) return
    const toEl = metaExpanded ? inlineTitleRef.current : soloTitleRef.current
    const fromEl = metaExpanded ? soloTitleRef.current : inlineTitleRef.current
    if (!toEl || !fromEl) return
    if (prefersReducedMotion() || typeof toEl.animate !== 'function') return
    // 新侧单行判定（scrollHeight，不受过渡压扁影响）；两侧均单行即排版
    // 逐字一致（同内容同字体、单行无换行差异），纯平移严格成立
    if (!isSingleLineRow(toEl)) return
    // 飞行终点基于稳定不变量而非 t=0 暂态盒：dx 按首个子段锚点（居中
    // solo 与左对齐 inline 的盒内内容位置不同，见 anchorLeft），x 两向
    // 在 t=0 均稳定（纵向过渡不影响列 x）；y 恒等（dy=0——两副本终点
    // 都在标记行底 + 1em 同一水平位）。若在 t=0 量 toEl 的 rect.y/
    // height：展开向 fold 顶被尚未收缩的 solo 压低约一行（克隆飞到暂
    // 态位、结束再跳回终位＝录屏「先低后跳闪」根因），折叠向 solo 行
    // 盒被 0fr 压扁成高 0（高度门槛误判→整段跳过＝无飞行根因）
    const dx = anchorLeft(toEl) - from.anchor
    const dy = 0
    // 飞行克隆：fixed 钉旧位（脱离折叠容器裁切/透明），位移到新位；
    // 克隆携带原 class，页面样式表自然生效
    const clone = fromEl.cloneNode(true) as HTMLElement
    Object.assign(clone.style, {
      position: 'fixed',
      left: `${from.rect.left}px`,
      top: `${from.rect.top}px`,
      width: `${from.rect.width}px`,
      margin: '0',
      pointerEvents: 'none',
    })
    document.body.appendChild(clone)
    const anim = clone.animate(
      [
        { transform: 'translate(0px, 0px)' },
        { transform: `translate(${dx}px, ${dy}px)` },
      ],
      { duration: 280, easing: 'ease-in-out' },
    )
    fromEl.style.visibility = 'hidden'
    toEl.style.visibility = 'hidden'
    flightRef.current = { anim, clone, fromEl, toEl }
    anim.finished
      .then(() => {
        if (flightRef.current?.anim === anim) cleanupFlight()
      })
      .catch(() => {
        /* cancel 走 cleanupFlight，这里只吞 abort */
      })
  }, [metaExpanded])

  const titleRowContent = (
    <>
      <bdi className="mark-card__title">{mark.title}</bdi>
      {year != null ? <span>({year})</span> : null}
      {/* bdi 隔离：阿文等 RTL 别名在行内自持方向、不扰动前后段 */}
      {mark.alt_title ? <bdi>{mark.alt_title}</bdi> : null}
    </>
  )
  const renderTitleRow = (variant: 'solo' | 'inline') =>
    variant === 'solo' ? (
      <div className="mark-card__title-solo">
        <div
          className={
            settings.collapsedTitleAlign === 'left'
              ? 'mark-card__title-row mark-card__title-row--solo mark-card__title-row--solo-left'
              : 'mark-card__title-row mark-card__title-row--solo'
          }
          ref={soloTitleRef}
        >
          {titleRowContent}
        </div>
      </div>
    ) : (
      <div className="mark-card__title-row" ref={inlineTitleRef}>
        {titleRowContent}
      </div>
    )

  return (
    <li
      className={getCardSurfaceClassName('mark-card')}
      data-expanded={metaExpanded ? 'true' : 'false'}
      /* mac 原生壳右键菜单取条目豆瓣链接/标题用（app-mac 注入脚本按
         li[data-subject-url] 提取）——纯惰性数据属性，web 端零行为差异 */
      data-subject-url={mark.subject_url}
      data-title={mark.title}
    >
      <div className="mark-card__header">
        {/* 日期·动词·星级必须同在一个 span 内：顶栏是 grid，散写的
            文本节点/元素会成为独立 grid 项被自动布局错位 */}
        <span>
          {formatMarkCardDateBarText(mark.date)} {CATEGORY_VERBS[mark.category] ?? '标记'}
          {mark.my_rating ? (
            <>
              {' '}
              <StarRating
                className="mark-card-rating"
                label={`我的评分：${mark.my_rating} 星`}
                value={mark.my_rating}
              />
            </>
          ) : null}
        </span>
        {hasSubjectInfo ? (
          <GhostIconButton
            aria-expanded={metaExpanded}
            aria-label={metaExpanded ? '收起元信息' : '展开元信息'}
            className="mark-card__meta-toggle"
            data-expanded={metaExpanded ? 'true' : undefined}
            onClick={toggleMeta}
            size="sm"
            type="button"
          >
            <ChevronRight aria-hidden="true" />
          </GhostIconButton>
        ) : null}
      </div>
      {renderTitleRow('solo')}
      {hasSubjectInfo ? (
        <div className="mark-card__meta-region" data-expanded={metaExpanded ? 'true' : 'false'}>
          <div className="mark-card__subject">
            {cover ? (
              <MarkCover category={mark.category} cover={cover} key={cover} title={mark.title} />
            ) : (
              <div className="mark-card__cover mark-card__cover--empty" data-category={mark.category} />
            )}
            <div className="mark-card__info">
              {renderTitleRow('inline')}
              {doubanRating != null ? (
                <div className="mark-card__douban-rating">
                  <StarRating
                    className="mark-card-douban-stars"
                    label={`豆瓣评分：${doubanRating.toFixed(1)} 分`}
                    value={doubanRating / 2}
                  />
                  {doubanRating.toFixed(1)}
                </div>
              ) : null}
              {metaLine ? (
                <div className="mark-card__meta">{metaLine}</div>
              ) : mark.intro ? (
                <div className="mark-card__intro">{mark.intro}</div>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}
      {commentDoc ? (
        <div className="mark-card__comment">
          <WdfRender content={commentDoc} />
        </div>
      ) : null}
    </li>
  )
}

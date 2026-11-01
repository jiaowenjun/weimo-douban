import { useEffect } from 'react'
import { useLocation } from 'react-router'
import {
  TopBarNotice,
  TopBarNoticeRing,
} from 'weimo-ui-core/components/top-bar-notice'

import { useDoubanUser } from './douban-user'
import { dismissFirstSyncDone, useFirstSyncPhase } from './first-sync'

/** 让位避让实测（island-demo v1 采纳的生产版）：岛与前置标题/面包屑/
 *  侧栏触发钮/右侧钮是否遮叠无法用媒体查询或 :has 近似（岛宽随文案
 *  变、遮叠随视口与标题长短变），逐元素比对矩形（含 8px 余量），被压
 *  到的挂 island-yield 类渐隐让路、岛退场后 320ms 延迟回弹（时序在
 *  index.css 基态 transition-delay）；岛不可见全摘。挂类走 DOM：目标
 *  是模板/tagtree 组件非本站元素。重测时机=可见性翻转（tick，岛收起
 *  时退场保留令尺寸不变、RO 捕捉不到）+RO（岛宽/视口/标题宽逐拍）+
 *  pathname（换页换标题胶囊后重挂观察） */
function useIslandYield(tick: string, pathname: string) {
  useEffect(() => {
    const noticeEl = document.querySelector('.top-bar-notice')
    if (!noticeEl) return
    const MARGIN = 8
    const targets = () => [
      ...document.querySelectorAll(
        '.top-bar__slot--left .weimo-site-template__top-bar-slot > *',
      ),
      ...document.querySelectorAll('.top-bar__slot--right button'),
    ]
    const update = () => {
      const visible = noticeEl.getAttribute('data-visible') === 'true'
      const island = noticeEl.getBoundingClientRect()
      for (const el of targets()) {
        const rect = el.getBoundingClientRect()
        const hit =
          visible &&
          island.width > 0 &&
          rect.width > 0 &&
          rect.left < island.right + MARGIN &&
          rect.right > island.left - MARGIN
        el.classList.toggle('island-yield', hit)
      }
    }
    update()
    const observer = new ResizeObserver(update)
    observer.observe(noticeEl)
    // frame 宽跟视口走：兜住不发 resize 事件的仿真环境（IAB CDP 视口
    // 缩放实测不发 window resize），也覆盖无 resize 的布局变化
    const frameEl = document.querySelector('.top-bar__frame')
    if (frameEl) observer.observe(frameEl)
    for (const el of targets()) observer.observe(el)
    // 视口变化主通道走 window.resize：RO(documentElement) 在 Chromium
    // 不随视口 resize 触发（实测踩坑），演示页曾靠长标题截断令
    // RO(leading) 触发的侧路侥幸覆盖
    window.addEventListener('resize', update)
    return () => {
      observer.disconnect()
      window.removeEventListener('resize', update)
      document
        .querySelectorAll('.island-yield')
        .forEach((el) => el.classList.remove('island-yield'))
    }
  }, [tick, pathname])
}

/** 顶栏灵动岛（TopBarNotice）：首发同步进度全局可见（欢迎页/任意页
 *  顶部常驻），短文案——syncing「同步中 N%」、done「同步完成，共 N 条」
 *  （duration 2.4s 到点经 onDismissed 收起进 dismissed，退场动画由组件
 *  播完）；文案右侧配进度环（syncing=percent 进度弧，首轮 percent 未
 *  回填环显 0 仅灰底；done=满环+对勾），进度跳变与文案数字同拍。
 *  其余态（triggering/failed/dismissed/未登录）children 置空即收起。
 *  轮询生命周期在 FirstSyncPollingBridge，本组件纯展示 */
export function TopBarSyncNotice() {
  const { uid } = useDoubanUser()
  const phase = useFirstSyncPhase(uid)
  const { pathname } = useLocation()
  // 岛可见性翻转的重测拍（syncing/done 可见，其余态不可见）；岛宽随
  // percent 的逐拍变化由 RO(noticeEl) 捕捉，不必进 tick
  useIslandYield(phase?.phase ?? 'none', pathname)
  const isDone = phase?.phase === 'done'
  const text =
    phase?.phase === 'syncing'
      ? phase.percent != null
        ? `同步中 ${phase.percent}%`
        : '同步中'
      : phase?.phase === 'done'
        ? `同步完成，共 ${phase.items} 条`
        : null
  // 进度环取值：syncing=percent（首轮未回填按 0 仅灰底环）、done=100
  // 满环+对勾；其余态不渲染环（text 已 null）。done 显式传相位机结果
  // 而非吃 progress>=100 默认——后端 percent 可能先于任务终态到满，
  // 对勾须等真完成（core TopBarNoticeRing 的 done 覆盖式为此而设）
  const ringProgress =
    phase?.phase === 'syncing' ? (phase.percent ?? 0) : isDone ? 100 : 0
  return (
    <TopBarNotice
      duration={isDone ? 2_400 : undefined}
      onDismissed={
        uid && isDone
          ? () => dismissFirstSyncDone(uid)
          : undefined
      }
    >
      {text !== null && (
        <>
          <span>{text}</span>
          <TopBarNoticeRing done={isDone} progress={ringProgress} />
        </>
      )}
    </TopBarNotice>
  )
}

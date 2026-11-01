import { TopBarNotice } from 'weimo-ui-core/components/top-bar-notice'

import { useDoubanUser } from './douban-user'
import { dismissFirstSyncDone, useFirstSyncPhase } from './first-sync'

import './top-bar-sync-notice.css'

/** 进度环几何：16 视框、r=6，周长两位小数参与 dasharray/dashoffset */
const RING_CIRCUMFERENCE = 2 * Math.PI * 6

/** 同步进度环：灰底环 + 绿色进度弧，90ms 线性过渡抹平轮询回填的进度
 *  跳变；done=满环内侧描出绿色对勾（线绘制动画，环到对勾一笔相承），
 *  整图再轻微弹性 pop——移植自 weimo-ui-core 演示页 bar.tsx 的
 *  TopBarNoticeProgressRing 样例（配色/几何原样，done 由调用方相位机
 *  给出而非 percent>=100 推导：后端 percent 可能先于任务终态到 100，
 *  对勾须等真完成） */
function SyncProgressRing({ done, progress }: { done?: boolean; progress: number }) {
  const dashOffset = Math.round(RING_CIRCUMFERENCE * (1 - progress / 100) * 100) / 100

  return (
    <svg
      aria-hidden="true"
      className="top-bar-sync-notice__ring"
      data-done={done}
      viewBox="0 0 16 16"
    >
      <circle
        className="top-bar-sync-notice__ring-track"
        cx="8"
        cy="8"
        fill="none"
        r="6"
        strokeWidth="2.5"
      />
      <circle
        className="top-bar-sync-notice__ring-arc"
        cx="8"
        cy="8"
        fill="none"
        r="6"
        strokeDasharray={Math.round(RING_CIRCUMFERENCE * 100) / 100}
        strokeDashoffset={dashOffset}
        strokeLinecap="round"
        strokeWidth="2.5"
        transform="rotate(-90 8 8)"
      />
      {done && (
        <path
          className="top-bar-sync-notice__ring-check"
          d="M5 8.6 L7 10.6 L11 5.4"
          fill="none"
          pathLength={1}
          strokeDasharray={1}
          strokeDashoffset={0}
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="2.2"
        />
      )}
    </svg>
  )
}

/** 顶栏灵动岛（TopBarNotice）：首发同步进度全局可见（欢迎页/任意页
 *  顶部常驻），短文案——syncing「同步中 N%」、done「同步完成，共 N 条」
 *  （duration 2.4s 到点经 onDismissed 收起进 dismissed，退场动画由组件
 *  播完）；文案右侧配进度环（syncing=percent 进度弧，首轮 percent 未
 *  回填环显 0 仅灰底；done=满环+对勾），进度跳变与文案数字同拍。其余
 *  态（triggering/failed/dismissed/未登录）children 置空即收起。轮询
 *  生命周期在 FirstSyncPollingBridge，本组件纯展示 */
export function TopBarSyncNotice() {
  const { uid } = useDoubanUser()
  const phase = useFirstSyncPhase(uid)
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
  // 满环+对勾；其余态不渲染环（text 已 null）
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
          <SyncProgressRing done={isDone} progress={ringProgress} />
        </>
      )}
    </TopBarNotice>
  )
}

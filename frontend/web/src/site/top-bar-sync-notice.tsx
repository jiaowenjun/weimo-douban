import {
  TopBarNotice,
  TopBarNoticeRing,
} from 'weimo-ui-core/components/top-bar-notice'

import { useDoubanUser } from './douban-user'
import { dismissFirstSyncDone, useFirstSyncPhase } from './first-sync'

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

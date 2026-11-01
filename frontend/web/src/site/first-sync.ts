/** 首发同步会话级状态机 + 生命周期轮询（原住 marks-page，灵动岛通知
 *  上移 App 层后进度须全局可见、轮询不能随页面卸载停——提为本模块，
 *  订阅挂 DoubanUserProvider 内的 FirstSyncPollingBridge，展示挂
 *  top-bar-sync-notice（App topBarCenter），触发由 marks-page 的
 *  not-cached 态调用） */
import { useContext, useEffect, useSyncExternalStore } from 'react'

import { queryClient } from '../query-client'
import { CacheVersionContext } from './cache-version'
import { fetchDoubanSyncTask, triggerDoubanFirstSync } from './api'
import { doubanQueryKeys } from './douban-queries'

/** uid → 进度。triggering=POST 进行中；syncing=任务运行中（携带 task_id
 *  供轮询，percent 随轮询回填）；done=任务到终态（completed/partial），
 *  完成通知展示中（灵动岛 duration 到点转 dismissed）；dismissed=通知
 *  已收起（含 failed 终态，静默收起不打扰）；failed=触发失败（回落
 *  手动指引文案） */
type FirstSync =
  | { phase: 'triggering' }
  | { phase: 'syncing'; taskId: string; percent?: number }
  | { phase: 'done'; items: number }
  | { phase: 'dismissed' }
  | { phase: 'failed' }

/** Map 是模块级可变源，组件须经 useSyncExternalStore 订阅读取——render
 *  期直读会被 React Compiler 按 uid 记忆化，异步回填后重渲染拿的是
 *  缓存值（展示不切换的根因） */
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

/** 后端任务终态（TERMINAL_STATUSES 同款）：到任一终态即停止任务轮询 */
const SYNC_TASK_TERMINAL = new Set(['completed', 'partial', 'failed'])

/** 触发首发同步（幂等：会话级 Map 守卫，重复调用零副作用；后端
 *  single-flight 并入运行中任务）；触发失败本会话不自动重试（刷新
 *  页面重走一次） */
export function triggerFirstSyncOnce(uid: string) {
  if (firstSyncState.has(uid)) return
  setFirstSync(uid, { phase: 'triggering' })
  void triggerDoubanFirstSync(uid).then((task) => {
    setFirstSync(
      uid,
      task ? { phase: 'syncing', taskId: task.task_id } : { phase: 'failed' },
    )
  })
}

/** 完成通知收起（灵动岛 duration 到点回调）：done → dismissed */
export function dismissFirstSyncDone(uid: string) {
  if (firstSyncState.get(uid)?.phase === 'done') {
    setFirstSync(uid, { phase: 'dismissed' })
  }
}

/** 订阅首发同步进度（任意组件可读；轮询生命周期归 FirstSyncPollingBridge） */
export function useFirstSyncPhase(uid: string | null): FirstSync | undefined {
  return useSyncExternalStore(
    subscribeFirstSync,
    () => (uid ? firstSyncState.get(uid) : undefined),
    () => undefined,
  )
}

/** 生命周期轮询（挂 DoubanUserProvider 内全局唯一实例）——syncing 任务
 *  或富集 pending>0（versions.subjects_pending，旧后端按 0）期间每 3s
 *  失效版本查询：首发直写的每页落库与富集逐条落库都经 revision bump →
 *  版本 key 变化 → 数据查询自动重拉（60s 常规轮询兜底）；同步中同时查
 *  任务终态——到 completed/partial 给完成句号（灵动岛 duration 自动收
 *  起），failed 静默。轮询失败静默由下一 tick 重试；存活条件用布尔
 *  （pending 数值每条富集都变，作依赖会反复重启 interval） */
export function FirstSyncPollingBridge({ uid }: { uid: string | null }) {
  const syncPhase = useFirstSyncPhase(uid)
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
              // 几页落库尽快拉全（完成通知报「共 N 条」时列表尽量对齐）
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
  return null
}

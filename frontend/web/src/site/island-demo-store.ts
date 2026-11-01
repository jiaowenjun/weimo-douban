import { useSyncExternalStore } from 'react'

/** 灵动岛遮挡改善方案对比演示（#/island-demo）的全局状态——岛挂在
 *  顶栏（App topBarCenter）、控制器在页面体内、让位 css 键于 #root
 *  属性，三方共读一份；与 first-sync 同款模块级 store + 订阅器
 *  （React Compiler 下组件必须经 useSyncExternalStore 读模块态）。
 *  仅演示路由使用，不影响线上同步通知链路 */
export type IslandDemoVariant = 'baseline' | 'v1' | 'v2' | 'v3' | 'v4'

export type IslandDemoState = {
  /** 当前对比方案：baseline=现状基线；v1 让位避让；v2 渐变模糊光晕；
   *  v3 下移悬浮；v4 岛瘦身。驱动 #root data-island-demo 与岛渲染分支 */
  variant: IslandDemoVariant
  /** 前置标题切长文案（模拟分类页长面包屑，宽屏也能撞上岛） */
  longTitle: boolean
  /** 岛是否可见（false=children 置空收起，可观察 v1 回弹时序） */
  active: boolean
  /** 完成态（满环+对勾文案） */
  done: boolean
  /** 进度 0-100（滑杆/播放器驱动） */
  progress: number
  /** 播放一轮的轮次号：递增重启计时器；手动干预归 0 停表 */
  run: number
}

let islandDemoState: IslandDemoState = {
  variant: 'baseline',
  longTitle: false,
  active: false,
  done: false,
  progress: 0,
  run: 0,
}

const islandDemoListeners = new Set<() => void>()

export function setIslandDemo(patch: Partial<IslandDemoState>) {
  islandDemoState = { ...islandDemoState, ...patch }
  for (const notify of islandDemoListeners) notify()
}

export function useIslandDemo(): IslandDemoState {
  return useSyncExternalStore(
    (notify) => {
      islandDemoListeners.add(notify)
      return () => islandDemoListeners.delete(notify)
    },
    () => islandDemoState,
    () => islandDemoState,
  )
}

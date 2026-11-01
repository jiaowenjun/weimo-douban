import { useEffect } from 'react'

import { FrostedLabel } from 'weimo-ui-core/components/frosted-label'
import {
  TopBarNotice,
  TopBarNoticeRing,
} from 'weimo-ui-core/components/top-bar-notice'

import {
  setIslandDemo,
  useIslandDemo,
  type IslandDemoVariant,
} from './island-demo-store'

import './island-demo.css'

/** 方案卡：baseline=现状基线（不动），v1-v4 对应四套改善方案 */
const ISLAND_DEMO_VARIANTS: Array<{
  id: IslandDemoVariant
  name: string
  summary: string
}> = [
  {
    id: 'baseline',
    name: '基线（现状）',
    summary: '现网行为不动：岛严格居中，窄幅下盖住标题胶囊尾部',
  },
  {
    id: 'v1',
    name: 'v1 让位避让',
    summary: '岛可见时标题渐隐让路、退出后延迟回弹（纯 CSS :has，内容绕开岛）',
  },
  {
    id: 'v2',
    name: 'v2 渐变模糊光晕',
    summary: '岛外扩两层渐进 backdrop 模糊+淡色晕，周边内容近岛雾化',
  },
  {
    id: 'v3',
    name: 'v3 下移悬浮',
    summary: '岛可见时降到顶栏下方悬浮；演示为恒降，生产按实测避让',
  },
  {
    id: 'v4',
    name: 'v4 岛瘦身',
    summary: '文字去百分比由环承载进度，岛宽约 -25%，多数宽度不再相撞',
  },
]

/** 播放节奏：6s 走满、满环停 2.4s 收起（对齐生产 done 自动收起时长） */
const DEMO_SYNC_DURATION = 6_000
const DEMO_DONE_HOLD = 2_400

/** 演示岛（顶栏 topBarCenter）：按 store.variant 渲染各方案形态。
 *  播放器挂在岛侧——生命周期与岛同进退；手动滑杆/完成勾选/收起均
 *  归 0 run 停表。文案 v4 去百分比、其余与生产同步通知一致 */
export function IslandDemoNotice() {
  const demo = useIslandDemo()

  useEffect(() => {
    if (demo.run === 0) return

    const startedAt = performance.now()
    let holdTimer: ReturnType<typeof setTimeout> | undefined
    const progressTimer = setInterval(() => {
      const elapsed = performance.now() - startedAt
      const next = Math.min(100, Math.round((elapsed / DEMO_SYNC_DURATION) * 100))
      setIslandDemo({ progress: next })

      if (next >= 100) {
        clearInterval(progressTimer)
        setIslandDemo({ done: true })
        holdTimer = setTimeout(() => setIslandDemo({ active: false, run: 0 }), DEMO_DONE_HOLD)
      }
    }, 80)

    return () => {
      clearInterval(progressTimer)
      if (holdTimer !== undefined) clearTimeout(holdTimer)
    }
  }, [demo.run])

  const text = demo.done
    ? '同步完成，共 291 条'
    : demo.variant === 'v4'
      ? '同步中'
      : `同步中 ${demo.progress}%`

  const notice = (
    <TopBarNotice>
      {demo.active ? (
        <>
          <span>{text}</span>
          <TopBarNoticeRing done={demo.done} progress={demo.done ? 100 : demo.progress} />
        </>
      ) : null}
    </TopBarNotice>
  )

  // v2 光晕/v3 下移需要岛外包装层（伪元素雾晕不进岛内、位移动画不与
  // 岛自身 scale 进出场打架），其余方案岛直接落位
  if (demo.variant === 'v2') {
    return <span className="island-demo-halo-host">{notice}</span>
  }
  if (demo.variant === 'v3') {
    return <span className="island-demo-drop-host">{notice}</span>
  }
  return notice
}

/** 演示前置标题（顶栏 leading 槽）：短案对标「全部标记」页标题胶囊，
 *  长案模拟分类页长面包屑（宽屏也撞得上岛）；v1 让位 css 锚在本类上 */
export function IslandDemoLeading() {
  const demo = useIslandDemo()

  return (
    <FrostedLabel className="island-demo-leading" size="lg">
      {demo.longTitle ? '影视/美国/科幻冒险/九十年代经典长片' : '岛对比'}
    </FrostedLabel>
  )
}

/** 对比演示页（#/island-demo）：方案卡单选 + 播放/手动控制；效果落在
 *  顶栏（岛居中、标题在左）——窄窗口（iPhone 幅宽）最接近实拍撞车场景 */
export function IslandDemoPage() {
  const demo = useIslandDemo()

  // 方案选择落到 #root data-island-demo：v1/v3 的让位/下移 css 以它为
  // 开关（与生产拟采用的选择器机制同构），离开页面即摘除
  useEffect(() => {
    const root = document.getElementById('root')
    if (!root) return
    root.dataset.islandDemo = demo.variant
    return () => {
      delete root.dataset.islandDemo
    }
  }, [demo.variant])

  return (
    <div className="island-demo">
      <p className="island-demo__hint">
        效果在顶栏：选方案后「播放一轮」看进出场与回弹，或拖进度手动对比；
        窄窗口（≈iPhone 幅宽）最接近实拍撞车场景，长标题模拟分类页面包屑。
      </p>
      <div className="island-demo__cards" role="radiogroup" aria-label="遮挡改善方案">
        {ISLAND_DEMO_VARIANTS.map((item) => (
          <button
            aria-checked={demo.variant === item.id}
            className="island-demo__card"
            key={item.id}
            onClick={() => setIslandDemo({ variant: item.id })}
            role="radio"
            type="button"
          >
            <span className="island-demo__card-name">{item.name}</span>
            <span className="island-demo__card-summary">{item.summary}</span>
          </button>
        ))}
      </div>
      <div className="island-demo__controls">
        <button
          className="island-demo__control"
          onClick={() =>
            setIslandDemo({ active: true, done: false, progress: 0, run: demo.run + 1 })
          }
          type="button"
        >
          播放一轮
        </button>
        <button
          className="island-demo__control"
          onClick={() => setIslandDemo({ active: false, run: 0 })}
          type="button"
        >
          收起
        </button>
        <label className="island-demo__control">
          进度
          <input
            onChange={(event) =>
              setIslandDemo({
                active: true,
                done: false,
                progress: Number(event.target.value),
                run: 0,
              })
            }
            type="range"
            value={demo.done ? 100 : demo.progress}
          />
          {demo.done ? 100 : demo.progress}%
        </label>
        <label className="island-demo__control">
          <input
            checked={demo.done}
            onChange={(event) =>
              setIslandDemo({ active: true, done: event.target.checked, run: 0 })
            }
            type="checkbox"
          />
          完成态
        </label>
        <label className="island-demo__control">
          <input
            checked={demo.longTitle}
            onChange={(event) => setIslandDemo({ longTitle: event.target.checked })}
            type="checkbox"
          />
          长标题
        </label>
      </div>
    </div>
  )
}

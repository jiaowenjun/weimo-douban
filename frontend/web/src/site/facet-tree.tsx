import { useMemo } from 'react'
import type { ReactNode } from 'react'
import { BookOpen, Clapperboard, Music } from 'lucide-react'

import { TagTree } from 'weimo-ui-tagtree/components/tag-tree'
import type { TagTreeNode } from 'weimo-ui-tagtree/components/tag-tree'

import type { DoubanDims } from './api'
import { aggregateDims } from './facet-engine'
import {
  GROUP_DIMENSION,
  PRIMARY_DIMENSION,
  escapeFacetValue,
  facetValueLabel,
  pathTag,
} from './facet-model'
import type { FacetStep } from './facet-model'
import {
  CATEGORY_NOUNS,
  isMarksCategory,
  MARKS_CATEGORIES,
  type NavTarget,
} from './navigation'
import type { MarksCategory } from './navigation'

/** 一级类别根行图标（根兼任类别导航，观感对齐原导航树行） */
const CATEGORY_ICONS: Record<MarksCategory, ReactNode> = {
  book: <BookOpen aria-hidden="true" />,
  movie: <Clapperboard aria-hidden="true" />,
  music: <Music aria-hidden="true" />,
}

type TagAction = {
  category: MarksCategory
  /** 点击该节点应设置的筛选路径（空路径 = 清除） */
  path: FacetStep[]
}

type TreeIndex = {
  nodes: TagTreeNode[]
  /** tag 路径 → 点击动作 */
  byTag: Map<string, TagAction>
}

/** 主维度值叶构建（条件聚合按 filters 前缀，叶路径= filters+叶步）：
 *  挂分组值叶下（三层，topKUnder 封顶语境），分组维度全无值的兜底
 *  直挂类别根下（两层，singleton 合并语境） */
function buildPrimaryLeaves(
  category: MarksCategory,
  items: DoubanDims['items'],
  filters: readonly FacetStep[],
  parentTag: string,
  byTag: Map<string, TagAction>,
): TagTreeNode[] {
  const { key: leafDim } = PRIMARY_DIMENSION[category]
  const scoped = aggregateDims(category, items, filters)
  return (scoped[leafDim] ?? []).map(({ value, count }) => {
    const tag = `${parentTag}/${leafDim}/${escapeFacetValue(value)}`
    byTag.set(tag, {
      category,
      path: [...filters, { dim: leafDim, value }],
    })
    return {
      tag,
      label: facetValueLabel(category, leafDim, value),
      itemCount: count,
    }
  })
}

/** 分组值叶 + 其下主维度值叶（三层钻取的下两层，分组维度见
 *  GROUP_DIMENSION）：分组值叶挂 parent 下（条件聚合，country/genre
 *  值叶自身 topK 封顶、decade 不封顶），每片叶再挂主维度值叶；无分组
 *  维度或分组维度全无值时兜底回主维度直挂（两层） */
function buildGroupedChildren(
  category: MarksCategory,
  items: DoubanDims['items'],
  parentFilter: readonly FacetStep[],
  parentTag: string,
  byTag: Map<string, TagAction>,
): TagTreeNode[] {
  const group = GROUP_DIMENSION[category]
  if (!group) {
    return buildPrimaryLeaves(category, items, parentFilter, parentTag, byTag)
  }
  const groups = aggregateDims(category, items, parentFilter)[group.key] ?? []
  let children: TagTreeNode[] = groups.map(({ value, count }) => {
    const tag = `${parentTag}/${group.key}/${escapeFacetValue(value)}`
    const path: FacetStep[] = [...parentFilter, { dim: group.key, value }]
    byTag.set(tag, { category, path })
    return {
      tag,
      label: facetValueLabel(category, group.key, value),
      itemCount: count,
      children: buildPrimaryLeaves(category, items, path, tag, byTag),
    }
  })
  if (children.length === 0) {
    children = buildPrimaryLeaves(
      category,
      items,
      parentFilter,
      parentTag,
      byTag,
    )
  }
  return children
}

/** 三类别常驻树（三类别同构）：「类别根（点行=跳该类别清筛选）→
 *  分组值叶 → 主维度值叶」三层（影视 国家/地区→类型、图书 年代→
 *  出版社、音乐 流派→年代，见 buildGroupedChildren；分组维度全无值
 *  兜底回主维度直挂两层）。行计数走 itemCount（tagtree 0.3.0 起渲染
 *  在行尾折叠开关左侧，与开关同占位色），根行计数=类别条目总数；
 *  计数已按降序返回直接保序；某类别缺失（未同步/拉取失败）或无值
 *  则不出根。叶 tag 保持「类别/维度(+/维度)/值」多段——中间维度
 *  不出现在行 label 但保留在 tag 语义中（选中叶的祖先自动展开按树
 *  结构生效） */
function buildTree(
  dimsByCategory: Partial<Record<MarksCategory, DoubanDims>>,
): TreeIndex {
  const byTag = new Map<string, TagAction>()
  const nodes = MARKS_CATEGORIES.flatMap((category) => {
    const dims = dimsByCategory[category]
    if (!dims) return []
    const children = buildGroupedChildren(
      category,
      dims.items,
      [],
      category,
      byTag,
    )
    if (children.length === 0) return []
    return [
      {
        tag: category,
        label: CATEGORY_NOUNS[category],
        // 根行计数=类别条目总数（items 为全量轻量投影）——不是叶计数之和：
        // 主维度可能长尾合并（__other__）或有条目无该维度值，求和会偏小
        itemCount: dims.items.length,
        icon: CATEGORY_ICONS[category],
        children,
      },
    ]
  })
  return { nodes, byTag }
}

/** 分面筛选树（纯展示 + 路径回调）：一级类别根点行=跳该类别清筛选
 *  （onPathSelect 上抛空路径，展开/收起仍走行内折叠开关）；各值叶是
 *  筛选条件——点击设路径（已选同路径再点取消），跨类别值叶交上层跳
 *  路由。展开态受控由上层持有（选中叶的祖先链 TagTree 会自动展开） */
export function FacetTree({
  activeCategory,
  dimsByCategory,
  expandedTags,
  onExpandedTagsChange,
  onPathSelect,
  path,
}: {
  /** 当前路由目标：真实类别或 all（合并视图，无分面）；选中指示必属
   *  它（URL 派生）。演示页等无类别时传 undefined */
  activeCategory: NavTarget | undefined
  /** 三类别维度数据（聚合/条件计数由 facet-engine 本地算） */
  dimsByCategory: Partial<Record<MarksCategory, DoubanDims>>
  /** 当前类别的筛选路径（URL 派生，顺序即步骤） */
  path: FacetStep[]
  expandedTags: string[]
  onExpandedTagsChange: (tags: string[]) => void
  /** 路径改写：目标类别 + 完整路径（空数组 = 清除筛选） */
  onPathSelect: (target: NavTarget, path: FacetStep[]) => void
}) {
  const { nodes, byTag } = useMemo(
    () => buildTree(dimsByCategory),
    [dimsByCategory],
  )
  // 选中指示两档：筛选中选中末步对应节点；无筛选时当前导航目标顶上
  // （类别根或「全部标记」——原导航树的当前位置指示由根接管）
  const selectedTag =
    activeCategory !== undefined && path.length > 0
      ? pathTag(activeCategory, path)
      : activeCategory

  return (
    <TagTree
      aria-label="分面筛选"
      // 空树只剩「从未同步」一种可达路径（失败走侧边栏重试行），默认
      // 「暂无标签」指代不明，换分类语境文案
      emptyLabel="暂无分类数据"
      expandedTags={expandedTags}
      nodes={nodes}
      onExpandedTagsChange={onExpandedTagsChange}
      onSelect={(tag) => {
        // 一级类别根（图书/音乐，tag 无 /）：点行=跳该类别清筛选
        if (!tag.includes('/')) {
          if (isMarksCategory(tag)) onPathSelect(tag, [])
          return
        }
        const hit = byTag.get(tag)
        if (!hit) return
        // 再点已选路径 = 取消；跨类别/其他路径 = 设置
        const isSame =
          activeCategory === hit.category &&
          hit.path.length === path.length &&
          hit.path.every(
            (step, i) => path[i].dim === step.dim && path[i].value === step.value,
          )
        onPathSelect(hit.category, isSame ? [] : hit.path)
      }}
      selectedTag={selectedTag}
      variant="no-action"
    />
  )
}

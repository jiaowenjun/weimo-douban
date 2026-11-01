/** 标签树的展示逻辑引擎（纯函数，无副作用）——聚合、长尾 singleton
 *  合并、条件聚合、路径筛选全在前端完成；后端只出维度原始数据
 *  （subject-dims 端点）。所有规则与树交互的口径唯一：树显示多少、
 *  筛选就命中多少。 */

import type { DoubanDimsItem } from './api'
import {
  DIMENSIONS,
  GROUP_DIMENSION,
  OTHER_FACET_VALUE,
  PRIMARY_DIMENSION,
  type FacetStep,
} from './facet-model.ts'
import type { MarksCategory } from './navigation'

/** 维度键 → dims 投影的数组字段名（派生维度 decade 走特判） */
const DIM_FIELDS: Record<string, string> = {
  author: 'authors',
  country: 'countries',
  director: 'directors',
  genre: 'genres',
  press: 'press',
  singer: 'singers',
  translator: 'translators',
}

/** 长尾合并的维度开关：仅树上展示且长尾明显的维度开启——影视类型、
 *  图书出版社；音乐流派不合并（值数少，singleton 保留原样更直观） */
function mergeEnabled(category: MarksCategory, dim: string): boolean {
  return (
    (category === 'movie' && dim === 'genre') ||
    (category === 'book' && dim === 'press')
  )
}

/** 条目在某维度的值集合（派生维度含中文语义：decade 由 year 每 10
 *  年派生）；数组保序去重（intro 合成可能重复），无值返回空集合 */
function itemValues(dim: string, item: DoubanDimsItem): string[] {
  if (dim === 'decade') {
    const y = item.dims.year
    return y && y > 1900 && y < 2100 ? [`${Math.floor(y / 10) * 10}s`] : []
  }
  const field = DIM_FIELDS[dim]
  const list = field ? item.dims[field as keyof DoubanDimsItem['dims']] : undefined
  if (!Array.isArray(list)) return []
  const out: string[] = []
  for (const v of list) {
    if (v && !out.includes(v)) out.push(v)
  }
  return out
}

/** 维度的树上展示值上限（DIMENSIONS 配置 topK；未配置即不封顶） */
function topKOf(category: MarksCategory, dim: string): number | undefined {
  return DIMENSIONS[category].find((d) => d.key === dim)?.topK
}

/** 主维度挂分组值叶下（树三层钻取，见 GROUP_DIMENSION）的封顶值：
 *  五个最多的值 + 其他 */
const PRIMARY_TOP_K = 5

/** 维度在给定步骤序列下的树上展示值上限：主维度仅当序列含本类别
 *  分组维度步时封顶（PRIMARY_TOP_K），其余场合（分组维度无值的
 *  兜底两层、音乐主维度直挂）不封顶、走 singleton 合并；分组维度
 *  自身（country）恒为 DIMENSIONS 配置的 topK——展示聚合（传
 *  filters）与 __other__ 匹配（传该步前缀）两侧同源此判定，口径唯一 */
function topKUnder(
  category: MarksCategory,
  dim: string,
  steps: readonly Pick<FacetStep, 'dim'>[],
): number | undefined {
  const group = GROUP_DIMENSION[category]
  if (
    group &&
    dim === PRIMARY_DIMENSION[category].key &&
    steps.some((s) => s.dim === group.key)
  ) {
    return PRIMARY_TOP_K
  }
  return topKOf(category, dim)
}

/** 某维度在某条目集上的值计数（聚合与 __other__ 匹配共用的底座） */
function valueCounts(
  dim: string,
  items: readonly DoubanDimsItem[],
): Map<string, number> {
  const counts = new Map<string, number>()
  for (const item of items) {
    for (const v of itemValues(dim, item)) {
      counts.set(v, (counts.get(v) ?? 0) + 1)
    }
  }
  return counts
}

/** 某维度在某条目集上的 singleton 值集（计数恰为 1 的值）——「其他」
 *  聚合与过滤匹配的共同依据 */
function singletonSet(
  dim: string,
  items: readonly DoubanDimsItem[],
): Set<string> {
  const counts = valueCounts(dim, items)
  const out = new Set<string>()
  for (const [value, count] of counts) {
    if (count === 1) out.add(value)
  }
  return out
}

/** __other__ 步骤的命中规则（与聚合折叠同源，树显示多少筛选就命中
 *  多少）：topK 维度=含「展示集外」任意值——展示集按传入的（该步
 *  前缀）命中集内计数降序（同计数按值升序）取前 topK 现算，保证与
 *  树上看到的展示集一致；其余维度（singleton 折叠）=含 singleton 值。
 *  topK 由调用方按 topKUnder(该步前缀) 判定传入 */
function matchesOtherBucket(
  dim: string,
  values: readonly string[],
  scoped: readonly DoubanDimsItem[],
  topK: number | undefined,
): boolean {
  if (topK !== undefined) {
    const ranked = [...valueCounts(dim, scoped).entries()].sort(
      (a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1),
    )
    const displayed = new Set(ranked.slice(0, topK).map(([v]) => v))
    return values.some((v) => !displayed.has(v))
  }
  const singletons = singletonSet(dim, scoped)
  return values.some((v) => singletons.has(v))
}

/** 单条目是否命中筛选路径（各步 AND，逐步累积）。__other__ 步的桶
 *  展示集按「该步之前的前缀步骤命中集」计算——与树上该层的条件聚合
 *  口径一致（桶成员资格在节点展示时已定，不随其后步骤重算，否则
 *  「其他地区→剧情」的树上计数与点击命中会因前五集合在前缀子集内
 *  重排而漂移）。普通步直接包含判定 */
export function itemMatches(
  category: MarksCategory,
  item: DoubanDimsItem,
  steps: readonly FacetStep[],
  allItems: readonly DoubanDimsItem[],
): boolean {
  if (steps.length === 0) return true
  let scope = allItems
  for (let i = 0; i < steps.length; i++) {
    const step = steps[i]
    const stepTopK = topKUnder(category, step.dim, steps.slice(0, i))
    const hit =
      step.value !== OTHER_FACET_VALUE
        ? itemValues(step.dim, item).includes(step.value)
        : matchesOtherBucket(
            step.dim,
            itemValues(step.dim, item),
            scope,
            stepTopK,
          )
    if (!hit) return false
    scope = scope.filter((it) =>
      step.value !== OTHER_FACET_VALUE
        ? itemValues(step.dim, it).includes(step.value)
        : matchesOtherBucket(
            step.dim,
            itemValues(step.dim, it),
            scope,
            stepTopK,
          ),
    )
  }
  return true
}

export type DimCounts = { value: string; count: number }[]

/** 聚合：各维度值计数（计数 = 含该值的条目数），排序计数降序、同计数
 *  按值升序；例外 decade——年代值叶不按计数排，按年代本身升序（值
 *  格式统一「YYYYs」，字符串升序即年代先后）。封顶语境（topKUnder
 *  按 filters 判定，分组值叶自身与分组之下的主维度）优先且独占：值数
 *  超上限时仅留计数前 topK，其余收 __other__（计数 = 含展示集外值的
 *  条目数，多值条目只计一次）；未超上限原样全展——singleton 合并
 *  不参与（封顶已保证行数上限，小桶折叠反而丢信息）。非封顶语境的
 *  开启维度 singleton 值 ≥2 个时收进 __other__ 钉末尾（仅 1 个保留
 *  原值）。filters 提供时先按命中集过滤再聚合（条件聚合，如分组值
 *  叶分支内的类型分布） */
export function aggregateDims(
  category: MarksCategory,
  items: readonly DoubanDimsItem[],
  filters: readonly FacetStep[] = [],
): Record<string, DimCounts> {
  const scoped = filterItems(category, items, filters)
  const out: Record<string, DimCounts> = {}
  for (const { key: dim } of DIMENSIONS[category]) {
    const list: DimCounts = [...valueCounts(dim, scoped)].map(
      ([value, count]) => ({ value, count }),
    )
    // decade 按年代本身升序（见头注释）；其余维度计数降序、同计数值升序
    if (dim === 'decade') {
      list.sort((a, b) => (a.value < b.value ? -1 : 1))
    } else {
      list.sort((a, b) => b.count - a.count || (a.value < b.value ? -1 : 1))
    }
    const topK = topKUnder(category, dim, filters)
    if (topK !== undefined) {
      if (list.length > topK) {
        const displayed = new Set(list.slice(0, topK).map((x) => x.value))
        const restCount = scoped.filter((it) =>
          itemValues(dim, it).some((v) => !displayed.has(v)),
        ).length
        out[dim] = [
          ...list.slice(0, topK),
          { value: OTHER_FACET_VALUE, count: restCount },
        ]
      } else {
        out[dim] = list
      }
      continue
    }
    if (mergeEnabled(category, dim)) {
      const singletons = new Set(
        list.filter((x) => x.count === 1).map((x) => x.value),
      )
      if (singletons.size >= 2) {
        const otherCount = scoped.filter((it) =>
          itemValues(dim, it).some((v) => singletons.has(v)),
        ).length
        out[dim] = [
          ...list.filter((x) => x.count > 1),
          { value: OTHER_FACET_VALUE, count: otherCount },
        ]
        continue
      }
    }
    out[dim] = list
  }
  return out
}

/** 路径 → 命中条目 id（保持 items 的 date DESC 稳定序——分页切片的
 *  顺序权威） */
export function filterIds(
  category: MarksCategory,
  items: readonly DoubanDimsItem[],
  path: readonly FacetStep[],
): string[] {
  return filterItems(category, items, path).map((item) => item.id)
}

/** Each prefix and its "other" bucket are computed once for the whole collection. */
function filterItems(
  category: MarksCategory,
  items: readonly DoubanDimsItem[],
  path: readonly FacetStep[],
): readonly DoubanDimsItem[] {
  let scope = items
  for (let i = 0; i < path.length; i++) {
    const step = path[i]
    if (step.value !== OTHER_FACET_VALUE) {
      scope = scope.filter((item) => itemValues(step.dim, item).includes(step.value))
      continue
    }
    const counts = valueCounts(step.dim, scope)
    const topK = topKUnder(category, step.dim, path.slice(0, i))
    const bucket = topK === undefined
      ? new Set([...counts].filter(([, count]) => count === 1).map(([value]) => value))
      : new Set([...counts].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
        .slice(topK).map(([value]) => value))
    scope = scope.filter((item) => itemValues(step.dim, item).some((value) => bucket.has(value)))
  }
  return scope
}

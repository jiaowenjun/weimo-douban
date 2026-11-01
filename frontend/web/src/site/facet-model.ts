import type { MarksCategory, NavTarget } from './navigation'

/** 分面维度的展示顺序与文案（键集合由后端保证，顺序与命名归前端）。
 *  作 URL ?facet= 白名单校验与 meta 行文案；树上按维度另配展示规则：
 *  三类别同构走分组值叶→主维度两层值叶（见
 *  GROUP_DIMENSION/PRIMARY_DIMENSION），country 值叶封顶（条目
 *  topK=5），分组之下的主维度封顶为条件规则——均见
 *  facet-engine topKUnder */
export const DIMENSIONS: Record<
  MarksCategory,
  readonly { key: string; label: string; topK?: number; otherLabel?: string }[]
> = {
  movie: [
    { key: 'genre', label: '类型' },
    // topK：树上仅展示计数前五的值，其余收进其他桶（facet-engine 聚合
    // 与 __other__ 匹配同源此值）；otherLabel：其他桶展示名（缺省
    // 「其他+维度名」→ 此处按需作「其他地区」）
    { key: 'country', label: '国家/地区', topK: 5, otherLabel: '其他地区' },
    { key: 'decade', label: '年代' },
    { key: 'director', label: '导演' },
  ],
  book: [
    { key: 'press', label: '出版社' },
    { key: 'author', label: '作者' },
    { key: 'translator', label: '译者' },
    { key: 'decade', label: '年代' },
  ],
  music: [
    { key: 'genre', label: '流派' },
    { key: 'singer', label: '歌手' },
    { key: 'decade', label: '年代' },
  ],
}

/** 每类别的树展示主维度（底层值叶，挂分组值叶之下见
 *  GROUP_DIMENSION）：影视=类型、图书=出版社（图书无类型/分类数据
 *  ——rexxar tags 仅 2/55 有值、丛书聚合 ≤2，出版社是 book 元数据里
 *  头部聚合力最强的内容谱系维度）、音乐=年代 */
export const PRIMARY_DIMENSION: Record<
  MarksCategory,
  { key: string; label: string }
> = {
  book: { key: 'press', label: '出版社' },
  movie: { key: 'genre', label: '类型' },
  music: { key: 'decade', label: '年代' },
}

/** 主维度之上的分组维度（树为 类别根 → 分组值叶 → 主维度值叶 三层，
 *  值叶均按计数降序）：影视=country、图书=decade、音乐=genre（均挂
 *  类别根下；decade 由 year 每 10 年派生）。
 *  分组值叶自身封顶走 DIMENSIONS 条目 topK（country=5，decade/genre
 *  不封顶），分组之下的主维度值叶封顶是条件规则——facet-engine
 *  topKUnder 按本表判定 */
export const GROUP_DIMENSION: Record<
  MarksCategory,
  { key: string; label: string } | null
> = {
  book: { key: 'decade', label: '年代' },
  movie: { key: 'country', label: '国家/地区' },
  music: { key: 'genre', label: '流派' },
}

/** 后端长尾合并的保留字面量（计数 1 的值聚合进它，仅影视类型与图书
 *  出版社开启；topK 封顶的其余桶亦复用它——country 值叶与 country
 *  下的 genre 三级，同一次聚合封顶与 singleton 二选一，见
 *  facet-engine），与后端 facets::OTHER_VALUE 对齐 */
export const OTHER_FACET_VALUE = '__other__'

/** 值的展示名：保留字面量 → 维度配置的 otherLabel（其他地区），
 *  缺省「其他+维度名」（其他类型/其他出版社），其余原样 */
export function facetValueLabel(
  category: MarksCategory,
  dim: string,
  value: string,
): string {
  if (value !== OTHER_FACET_VALUE) return value
  const entry = DIMENSIONS[category].find((d) => d.key === dim)
  return entry?.otherLabel ?? `其他${entry?.label ?? dim}`
}

/** 一步筛选（dim/value 均为后端原始值，不转义） */
export type FacetStep = {
  dim: string
  value: string
}

/** 树标签值含 / 会破坏 tag 路径的层级语义，替换为全角（树构建与
 *  选中指示共用） */
export function escapeFacetValue(value: string): string {
  return value.replaceAll('/', '／')
}

/** 路径 → 请求/URL 用的 facet 参数串（dim=value;dim=value，顺序即步骤） */
export function facetPathKey(path: readonly FacetStep[]): string {
  return path.map((step) => `${step.dim}=${step.value}`).join(';')
}

/** 筛选路径 → 树节点 tag（与 facet-tree buildTree 的拼装规则一致；
 *  选中指示用） */
export function pathTag(category: NavTarget, path: readonly FacetStep[]): string {
  const [first, ...rest] = path
  let tag = `${category}/${first.dim}/${escapeFacetValue(first.value)}`
  for (const step of rest) {
    tag += `/${step.dim}/${escapeFacetValue(step.value)}`
  }
  return tag
}

/** URL ?facet= 参数 → 筛选步骤（顺序保留）。按 ; 拆步逐步校验维度
 *  白名单——非法步起整体截断（手改 URL 容错保留合法前缀）；同维度
 *  重复只留首见。侧边栏树与列表页共享的解析入口，后端仍兜底校验 */
export function parseFacetPath(
  category: MarksCategory,
  raw: string | null,
): FacetStep[] {
  if (!raw) return []
  const seen = new Set<string>()
  const steps: FacetStep[] = []
  for (const part of raw.split(';')) {
    const eq = part.indexOf('=')
    if (eq <= 0) break
    const dim = part.slice(0, eq)
    const value = part.slice(eq + 1)
    if (!value) break
    if (!DIMENSIONS[category].some((d) => d.key === dim)) break
    if (seen.has(dim)) continue
    seen.add(dim)
    steps.push({ dim, value })
  }
  return steps
}

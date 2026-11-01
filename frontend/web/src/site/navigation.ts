/** 站点书影音类别键（与路由段一致） */
export type MarksCategory = 'movie' | 'book' | 'music'

/** 「全部标记」伪类别：路由段/筛选树顶导航节点的 tag，书影音三类别
 *  合并视图；非真实豆瓣类别，不入 MARKS_CATEGORIES——筛选树类别根与
 *  统计区仍按三类 */
export const ALL_MARKS_TAG = 'all'

/** 树/路由可导航目标：真实类别或「全部标记」 */
export type NavTarget = MarksCategory | typeof ALL_MARKS_TAG

/** 站点只展示「看过」（collect）的标记 */
export const MARKS_STATUS = 'collect'

export const DEFAULT_MARKS_PATH = '/movie'

/** 侧边栏条目顺序 */
export const MARKS_CATEGORIES: readonly MarksCategory[] = [
  'movie',
  'book',
  'music',
]

/** 类别中文名；movie 显示「影视」——豆瓣 movie 类目本身混合电影与
 *  电视剧（rexxar is_tv 区分），全链路（树/面包屑/标题/统计）统一
 *  聚合名不细分形态；亦作 isMarksCategory 的键 */
export const CATEGORY_NOUNS: Record<MarksCategory, string> = {
  book: '图书',
  movie: '影视',
  music: '音乐',
}

/** 页面标题动词：看过的影视 / 读过的图书 / 听过的音乐 */
const CATEGORY_VERBS: Record<MarksCategory, string> = {
  book: '读',
  movie: '看',
  music: '听',
}

/** 列表 meta 行完成态动词：芝麻 已看过 183 部影视 */
export const CATEGORY_PAST_VERBS: Record<MarksCategory, string> = {
  book: '已读过',
  movie: '已看过',
  music: '已听过',
}

/** 列表 meta 行量词名词，与 CATEGORY_PAST_VERBS 配对拼「已读过 55
 *  本书」 */
export const CATEGORY_UNIT_NOUNS: Record<MarksCategory, string> = {
  book: '本书',
  movie: '部影视',
  music: '张专辑',
}

export function isMarksCategory(value: string): value is MarksCategory {
  return value in CATEGORY_NOUNS
}

// --------------------------------------------------------------------------- //
// 我的评分筛选（「全部标记」视图专用，URL ?rating=）
// --------------------------------------------------------------------------- //

/** 星级 → 中文名词（文案兜底用）；键即后端 my_rating 取值 1-5 */
export const RATING_NOUNS: Record<number, string> = {
  1: '一星',
  2: '二星',
  3: '三星',
  4: '四星',
  5: '五星',
}

/** 评分筛选键："N"（1-5 精确星级）或 "lteN"（1-5，N 星及以下——未评
 *  NULL 不入任何档）；与后端 marks 端点 rating 参数同构 */
export type RatingKey = string

/** 「全部标记」导航子节点：我的评分快捷筛选入口（后端 rating 参数仅
 *  all 合并视图支持——单类别筛选走前端 dims ids 模式，评分不在其中，
 *  故入口只挂 all 下）；1-5 精确档全量铺开（'lteN' 区间键已不入导航，
 *  仅留 URL 兜底解析） */
export const NAV_RATING_STEPS: readonly { key: RatingKey; label: string }[] = [
  { key: '5', label: '五星力荐' },
  { key: '4', label: '四星推荐' },
  { key: '3', label: '三星中评' },
  { key: '2', label: '两星负评' },
  { key: '1', label: '一星差评' },
]

/** 导航树评分子节点 tag（「全部标记」树内层级路径，拼装风格与 facet
 *  树的 类别/维度/值 一致） */
export function ratingNavTag(key: RatingKey): string {
  return `${ALL_MARKS_TAG}/rating/${key}`
}

/** URL ?rating= 参数 → 合法筛选键（缺省/非法 undefined 按无筛选处理，
 *  后端仍会兜底校验） */
export function parseRatingParam(raw: string | null): RatingKey | undefined {
  if (!raw) return undefined
  return /^(?:[1-5]|lte[1-5])$/.test(raw) ? raw : undefined
}

/** 单条 my_rating 是否命中评分筛选键（未评不命中任何档——侧边栏
 *  子节点计数与后端 <= 比较口径一致） */
export function ratingKeyMatches(
  key: RatingKey,
  myRating: number | null | undefined,
): boolean {
  if (myRating == null) return false
  return key.startsWith('lte')
    ? myRating <= Number(key.slice(3))
    : myRating === Number(key)
}

/** 评分筛选键 → meta 行后缀文案；导航入口外的键（手改 URL 的 'lteN'
 *  区间键等——1-5 精确档已全量在导航）按名词表兜底拼 */
export function ratingFilterLabel(key: RatingKey): string {
  const hit = NAV_RATING_STEPS.find((step) => step.key === key)
  if (hit) return hit.label
  if (key.startsWith('lte')) {
    const noun = RATING_NOUNS[Number(key.slice(3))]
    return noun ? `${noun}及以下标记` : key
  }
  const noun = RATING_NOUNS[Number(key)]
  return noun ? `我的${noun}标记` : key
}

export type MarksView = {
  activeItemId: string
  category: NavTarget
  pageTitle: string
}

/** 校验路由参数并派生导航高亮项与页面标题；非法值返回 null。
 *  all 为三类别合并视图（全部标记），无分面维度 */
export function resolveMarksView(
  category: string | undefined,
): MarksView | null {
  if (category === ALL_MARKS_TAG) {
    return {
      activeItemId: ALL_MARKS_TAG,
      category: ALL_MARKS_TAG,
      pageTitle: '全部标记',
    }
  }
  if (!category || !isMarksCategory(category)) return null

  return {
    activeItemId: category,
    category,
    // 「动词过的类别名」：看过的影视 / 读过的图书 / 听过的音乐
    pageTitle: `${CATEGORY_VERBS[category]}过的${CATEGORY_NOUNS[category]}`,
  }
}

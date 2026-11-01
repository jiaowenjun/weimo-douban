import { createContext } from 'react'

export type CacheVersions = {
  profile: string
  stats: string
  movie: string
  book: string
  music: string
  all: string
  /** Marks paging guards exclude subjects; absent on legacy servers. */
  movie_marks?: string
  book_marks?: string
  music_marks?: string
  all_marks?: string
  /** 富集待处理 job 数（全局）——同步终态后前端据此维持 3s 版本轮询
   *  （富集逐条落库持续可见），归零自动停；旧后端无此字段按 0 兜底 */
  subjects_pending?: number
}
export const CacheVersionContext = createContext<CacheVersions | null | undefined>(undefined)

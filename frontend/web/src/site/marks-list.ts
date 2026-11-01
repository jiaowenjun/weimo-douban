import { QueryObserver } from '@tanstack/react-query'
import type { QueryClient } from '@tanstack/react-query'
import type { DoubanMark, DoubanMarksPage, MarksResult } from './api'
import type { CacheVersions } from './cache-version'

export const MARKS_PAGE_SIZE = 20
const MAX_RECOVERIES = 2

export type MarksVersions = { records: string; content: string; paging?: string }

export function marksVersions(versions: CacheVersions | null | undefined, category: string): MarksVersions {
  const key = category as 'movie' | 'book' | 'music' | 'all'
  const content = versions === undefined ? 'pending' : versions?.[key] ?? 'unversioned'
  const value = versions?.[`${key}_marks`]
  const paging = typeof value === 'string' && value.length > 0 ? value : undefined
  return { records: paging ?? content, content, paging }
}

export type MarksListInput = MarksVersions & {
  // undefined = unfiltered; null = waiting for a current dims projection.
  ids?: readonly string[] | null
}
type ReadyInput = MarksVersions & { ids?: readonly string[] }
type Generation = { key: string; input: ReadyInput; count: number }
type PageData = { contentVersion: string; data: DoubanMarksPage }
type Failure = Extract<MarksResult, { ok: false }>
export type MarksPageRequest = ReadyInput & { page: number }
export type MarksListSnapshot = {
  phase: 'loading' | 'list' | 'not-cached' | 'error'
  items: DoubanMark[]
  total: number
  loadedPages: number
  cachedAt: string
  loadingMore: boolean
  refreshing: boolean
  error: string | null
  canLoadMore: boolean
}

function generationKey(input: ReadyInput, filterKey: string) {
  return JSON.stringify([filterKey, input.records, input.ids ?? null])
}

/** Page identity excludes content freshness. The format marker prevents old
 * persisted MarksResult payloads from being mistaken for stamped page data. */
export function marksPageKey(uid: string, category: string, rating: string | undefined, input: ReadyInput, page: number) {
  return ['douban', uid, 'marks', category, 'pages-v2', input.records, {
    page, pageSize: MARKS_PAGE_SIZE, rating: rating ?? null,
    ids: input.ids?.slice((page - 1) * MARKS_PAGE_SIZE, page * MARKS_PAGE_SIZE).join(',') ?? null,
  }] as const
}

/** One controller per navigation/filter scope. Payloads live only in QueryClient;
 * the display keeps a generation and page count, never a detached items copy.
 * A single worker prioritizes requested pages over detail refreshes. It finishes
 * an in-flight read and coalesces new content signals instead of cancelling it.
 * Filter replacements use a pinned ordered ID set and commit all visible pages
 * together; subsequent ID changes wait for that replacement to finish. */
export class MarksListController {
  private client: QueryClient
  private uid: string
  private category: string
  private rating: string | undefined
  private filterKey: string
  private fetchPage: (request: MarksPageRequest) => Promise<MarksResult>
  private fetchVersions: () => Promise<MarksVersions>
  private input: MarksListInput | null = null
  private display: Generation | null = null
  private target: Generation | null = null
  private desiredPages = 1
  private failure: Failure | null = null
  private recoveries = 0
  private nextRefreshPage = 1
  private running = false
  private wake = false
  private epoch = 0
  private listeners = new Set<() => void>()
  private stopCache: (() => void) | undefined
  private pinnedPages = new Map<string, () => void>()
  private snapshot: MarksListSnapshot = {
    phase: 'loading', items: [], total: 0, loadedPages: 0, cachedAt: '',
    loadingMore: false, refreshing: false, error: null, canLoadMore: false,
  }

  constructor(options: {
    client: QueryClient; uid: string; category: string; rating?: string; filterKey?: string
    fetchPage: (request: MarksPageRequest) => Promise<MarksResult>
    fetchVersions: () => Promise<MarksVersions>
  }) {
    this.client = options.client
    this.uid = options.uid
    this.category = options.category
    this.rating = options.rating
    this.filterKey = options.filterKey ?? ''
    this.fetchPage = options.fetchPage
    this.fetchVersions = options.fetchVersions
  }

  getSnapshot = () => this.snapshot

  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    if (!this.stopCache) {
      this.stopCache = this.client.getQueryCache().subscribe((event) => {
        const key = event.query.queryKey
        if (key[0] === 'douban' && key[1] === this.uid && key[2] === 'marks' &&
          key[3] === this.category && event.type === 'updated' && event.action.type === 'success') {
          this.publish()
        }
      })
      this.start()
    }
    return () => {
      this.listeners.delete(listener)
      if (!this.listeners.size) {
        this.stopCache?.()
        this.stopCache = undefined
        for (const stop of this.pinnedPages.values()) stop()
        this.pinnedPages.clear()
        this.epoch++
        this.running = false
      }
    }
  }

  update(input: MarksListInput) {
    const changedRecords = this.input?.records !== input.records
    this.input = input
    // New first-sync data recovers a previous 404/network failure. Repeated 409
    // recovery remains bounded across manifest changes until an explicit retry.
    if (changedRecords && this.recoveries < MAX_RECOVERIES) this.failure = null
    this.publish()
    this.start()
  }

  loadMore = () => {
    if (!this.snapshot.canLoadMore || this.snapshot.loadingMore) return
    this.desiredPages = Math.max(this.desiredPages, (this.display?.count ?? 0) + 1)
    this.failure = null
    this.recoveries = 0
    this.publish()
    this.start()
  }

  retry = () => {
    this.failure = null
    this.recoveries = 0
    this.publish()
    this.start()
  }

  private key(generation: Generation, page: number) {
    return marksPageKey(this.uid, this.category, this.rating, generation.input, page)
  }

  private page(generation: Generation, page: number) {
    return this.client.getQueryData<PageData>(this.key(generation, page))
  }

  private readyInput(): ReadyInput | null {
    if (!this.input || this.input.records === 'pending') return null
    if (this.input.ids === null) {
      // While dims refreshes, paging can continue against the displayed fixed
      // match set, provided its records guard is still current.
      return this.display?.input.records === this.input.records
        ? { ...this.input, ids: this.display.input.ids }
        : null
    }
    return this.input as ReadyInput
  }

  private total(generation: Generation) {
    return generation.input.ids?.length ?? this.page(generation, 1)?.data.total ?? 0
  }

  private publish() {
    this.pinVisiblePages()
    const display = this.display
    const pages = display ? Array.from({ length: display.count }, (_, i) => this.page(display, i + 1)?.data) : []
    const total = display ? this.total(display) : 0
    const loadingMore = this.running && !this.failure && this.desiredPages > (display?.count ?? 1)
    const input = this.readyInput()
    this.snapshot = {
      phase: display ? 'list' : this.failure?.kind === 'not-cached' ? 'not-cached' : this.failure ? 'error' : 'loading',
      items: pages.flatMap(page => page?.items ?? []), total,
      loadedPages: display?.count ?? 0, cachedAt: pages[0]?.cached_at ?? '',
      loadingMore,
      refreshing: !!display && (this.running || !input || generationKey(input, this.filterKey) !== display.key),
      error: this.failure?.message ?? null,
      canLoadMore: !!display && this.input?.records === display.input.records &&
        display.count * MARKS_PAGE_SIZE < total,
    }
    for (const listener of this.listeners) listener()
  }

  private pinVisiblePages() {
    if (!this.listeners.size) return
    const wanted = new Set<string>()
    for (const generation of [this.display, this.target]) {
      if (!generation || generation.input.ids?.length === 0) continue
      const count = generation === this.target ? this.desiredPages : generation.count
      for (let page = 1; page <= count; page++) {
        const queryKey = this.key(generation, page)
        const key = JSON.stringify(queryKey)
        wanted.add(key)
        if (!this.pinnedPages.has(key)) {
          // Observers retain displayed pages against GC. The worker owns fetch
          // scheduling so an automatic refetch cannot cancel a user's next page.
          const observer = new QueryObserver(this.client, { queryKey, enabled: false })
          this.pinnedPages.set(key, observer.subscribe(() => {}))
        }
      }
    }
    for (const [key, stop] of this.pinnedPages) {
      if (!wanted.has(key)) { stop(); this.pinnedPages.delete(key) }
    }
  }

  private start() {
    if (this.running) { this.wake = true; return }
    if (this.failure || !this.listeners.size || !this.input) return
    this.wake = false
    this.running = true
    const epoch = this.epoch
    this.publish()
    void this.work(epoch).finally(() => {
      if (epoch !== this.epoch) return
      this.running = false
      this.publish()
      if (this.wake) this.start()
    })
  }

  private async read(generation: Generation, page: number, refresh: boolean) {
    this.pinVisiblePages()
    // Stamp the content signal at request START. If it changes during the read,
    // the worker sees the mismatch and schedules a trailing refresh. This stamp
    // survives IndexedDB hydration, including a missed pending > 0 -> 0 edge.
    const versions = this.input?.records === generation.input.records ? this.input : generation.input
    const request: MarksPageRequest = { ...generation.input, ...versions, ids: generation.input.ids, page }
    return this.client.fetchQuery({
      queryKey: this.key(generation, page),
      queryFn: async (): Promise<PageData> => {
        const result = await this.fetchPage(request)
        if (!result.ok) throw result
        return { data: result.data, contentVersion: request.content }
      },
      staleTime: refresh ? 0 : Infinity,
      retry: false,
    })
  }

  private async recover(error: unknown, generation: Generation, epoch: number) {
    if (epoch !== this.epoch) return false
    const failure: Failure = error && typeof error === 'object' && 'kind' in error && 'message' in error
      ? error as Failure
      : { ok: false, kind: 'error', message: error instanceof Error ? error.message : '网络错误，请稍后重试' }
    const latest = this.readyInput()
    if (latest && generationKey(latest, this.filterKey) !== generation.key &&
      (failure.kind !== 'version-stale' || this.recoveries < MAX_RECOVERIES)) {
      if (failure.kind === 'version-stale') this.recoveries++
      this.target = null
      return true
    }
    if (failure.kind === 'version-stale' && this.recoveries < MAX_RECOVERIES) {
      this.recoveries++
      try {
        const versions = await this.fetchVersions()
        if (epoch !== this.epoch) return false
        const current = this.input
        // A new records version requires a matching dims projection. Never
        // carry filtered IDs from the previous records snapshot into it.
        this.input = {
          ...versions,
          ids: current?.ids === undefined ? undefined :
            current.records === versions.records ? current.ids : null,
        }
        this.target = null
        return true
      } catch {
        if (epoch !== this.epoch) return false
      }
    }
    this.failure = failure.kind === 'version-stale'
      ? { ...failure, message: '数据仍在更新，请稍后重试；已加载的内容已保留。' }
      : failure
    return false
  }

  private async work(epoch: number) {
    while (epoch === this.epoch && !this.failure) {
      const input = this.readyInput()
      if (!input) return
      if (this.target && this.target.input.records !== input.records) this.target = null
      if (!this.target && (!this.display || this.display.key !== generationKey(input, this.filterKey) || this.desiredPages > this.display.count)) {
        this.target = { key: generationKey(input, this.filterKey), input, count: 1 }
      }
      const generation = this.target ?? this.display!
      if (this.target) {
        const first = this.page(generation, 1)
        const empty = generation.input.ids?.length === 0
        const count = first || empty
          ? Math.min(this.desiredPages, Math.max(1, Math.ceil(this.total(generation) / MARKS_PAGE_SIZE)))
          : 1
        const missing = empty ? 0 : Array.from({ length: count }, (_, i) => i + 1)
          .find(page => !this.page(generation, page))
        if (!missing) {
          this.display = { ...generation, count }
          this.target = null
          this.desiredPages = count
          this.recoveries = 0
          this.publish()
          continue
        }
        try {
          await this.read(generation, missing, false)
        } catch (error) {
          if (!await this.recover(error, generation, epoch)) return
        }
        continue
      }
      // Same records and ordered IDs: update details in place, including every
      // appended page. Only one read is in flight, so loading more takes priority
      // at the next iteration even under continuous enrichment.
      const outdated = generation.input.ids?.length === 0 ? undefined :
        Array.from({ length: generation.count }, (_, i) => (this.nextRefreshPage - 1 + i) % generation.count + 1).find(page => {
          const key = this.key(generation, page)
          const state = this.client.getQueryState<PageData>(key)
          return !state?.data || state.data.contentVersion !== input.content || state.isInvalidated ||
            (input.content === 'unversioned' && Date.now() - state.dataUpdatedAt >= 60_000)
        })
      if (!outdated) return
      this.nextRefreshPage = outdated % generation.count + 1
      try {
        await this.read(generation, outdated, true)
      } catch (error) {
        if (!await this.recover(error, generation, epoch)) return
      }
    }
  }
}

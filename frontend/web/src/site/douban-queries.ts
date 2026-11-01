/** Long-lived successful queries are versioned and persisted. Failures throw so a
 * failed refresh cannot overwrite the last successful data. */
import { useContext } from 'react'
import { CacheVersionContext } from './cache-version'
import { useQuery } from '@tanstack/react-query'
import type { UseQueryResult } from '@tanstack/react-query'

import {
  DOUBAN_CACHE_GC_TIME_MS,
  DOUBAN_CACHE_STALE_TIME_MS,
  queryClient,
} from '../query-client'
import {
  DoubanDataQueryError,
  fetchCacheVersions,
  fetchDoubanProfile,
  fetchDoubanStats,
  fetchDoubanSubjectDims,
} from './api'
import type { MarksCategory } from './navigation'

/** Shared query scopes for profile, statistics and facet projections. */
export const doubanQueryKeys = {
  all: ['douban'] as const,
  uid: (uid: string) => ['douban', uid] as const,
  profile: (uid: string) => [...doubanQueryKeys.uid(uid), 'profile'] as const,
  stats: (uid: string) => [...doubanQueryKeys.uid(uid), 'stats'] as const,
  dims: (uid: string, category: string) =>
    [...doubanQueryKeys.uid(uid), 'dims', category] as const,
}

const cacheOptions = {
  staleTime: DOUBAN_CACHE_STALE_TIME_MS,
  gcTime: DOUBAN_CACHE_GC_TIME_MS,
} as const

export function useCacheVersionsQuery(uid: string | null) {
  return useQuery({
    queryKey: [...doubanQueryKeys.uid(uid ?? ''), 'versions'],
    queryFn: () => fetchCacheVersions(uid ?? ''),
    enabled: uid !== null,
    staleTime: 0,
    refetchInterval: 60_000,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  })
}

async function requireData<T>(promise: Promise<T | null>): Promise<T> {
  const data = await promise
  // null 只来自端点 404——该维度从未同步过
  if (data === null) throw new DoubanDataQueryError('not-cached', '数据还没有同步过')
  return data
}

function useVersion(category: string): string {
  const versions = useContext(CacheVersionContext)
  if (versions === undefined) return 'pending'
  // 版本串域（排除 subjects_pending 计数字段——它不是任何查询的版本键）
  return versions?.[category as 'profile' | 'stats' | 'movie' | 'book' | 'music' | 'all'] ??
    'unversioned'
}

/** Keep a previous successful revision visible when fetching a new one fails.
 * This is display-only: never write an old payload under a new version key. */
function withCachedFallback<T>(query: UseQueryResult<T, Error>, prefix: readonly unknown[]) {
  const previous = query.data === undefined
    ? queryClient.getQueryCache().findAll({ queryKey: prefix })
      .filter((candidate) => candidate.state.data !== undefined)
      .sort((a, b) => b.state.dataUpdatedAt - a.state.dataUpdatedAt)[0]?.state.data as T | undefined
    : undefined
  return {
    ...query,
    data: query.data ?? previous,
    isPlaceholderData: query.isPlaceholderData || previous !== undefined,
  }
}

/** 用户资料（头像/用户名）；失败保留旧数据——界面回退 uid 展示 */
export function useDoubanProfileQuery(uid: string | null, version = 'unversioned') {
  const query = useQuery({
    queryKey: [...doubanQueryKeys.profile(uid ?? ''), version],
    queryFn: () => requireData(fetchDoubanProfile(uid ?? '', version)),
    placeholderData: (previous, query) => query?.queryKey[1] === uid ? previous : undefined,
    ...cacheOptions,
    staleTime: version === 'unversioned' ? 60_000 : DOUBAN_CACHE_STALE_TIME_MS,
    enabled: uid !== null && version !== 'pending',
  })
  return withCachedFallback(query, doubanQueryKeys.profile(uid ?? ''))
}

/** 标记统计（按类别计数 + 按日计数）；失败保留旧数据——侧边栏统计
 *  区静默隐藏 */
export function useDoubanStatsQuery(uid: string | null) {
  const version = useVersion('stats')
  const query = useQuery({
    queryKey: [...doubanQueryKeys.stats(uid ?? ''), version],
    queryFn: () => requireData(fetchDoubanStats(uid ?? '', version)),
    placeholderData: (previous, query) => query?.queryKey[1] === uid ? previous : undefined,
    ...cacheOptions,
    staleTime: version === 'unversioned' ? 60_000 : DOUBAN_CACHE_STALE_TIME_MS,
    enabled: uid !== null && version !== 'pending',
  })
  return withCachedFallback(query, doubanQueryKeys.stats(uid ?? ''))
}

/** 条目维度投影（筛选树数据源）；失败/未同步不持久化——树该类别
 *  分支隐藏。侧边栏（三类别并行）与列表页（当前类别）共用同一 key，
 *  是消除重复请求的核心 */
export function useDoubanDimsQuery(
  uid: string | null,
  category: MarksCategory | null,
) {
  const version = useVersion(category ?? '')
  const query = useQuery({
    queryKey: [...doubanQueryKeys.dims(uid ?? '', category ?? ''), version],
    queryFn: () => requireData(fetchDoubanSubjectDims(uid ?? '', category ?? '', version)),
    placeholderData: (previous, query) => query?.queryKey[1] === uid && query.queryKey[3] === category ? previous : undefined,
    ...cacheOptions,
    staleTime: version === 'unversioned' ? 60_000 : DOUBAN_CACHE_STALE_TIME_MS,
    enabled: uid !== null && category !== null && version !== 'pending',
  })
  return withCachedFallback(query, doubanQueryKeys.dims(uid ?? '', category ?? ''))
}

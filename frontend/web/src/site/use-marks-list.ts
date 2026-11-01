import { useContext, useEffect, useMemo, useSyncExternalStore } from 'react'
import { queryClient } from '../query-client'
import { fetchCacheVersions, fetchDoubanMarks } from './api'
import { CacheVersionContext } from './cache-version'
import { MarksListController, marksVersions, MARKS_PAGE_SIZE } from './marks-list'

export function useMarksList(args: {
  uid: string | null
  category: string | null
  facetKey: string
  rating?: string
  ids?: readonly string[] | null
}) {
  const { uid, category, rating, facetKey } = args
  const versions = useContext(CacheVersionContext)
  const { records, content, paging } = marksVersions(versions, category ?? '')
  // A navigation change starts a new visible range even if two facets happen
  // to match the same IDs; cached page data can still be reused.
  const controller = useMemo(() => new MarksListController({
    client: queryClient, uid: uid ?? '', category: category ?? '', rating, filterKey: facetKey,
    fetchPage: (request) => fetchDoubanMarks(
      uid ?? '', category ?? '', request.page, MARKS_PAGE_SIZE,
      request.ids?.slice((request.page - 1) * MARKS_PAGE_SIZE, request.page * MARKS_PAGE_SIZE),
      rating, request.content, request.paging,
    ),
    fetchVersions: async () => marksVersions(await queryClient.fetchQuery({
      queryKey: ['douban', uid ?? '', 'versions'],
      queryFn: () => fetchCacheVersions(uid ?? ''), staleTime: 0,
    }), category ?? ''),
  }), [uid, category, rating, facetKey])
  // Compare the complete ordered match set. A new dims object with the same
  // IDs only refreshes details; it does not create a different list generation.
  const idsKey = args.ids === null ? null : args.ids?.join(',')
  useEffect(() => {
    controller.update({
      records, content, paging,
      ids: !uid || !category || idsKey === null ? null :
        idsKey === undefined ? undefined : idsKey ? idsKey.split(',') : [],
    })
  }, [controller, uid, category, records, content, paging, idsKey])
  const state = useSyncExternalStore(controller.subscribe, controller.getSnapshot)
  return { ...state, loadMore: controller.loadMore, retry: controller.retry }
}

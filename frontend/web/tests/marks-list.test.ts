import assert from 'node:assert/strict'
import { test } from 'node:test'
import { setTimeout as delay } from 'node:timers/promises'
import { QueryClient, dehydrate, hydrate } from '@tanstack/react-query'
import { MarksListController, marksVersions, marksPageKey } from '../src/site/marks-list.ts'
import type { MarksListInput, MarksPageRequest, MarksVersions } from '../src/site/marks-list.ts'
import type { DoubanMark, MarksResult } from '../src/site/api.ts'

const initial: MarksListInput = { records: 'r1', content: 'c1', paging: 'r1' }
const ids = (count: number, start = 1) => Array.from({ length: count }, (_, i) => String(start + i))
const itemIds = (controller: MarksListController) => controller.getSnapshot().items.map(item => item.subject_id)

function page(request: MarksPageRequest, all = ids(65)): MarksResult {
  const selected = request.ids ?? all
  return { ok: true, data: {
    user: 'alice', category: 'music', status: 'collect', cached_at: '', fresh: true,
    page: request.page, page_size: 20, total: selected.length,
    items: selected.slice((request.page - 1) * 20, request.page * 20).map(id => ({
      subject_id: id, title: request.records, intro: request.content,
    }) as DoubanMark),
  } }
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(r => { resolve = r })
  return { promise, resolve }
}

async function until(predicate: () => boolean) {
  for (let i = 0; i < 1000; i++) {
    if (predicate()) return
    await delay(1)
  }
  assert.fail('list did not reach the expected state')
}

function setup(t: { after: (fn: () => void) => void }, options: {
  client?: QueryClient
  fetchPage?: (request: MarksPageRequest) => Promise<MarksResult>
  fetchVersions?: () => Promise<MarksVersions>
} = {}) {
  const client = options.client ?? new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } })
  const controller = new MarksListController({
    client, uid: 'alice', category: 'music',
    fetchPage: options.fetchPage ?? (async request => page(request)),
    fetchVersions: options.fetchVersions ?? (async () => initial),
  })
  const stop = controller.subscribe(() => {})
  t.after(() => { stop(); client.clear() })
  return { client, controller }
}

test('enrichment during load-more preserves the click and refreshes every loaded page', async t => {
  const pending = deferred()
  let secondStarted = false
  const { controller } = setup(t, { fetchPage: async request => {
    if (request.page === 2 && request.content === 'c1') {
      secondStarted = true
      await pending.promise
    }
    return page(request)
  } })
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => secondStarted)
  controller.update({ ...initial, content: 'c2' })
  controller.update({ ...initial, content: 'c3' })
  pending.resolve()
  await until(() => controller.getSnapshot().items.length === 40 &&
    controller.getSnapshot().items.every(item => item.intro === 'c3'))
  assert.deepEqual(itemIds(controller), ids(40))
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 60)
  controller.update({ ...initial, content: 'c4' })
  await until(() => controller.getSnapshot().items.every(item => item.intro === 'c4'))
  assert.deepEqual(itemIds(controller), ids(60))
})

test('filtered ID changes commit a complete replacement, including a click during rebuilding', async t => {
  const pending = deferred()
  let rebuilding = false
  const { controller } = setup(t, { fetchPage: async request => {
    if (request.ids?.[0] === '2' && request.page === 2) {
      rebuilding = true
      await pending.promise
    }
    return page(request)
  } })
  controller.update({ ...initial, ids: ids(65) })
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 40)
  controller.update({ ...initial, content: 'c2', ids: ids(64, 2) })
  await until(() => rebuilding)
  assert.deepEqual(itemIds(controller), ids(40), 'new first page must not mix with old second page')
  assert.equal(controller.getSnapshot().total, 65, 'old result retains its own count')
  controller.loadMore()
  pending.resolve()
  await until(() => controller.getSnapshot().items.length === 60 && itemIds(controller)[0] === '2')
  assert.deepEqual(itemIds(controller), ids(60, 2))
  assert.equal(controller.getSnapshot().total, 64)
  controller.update({ ...initial, content: 'c3', ids: [] })
  await until(() => controller.getSnapshot().total === 0)
  assert.deepEqual(itemIds(controller), [])
})

test('records 409 rebuilds the entire requested range without mixing generations', async t => {
  const pending = deferred()
  let conflict = false
  let replacementStarted = false
  let recoveries = 0
  const { controller } = setup(t, {
    fetchVersions: async () => { recoveries++; return { records: 'r2', content: 'c2', paging: 'r2' } },
    fetchPage: async request => {
      if (request.records === 'r1' && request.page === 3) {
        conflict = true
        return { ok: false, kind: 'version-stale', message: 'changed' }
      }
      if (request.records === 'r2' && request.page === 2) {
        replacementStarted = true
        await pending.promise
      }
      return page(request, ids(65, request.records === 'r2' ? 2 : 1))
    },
  })
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 40)
  controller.loadMore()
  await until(() => replacementStarted)
  assert.equal(conflict, true)
  assert.deepEqual(itemIds(controller), ids(40))
  pending.resolve()
  await until(() => controller.getSnapshot().items.length === 60)
  assert.deepEqual(itemIds(controller), ids(60, 2))
  assert.ok(controller.getSnapshot().items.every(item => item.title === 'r2'))
  assert.equal(recoveries, 1)
})

test('repeated 409s stop automatically and expose an explicit retry', async t => {
  let revisions = 1
  let calls = 0
  let fail = true
  const { controller } = setup(t, {
    fetchVersions: async () => ({ records: `r${++revisions}`, content: `c${revisions}`, paging: `r${revisions}` }),
    fetchPage: async request => {
      calls++
      return fail ? { ok: false, kind: 'version-stale', message: 'changed' } : page(request)
    },
  })
  controller.update(initial)
  await until(() => controller.getSnapshot().phase === 'error')
  assert.equal(calls, 3)
  controller.update({ records: 'r4', content: 'c4', paging: 'r4' })
  await delay(10)
  assert.equal(calls, 3, 'polling must not reset the recovery limit')
  fail = false
  controller.retry()
  await until(() => controller.getSnapshot().items.length === 20)
  assert.equal(controller.getSnapshot().error, null)
})

test('hydrated page stamps refresh missed enrichment, while unchanged pages stay cached', async t => {
  const first = setup(t)
  first.controller.update(initial)
  await until(() => first.controller.getSnapshot().items.length === 20)
  first.controller.loadMore()
  await until(() => first.controller.getSnapshot().items.length === 40)
  const saved = dehydrate(first.client)
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } })
  hydrate(client, saved)
  let calls = 0
  const { controller } = setup(t, { client, fetchPage: async request => { calls++; return page(request) } })
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 40)
  assert.equal(calls, 0)
  controller.update({ ...initial, content: 'c2' })
  await until(() => controller.getSnapshot().items.every(item => item.intro === 'c2'))
  assert.equal(calls, 2)
  const key = marksPageKey('alice', 'music', undefined, initial, 2)
  const cached = client.getQueryData<{ data: { items: DoubanMark[] } }>(key)!
  client.setQueryData(key, { ...cached, data: { ...cached.data, items: cached.data.items.map(item => ({ ...item, intro: 'external' })) } })
  assert.equal(controller.getSnapshot().items[20].intro, 'external', 'appended pages subscribe to cache updates')
})

test('a failed detail refresh preserves all visible pages and retry repairs them', async t => {
  let fail = false
  const { controller } = setup(t, { fetchPage: async request =>
    fail ? { ok: false, kind: 'error', message: 'offline' } : page(request) })
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 40)
  fail = true
  controller.update({ ...initial, content: 'c2' })
  await until(() => controller.getSnapshot().error === 'offline')
  assert.deepEqual(itemIds(controller), ids(40))
  fail = false
  controller.retry()
  await until(() => controller.getSnapshot().items.every(item => item.intro === 'c2'))
})

test('new records invalidate filtered IDs until a matching projection arrives', async t => {
  const requests: MarksPageRequest[] = []
  const { controller } = setup(t, {
    fetchVersions: async () => ({ records: 'r2', content: 'c2', paging: 'r2' }),
    fetchPage: async request => {
      requests.push(request)
      if (request.records === 'r1' && request.page === 2) return { ok: false, kind: 'version-stale', message: 'changed' }
      return page(request)
    },
  })
  controller.update({ ...initial, ids: ids(65) })
  await until(() => controller.getSnapshot().items.length === 20)
  controller.loadMore()
  await until(() => requests.length === 2 && !controller.getSnapshot().loadingMore)
  assert.ok(requests.every(request => request.records === 'r1'))
  controller.update({ records: 'r2', content: 'c2', paging: 'r2', ids: ids(64, 2) })
  await until(() => controller.getSnapshot().items.length === 40)
  assert.deepEqual(itemIds(controller), ids(40, 2))
})

test('version negotiation falls back to the full legacy guard', () => {
  const legacy = { profile: 'p1', stats: 's1', movie: 'm1', book: 'b1', music: 'c1', all: 'a1' }
  assert.deepEqual(marksVersions(legacy, 'music'), { records: 'c1', content: 'c1', paging: undefined })
  assert.deepEqual(marksVersions({ ...legacy, music_marks: 'r1' }, 'music'), initial)
})

test('a click during a slow detail refresh immediately shows loading and queues only one page', async t => {
  const pending = deferred()
  let refreshing = false
  const requests: number[] = []
  const { controller } = setup(t, { fetchPage: async request => {
    requests.push(request.page)
    if (request.page === 1 && request.content === 'c2') {
      refreshing = true
      await pending.promise
    }
    return page(request)
  } })
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
  controller.update({ ...initial, content: 'c2' })
  await until(() => refreshing)
  controller.loadMore()
  assert.equal(controller.getSnapshot().loadingMore, true)
  controller.loadMore()
  pending.resolve()
  await until(() => controller.getSnapshot().items.length === 40)
  assert.deepEqual(requests, [1, 1, 2])
})

test('dims refresh keeps the current fixed filter pageable and mounted pages resist GC', async t => {
  const { client, controller } = setup(t)
  controller.update({ ...initial, ids: ids(65) })
  await until(() => controller.getSnapshot().items.length === 20)
  controller.update({ ...initial, content: 'c2', ids: null })
  controller.loadMore()
  await until(() => controller.getSnapshot().items.length === 40)
  assert.deepEqual(itemIds(controller), ids(40))
  const key = marksPageKey('alice', 'music', undefined, { ...initial, ids: ids(65) }, 2)
  assert.equal(client.getQueryCache().find({ queryKey: key, exact: true })?.getObserversCount(), 1)
})

test('unmount during a read cannot commit or notify the old navigation scope', async t => {
  const pending = deferred()
  const client = new QueryClient({ defaultOptions: { queries: { gcTime: Infinity } } })
  t.after(() => client.clear())
  let notifications = 0
  const controller = new MarksListController({
    client, uid: 'alice', category: 'music',
    fetchVersions: async () => initial,
    fetchPage: async request => { await pending.promise; return page(request) },
  })
  const stop = controller.subscribe(() => { notifications++ })
  controller.update(initial)
  stop()
  const before = notifications
  pending.resolve()
  await delay(10)
  assert.equal(notifications, before)
  assert.equal(controller.getSnapshot().items.length, 0)
  // React StrictMode can reconnect the same controller. A completed cache read
  // is reused, while its abandoned worker must not commit into the new lifetime.
  const reconnect = controller.subscribe(() => {})
  t.after(reconnect)
  controller.update(initial)
  await until(() => controller.getSnapshot().items.length === 20)
})

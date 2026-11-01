import assert from 'node:assert/strict'
import { test } from 'node:test'
import { aggregateDims, filterIds, itemMatches } from '../src/site/facet-engine.ts'
import { facetValueLabel } from '../src/site/facet-model.ts'
import type { DoubanDimsItem } from '../src/site/api.ts'
import type { FacetStep } from '../src/site/facet-model.ts'

test('batched facet filtering preserves prefix/other semantics on overlapping values', () => {
  const items: DoubanDimsItem[] = Array.from({ length: 80 }, (_, i) => ({
    id: String(i), date: '2026-10-01', dims: {
      countries: [`country${i % 9}`, `country${i % 4}`],
      genres: [`genre${i % 7}`, i % 3 ? '剧情' : '喜剧'],
      is_tv: i % 5 === 0, year: 1980 + i % 40,
    },
  }))
  const paths: FacetStep[][] = [[], [{dim:'decade',value:'1980s'}], [{dim:'country',value:'__other__'}],
    [{dim:'country',value:'__other__'}, {dim:'genre',value:'剧情'}],
    [{dim:'country',value:'__other__'}, {dim:'genre',value:'__other__'}, {dim:'decade',value:'1980s'}]]
  for (const path of paths) {
    const expected = items.filter(item => itemMatches('movie', item, path, items)).map(item => item.id)
    assert.deepEqual(filterIds('movie', items, path), expected)
    const filtered = items.filter(item => expected.includes(item.id))
    // Non-topK dimension remains equal to aggregating the already filtered set.
    assert.deepEqual(aggregateDims('movie', items, path).decade, aggregateDims('movie', filtered).decade)
  }
})

test('music genre leaves cap at top five with __other__ bucket', () => {
  // 7 个流派计数互异（10/6/5/4/3/2/1）：前五入树，末两位（3 条）进其他桶
  const plan: [string, number][] = [
    ['流行', 10], ['民谣', 6], ['摇滚', 5], ['古典', 4], ['爵士', 3], ['原声', 2], ['轻音乐', 1],
  ]
  const items: DoubanDimsItem[] = plan.flatMap(([genre, count], g) =>
    Array.from({ length: count }, (_, i) => ({
      id: `${g}-${i}`, date: '2026-10-01', dims: {
        genres: [genre], singers: [`歌手${g}`], year: 1980 + g,
      },
    })),
  )
  const agg = aggregateDims('music', items)
  assert.deepEqual(agg.genre, [
    { value: '流行', count: 10 }, { value: '民谣', count: 6 }, { value: '摇滚', count: 5 },
    { value: '古典', count: 4 }, { value: '爵士', count: 3 }, { value: '__other__', count: 3 },
  ])
  // 树显示多少筛选就命中多少：其他流派步 = 含展示集外值的条目（含再钻年代的复合路径）
  const paths: FacetStep[][] = [[], [{ dim: 'genre', value: '__other__' }], [{ dim: 'genre', value: '民谣' }],
    [{ dim: 'genre', value: '__other__' }, { dim: 'decade', value: '1980s' }]]
  for (const path of paths) {
    const expected = items.filter(item => itemMatches('music', item, path, items)).map(item => item.id)
    assert.deepEqual(filterIds('music', items, path), expected)
  }
  // 未超上限原样全展：值数 ≤ topK 不出其他桶
  const few = items.filter(item => ['流行', '民谣', '摇滚'].includes(item.dims.genres![0]))
  assert.equal(aggregateDims('music', few).genre.some(x => x.value === '__other__'), false)
  // 其他桶展示名走默认拼法「其他流派」
  assert.equal(facetValueLabel('music', 'genre', '__other__'), '其他流派')
})

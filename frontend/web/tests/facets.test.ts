import assert from 'node:assert/strict'
import { test } from 'node:test'
import { aggregateDims, filterIds, itemMatches } from '../src/site/facet-engine.ts'
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

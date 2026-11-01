import { useState } from 'react'
import { Download } from 'lucide-react'

import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from 'weimo-ui-core/components/coss/dialog'
import { FrostedIconButton } from 'weimo-ui-core/components/frosted-icon-button'
import { TextButton } from 'weimo-ui-core/components/text-button'

import { fetchDoubanMarks } from './api'
import type { DoubanMark } from './api'
import { useDoubanUser } from './douban-user'
import { CATEGORY_NOUNS, isMarksCategory } from './navigation'

/** 导出翻页大小：后端 marks 端点 page_size 上限 200 */
const EXPORT_PAGE_SIZE = 200

/** 翻页防御上限（200 页 × 200 条 = 40,000 条）：后端异常返回时防死循环 */
const MAX_EXPORT_PAGES = 200

/** 同页重试上限：409 版本已变（富集写库持续触发）是瞬态，重拉同页即可 */
const MAX_PAGE_RETRIES = 3

/** CSV 表头（列序即导出列序） */
const CSV_HEADER = [
  '类别',
  '标题',
  '又名',
  '年份',
  '我的评分',
  '标记日期',
  '评语',
  /** 即 subject_url——豆瓣条目页直达链接（movie.douban.com/subject/…） */
  '豆瓣链接',
] as const

/** CSV 单元格转义：统一双引号包裹、内嵌引号翻倍——评语里的逗号/引号/
 *  换行均合法；null/undefined 输出空 */
function csvCell(value: string | number | null | undefined): string {
  const text = value == null ? '' : String(value)
  return `"${text.replaceAll('"', '""')}"`
}

/** 标记列表 → CSV 文本（不含 BOM，下载时由调用方前置）；行尾 \r\n
 *  对 Excel 直接打开更友好 */
function marksToCsv(marks: readonly DoubanMark[]): string {
  const lines = [CSV_HEADER.map(csvCell).join(',')]
  for (const mark of marks) {
    lines.push(
      [
        isMarksCategory(mark.category) ? CATEGORY_NOUNS[mark.category] : mark.category,
        mark.title,
        mark.alt_title,
        mark.subject?.year ?? '',
        mark.my_rating ?? '',
        mark.date,
        mark.comment,
        mark.subject_url,
      ]
        .map(csvCell)
        .join(','),
    )
  }
  return lines.join('\r\n')
}

/** 触发浏览器下载：Blob + 临时 <a> click */
function downloadTextFile(filename: string, content: string, mime: string) {
  const url = URL.createObjectURL(new Blob([content], { type: mime }))
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  anchor.click()
  // 立即 revoke 在 WebKit 系（Safari / macOS App 的 WKWebView）会让刚
  // 起步的下载按 URL 取不到 blob 内容而失败——click 只是异步发起，宿主
  // 稍后才回读；延迟释放对 Chrome 等无影响，百 KB 级 CSV 短暂驻留可忽略
  setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

/** 全量拉取登录用户的标记（all 合并视图，日期倒序）：循环翻页直到取满
 *  total；翻页间数据被重新发布（409 瞬态）重拉同页，按标记键去重兜底
 *  重复/移位，其余失败抛错（message 直达导出失败对话框） */
async function fetchAllMarks(uid: string): Promise<DoubanMark[]> {
  const items: DoubanMark[] = []
  const seen = new Set<string>()
  for (let page = 1; page <= MAX_EXPORT_PAGES; page++) {
    let result = await fetchDoubanMarks(uid, 'all', page, EXPORT_PAGE_SIZE)
    for (
      let retry = 0;
      !result.ok && result.kind === 'version-stale' && retry < MAX_PAGE_RETRIES;
      retry++
    ) {
      result = await fetchDoubanMarks(uid, 'all', page, EXPORT_PAGE_SIZE)
    }
    if (!result.ok) throw new Error(result.message)
    for (const mark of result.data.items) {
      const key = mark.comment_id || mark.subject_id
      if (seen.has(key)) continue
      seen.add(key)
      items.push(mark)
    }
    if (result.data.items.length === 0 || items.length >= result.data.total) break
  }
  return items
}

/** 顶栏导出入口：顶栏右侧的下载按钮，点击后全量拉取登录用户的标记
 *  （all 合并视图）生成 CSV 下载（UTF-8 BOM 前置，Excel 直接打开中文
 *  不乱码）；未登录点击转登录对话框，拉取失败以对话框提示 */
export function TopBarExport() {
  const { uid, openLogin } = useDoubanUser()
  const [exporting, setExporting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function handleExport() {
    if (!uid || exporting) return
    setExporting(true)
    try {
      const marks = await fetchAllMarks(uid)
      const date = new Date().toISOString().slice(0, 10)
      downloadTextFile(
        `douban-marks-${uid}-${date}.csv`,
        `\uFEFF${marksToCsv(marks)}`,
        'text/csv;charset=utf-8',
      )
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '导出失败，请稍后重试')
    } finally {
      setExporting(false)
    }
  }

  return (
    <>
      <FrostedIconButton
        aria-label={exporting ? '正在导出' : '导出标记 CSV'}
        disabled={exporting}
        onClick={() => (uid ? void handleExport() : openLogin())}
        tooltip={exporting ? '正在导出…' : '导出标记 CSV'}
      >
        <Download aria-hidden="true" />
      </FrostedIconButton>
      <Dialog
        onOpenChange={(open) => {
          if (!open) setError(null)
        }}
        open={error !== null}
      >
        <DialogPopup>
          <DialogHeader>
            <DialogTitle>导出失败</DialogTitle>
            <DialogDescription>{error}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <TextButton onClick={() => setError(null)} type="button">
              知道了
            </TextButton>
          </DialogFooter>
        </DialogPopup>
      </Dialog>
    </>
  )
}

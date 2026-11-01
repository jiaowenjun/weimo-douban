import { useEffect, useState } from 'react'
import { LabeledSwitch } from 'weimo-ui-core/components/labeled-switch'

type Preference = { available: boolean; enabled: boolean }
const API_BASE = import.meta.env.VITE_API_BASE_URL ?? '/api/v1'

/** Desktop-only preference, persisted by the local server across random ports. */
export function SubjectCacheSetting({ open }: { open: boolean }) {
  const [preference, setPreference] = useState<Preference | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    if (!open || !['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname)) return
    const controller = new AbortController()
    void fetch(`${API_BASE}/app/subject-cache`, { signal: controller.signal })
      .then(async (response) => {
        if (response.ok) setPreference(await response.json() as Preference)
      })
      .catch(() => { /* Older desktop versions do not expose this setting. */ })
    return () => controller.abort()
  }, [open])

  if (!preference) return null

  async function change(enabled: boolean) {
    setPending(true)
    setError('')
    try {
      const response = await fetch(`${API_BASE}/app/subject-cache`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled }),
      })
      if (!response.ok) throw new Error('保存失败，请重试')
      setPreference(await response.json() as Preference)
    } catch {
      setError('保存失败，请重试')
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="settings-dialog__group settings-dialog__cache">
      <div className="settings-dialog__group-title">同步加速</div>
      <fieldset className="settings-dialog__item settings-dialog__cache-control" disabled={pending || !preference.available}>
        <span className="settings-dialog__item-name">使用中心详情缓存</span>
        <LabeledSwitch
          ariaLabel="使用中心详情缓存"
          checked={preference.enabled}
          labelOff="关闭"
          labelOn="开启"
          onCheckedChange={(enabled) => { void change(enabled) }}
        />
      </fieldset>
      <p className="settings-dialog__description">
        开启后向微墨服务器查询条目编号，以加快详情获取。服务器会收到条目编号和请求 IP；你的评分和短评保留在本机。中心不可用时自动从豆瓣获取。
      </p>
      {!preference.available && <p className="settings-dialog__description">已通过启动配置关闭。</p>}
      {error && <p role="alert" className="settings-dialog__description">{error}</p>}
    </div>
  )
}

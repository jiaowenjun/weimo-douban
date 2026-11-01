import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'

import { queryClient } from '../query-client'
import { clearPersistedQueryCache } from '../query-persistence'
import { CacheVersionContext } from './cache-version'
import { FirstSyncPollingBridge } from './first-sync'
import {
  DOUBAN_UID_STORAGE_KEY,
  DoubanUserContext,
  readStoredDoubanUid,
} from './douban-user'
import { doubanQueryKeys, useCacheVersionsQuery, useDoubanProfileQuery } from './douban-queries'
import { LoginDialog } from './login-dialog'

function persistDoubanUid(uid: string | null) {
  try {
    if (uid) {
      localStorage.setItem(DOUBAN_UID_STORAGE_KEY, uid)
    } else {
      localStorage.removeItem(DOUBAN_UID_STORAGE_KEY)
    }
  } catch {
    // localStorage 不可用时仅保留内存态
  }
}

/** 站点身份：豆瓣用户 ID 存 localStorage，后续 /users/{uid}/marks 等请求都依赖它。
 *  登录对话框的开关也持在这里，侧边栏入口与内容页的登录引导共用一份。
 *  资料经 react-query 缓存（key 带 uid 作用域，切换 uid 即换缓存）；
 *  切号/登出时清空全部查询缓存（对齐 biji logout 清缓存惯例） */
export function DoubanUserProvider({ children }: { children: ReactNode }) {
  const [uid, setUid] = useState<string | null>(readStoredDoubanUid)
  const [loginOpen, setLoginOpen] = useState(false)

  // 后端未同步过该用户或不可达时保留已有资料，无资料则回退 uid 展示，
  // 不阻塞登录态
  const versionsQuery = useCacheVersionsQuery(uid)
  const versions = versionsQuery.data ?? (versionsQuery.isPending ? undefined : null)
  const profileQuery = useDoubanProfileQuery(uid, versions === undefined ? 'pending' : versions?.profile)
  const profile = profileQuery.data ?? null

  // A transient read error should recover even when content itself has not changed.
  useEffect(() => {
    if (!uid || !versionsQuery.dataUpdatedAt || versionsQuery.isError) return
    void queryClient.refetchQueries({
      queryKey: doubanQueryKeys.uid(uid),
      type: 'active',
      predicate: (query) => query.queryKey[2] !== 'versions' && query.state.status === 'error',
    })
  }, [uid, versionsQuery.dataUpdatedAt, versionsQuery.isError])

  const signIn = useCallback(
    (nextUid: string) => {
      const normalized = nextUid.trim()
      if (!normalized) return
      if (normalized !== uid) {
        void queryClient.cancelQueries({ queryKey: doubanQueryKeys.all })
        queryClient.removeQueries({ queryKey: doubanQueryKeys.all })
        clearPersistedQueryCache()
      }
      setUid(normalized)
      persistDoubanUid(normalized)
    },
    [uid],
  )

  const signOut = useCallback(() => {
    void queryClient.cancelQueries({ queryKey: doubanQueryKeys.all })
    queryClient.removeQueries({ queryKey: doubanQueryKeys.all })
    clearPersistedQueryCache()
    setUid(null)
    persistDoubanUid(null)
  }, [])

  const openLogin = useCallback(() => setLoginOpen(true), [])

  const value = useMemo(
    () => ({ uid, profile, signIn, signOut, openLogin }),
    [uid, profile, signIn, signOut, openLogin],
  )

  return (
    <DoubanUserContext.Provider value={value}>
      <CacheVersionContext.Provider value={versions}>
        {/* 首发同步生命周期轮询（全局唯一实例，需读 CacheVersionContext
            的 pending 信号，故挂 Provider 内侧） */}
        <FirstSyncPollingBridge uid={uid} />
        {children}
      </CacheVersionContext.Provider>
      <LoginDialog onOpenChange={setLoginOpen} open={loginOpen} />
    </DoubanUserContext.Provider>
  )
}

import { useRef, useState } from 'react'
import { useNavigate } from 'react-router'
import { ArrowLeft, ArrowRight, UserRound } from 'lucide-react'

import {
  Dialog,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from 'weimo-ui-core/components/coss/dialog'
import { TextButton } from 'weimo-ui-core/components/text-button'

import { fetchDoubanProfile, resolveApiUrl, verifyDoubanUser } from './api'
import type { DoubanProfile, DoubanVerifyErrorKind } from './api'
import { useDoubanUser } from './douban-user'
import './login-dialog.css'

type LoginDialogProps = {
  onOpenChange: (open: boolean) => void
  open: boolean
}

/** 验证失败的展示文案（后端 message 是通用措辞，这里按定稿口径覆盖） */
const VERIFY_ERROR_COPY: Record<DoubanVerifyErrorKind, string> = {
  'not-found': '未找到该用户，请检查 ID 是否正确',
  unavailable: '豆瓣暂时无法访问，请稍后再试',
  'rate-limited': '操作过于频繁，请稍后再试',
  error: '服务暂不可用，请稍后再试',
}

/** 验证状态机：unverified（按钮=验证 ID）→ testing（验证中…）→ verified（按钮=开始备份）
 *  / error（行内提示，按钮回验证 ID）。修改输入即过期退回 unverified，
 *  新值需重新验证（曾验证过的 ID 走后端本地捷径毫秒过） */
type VerifyState =
  | { phase: 'unverified' }
  | { phase: 'testing' }
  | { phase: 'verified'; profile: DoubanProfile }
  | { phase: 'error'; message: string }

/** 豆瓣 ID 对话框：输入豆瓣用户 ID 作为站点身份（全程免账号密码，与
 *  欢迎页「免登录」口径一致），验证通过展示头像/用户名预览、开始
 *  备份后侧边栏身份栏立即有资料；已填时可切换或清除；底部「返回主页」关框
 *  并退回欢迎页（未填误入内容页的退路） */
export function LoginDialog({ onOpenChange, open }: LoginDialogProps) {
  const { uid, profile, signIn, signOut } = useDoubanUser()
  const navigate = useNavigate()
  const [draft, setDraft] = useState('')
  const [verify, setVerify] = useState<VerifyState>({ phase: 'unverified' })
  // 预览收起动画的内容载体（用途详见下方 preview 计算处的注释）
  const [lastProfile, setLastProfile] = useState<DoubanProfile | null>(null)
  // 竞态守卫：每次发起验证自增；编辑输入同样自增作废在途响应，
  // 迟到的结果（旧 ID 的）不覆盖当前状态
  const testSeqRef = useRef(0)
  // 每次打开时重置草稿与验证态（render 期调整，避免 effect 级联渲染）
  const [prevOpen, setPrevOpen] = useState(open)
  if (open !== prevOpen) {
    setPrevOpen(open)
    if (open) {
      setDraft(uid ?? '')
      setVerify({ phase: 'unverified' })
      setLastProfile(null)
    }
  }

  const normalized = draft.trim()

  function handleDraftChange(value: string) {
    setDraft(value)
    // 修改即过期：任何非 unverified 态退回未验证态（作废在途响应）；
    // 即便改回原值也不再自动恢复——验证过的身份是快照，不是实时推断
    if (verify.phase === 'unverified') return
    testSeqRef.current += 1
    setVerify({ phase: 'unverified' })
  }

  async function runTest() {
    const target = draft.trim()
    if (!target) return
    const seq = ++testSeqRef.current
    setVerify({ phase: 'testing' })
    // 第一跳：本地资料读（曾同步/曾验证过即命中，零豆瓣请求、零冷却）
    try {
      const local = await fetchDoubanProfile(target)
      if (local) {
        if (seq === testSeqRef.current) {
          setVerify({ phase: 'verified', profile: local })
        }
        return
      }
    } catch {
      // 本地读失败（网络/服务端）不终止——第二跳会给出更准确的错误
    }
    // 第二跳：verify 端点（后端本地未命中必抓豆瓣一页，资料入库后返回）
    const result = await verifyDoubanUser(target)
    if (seq !== testSeqRef.current) return
    if (result.ok) {
      setVerify({ phase: 'verified', profile: result.data })
    } else {
      setVerify({ phase: 'error', message: VERIFY_ERROR_COPY[result.kind] })
    }
  }

  function confirmSignIn() {
    if (verify.phase !== 'verified') return
    signIn(verify.profile.uid)
    onOpenChange(false)
  }

  function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    confirmSignIn()
  }

  // input 的 Enter 不走隐式提交（部分 webview 不触发），显式处理；
  // 语义跟随主按钮：已验证=开始备份，否则=验证 ID；主按钮隐藏态
  // （已登录且未改动，见 footer）同样不响应
  function handleInputKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key !== 'Enter') return
    event.preventDefault()
    if (uid && normalized === uid) return
    if (verify.phase === 'verified') {
      confirmSignIn()
    } else if (verify.phase !== 'testing' && normalized) {
      void runTest()
    }
  }

  // 清除 ID 后对话框不退出：页面用户内容随 signOut 清空，对话框内
  // 清空输入与验证态、清除按钮因 uid 置空自然消失，恢复登录前初始
  // 状态（预览区随收起动画淡出，lastProfile 不重置以保留收起内容）
  function handleSignOut() {
    signOut()
    setDraft('')
    setVerify({ phase: 'unverified' })
  }

  function handleBack() {
    onOpenChange(false)
    navigate('/')
  }

  // 预览区：验证通过的身份优先（将要备份的是谁，行内右半）；未改动草稿且已登录时
  // 展示当前账号；改动了新 ID 尚未验证则空白（不给错误暗示）
  const preview: DoubanProfile | null =
    verify.phase === 'verified'
      ? verify.profile
      : uid && normalized === uid && profile
        ? profile
        : null
  // 收起动画的内容载体（render 期调整，同 prevOpen 模式）：preview 变
  // null 时若同步卸载内容，盒子会瞬间塌掉没有过渡——lastProfile 保留
  // 上一次的资料，让盒子带着内容一起收
  if (preview && preview !== lastProfile) {
    setLastProfile(preview)
  }
  const shownProfile = preview ?? lastProfile

  // 已登记态：已登录且输入未改动——标题换「已登记豆瓣 ID」、输入框
  // readOnly 只读展示（换号走 清除 ID→重新输入，不再编辑切换），主按钮
  // 同态隐藏（见 footer）；清除 ID 后 uid 置空自然回到输入态
  const manageMode = !!uid && normalized === uid

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogPopup className="login-dialog">
        <form className="login-dialog__form" onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>
              {manageMode ? '已登记豆瓣 ID' : '请输入你的豆瓣 ID'}
            </DialogTitle>
          </DialogHeader>
          <DialogPanel>
            <div
              className={
                preview
                  ? 'login-dialog__row'
                  : 'login-dialog__row login-dialog__row--solo'
              }
            >
              <label className="login-dialog__field">
                {/* macOS WebKit 默认开自动拼写订正/自动大写，豆瓣 ID 是纯
                    字母数字串，被订正成英文单词反而干扰输入，关掉三件套 */}
                <input
                  autoCapitalize="none"
                  autoCorrect="off"
                  className="login-dialog__input"
                  name="douban-uid"
                  onChange={(event) => handleDraftChange(event.target.value)}
                  onKeyDown={handleInputKeyDown}
                  placeholder="从「我的豆瓣」查找豆瓣 ID"
                  readOnly={manageMode}
                  spellCheck={false}
                  type="text"
                  value={draft}
                />
              </label>
              <div
                aria-hidden={!preview}
                className={
                  preview
                    ? 'login-dialog__preview'
                    : 'login-dialog__preview login-dialog__preview--hidden'
                }
              >
                <div className="login-dialog__account">
                  {shownProfile ? (
                    <>
                      {shownProfile.avatar ? (
                        <img
                          alt=""
                          aria-hidden="true"
                          className="login-dialog__account-avatar"
                          src={resolveApiUrl(shownProfile.avatar)}
                        />
                      ) : (
                        <UserRound
                          aria-hidden="true"
                          className="login-dialog__account-avatar"
                        />
                      )}
                      <span className="login-dialog__account-name">
                        {shownProfile.username || shownProfile.uid}
                      </span>
                    </>
                  ) : null}
                </div>
              </div>
            </div>
            {verify.phase === 'error' ? (
              <p className="login-dialog__error" role="alert">
                {verify.message}
              </p>
            ) : null}
          </DialogPanel>
          <DialogFooter>
            <TextButton
              className="login-dialog__back"
              onClick={handleBack}
              type="button"
              variant="ghost"
            >
              <ArrowLeft aria-hidden size={16} />
              返回主页
            </TextButton>
            {uid ? (
              <TextButton onClick={handleSignOut} type="button">
                清除 ID
              </TextButton>
            ) : null}
            {/* 已登录且输入未改动（=当前 uid）时无主按钮——身份已知、
                预览行就在上方，验证/备份都无事可做，只留清除；改成
                新 ID 即恢复验证→开始备份链（换号流程不断） */}
            {uid && normalized === uid ? null : verify.phase === 'verified' ? (
              <TextButton type="submit">
                开始备份
                <ArrowRight aria-hidden size={16} />
              </TextButton>
            ) : (
              <TextButton
                disabled={!normalized || verify.phase === 'testing'}
                onClick={() => void runTest()}
                type="button"
              >
                {verify.phase === 'testing' ? '验证中…' : '验证 ID'}
              </TextButton>
            )}
          </DialogFooter>
        </form>
      </DialogPopup>
    </Dialog>
  )
}

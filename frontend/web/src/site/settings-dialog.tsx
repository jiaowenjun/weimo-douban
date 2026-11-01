import {
  Dialog,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from 'weimo-ui-core/components/coss/dialog'
import { LabeledSwitch } from 'weimo-ui-core/components/labeled-switch'

import { useSiteSettings } from './site-settings'
import { SubjectCacheSetting } from './subject-cache-setting'
import './settings-dialog.css'

type SettingsDialogProps = {
  onOpenChange: (open: boolean) => void
  open: boolean
}

/** 设置对话框：站点本地偏好（localStorage 持久化，见 site-settings），
 *  由 SiteSettingsProvider 挂载、openSettings 打开（同 openLogin 的
 *  「对话框归 Provider」模式）；开关拨动即时生效并持久化，无确认按
 *  按钮——ESC/点遮罩即关。「卡片显示」组：条目信息初始展开态（未手
 *  动拨过开关的已挂载卡片随设置变化即时翻转，手动拨过的保留用户态）
 *  与折叠态标题行对齐（纯展示，已挂载卡片即时生效）。
 *  文案口径：名称是纯名词不含状态值（如「条目详细信息」而非「默认
 *  展开」），状态交给开关两侧状态词补全，任意档位拼读都通顺；不配
 *  说明文字，保持整站简洁克制。后续设置项在此按分组追加 */
export function SettingsDialog({ onOpenChange, open }: SettingsDialogProps) {
  const { settings, updateSettings } = useSiteSettings()

  return (
    <Dialog onOpenChange={onOpenChange} open={open}>
      <DialogPopup className="settings-dialog">
        <DialogHeader>
          <DialogTitle>设置</DialogTitle>
        </DialogHeader>
        <DialogPanel>
          <div className="settings-dialog__group">
            <div className="settings-dialog__group-title">卡片显示</div>
            <div className="settings-dialog__item">
              <span className="settings-dialog__item-name">条目详细信息</span>
              <LabeledSwitch
                ariaLabel="条目详细信息"
                checked={settings.defaultMetaExpanded}
                labelOff="默认折叠"
                labelOn="默认展开"
                onCheckedChange={(checked) =>
                  updateSettings({ defaultMetaExpanded: checked })
                }
              />
            </div>
            <div className="settings-dialog__item">
              <span className="settings-dialog__item-name">折叠时标题对齐</span>
              <LabeledSwitch
                ariaLabel="折叠时标题对齐"
                checked={settings.collapsedTitleAlign !== 'left'}
                labelOff="左对齐"
                labelOn="居中"
                onCheckedChange={(checked) =>
                  updateSettings({ collapsedTitleAlign: checked ? 'center' : 'left' })
                }
              />
            </div>
          </div>
          <SubjectCacheSetting open={open} />
        </DialogPanel>
      </DialogPopup>
    </Dialog>
  )
}

import AppKit
import Carbon.HIToolbox

/// 热键录制小面板：浮动置顶，捕获下一组「至少一个修饰键 + 普通键」的组合；
/// Esc 取消、仅按修饰键或功能键提示不收、下方按钮一键恢复默认 ⌘⇧M。
/// 程序化构建 UI（壳内无 XIB/storyboard），关闭经 windowWillClose 通知持有方释放。
final class HotKeyRecorder: NSObject, NSWindowDelegate {

    /// 录制完成（keyCode, 修饰位, 键名）——由持有方换绑并刷新菜单。
    /// 回调在主线程经 Task @MainActor 投递（monitor 在主线程触发）
    var onRecord: (@MainActor (UInt32, NSEvent.ModifierFlags, String) -> Void)?
    /// 取消/关闭（含恢复默认后的关闭）
    var onClose: (@MainActor () -> Void)?
    var onRestoreDefault: (@MainActor () -> Void)?

    private var panel: NSPanel!
    private var comboLabel: NSTextField!
    private var hintLabel: NSTextField!
    private var monitor: Any?

    init(currentCombo: String) {
        super.init()

        let contentRect = NSRect(x: 0, y: 0, width: 360, height: 168)
        panel = NSPanel(
            contentRect: contentRect,
            styleMask: [.titled, .closable],
            backing: .buffered, defer: false)
        panel.title = "录制全局快捷键"
        panel.level = .floating
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        panel.center()

        let instruction = NSTextField(labelWithString: "按下新的组合：至少一个修饰键 + 普通键")
        instruction.font = .systemFont(ofSize: 13, weight: .medium)
        instruction.alignment = .center

        comboLabel = NSTextField(labelWithString: currentCombo)
        comboLabel.font = .systemFont(ofSize: 34, weight: .semibold)
        comboLabel.alignment = .center

        hintLabel = NSTextField(labelWithString: "Esc 取消")
        hintLabel.font = .systemFont(ofSize: 11)
        hintLabel.textColor = .secondaryLabelColor
        hintLabel.alignment = .center

        let restore = NSButton(title: "恢复默认 ⌘⇧M", target: self, action: #selector(restoreTapped))
        restore.bezelStyle = .rounded

        let stack = NSStackView(views: [instruction, comboLabel, hintLabel, restore])
        stack.orientation = .vertical
        stack.spacing = 10
        stack.alignment = .centerX
        stack.frame = NSRect(x: 12, y: 12, width: contentRect.width - 24, height: contentRect.height - 24)
        stack.autoresizingMask = [.width, .height]
        panel.contentView?.addSubview(stack)

        panel.makeKeyAndOrderFront(nil)
        // accessory app 须主动激活才能抢到键盘焦点接收 keyDown
        NSApp.activate(ignoringOtherApps: true)
    }

    // MARK: 录制（面板成为 key 后装本地 monitor）

    func windowDidBecomeKey(_ notification: Notification) {
        installMonitor()
    }

    func windowDidResignKey(_ notification: Notification) {
        removeMonitor()
    }

    func windowWillClose(_ notification: Notification) {
        removeMonitor()
        let onClose = onClose
        Task { @MainActor in onClose?() }
    }

    private func installMonitor() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, event.window === self.panel else { return event }
            return self.consume(event)
        }
    }

    private func removeMonitor() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
        }
        monitor = nil
    }

    /// 吃掉面板内的 keyDown：Esc=取消；合法组合=回调节目并关闭；其余提示
    private func consume(_ event: NSEvent) -> NSEvent? {
        if event.keyCode == UInt16(kVK_Escape) {
            panel.close()
            return nil
        }
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let display = (event.charactersIgnoringModifiers ?? "").trimmingCharacters(in: .whitespaces).uppercased()
        let hasModifier = modifiers.contains(.command) || modifiers.contains(.option)
            || modifiers.contains(.control) || modifiers.contains(.shift)
        // 纯修饰键（含左右成对键与 fn）或取不到键名（部分功能键）不收
        if !hasModifier || display.isEmpty || isModifierKeyCode(event.keyCode) {
            flashHint("需含修饰键 + 普通键（Esc 取消）")
            return nil
        }
        let keyCode = UInt32(event.keyCode)
        let onRecord = onRecord
        Task { @MainActor in onRecord?(keyCode, modifiers, display) }
        panel.close()
        return nil
    }

    @objc private func restoreTapped() {
        let onRestoreDefault = onRestoreDefault
        Task { @MainActor in onRestoreDefault?() }
        comboLabel.stringValue = "⌘⇧M"
        flashHint("已恢复默认")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
            self?.panel.close()
        }
    }

    private func flashHint(_ text: String) {
        hintLabel.stringValue = text
        hintLabel.textColor = .systemRed
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak self] in
            guard let self, self.panel.isVisible else { return }
            self.hintLabel.stringValue = "Esc 取消"
            self.hintLabel.textColor = .secondaryLabelColor
        }
    }

    /// 左右 Shift/Command/Option/Control 与 fn 的键码（按下的是修饰键本身）
    private func isModifierKeyCode(_ keyCode: UInt16) -> Bool {
        switch keyCode {
        case UInt16(kVK_Shift), UInt16(kVK_RightShift),
             UInt16(kVK_Command), UInt16(kVK_RightCommand),
             UInt16(kVK_Option), UInt16(kVK_RightOption),
             UInt16(kVK_Control), UInt16(kVK_RightControl),
             UInt16(kVK_Function):
            return true
        default:
            return false
        }
    }
}

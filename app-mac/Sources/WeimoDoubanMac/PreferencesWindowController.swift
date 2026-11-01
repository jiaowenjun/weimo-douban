import AppKit
import ServiceManagement

/// 壳设置偏好窗（控件原生化第一件，路线图控件层的正确打开方式：把「壳
/// 职能」的界面原生出，内容偏好留在 web 弹层）。标准 macOS 偏好窗口：
/// ⌘,（应用菜单）/ Dock 菜单 / 托盘菜单三入口。行式布局三行——
/// 开机自启（NSSwitch）/ 全局唤起快捷键（复用 HotKeyRecorder 浮层录制）/
/// 数据文件夹入口。混合形态协同：窗开期间 .regular，关窗时若主窗口也
/// 不可见则降回 .accessory 纯托盘。
final class PreferencesWindowController: NSObject, NSWindowDelegate {

    private let hotKey: HotKeyCenter
    /// 关窗后是否可降回纯托盘（主窗口也不可见时才降）——AppDelegate 注入
    private let shouldDropToAccessory: @MainActor () -> Bool

    private var window: NSWindow?
    private var launchSwitch: NSSwitch?
    private var hotKeyLabel: NSTextField?
    private var recorder: HotKeyRecorder?

    init(hotKey: HotKeyCenter, shouldDropToAccessory: @escaping () -> Bool) {
        self.hotKey = hotKey
        self.shouldDropToAccessory = shouldDropToAccessory
        super.init()
    }

    func show() {
        if window == nil {
            buildWindow()
        }
        refresh()
        NSApp.setActivationPolicy(.regular)
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// 展示前对齐当前态（自启可能被系统设置改动、热键可能经录制更新）
    private func refresh() {
        launchSwitch?.state = ShellSupport.launchAtLoginEnabled ? .on : .off
        hotKeyLabel?.stringValue = hotKey.displayCombo
    }

    // MARK: - UI 构建（程序化，与壳内其余窗口同风格无 XIB）

    private func buildWindow() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 470, height: 212),
            styleMask: [.titled, .closable],
            backing: .buffered, defer: false)
        window.title = "偏好设置"
        window.isReleasedWhenClosed = false
        window.delegate = self
        if UserDefaults.standard.string(forKey: "NSWindow Frame PreferencesWindow") == nil {
            window.center()
        }
        _ = window.setFrameAutosaveName("PreferencesWindow")

        let launchLabel = rowLabel("开机自动启动")
        let launchSwitch = NSSwitch()
        // dev 直跑无 bundle，SMAppService 不可用——开关禁用而非点了没反应
        launchSwitch.isEnabled = ShellSupport.isBundled
        launchSwitch.target = self
        launchSwitch.action = #selector(launchSwitchToggled(_:))
        self.launchSwitch = launchSwitch

        let hotKeyTitle = rowLabel("全局唤起快捷键")
        let hotKeyLabel = NSTextField(labelWithString: hotKey.displayCombo)
        hotKeyLabel.font = .systemFont(ofSize: 13, weight: .medium)
        self.hotKeyLabel = hotKeyLabel
        let recordButton = roundedButton("录制…", action: #selector(recordTapped))

        let dataLabel = rowLabel("数据文件夹")
        let revealButton = roundedButton("在访达中显示…", action: #selector(revealTapped))

        let content = window.contentView!
        let rows = [
            row(views: [launchLabel, launchSwitch]),
            row(views: [hotKeyTitle, hotKeyLabel, recordButton]),
            row(views: [dataLabel, revealButton]),
        ]
        rows.forEach { stack in
            stack.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(stack)
        }
        var constraints: [NSLayoutConstraint] = []
        var topAnchor: NSLayoutYAxisAnchor = content.topAnchor
        for (index, stack) in rows.enumerated() {
            let inset: CGFloat = index == 0 ? 20 : 18
            constraints += [
                stack.topAnchor.constraint(equalTo: topAnchor, constant: inset),
                stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
                stack.trailingAnchor.constraint(lessThanOrEqualTo: content.trailingAnchor, constant: -20),
            ]
            topAnchor = stack.bottomAnchor
        }
        // 标签列定宽对齐（表单观感）
        [launchLabel, hotKeyTitle, dataLabel].forEach { label in
            label.widthAnchor.constraint(equalToConstant: 120).isActive = true
        }
        NSLayoutConstraint.activate(constraints)
        self.window = window
    }

    private func rowLabel(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: 13)
        return label
    }

    private func roundedButton(_ title: String, action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.bezelStyle = .rounded
        return button
    }

    /// 水平行：标签左、控件随其后（System Settings 式左对齐控件列）
    private func row(views: [NSView]) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.spacing = 10
        stack.alignment = .centerY
        return stack
    }

    // MARK: - 动作

    @objc private func launchSwitchToggled(_ sender: NSSwitch) {
        let target = sender.state == .on
        if !ShellSupport.setLaunchAtLogin(target) {
            sender.state = target ? .off : .on  // 失败回滚（系统设置外部改动等）
        }
    }

    @objc private func recordTapped() {
        let recorder = HotKeyRecorder(currentCombo: hotKey.displayCombo)
        recorder.onRecord = { [weak self] keyCode, modifiers, display in
            guard let self else { return }
            if self.hotKey.update(keyCode: keyCode, modifiers: modifiers, display: display) {
                self.refresh()
            } else {
                self.alert("该组合注册失败（可能被其他 App 占用），请换一个")
            }
        }
        recorder.onRestoreDefault = { [weak self] in
            self?.hotKey.restoreDefault()
            self?.refresh()
        }
        recorder.onClose = { [weak self] in self?.recorder = nil }
        self.recorder = recorder
    }

    @objc private func revealTapped() {
        ShellSupport.revealDataDir()
    }

    private func alert(_ message: String) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "好")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    // MARK: NSWindowDelegate —— 关窗若主窗口也不可见则降回纯托盘

    func windowWillClose(_ notification: Notification) {
        // delegate 回调运行于主线程（非隔离上下文），assumeIsolated 进闭包
        let dropToAccessory = MainActor.assumeIsolated { self.shouldDropToAccessory() }
        if dropToAccessory {
            NSApp.setActivationPolicy(.accessory)
        }
    }
}

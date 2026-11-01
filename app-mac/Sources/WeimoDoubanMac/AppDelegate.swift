import AppKit

/// 原生壳总控：托盘（NSStatusItem + 菜单）、sidecar 生命周期、窗口创建、
/// 单实例与 Tauri 版共存警告、同步监控接线。对应 Tauri 版 main.rs 的壳职责
/// （端口绑定/窗口/托盘/更新/单实例），原生重写。
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {

    private var statusItem: NSStatusItem?
    private let sidecar = SidecarServer()
    private var windowController: MainWindowController?
    private var syncMonitor: SyncMonitor?

    // 托盘菜单项引用（状态行文本刷新 / 立即同步禁用态）
    private var statusLineItem: NSMenuItem?
    private var syncNowItem: NSMenuItem?

    // 全局快捷键（路线图 1.5）：⌘⇧M 唤起/收起，偏好窗可重录
    private let hotKey = HotKeyCenter()

    /// 壳设置原生偏好窗（⌘,/Dock/托盘三入口）：自启开关、热键录制、数据
    /// 文件夹——托盘随之瘦身；内容偏好（卡片显示）留 web 弹层
    private lazy var preferences = PreferencesWindowController(
        hotKey: hotKey,
        shouldDropToAccessory: { [weak self] in
            self?.windowController?.isWindowVisible != true
        })

    /// 进程存活期的强引用兜底：NSApplication.delegate 为弱引用，main.swift
    /// 闭包局部变量在 release 优化下理论上可能被提前释放——静态持有杜绝整
    /// 个壳（托盘/窗口/监控）随 delegate 回收而静默消失的一类整案
    private static var retention: AppDelegate?

    override init() {
        super.init()
        Self.retention = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard handleSingleInstance() else { NSApp.terminate(nil); return }

        Notifier.shared.setup()
        Notifier.shared.onActivate = { [weak self] in self?.showMainWindow() }

        // 二次启动的唤起信号（单实例分支在退出前投递）
        DistributedNotificationCenter.default().addObserver(
            self, selector: #selector(showMainWindow),
            name: Notification.Name("WeimoDoubanNativeShowWindow"), object: nil)

        warnIfTauriRunning()
        buildStatusItem()
        buildMainMenu()

        // 全局快捷键：注册失败（组合被占）不致命——告警一次，可经托盘菜单重录
        hotKey.onTrigger = { [weak self] in self?.hotkeyTriggered() }
        if !hotKey.register() {
            alert("全局快捷键 \(hotKey.displayCombo) 注册失败（可能被其他 App 占用），可在托盘菜单「唤起快捷键」重新录制。")
        }

        sidecar.onReady = { [weak self] baseURL in
            self?.serverReady(baseURL: baseURL)
        }
        sidecar.onUnexpectedExit = { status in
            self.fatalAlert("本地服务异常退出（status \(status)），App 即将退出。可尝试重新启动。")
        }
        do {
            try sidecar.start(binary: serverBinary(), dataDir: ShellSupport.dataDir)
        } catch {
            fatalAlert("本地服务启动失败：\(error.localizedDescription)")
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        sidecar.shutdown()
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }

    /// 混合形态下 Dock 图标点击 = 重开主窗口（accessory 期间 Dock 不在，
    /// 此回调只在 .regular + 窗口已收起后短窗口期有意义，兜底无害）
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        showMainWindow()
        return true
    }

    /// Dock 右键菜单（.regular 窗口期可见）
    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        let menu = NSMenu()
        let open = NSMenuItem(title: "打开主窗口", action: #selector(showMainWindow), keyEquivalent: "")
        open.target = self
        menu.addItem(open)
        let prefs = NSMenuItem(title: "偏好设置…", action: #selector(showPreferences), keyEquivalent: "")
        prefs.target = self
        menu.addItem(prefs)
        return menu
    }

    // MARK: - 启动装配

    /// 服务就绪 → 建窗 + 起监控（服务未就绪前开窗只会砸在空端口上）
    private func serverReady(baseURL: URL) {
        let controller = MainWindowController(baseURL: baseURL)
        windowController = controller
        controller.show()

        let monitor = SyncMonitor(baseURL: baseURL)
        monitor.onStatusLine = { [weak self] line in
            self?.statusLineItem?.title = "状态：\(line)"
            self?.syncNowItem?.isEnabled = !line.hasPrefix("同步中")
        }
        monitor.onNotify = { title, body in
            Notifier.shared.post(title: title, body: body)
        }
        syncMonitor = monitor
        monitor.start()
    }

    /// sidecar 二进制定位：dev 走 WEIMO_MAC_SERVER_BIN（dev.sh 注入），
    /// bundle 内取 Resources；都未命中给一个相对路径让 start() 报可读错误
    private func serverBinary() -> URL {
        if let path = ProcessInfo.processInfo.environment["WEIMO_MAC_SERVER_BIN"] {
            return URL(fileURLWithPath: path)
        }
        return Bundle.main.url(forResource: "weimo-douban-mac-server", withExtension: nil)
            ?? URL(fileURLWithPath: "weimo-douban-mac-server")
    }

    /// 单实例：bundle 运行时按 bundle id 判重（防双进程写同一 SQLite），
    /// 二次启动唤醒既有实例窗口后自退；dev 直跑（无 bundle id）不判重
    private func handleSingleInstance() -> Bool {
        guard let bundleId = Bundle.main.bundleIdentifier else { return true }
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId)
            .filter { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier }
        guard let existing = others.first else { return true }
        DistributedNotificationCenter.default().postNotificationName(
            Notification.Name("WeimoDoubanNativeShowWindow"), object: nil, deliverImmediately: true)
        existing.activate()
        return false
    }

    /// 与 Tauri 版共存警告：两版共用同一 SQLite（未开 WAL、busy_timeout 仅
    /// sqlx 默认 5s），双进程同时写有 SQLITE_BUSY 风险——告知后由用户定夺
    private func warnIfTauriRunning() {
        let tauriRunning = !NSRunningApplication.runningApplications(
            withBundleIdentifier: ShellSupport.tauriBundleIdentifier).isEmpty
        guard tauriRunning else { return }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "检测到 Tauri 版微墨·豆瓣正在运行"
        alert.informativeText = "两版共用同一数据库，同时运行可能产生写冲突（未开 WAL）。\n建议先退出其中一版再继续。"
        alert.addButton(withTitle: "仍要运行")
        alert.addButton(withTitle: "退出")
        NSApp.activate(ignoringOtherApps: true)
        if alert.runModal() == .alertSecondButtonReturn {
            NSApp.terminate(nil)
        }
    }

    /// 系统主菜单（.regular 形态窗口激活时菜单栏展示；accessory 期间不显示）：
    /// 最小集——关于（标准面板，bundle 信息）/隐藏/退出
    private func buildMainMenu() {
        let mainMenu = NSMenu()
        let appMenuItem = NSMenuItem()
        mainMenu.addItem(appMenuItem)
        let appMenu = NSMenu()
        appMenuItem.submenu = appMenu
        appMenu.addItem(
            withTitle: "关于微墨·豆瓣", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
            keyEquivalent: "")
        appMenu.addItem(.separator())
        let preferencesItem = NSMenuItem(
            title: "偏好设置…", action: #selector(showPreferences), keyEquivalent: ",")
        preferencesItem.keyEquivalentModifierMask = .command
        appMenu.addItem(preferencesItem)
        appMenu.addItem(.separator())
        appMenu.addItem(
            withTitle: "隐藏微墨·豆瓣", action: #selector(NSApplication.hide(_:)),
            keyEquivalent: "h")
        appMenu.addItem(.separator())
        appMenu.addItem(
            withTitle: "退出微墨·豆瓣", action: #selector(NSApplication.terminate(_:)),
            keyEquivalent: "q")
        NSApp.mainMenu = mainMenu
    }

    // MARK: - 托盘

    private func buildStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        // 托盘位置记忆（用户 Cmd-拖动调整后跨启动恢复）
        item.autosaveName = "MainStatusItem"
        // 主图标=与 Tauri 版统一的白墨滴资产（app/icons/tray-icon.png，
        // e38eac6 的 22.5% 圆角裁切纯白墨滴）；资产缺失回落 SF Symbol 再
        // 回落文本 title，杜绝隐形托盘（clapperboard 符号实测部分 macOS 为 nil）
        if let icon = trayIconImage() {
            item.button?.image = icon
            NSLog("[tray] 图标已挂载 墨滴 template")
        } else if let symbolName = ["film", "book"].first(where: {
            NSImage(systemSymbolName: $0, accessibilityDescription: nil) != nil
        }), let image = NSImage(systemSymbolName: symbolName, accessibilityDescription: "微墨·豆瓣") {
            image.isTemplate = true
            item.button?.image = image
            NSLog("[tray] 墨滴资产缺失，回落 symbol=%@", symbolName)
        } else {
            item.button?.title = "微墨"
            NSLog("[tray] SF Symbol 候选全不可用，回落文本 title")
        }

        let menu = NSMenu()

        let open = NSMenuItem(title: "打开主窗口", action: #selector(showMainWindow), keyEquivalent: "o")
        open.target = self
        open.keyEquivalentModifierMask = .command
        menu.addItem(open)

        let statusLine = NSMenuItem(title: "状态：启动中…", action: nil, keyEquivalent: "")
        statusLine.isEnabled = false
        menu.addItem(statusLine)
        statusLineItem = statusLine

        let syncNow = NSMenuItem(title: "立即同步", action: #selector(syncNowTapped), keyEquivalent: "s")
        syncNow.target = self
        syncNow.keyEquivalentModifierMask = .command
        menu.addItem(syncNow)
        syncNowItem = syncNow

        let openData = NSMenuItem(title: "打开数据文件夹", action: #selector(openDataFolderTapped), keyEquivalent: "")
        openData.target = self
        menu.addItem(openData)

        menu.addItem(.separator())

        let prefs = NSMenuItem(title: "偏好设置…", action: #selector(showPreferences), keyEquivalent: ",")
        prefs.target = self
        prefs.keyEquivalentModifierMask = .command
        menu.addItem(prefs)

        let github = NSMenuItem(title: "GitHub 仓库", action: #selector(openGitHubTapped), keyEquivalent: "")
        github.target = self
        menu.addItem(github)

        menu.addItem(.separator())

        let quit = NSMenuItem(title: "退出微墨·豆瓣", action: #selector(quitTapped), keyEquivalent: "q")
        quit.target = self
        quit.keyEquivalentModifierMask = .command
        menu.addItem(quit)

        item.menu = menu
        statusItem = item
    }

    /// 墨滴托盘图标：bundle Resources/tray-icon.png（build-app.sh 从 Tauri 版
    /// 资产拷入，只读不改）；dev 直跑无 bundle，回落仓库源资产
    /// ../app/icons/tray-icon.png（dev.sh 的 cwd=app-mac）。isTemplate=true
    /// 只取 alpha 形状、系统按菜单栏明暗自适配前景——暗色下即 Tauri 版纯白
    /// 墨滴，亮色下系统描黑（Tauri 版已知亮色隐形的边界，即 e38eac6 注释
    /// 预留的 template 方案）。18pt 逻辑尺寸，墨滴占画布 ~73% 留呼吸距
    private func trayIconImage() -> NSImage? {
        let url = Bundle.main.url(forResource: "tray-icon", withExtension: "png")
            ?? URL(fileURLWithPath: "../app/icons/tray-icon.png")
        guard let image = NSImage(contentsOfFile: url.path) else { return nil }
        image.isTemplate = true
        image.size = NSSize(width: 18, height: 18)
        return image
    }

    // MARK: - 菜单动作

    @objc func showMainWindow() {
        // 服务未就绪时窗口尚未创建（就绪即自动弹出，此处无操作）
        windowController?.show()
    }

    /// 全局快捷键 toggle：用户正看着本 App（窗口可见且前台）→ 收起；
    /// 其余情况唤起聚焦（Raycast 式随手查）。2.3 直落搜索待前端搜索功能
    /// 落地后在唤起分支补 evaluateJavaScript 聚焦搜索框
    private func hotkeyTriggered() {
        if let controller = windowController, controller.isFrontmostVisible {
            controller.hide()
        } else {
            windowController?.show()
        }
    }

    /// 偏好设置窗（⌘, / Dock 菜单 / 托盘菜单三入口收敛于此）
    @objc private func showPreferences() {
        preferences.show()
    }

    @objc private func syncNowTapped() {
        guard let monitor = syncMonitor else { return }
        syncNowItem?.isEnabled = false
        syncNowItem?.title = "同步请求中…"
        Task {
            if let message = await monitor.triggerSyncAll() {
                alert(message)
            }
            syncNowItem?.isEnabled = true
            syncNowItem?.title = "立即同步"
        }
    }

    @objc private func openDataFolderTapped() {
        ShellSupport.revealDataDir()
    }

    @objc private func openGitHubTapped() {
        ShellSupport.openExternal(ShellSupport.repositoryURL)
    }

    @objc private func quitTapped() {
        NSApp.terminate(nil)
    }

    // MARK: - 弹窗

    private func alert(_ message: String) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "好")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    private func fatalAlert(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = message
        alert.addButton(withTitle: "退出")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
        NSApp.terminate(nil)
    }
}

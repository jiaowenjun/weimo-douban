import AppKit
import WebKit

/// 主窗口：WKWebView 承载内嵌服务器上的现有 web UI（零前端改动）。
/// - 壳检测：documentStart 注入 `window.isTauri = true`——复用前端
///   welcome-page 现成分支进入「App 壳内」语义（隐藏下载 CTA/GitHub 入口、
///   CTA 变「开始备份」），本壳借同款约定，前端一行不改
/// - 窗口状态：frameAutosaveName 原生记忆位置尺寸（路线图 1.3），首启居中
/// - 关窗 = 隐藏不退出（托盘常驻，订阅/富集后台任务持续推进）
/// - 导航策略：仅放行 127.0.0.1 同源；外链交系统默认浏览器（路线图 1.4）；
///   CSV 导出（blob 锚点）经 shouldPerformDownload → WKDownload → NSSavePanel
///   原生保存面板（路线图 2.1 的原生形态）
final class MainWindowController: NSObject, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate, WKScriptMessageHandler {

    /// contextmenu 桥脚本（路线图 2.2）：右键落在标记卡片上（前端 MarkCard
    /// 根节点惰性携带 data-subject-url/data-title，纯属性 web 端零行为差异）
    /// 拦截默认菜单并回传标题/豆瓣链接，壳弹原生 NSMenu；无卡片上下文
    /// （空白处/普通文本）不拦截，保留 WebKit 默认行为。属性选择器与
    /// weimo-ui 类名解耦
    private static let contextMenuScript = """
    (function () {
      document.addEventListener('contextmenu', function (event) {
        var node = event.target;
        var element = node && node.closest ? node : (node ? node.parentElement : null);
        var card = element ? element.closest('li[data-subject-url]') : null;
        if (!card) return;
        event.preventDefault();
        window.webkit.messageHandlers.shellContextMenu.postMessage({
          url: card.getAttribute('data-subject-url') || '',
          title: card.getAttribute('data-title') || ''
        });
      }, false);
    })();
    """

    /// 标题栏一体化壳桥脚本（路线图 3.1）：
    /// - 拖拽：mousedown 落在顶栏条带内且非交互元素（磨砂条下滚过的卡片
    ///   经 closest(INTERACTIVE) 放行，点卡片仍是点卡片）→ preventDefault +
    ///   通知壳 performDrag——顶栏 frame 是 pointer-events:none，事件本就
    ///   落在下层元素，故在 document 捕获层按几何判定
    /// - 双击顶栏空白 → 窗口 zoom（macOS 标题栏惯例）
    /// - 布局探针：load/resize 后量 .top-bar/.top-bar__frame/左槽首个交互
    ///   元素矩形回传壳打日志——红绿灯让位用数据核验（traffic lights 占
    ///   x∈[0,78] y∈[0,28]，须 leftInteractive.x ≥ 78 或不存在）
    private static let chromeScript = """
    (function () {
      var INTERACTIVE = 'button, a, input, select, textarea, label, [role="button"]';
      function strip() {
        var bar = document.querySelector('.top-bar');
        return bar ? bar.getBoundingClientRect() : null;
      }
      function inDragZone(event) {
        var rect = strip();
        if (!rect || event.clientY > rect.bottom) return false;
        var node = event.target;
        var element = node && node.closest ? node : (node ? node.parentElement : null);
        return !(element && element.closest(INTERACTIVE));
      }
      document.addEventListener('mousedown', function (event) {
        if (event.button === 0 && inDragZone(event)) {
          event.preventDefault();
          window.webkit.messageHandlers.shellChrome.postMessage('drag');
        }
      }, true);
      document.addEventListener('dblclick', function (event) {
        if (inDragZone(event)) {
          window.webkit.messageHandlers.shellChrome.postMessage('zoom');
        }
      }, true);
      function rounded(el) {
        if (!el) return null;
        var r = el.getBoundingClientRect();
        return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
      }
      function probe() {
        var bar = document.querySelector('.top-bar');
        if (!bar) return;
        var left = bar.querySelector('.top-bar__slot--left') || bar.querySelector('.top-bar__slot');
        window.webkit.messageHandlers.shellChrome.postMessage({
          kind: 'probe', path: location.hash,
          bar: rounded(bar),
          frame: rounded(bar.querySelector('.top-bar__frame')),
          leftSlot: rounded(left),
          leftInteractive: rounded(left && left.querySelector(INTERACTIVE))
        });
      }
      window.addEventListener('load', function () {
        setTimeout(probe, 800);
        setTimeout(probe, 3000);
      });
      var resizeTimer = 0;
      window.addEventListener('resize', function () {
        clearTimeout(resizeTimer);
        resizeTimer = setTimeout(probe, 300);
      });
    })();
    """

    private var window: NSWindow!
    private var webView: ShellWebView!
    private let baseURL: URL
    private var activeDownloads: Set<WKDownload> = []
    private var loadRetries = 0
    private var hasCommittedOnce = false
    /// 实际加载地址（WEIMO_MAC_START_URL 测试覆盖后的初始 URL）
    private let initialURL: URL

    /// 记录最近一次 mouseDown 供 performDrag 用（消息桥异步到达时
    /// NSApp.currentEvent 可能已被 mouseMove 顶掉，取事件存根才稳）
    private final class ShellWebView: WKWebView {
        var lastMouseDown: NSEvent?
        override func mouseDown(with event: NSEvent) {
            lastMouseDown = event
            super.mouseDown(with: event)
        }
    }

    /// 测试钩子：WEIMO_MAC_START_URL 覆盖初始地址——完整 URL 原样用；以 #
    /// 开头视为拼在本实例 baseURL 上的 hash（端到端探针直达 /#/movie 等
    /// 深链，回环端口每次随机，写死端口必踩死链）
    private static func startURL(default baseURL: URL) -> URL {
        guard let raw = ProcessInfo.processInfo.environment["WEIMO_MAC_START_URL"] else {
            return baseURL
        }
        if raw.hasPrefix("#"), let url = URL(string: baseURL.absoluteString + raw) {
            return url
        }
        return URL(string: raw) ?? baseURL
    }

    init(baseURL: URL) {
        self.baseURL = baseURL
        self.initialURL = Self.startURL(default: baseURL)
        super.init()

        let contentRect = NSRect(x: 0, y: 0, width: 1280, height: 860)
        window = NSWindow(
            contentRect: contentRect,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "微墨·豆瓣"
        // 标题栏一体化（路线图 3.1）：内容顶到窗口顶、系统标题栏透明隐藏，
        // 红绿灯悬浮于 web 顶栏左侧空白带（窗口最小 960 恒 ≥700px 布局，
        // 左槽 margin 推至 ~329px；欢迎页左槽壳内为空——零 CSS 让位，
        // 探针日志核验）
        window.styleMask.insert(.fullSizeContentView)
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.minSize = NSSize(width: 960, height: 600)
        window.delegate = self
        // 原生窗口状态记忆：setFrameAutosaveName 存档有则自动恢复；无存档
        // （首启）先居中。存档判据 = UserDefaults 的 "NSWindow Frame <name>"
        // 键（AppKit 文档化存储位；frameUsingName 在新 SDK 已移除）
        let autosaveName = "MainWindow"
        if UserDefaults.standard.string(forKey: "NSWindow Frame \(autosaveName)") == nil {
            window.center()
        }
        _ = window.setFrameAutosaveName(autosaveName)

        let configuration = WKWebViewConfiguration()
        // 默认持久 dataStore：前端 localStorage 存 uid/主题等，必须持久
        configuration.websiteDataStore = .default()
        let userContentController = configuration.userContentController
        let isTauri = WKUserScript(
            source: "window.isTauri = true;",
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        userContentController.addUserScript(isTauri)
        userContentController.addUserScript(WKUserScript(
            source: Self.contextMenuScript,
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true))
        userContentController.addUserScript(WKUserScript(
            source: Self.chromeScript,
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true))
        // WKUserContentController 对 handler 强持有——弱代理切断
        // config→controller→self→webView 的经典保留环
        userContentController.add(
            WeakMessageProxy(target: self), name: "shellContextMenu")
        userContentController.add(
            WeakMessageProxy(target: self), name: "shellChrome")

        webView = ShellWebView(frame: window.contentView?.bounds ?? contentRect, configuration: configuration)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        window.contentView?.addSubview(webView)

        load()
    }

    private func load() {
        webView.load(URLRequest(url: initialURL))
    }

    /// 混合形态：窗口可见期间 .regular（Dock 图标 + Cmd+Tab + 系统菜单栏），
    /// 收起回 .accessory（纯托盘）——菜单栏常驻哲学不变，Dock 可达性随窗口
    func show() {
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// 收起到托盘：窗口 orderOut + 降回 accessory（Dock 图标随之消失）
    func hide() {
        window.orderOut(nil)
        NSApp.setActivationPolicy(.accessory)
    }

    /// 全局快捷键的收起判定：窗口可见 + 是 key window + App 在前台
    /// （用户正看着本 App）→ 此时热键语义为收起；其余情况唤起
    var isFrontmostVisible: Bool {
        window.isVisible && window.isKeyWindow && NSApp.isActive
    }

    // MARK: NSWindowDelegate —— 关窗 = 收起到托盘（Dock 图标随降回 accessory 消失）

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        hide()
        return false
    }

    // MARK: WKNavigationDelegate

    /// 首次加载失败兜底（READY 行与 serve 循环间的极小竞态）：重试至多 3 次。
    /// 之后的失败不重试（用户操作/后端问题，静默记录）
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        retryInitialLoadIfNeeded(error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        retryInitialLoadIfNeeded(error)
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        hasCommittedOnce = true
    }

    private func retryInitialLoadIfNeeded(_ error: Error) {
        guard !hasCommittedOnce, loadRetries < 3 else {
            NSLog("[window] 页面加载失败: \(error.localizedDescription)")
            return
        }
        loadRetries += 1
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            self?.load()
        }
    }

    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
        // 下载意图（CSV 导出的 blob/data 锚点）：交原生下载管线（→ NSSavePanel）
        if navigationAction.shouldPerformDownload {
            decisionHandler(.download)
            return
        }
        guard let url = navigationAction.request.url else {
            decisionHandler(.cancel)
            return
        }
        switch url.scheme?.lowercased() {
        case "http", "https":
            if url.host == "127.0.0.1" {
                decisionHandler(.allow)
            } else {
                // 外部链接 → 系统默认浏览器（路线图 1.4）
                ShellSupport.openExternal(url)
                decisionHandler(.cancel)
            }
        default:
            decisionHandler(.cancel)
        }
    }

    // MARK: WKUIDelegate —— window.open 兜底（壳内模式页面已隐藏此类入口）

    func webView(
        _ webView: WKWebView,
        createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction,
        windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        if let url = navigationAction.request.url {
            ShellSupport.openExternal(url)
        }
        return nil
    }

    // MARK: 原生右键菜单（WKScriptMessageHandler 桥，路线图 2.2）

    /// WKUserContentController 强持有 handler 的弱代理壳
    private final class WeakMessageProxy: NSObject, WKScriptMessageHandler {
        weak var target: WKScriptMessageHandler?
        init(target: WKScriptMessageHandler) {
            self.target = target
            super.init()
        }
        func userContentController(
            _ userContentController: WKUserContentController,
            didReceive message: WKScriptMessage
        ) {
            target?.userContentController(userContentController, didReceive: message)
        }
    }

    /// 消息在主线程送达，可直接弹菜单；鼠标位置取当前 NSEvent（消息异步
    /// 到达的毫秒级漂移可忽略）
    func userContentController(
        _ userContentController: WKUserContentController,
        didReceive message: WKScriptMessage
    ) {
        if message.name == "shellChrome" {
            handleChromeMessage(message)
            return
        }
        guard message.name == "shellContextMenu",
              let body = message.body as? [String: Any],
              let url = body["url"] as? String, !url.isEmpty
        else { return }
        presentCardContextMenu(title: (body["title"] as? String) ?? "", url: url)
    }

    /// 标题栏一体化桥（3.1）：drag/zoom 动作 + 布局探针回传
    private func handleChromeMessage(_ message: WKScriptMessage) {
        if let action = message.body as? String {
            switch action {
            case "drag":
                // mouseDown 存根（NSApp.currentEvent 可能已被后续事件顶掉）
                if let event = webView.lastMouseDown, event.type == .leftMouseDown {
                    window.performDrag(with: event)
                }
            case "zoom":
                window.zoom(nil)
            default:
                break
            }
            return
        }
        if let probe = message.body as? [String: Any], probe["kind"] as? String == "probe" {
            NSLog("[chrome] probe %@", String(describing: probe))
        }
    }

    private func presentCardContextMenu(title: String, url: String) {
        let menu = NSMenu()
        if !title.isEmpty {
            let copyTitle = NSMenuItem(title: "复制标题", action: #selector(copyTitleTapped(_:)), keyEquivalent: "")
            copyTitle.target = self
            copyTitle.representedObject = title
            menu.addItem(copyTitle)
        }
        let copyLink = NSMenuItem(title: "复制豆瓣链接", action: #selector(copyLinkTapped(_:)), keyEquivalent: "")
        copyLink.target = self
        copyLink.representedObject = url
        menu.addItem(copyLink)
        if let itemURL = URL(string: url) {
            let open = NSMenuItem(title: "在豆瓣打开", action: #selector(openLinkTapped(_:)), keyEquivalent: "")
            open.target = self
            open.representedObject = itemURL
            menu.addItem(.separator())
            menu.addItem(open)
        }
        // 屏幕坐标 → 窗口 → 视图两步走：convert(_:from: nil) 吃的是窗口坐标，
        // 直接喂屏幕点会被窗口原点再偏一次（曾致菜单弹到窗口外）
        let windowPoint = window.convertPoint(fromScreen: NSEvent.mouseLocation)
        let location = webView.convert(windowPoint, from: nil)
        menu.popUp(positioning: nil, at: location, in: webView)
    }

    @objc private func copyTitleTapped(_ sender: NSMenuItem) {
        copyToPasteboard((sender.representedObject as? String) ?? "")
    }

    @objc private func copyLinkTapped(_ sender: NSMenuItem) {
        copyToPasteboard((sender.representedObject as? String) ?? "")
    }

    @objc private func openLinkTapped(_ sender: NSMenuItem) {
        if let url = sender.representedObject as? URL {
            ShellSupport.openExternal(url)
        }
    }

    private func copyToPasteboard(_ string: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(string, forType: .string)
    }

    // MARK: 下载（WKDownloadDelegate）——CSV 导出落 NSSavePanel

    func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
        beginDownload(download)
    }

    func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
        beginDownload(download)
    }

    private func beginDownload(_ download: WKDownload) {
        // WKDownload.delegate 为弱引用，Set 持有至下载结束
        download.delegate = self
        activeDownloads.insert(download)
    }

    func download(
        _ download: WKDownload,
        decideDestinationUsing response: URLResponse,
        suggestedFilename: String,
        completionHandler: @escaping (URL?) -> Void
    ) {
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.nameFieldStringValue = suggestedFilename.isEmpty ? "导出.csv" : suggestedFilename
        panel.beginSheetModal(for: window) { response in
            completionHandler(response == .OK ? panel.url : nil)
        }
    }

    func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
        activeDownloads.remove(download)
        NSLog("[download] 下载失败: \(error.localizedDescription)")
    }
}

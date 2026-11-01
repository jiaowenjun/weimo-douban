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
final class MainWindowController: NSObject, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate {

    private var window: NSWindow!
    private var webView: WKWebView!
    private let baseURL: URL
    private var activeDownloads: Set<WKDownload> = []
    private var loadRetries = 0
    private var hasCommittedOnce = false

    init(baseURL: URL) {
        self.baseURL = baseURL
        super.init()

        let contentRect = NSRect(x: 0, y: 0, width: 1280, height: 860)
        window = NSWindow(
            contentRect: contentRect,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "微墨·豆瓣"
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
        let isTauri = WKUserScript(
            source: "window.isTauri = true;",
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        configuration.userContentController.addUserScript(isTauri)

        webView = WKWebView(frame: window.contentView?.bounds ?? contentRect, configuration: configuration)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        window.contentView?.addSubview(webView)

        load()
    }

    private func load() {
        webView.load(URLRequest(url: baseURL))
    }

    func show() {
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// 全局快捷键的收起判定：窗口可见 + 是 key window + App 在前台
    /// （用户正看着本 App）→ 此时热键语义为收起；其余情况唤起
    var isFrontmostVisible: Bool {
        window.isVisible && window.isKeyWindow && NSApp.isActive
    }

    func hide() {
        window.orderOut(nil)
    }

    // MARK: NSWindowDelegate —— 关窗 = 隐藏（托盘常驻）

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
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

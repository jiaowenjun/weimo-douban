import AppKit
import ServiceManagement

/// 壳层杂项支撑：共享数据目录、外链打开、Finder 定位、开机自启。
enum ShellSupport {

    /// Tauri 版 bundle id（共存检测用——两版共用同一 SQLite）
    static let tauriBundleIdentifier = "ink.weimo.douban"

    /// GitHub 仓库（托盘菜单入口；前端壳内模式已隐藏页面内的 GitHub 按钮）
    static let repositoryURL = URL(string: "https://github.com/jiaowenjun/weimo-douban")!

    /// 是否运行在 .app bundle 内（dev 直跑二进制时 bundleIdentifier 为 nil）
    static var isBundled: Bool { Bundle.main.bundleIdentifier != nil }

    /// 与 Tauri 版共用的数据目录（douban_export.db + covers/），免重抓。
    /// 双进程同时写同一 SQLite 有冲突风险（未开 WAL，busy_timeout 仅 5s），
    /// 由 AppDelegate 启动时检测并警告。
    static var dataDir: URL {
        let base = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first
            ?? URL(fileURLWithPath: NSHomeDirectory())
                .appendingPathComponent("Library/Application Support")
        let dir = base.appendingPathComponent("ink.weimo.douban", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// 默认浏览器打开外链（GitHub 仓库、误入的外部导航等）
    static func openExternal(_ url: URL) {
        NSWorkspace.shared.open(url)
    }

    /// 在访达中打开数据目录
    static func revealDataDir() {
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: dataDir.path)
    }

    // MARK: 开机自启（macOS 13+ SMAppService；dev 直跑无 bundle 不可用）

    /// 托盘常驻形态的自然搭配：数据新鲜度 = App 存活期（README 哲学同 Tauri 版）
    static var launchAtLoginEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    @discardableResult
    static func setLaunchAtLogin(_ enabled: Bool) -> Bool {
        guard isBundled else {
            NSLog("[shell] dev 直跑无 bundle，开机自启不可用")
            return false
        }
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            return true
        } catch {
            NSLog("[shell] 开机自启设置失败: \(error)")
            return false
        }
    }
}

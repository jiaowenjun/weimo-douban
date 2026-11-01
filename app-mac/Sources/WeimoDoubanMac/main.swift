import AppKit

// 微墨·豆瓣 mac 原生壳入口。编程式 accessory（LSUIElement 等价，无 Dock
// 图标）——dev 直跑二进制与 bundle 内运行行为一致（Info.plist 同时兜底）。
// main.swift 顶层代码非隔离，AppDelegate 为 @MainActor：assumeIsolated 进入
// （启动天然在主线程）；闭包内局部变量随 app.run() 阻塞存活，弱引用的
// delegate 不会被提前释放。
MainActor.assumeIsolated {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let appDelegate = AppDelegate()
    app.delegate = appDelegate
    app.run()
}

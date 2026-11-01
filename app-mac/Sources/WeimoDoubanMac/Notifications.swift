import Foundation
import UserNotifications

/// 系统通知薄封装（路线图 1.1）：同步/富集终态通知，点击唤起主窗口。
/// dev 直跑（无 bundle id）时 UNUserNotificationCenter 不可用——降级为
/// NSLog，功能不致命（托盘状态行仍实时）。
final class Notifier: NSObject, UNUserNotificationCenterDelegate {

    static let shared = Notifier()
    private var authorizationRequested = false

    /// 通知点击 → 唤起主窗口（主线程执行）
    var onActivate: (@MainActor () -> Void)?

    /// 启动时挂 delegate（须早于首次投递；无 bundle id 时安全跳过）
    func setup() {
        guard isAvailable else {
            NSLog("[notify] 无 bundle id，通知降级为日志（dev 运行）")
            return
        }
        UNUserNotificationCenter.current().delegate = self
    }

    /// UNUserNotificationCenter 要求进程有 bundle id（否则抛 NSException）
    private var isAvailable: Bool { Bundle.main.bundleIdentifier != nil }

    /// 投递一条通知；首次投递时顺带请求授权（上下文时机比启动即弹更自然，
    /// 拒绝授权则静默降级为日志）
    func post(title: String, body: String) {
        NSLog("[notify] 投递 %@ | %@", title, body)
        guard isAvailable else {
            return
        }
        let center = UNUserNotificationCenter.current()
        if authorizationRequested {
            addRequest(center, title: title, body: body)
            return
        }
        authorizationRequested = true
        center.requestAuthorization(options: [.alert, .sound]) { [weak self] granted, error in
            if let error {
                NSLog("[notify] 授权请求失败: \(error)")
            }
            guard granted else { return }
            DispatchQueue.main.async {
                self?.addRequest(center, title: title, body: body)
            }
        }
    }

    private func addRequest(_ center: UNUserNotificationCenter, title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        let request = UNNotificationRequest(
            identifier: UUID().uuidString, content: content, trigger: nil)
        center.add(request)
    }

    // MARK: UNUserNotificationCenterDelegate

    /// 通知点击 → 唤起主窗口（UNUserNotificationCenterDelegate 非主线程回调，
    /// 经 DispatchQueue.main + assumeIsolated 进入 MainActor 闭包）
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.onActivate?() }
            completionHandler()
        }
    }

    /// App 在前台时也以横幅展示（accessory app 无 Dock，默认行为即可满足，
    /// 显式声明避免实现差异）
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}

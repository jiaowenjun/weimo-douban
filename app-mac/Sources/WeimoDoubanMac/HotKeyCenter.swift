import AppKit
import Carbon.HIToolbox

/// 全局快捷键（路线图 1.5）：Carbon RegisterEventHotKey——非沙盒 app 系统级
/// 热键的标准通道（NSEvent 全局 monitor 无法吞键且不保证收到），菜单栏
/// 常驻 App 的「随手唤起」入口。默认 ⌘⇧M 唤起/收起主窗口；组合经录制面板
/// （HotKeyRecorder）重录，UserDefaults 持久化。
///
/// 2.3「唤起直落搜索」暂缓：前端搜索功能尚未实现（TopBarSearch 为占位
/// 弹窗），待其落地后在 onTrigger 的唤起分支加一行 evaluateJavaScript 聚焦
/// 搜索框即可——原生壳无 IPC 墙，webView 是自己的。
final class HotKeyCenter {

    /// 热键签名（FourCharCode 'WMK1'，区分同一进程内其他热键）
    private static let signature: OSType = 0x574D_4B31

    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?

    /// 热键命中回调（Carbon 事件在主线程送达，经 Task @MainActor 投递后触 UI）
    var onTrigger: (@MainActor () -> Void)?

    private(set) var keyCode: UInt32 = UInt32(kVK_ANSI_M)
    private(set) var modifiers: NSEvent.ModifierFlags = [.command, .shift]
    private(set) var keyDisplay = "M"

    private let defaults = UserDefaults.standard
    private let keyCodeKey = "hotkeyCode"
    private let modifiersKey = "hotkeyModifiers"
    private let displayKey = "hotkeyDisplay"

    init() {
        // 有任一持久化字段即视为已录制过（三字段同写，脏数据只认 keyCode+display）
        if let code = defaults.object(forKey: keyCodeKey) as? Int,
           let raw = defaults.object(forKey: modifiersKey) as? Int,
           let display = defaults.string(forKey: displayKey), !display.isEmpty {
            keyCode = UInt32(code)
            modifiers = NSEvent.ModifierFlags(rawValue: UInt(raw))
            keyDisplay = display
        }
    }

    /// 菜单/告警展示文案（⌃⌥⇧⌘ 固定次序 + 键名）
    var displayCombo: String {
        var combo = ""
        if modifiers.contains(.control) { combo += "⌃" }
        if modifiers.contains(.option) { combo += "⌥" }
        if modifiers.contains(.shift) { combo += "⇧" }
        if modifiers.contains(.command) { combo += "⌘" }
        return combo + keyDisplay
    }

    /// 注册系统热键；失败（组合被其他 App 占用等）返回 false 由调用方告警
    @discardableResult
    func register() -> Bool {
        removeHotKey()
        installHandlerIfNeeded()
        let hotKeyID = EventHotKeyID(signature: Self.signature, id: 1)
        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(
            keyCode, carbonModifiers, hotKeyID,
            GetApplicationEventTarget(), 0, &ref)
        guard status == noErr, ref != nil else {
            NSLog("[hotkey] 注册失败 status=%d combo=%@", status, displayCombo)
            return false
        }
        hotKeyRef = ref
        NSLog("[hotkey] 已注册 %@", displayCombo)
        return true
    }

    /// 换绑新组合：注册成功才落盘；失败回滚旧组合（旧热键恢复注册）
    @discardableResult
    func update(keyCode: UInt32, modifiers: NSEvent.ModifierFlags, display: String) -> Bool {
        let old = (self.keyCode, self.modifiers, self.keyDisplay)
        self.keyCode = keyCode
        self.modifiers = modifiers
        self.keyDisplay = display
        if register() {
            defaults.set(Int(keyCode), forKey: keyCodeKey)
            defaults.set(Int(modifiers.rawValue), forKey: modifiersKey)
            defaults.set(display, forKey: displayKey)
            return true
        }
        (self.keyCode, self.modifiers, self.keyDisplay) = old
        register()
        return false
    }

    /// 回到出厂 ⌘⇧M（清除持久化，录制面板「恢复默认」用）
    func restoreDefault() {
        keyCode = UInt32(kVK_ANSI_M)
        modifiers = [.command, .shift]
        keyDisplay = "M"
        register()
        defaults.removeObject(forKey: keyCodeKey)
        defaults.removeObject(forKey: modifiersKey)
        defaults.removeObject(forKey: displayKey)
    }

    // MARK: - Carbon 私有

    private func installHandlerIfNeeded() {
        guard handlerRef == nil else { return }
        var spec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed))
        // userData 以 passUnretained 传 self：center 随进程存活不释放
        let callback: @convention(c) (
            EventHandlerCallRef?, EventRef?, UnsafeMutableRawPointer?
        ) -> OSStatus = { _, event, userData in
            guard let event, let userData else { return noErr }
            var hotKeyID = EventHotKeyID()
            let status = GetEventParameter(
                event,
                EventParamName(kEventParamDirectObject),
                EventParamType(typeEventHotKeyID),
                nil,
                MemoryLayout<EventHotKeyID>.size,
                nil,
                &hotKeyID)
            guard status == noErr, hotKeyID.signature == HotKeyCenter.signature else { return noErr }
            let center = Unmanaged<HotKeyCenter>.fromOpaque(userData).takeUnretainedValue()
            Task { @MainActor in center.onTrigger?() }
            return noErr
        }
        var handler: EventHandlerRef?
        let status = InstallEventHandler(
            GetApplicationEventTarget(), callback, 1, &spec,
            Unmanaged.passUnretained(self).toOpaque(), &handler)
        if status == noErr {
            handlerRef = handler
        } else {
            NSLog("[hotkey] 事件处理器安装失败 status=%d", status)
        }
    }

    private func removeHotKey() {
        if let hotKeyRef {
            // C 名 RemoveEventHotKey，Swift 导入改名 UnregisterEventHotKey
            UnregisterEventHotKey(hotKeyRef)
        }
        hotKeyRef = nil
    }

    /// NSEvent 修饰位 → Carbon 修饰位（cmdKey/optionKey/controlKey/shiftKey）
    private var carbonModifiers: UInt32 {
        var value: UInt32 = 0
        if modifiers.contains(.command) { value |= UInt32(cmdKey) }
        if modifiers.contains(.option) { value |= UInt32(optionKey) }
        if modifiers.contains(.control) { value |= UInt32(controlKey) }
        if modifiers.contains(.shift) { value |= UInt32(shiftKey) }
        return value
    }
}

import Darwin
import Foundation

/// 无头服务器 sidecar：拉起 weimo-douban-mac-server 子进程（回环随机端口，
/// API + /douban/ 静态），逐行读 stdout 解析 `WEIMO_READY <url>` 就绪行；
/// 壳正常退出时 SIGTERM 子进程（1s 宽限后 SIGKILL 兜底），壳崩溃由服务端
/// 的父进程看门狗自杀兜底（两侧各管一半）。
final class SidecarServer {

    /// 就绪回调（主线程）：内嵌服务 base URL（http://127.0.0.1:<port>/douban/）
    var onReady: (@MainActor (URL) -> Void)?
    /// 意外退出回调（主线程）：非 shutdown() 触发的子进程退出
    var onUnexpectedExit: (@MainActor (Int32) -> Void)?

    private var process: Process?
    private var stopping = false
    private var lineBuffer = ""

    var isRunning: Bool { process?.isRunning ?? false }

    func start(binary: URL, dataDir: URL) throws {
        stopping = false
        lineBuffer = ""

        let process = Process()
        process.executableURL = binary
        process.arguments = ["--data-dir", dataDir.path, "--bind", "127.0.0.1:0"]
        var environment = ProcessInfo.processInfo.environment
        // 父进程探活锚点（服务端看门狗用；SYNC_* 环境变量原样透传）
        environment["WEIMO_PARENT_PID"] = String(ProcessInfo.processInfo.processIdentifier)
        process.environment = environment

        // backend 日志与 WEIMO_READY 都走 stdout（Rust Stdout 行缓冲，按行到达）
        let stdout = Pipe()
        process.standardOutput = stdout
        try process.run()
        self.process = process
        NSLog("[sidecar] 已启动 pid=%d", process.processIdentifier)

        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let chunk = handle.availableData
            guard !chunk.isEmpty else {
                handle.readabilityHandler = nil
                return
            }
            let text = String(data: chunk, encoding: .utf8) ?? ""
            self?.ingest(text)
        }

        // 退出监视：waitUntilExit 阻塞在后台线程，非主动停止即为意外退出
        let terminated = process
        DispatchQueue.global().async { [weak self] in
            terminated.waitUntilExit()
            let status = terminated.terminationStatus
            Task { @MainActor in
                guard let self, !self.stopping else { return }
                self.onUnexpectedExit?(status)
            }
        }
    }

    /// 正常退出路径：SIGTERM → 1s 宽限 → SIGKILL 兜底
    func shutdown() {
        stopping = true
        guard let process, process.isRunning else { return }
        process.terminate()
        NSLog("[sidecar] 已请求终止 pid=%d", process.processIdentifier)
        DispatchQueue.global().async {
            let deadline = Date().addingTimeInterval(1)
            while process.isRunning && Date() < deadline {
                Thread.sleep(forTimeInterval: 0.05)
            }
            if process.isRunning {
                kill(process.processIdentifier, SIGKILL)
                NSLog("[sidecar] 宽限超时，已 SIGKILL pid=%d", process.processIdentifier)
            }
        }
    }

    // MARK: - stdout 逐行解析

    private func ingest(_ text: String) {
        lineBuffer += text
        while let newline = lineBuffer.firstIndex(of: "\n") {
            let line = String(lineBuffer[..<newline]).trimmingCharacters(in: .whitespaces)
            lineBuffer = String(lineBuffer[lineBuffer.index(after: newline)...])
            consumeLine(line)
        }
    }

    private func consumeLine(_ line: String) {
        guard !line.isEmpty else { return }
        let marker = "WEIMO_READY "
        if line.hasPrefix(marker), let url = URL(string: String(line.dropFirst(marker.count))) {
            NSLog("[sidecar] 服务就绪: %@", url.absoluteString)
            Task { @MainActor in self.onReady?(url) }
        } else {
            // backend tracing 日志转发进统一日志（Console.app 可查）
            NSLog("[server] %@", line)
        }
    }
}

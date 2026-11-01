import Foundation

/// 同步/富集监控（路线图 1.1 路线 i：壳侧轮询自己的 localhost API，零后端
/// 改动；与前端灵动岛/侧边栏同端点同口径）。3s 轮询：
/// - 任务状态跃迁 → 系统通知（终态语义与后端一致：completed/partial/failed）
/// - `subjects_pending` >0 → 0 跃迁 → 「条目详情拉取完成」（全局计数，
///   任一 uid 的 cache-version 取值相同）
/// - 托盘状态行实时文本（同步中 P% / 详情拉取中 N 条 / 空闲）
///
/// 首帧只建基线，不回放历史终态（与前端灵动岛「完成即收起」同哲学：
/// 通知只报本会话内亲眼看到的过程）。@MainActor：全部状态主线程持有，
/// await 间隙自动回主线程续跑，UI 回调可直调。
@MainActor
final class SyncMonitor {

    private let host: String
    private let port: Int

    private var timer: Timer?
    private var pollInFlight = false
    private var hasBaseline = false
    private var taskStatuses: [String: String] = [:]
    private var users: [String] = []
    private var lastPending: Int?
    private var lastStatusLine = ""

    /// 托盘状态行文本（「同步中 42%」/「详情拉取中 12 条」/「空闲」）
    var onStatusLine: (@MainActor (String) -> Void)?
    /// 终态通知（title, body）
    var onNotify: (@MainActor (String, String) -> Void)?

    init(baseURL: URL) {
        let components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)
        host = components?.host ?? "127.0.0.1"
        port = components?.port ?? 80
    }

    func start() {
        // Timer 强持有 target（self 随进程存活，无需破环）
        let timer = Timer(timeInterval: 3, target: self, selector: #selector(tick), userInfo: nil, repeats: true)
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        tick()
    }

    @objc private func tick() {
        guard !pollInFlight else { return }
        pollInFlight = true
        Task {
            await poll()
            pollInFlight = false
        }
    }

    // MARK: 托盘「立即同步」：对全部已登记用户 POST（无请求体，后端固定
    // 全类别 + collect；返回 nil=已受理，非 nil=给用户的错误文案）

    func triggerSyncAll() async -> String? {
        if users.isEmpty {
            await refreshUsers()
        }
        guard !users.isEmpty else {
            return "尚无已登记用户，请先在主窗口完成首次使用"
        }
        for uid in users {
            guard let url = endpoint("/users/\(uid)/syncs") else { continue }
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
                    return "同步请求失败（HTTP \(http.statusCode)，用户 \(uid)）"
                }
            } catch {
                return "同步请求失败：\(error.localizedDescription)"
            }
        }
        Task { await poll() }  // 立即拉一轮让状态行动起来（不等下一个 3s）
        return nil
    }

    // MARK: - 轮询主体

    private func poll() async {
        // 1) 最近任务（source=all 同时覆盖手动/自动触发；默认 limit=20 只看最近）
        guard let list = await getJSON("/syncs", query: "source=all&limit=20") as? [String: Any],
              let tasks = list["tasks"] as? [[String: Any]]
        else { return }

        var runningID: String?
        for task in tasks {
            guard let id = task["task_id"] as? String,
                  let status = task["status"] as? String
            else { continue }
            if status == "queued" || status == "running" {
                runningID = runningID ?? id
            }
            handleTransition(id: id, status: status, items: task["items_count"] as? Int ?? 0)
        }
        hasBaseline = true

        // 2) 百分比：任务列表无 percent，取运行中任务详情（percent 可能为 null）
        var percent: Int?
        if let runningID, let detail = await getJSON("/syncs/\(runningID)") as? [String: Any] {
            percent = detail["percent"] as? Int
        }

        // 3) 富集 pending（全局计数；users 为空先补拉一次）
        if users.isEmpty {
            await refreshUsers()
        }
        var pending: Int?
        if let uid = users.first,
           let versions = await getJSON("/users/\(uid)/cache-version") as? [String: Any] {
            pending = versions["subjects_pending"] as? Int
            handlePending(pending ?? 0)
        }

        updateStatusLine(running: runningID != nil, percent: percent, pending: pending ?? lastPending ?? 0)
    }

    /// 任务状态跃迁：仅非终态 → 终态且本会话见过其非终态才通知（防把历史
    /// 终态任务在首次轮询时回放成通知）
    private func handleTransition(id: String, status: String, items: Int) {
        defer { taskStatuses[id] = status }
        guard hasBaseline,
              let previous = taskStatuses[id],
              previous == "queued" || previous == "running"
        else { return }
        switch status {
        case "completed":
            onNotify?("同步完成", "共 \(items) 条")
        case "partial":
            onNotify?("同步部分完成", "共 \(items) 条，部分维度抓取失败")
        case "failed":
            onNotify?("同步失败", "可稍后在主窗口重试，或检查网络")
        default:
            break
        }
    }

    /// 富集 pending >0 → 0 跃迁通知（只在会话内见过 >0 才报）
    private func handlePending(_ pending: Int) {
        defer { lastPending = pending }
        guard hasBaseline,
              let last = lastPending, last > 0, pending == 0
        else { return }
        onNotify?("条目详情拉取完成", "条目元信息富集已全部落库")
    }

    private func updateStatusLine(running: Bool, percent: Int?, pending: Int) {
        let line: String
        if running {
            line = percent.map { "同步中 \($0)%" } ?? "同步中…"
        } else if pending > 0 {
            line = "详情拉取中 \(pending) 条"
        } else {
            line = "空闲"
        }
        guard line != lastStatusLine else { return }
        lastStatusLine = line
        onStatusLine?(line)
    }

    // MARK: - HTTP 小件

    private func refreshUsers() async {
        guard let payload = await getJSON("/users") as? [String: Any],
              let list = payload["users"] as? [[String: Any]]
        else { return }
        users = list.compactMap { $0["uid"] as? String }
    }

    private func endpoint(_ path: String, query: String? = nil) -> URL? {
        var components = URLComponents()
        components.scheme = "http"
        components.host = host
        components.port = port
        components.path = "/api/v1" + path
        components.query = query
        return components.url
    }

    /// JSONSerialization 宽松解析（字段按需取，后端加字段零影响）
    private func getJSON(_ path: String, query: String? = nil) async -> Any? {
        guard let url = endpoint(path, query: query) else { return nil }
        do {
            let (data, _) = try await URLSession.shared.data(from: url)
            return try? JSONSerialization.jsonObject(with: data)
        } catch {
            return nil
        }
    }
}

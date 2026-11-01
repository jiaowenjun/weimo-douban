// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "WeimoDoubanMac",
    platforms: [.macOS(.v13)],
    targets: [
        // 零第三方依赖：AppKit / WKWebKit / UserNotifications / ServiceManagement 均系统框架
        .executableTarget(name: "WeimoDoubanMac", path: "Sources/WeimoDoubanMac")
    ]
)

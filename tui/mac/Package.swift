// swift-tools-version:5.9
// PVEStats Terminal: the terminal dashboard in a window of its own on the
// Mac, so the Dock, the app switcher and the menu bar show PVEStats and its
// icon. The window is a SwiftTerm terminal running tui's pvestats-tui.
// Built by tui/make-mac-app; Package.resolved pins SwiftTerm.
import PackageDescription

let package = Package(
    name: "PVEStatsTerminal",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/migueldeicaza/SwiftTerm", from: "1.2.0"),
    ],
    targets: [
        .executableTarget(name: "PVEStatsTerminal", dependencies: ["SwiftTerm"], path: "Sources"),
    ]
)

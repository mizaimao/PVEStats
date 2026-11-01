// swift-tools-version:5.9
// PVEStats Terminal: the terminal dashboard in a window of its own on the
// Mac, so the Dock, the app switcher and the menu bar show PVEStats and its
// icon. The window is the one every terminal app of Frank's has: TermKit,
// from the moose-term repo beside this one (~/Projects/moose-term), which
// brings SwiftTerm and pins it. Built by tui/make-mac-app.
import PackageDescription

let package = Package(
    name: "PVEStatsTerminal",
    platforms: [.macOS(.v14)],
    dependencies: [
        .package(path: "../../../moose-term"),
    ],
    targets: [
        .executableTarget(
            name: "PVEStatsTerminal",
            dependencies: [.product(name: "TermKit", package: "moose-term")],
            path: "Sources"
        ),
    ]
)

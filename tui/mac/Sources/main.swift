// The window: a SwiftTerm terminal running the dashboard that sits beside
// this program in the app's Resources, on the repo the app was built from
// (Info.plist, PVEStatsRoot). The dashboard redraws itself whenever the
// window is resized; quitting it (q or Esc) quits the app, and quitting the
// app (Cmd-Q, or closing the window) ends the dashboard.

import AppKit
import SwiftTerm

/// Columns and rows the window opens at: the dashboard's full layout.
let openCols = 190
let openRows = 50

final class AppDelegate: NSObject, NSApplicationDelegate, LocalProcessTerminalViewDelegate {
    private var window: NSWindow!
    private var terminal: LocalProcessTerminalView!

    func applicationDidFinishLaunching(_ notification: Notification) {
        let resources = Bundle.main.resourceURL ?? URL(fileURLWithPath: ".")
        let dashboard = resources.appendingPathComponent("pvestats-tui").path
        let root = Bundle.main.object(forInfoDictionaryKey: "PVEStatsRoot") as? String ?? ""

        terminal = LocalProcessTerminalView(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        terminal.font = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
        terminal.processDelegate = self
        terminal.resize(cols: openCols, rows: openRows)
        let size = terminal.getOptimalFrameSize().size

        window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "PVEStats"
        window.contentView = terminal
        window.center()
        window.setFrameAutosaveName("PVEStats dashboard")
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(terminal)

        // 24-bit colour, which SwiftTerm draws: the dashboard picks its
        // full palette from COLORTERM.
        var environment = Terminal.getEnvironmentVariables(termName: "xterm-256color")
        environment.append("COLORTERM=truecolor")
        terminal.startProcess(
            executable: dashboard,
            args: ["--root", root],
            environment: environment,
            execName: "pvestats-tui"
        )
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationWillTerminate(_ notification: Notification) {
        terminal?.terminate()
    }

    func processTerminated(source: TerminalView, exitCode: Int32?) {
        NSApp.terminate(nil)
    }

    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}
    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {}
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
}

/// The app menu, so Cmd-Q and Cmd-W work, named after the app.
@MainActor
func mainMenu() -> NSMenu {
    let menu = NSMenu()
    let appItem = NSMenuItem()
    menu.addItem(appItem)
    let appMenu = NSMenu()
    appMenu.addItem(withTitle: "Hide PVEStats", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
    appMenu.addItem(.separator())
    appMenu.addItem(withTitle: "Quit PVEStats", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    appItem.submenu = appMenu
    let windowItem = NSMenuItem()
    menu.addItem(windowItem)
    let windowMenu = NSMenu(title: "Window")
    windowMenu.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
    windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
    windowItem.submenu = windowMenu
    return menu
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    app.mainMenu = mainMenu()
    app.run()
}

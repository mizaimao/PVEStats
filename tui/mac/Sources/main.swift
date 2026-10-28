// The window: a SwiftTerm terminal running the dashboard that sits beside
// this program in the app's Resources, on the repo the app was built from
// (Info.plist, PVEStatsRoot). The dashboard redraws itself whenever the
// window is resized; quitting it (q or Esc) quits the app, and quitting the
// app (Cmd-Q, or closing the window) ends the dashboard. The window has no
// title bar of its own: the dashboard's top row runs through it, beside the
// window's buttons. The Theme menu saves the chosen theme in the repo's
// ui.json, where the dashboard takes it up within a second.

import AppKit
import SwiftTerm

/// Columns and rows the window opens at: the dashboard's full layout.
let openCols = 190
let openRows = 50

/// The terminal, saying when the dashboard gives it a background colour
/// (OSC 11), so the window around the grid can take it too.
final class DashboardView: LocalProcessTerminalView {
    var onBackground: ((NSColor) -> Void)?
    private var shown: NSColor?

    override func viewWillDraw() {
        super.viewWillDraw()
        let color = nativeBackgroundColor
        if color != shown {
            shown = color
            onBackground?(color)
        }
    }
}

/// Over the dashboard where a title bar would be: a press there moves the
/// window, and a double click does what a title bar's does, rather than
/// selecting the text under it.
final class TitleStrip: NSView {
    override var mouseDownCanMoveWindow: Bool { true }

    override func mouseDown(with event: NSEvent) {
        guard let window else { return }
        guard event.clickCount == 2 else {
            window.performDrag(with: event)
            return
        }
        switch UserDefaults.standard.string(forKey: "AppleActionOnDoubleClick") {
        case "Minimize": window.performMiniaturize(nil)
        case "None": break
        default: window.performZoom(nil)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, LocalProcessTerminalViewDelegate {
    private var window: NSWindow!
    private var terminal: DashboardView!
    private let dashboard = (Bundle.main.resourceURL ?? URL(fileURLWithPath: "."))
        .appendingPathComponent("pvestats-tui").path
    private let root = Bundle.main.object(forInfoDictionaryKey: "PVEStatsRoot") as? String ?? ""

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = mainMenu(themes: themeMenu())

        terminal = DashboardView(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        terminal.font = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
        terminal.processDelegate = self
        // One cell's size, from the grid's at two widths.
        terminal.resize(cols: openCols + 10, rows: openRows)
        let wider = terminal.getOptimalFrameSize().size
        terminal.resize(cols: openCols, rows: openRows)
        let grid = terminal.getOptimalFrameSize().size
        let cell = NSSize(width: (wider.width - grid.width) / 10, height: grid.height / CGFloat(openRows))

        // No title bar of its own: the dashboard's top row runs through it,
        // beside the window's buttons.
        window = NSWindow(
            contentRect: NSRect(origin: .zero, size: grid),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "PVEStats"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .none
        let (top, corner) = buttonRoom(cell: cell)

        // The grid starts low enough for its top row to sit level with the
        // buttons; the strip above it shows the window's background, which
        // follows the dashboard's.
        let content = NSView(frame: NSRect(x: 0, y: 0, width: grid.width, height: top + grid.height))
        terminal.frame = NSRect(x: 0, y: 0, width: grid.width, height: grid.height)
        terminal.autoresizingMask = [.width, .height]
        content.addSubview(terminal)
        // The title bar's height, or at least the top row's, kept for moving
        // the window.
        let bar = max(window.frame.height - window.contentLayoutRect.height, top + cell.height)
        let strip = TitleStrip(frame: NSRect(x: 0, y: content.frame.height - bar, width: grid.width, height: bar))
        strip.autoresizingMask = [.width, .minYMargin]
        content.addSubview(strip)
        window.contentView = content
        window.backgroundColor = terminal.nativeBackgroundColor
        terminal.onBackground = { [weak self] color in self?.window.backgroundColor = color }
        window.setFrame(NSRect(origin: .zero, size: content.frame.size), display: false)
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
            args: ["--root", root, "--titlebar", String(corner)],
            environment: environment,
            execName: "pvestats-tui"
        )
        NSApp.activate(ignoringOtherApps: true)
    }

    /// How far down the grid starts, for its top row to be level with the
    /// window's buttons, and how many of that row's columns the buttons cover.
    private func buttonRoom(cell: NSSize) -> (top: CGFloat, corner: Int) {
        window.layoutIfNeeded()
        let buttons = [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton]
            .compactMap { window.standardWindowButton($0) }
            .map { $0.convert($0.bounds, to: nil) }
        guard let first = buttons.first, let right = buttons.map(\.maxX).max(), right > 0 else {
            return (0, 0)
        }
        let middle = window.frame.height - first.midY
        let top = max(0, (middle - cell.height / 2).rounded())
        // A little space after the last button.
        return (top, Int(((right + 6) / cell.width).rounded(.up)))
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

    // ---------------------------------------------------------------- themes

    /// The dashboard's themes and which is in use, from the dashboard itself
    /// (pvestats-tui --themes), so the menu never lists one it lacks.
    private func themes() -> [(name: String, current: Bool)] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: dashboard)
        process.arguments = ["--root", root, "--themes"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return [] }
        let output = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: output, as: UTF8.self).split(separator: "\n").compactMap { line in
            let name = line.dropFirst(2).trimmingCharacters(in: .whitespaces)
            return name.isEmpty ? nil : (name, line.hasPrefix("*"))
        }
    }

    private func themeMenu() -> NSMenu {
        let menu = NSMenu(title: "Theme")
        for theme in themes() {
            let item = NSMenuItem(title: theme.name, action: #selector(chooseTheme(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = theme.name
            item.state = theme.current ? .on : .off
            menu.addItem(item)
        }
        return menu
    }

    /// Saves the theme in ui.json, keeping whatever else is there (the
    /// browser app's settings); the dashboard takes it up within a second.
    @objc private func chooseTheme(_ sender: NSMenuItem) {
        guard let name = sender.representedObject as? String else { return }
        let file = URL(fileURLWithPath: root).appendingPathComponent("ui.json")
        var saved: [String: Any] = [:]
        if let data = try? Data(contentsOf: file),
           let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] {
            saved = object
        }
        saved["theme"] = name
        guard let data = try? JSONSerialization.data(withJSONObject: saved, options: [.prettyPrinted, .sortedKeys]),
              (try? data.write(to: file, options: .atomic)) != nil
        else {
            NSSound.beep()
            return
        }
        for item in sender.menu?.items ?? [] {
            item.state = item === sender ? .on : .off
        }
    }
}

/// The app menu, so Cmd-Q and Cmd-W work, named after the app, then the
/// themes and the window.
@MainActor
func mainMenu(themes: NSMenu) -> NSMenu {
    let menu = NSMenu()
    let appItem = NSMenuItem()
    menu.addItem(appItem)
    let appMenu = NSMenu()
    appMenu.addItem(withTitle: "Hide PVEStats", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
    appMenu.addItem(.separator())
    appMenu.addItem(withTitle: "Quit PVEStats", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    appItem.submenu = appMenu
    let themeItem = NSMenuItem()
    menu.addItem(themeItem)
    themeItem.submenu = themes
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
    app.run()
}

// The window: a SwiftTerm terminal running the dashboard that sits beside
// this program in the app's Resources, on the repo the app was built from
// (Info.plist, PVEStatsRoot). The window is TermKit's (moose-term), as in
// every terminal app of Frank's. The dashboard's top row runs through the
// title bar, level with the window's buttons; it takes no mouse input, so a
// press anywhere moves the window. It redraws itself whenever the window is
// resized and draws nothing while the window can't be seen; quitting it (q
// or Esc) quits the app, and quitting the app (Cmd-Q, or closing the window)
// ends the dashboard. The Theme menu saves the chosen theme in the repo's
// ui.json, where the dashboard takes it up within a second.

import AppKit
import TermKit

/// Columns and rows the window opens at: the dashboard's full layout.
let openCols = 190
let openRows = 50

final class AppDelegate: NSObject, NSApplicationDelegate, LocalProcessTerminalViewDelegate {
    private var term: TermWindow!
    private var terminal: TermView!
    private let dashboard = (Bundle.main.resourceURL ?? URL(fileURLWithPath: "."))
        .appendingPathComponent("pvestats-tui").path
    private let root = Bundle.main.object(forInfoDictionaryKey: "PVEStatsRoot") as? String ?? ""

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = mainMenu(app: "PVEStats", edit: [], menus: [themeMenu()])

        terminal = TermView(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        terminal.processDelegate = self
        // Its own font, the system's monospaced one; the dashboard paints
        // every cell.
        term = TermWindow(title: "PVEStats", terminal: terminal, look: GhosttyLook(), cols: openCols, rows: openRows,
                          layout: .besideButtons)
        term.dragsFromAnywhere = true
        term.reportsShown = true
        term.show(saveAs: "PVEStats dashboard")

        // 24-bit colour, which SwiftTerm draws: the dashboard picks its
        // full palette from COLORTERM.
        var environment = Terminal.getEnvironmentVariables(termName: "xterm-256color")
        environment.append("COLORTERM=truecolor")
        terminal.startProcess(
            executable: dashboard,
            args: ["--root", root, "--titlebar", String(term.corner)],
            environment: environment,
            execName: "pvestats-tui"
        )
        term.started()
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
    /// browser app's settings); the dashboard takes it up within a second,
    /// and the strip above it follows.
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
        term.followBackgroundSoon()
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    app.run()
}

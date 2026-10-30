#!/usr/bin/env python3
"""PVEStats as a desktop window.

    .venv/bin/python app.py

Starts the dashboard program (tui/, Rust) with --serve, which polls the
machines and serves the page, and shows that page in a native webview:
WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux. No bundled
browser engine.

Closing the window stops the dashboard program. Nothing is left running, and
nothing is installed as a service.

For an ordinary browser with no window, run the program yourself:
    tui/target/release/pvestats-tui --serve
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
# The terminal dashboard's program, built for this Mac by tui/make-mac-app
# (or `cargo build --release` in tui/). With --serve it draws nothing and
# serves the page from the same sources the terminal draws.
DASHBOARD = ROOT / "tui" / "target" / "release" / "pvestats-tui"


def die(message):
    """Exit with a message that is seen however the app was started.

    From PVEStats.app there is no Terminal: stderr goes nowhere and the app
    simply never opens. So, like launch.command, say it in a dialog then.
    """
    print(message, file=sys.stderr)
    if sys.platform == "darwin" and not (sys.stdin and sys.stdin.isatty()):
        try:
            subprocess.run(
                ["osascript", "-e", "on run argv",
                 "-e", 'display alert "PVEStats could not start" '
                       "message (item 1 of argv) as critical",
                 "-e", "end run", message],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except OSError:
            pass
    sys.exit(1)


class Dashboard:
    """pvestats-tui --serve as a child process. It exits with the window:
    stop() ends it, and the program also exits when this process is gone."""

    def __init__(self):
        # stderr carries one line once the page is being served
        # ("dashboard  http://127.0.0.1:8080"), or why it could not start.
        self.process = subprocess.Popen(
            [str(DASHBOARD), "--serve", "--root", str(ROOT)],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, text=True)

    def url(self):
        """Where the page is, once it is served; or exit saying why not."""
        first = self.process.stderr.readline().strip()
        if first.startswith("dashboard "):
            return first.split()[-1] + "/"
        rest = self.process.stderr.read().strip()
        self.stop()
        die("\n".join(filter(None, [first, rest]))
            or f"pvestats-tui exited with {self.process.returncode}")

    def stop(self):
        if self.process.poll() is not None:
            return
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()


def keep_awake():
    """Opt out of macOS App Nap.

    An unfocused or occluded app gets throttled — timers stretch and threads
    are descheduled — which stalls polling and leaves real gaps in the
    charts, not just a stale page. The returned token must stay referenced
    for the activity to hold.

    Deliberately `AllowingIdleSystemSleep`: this keeps the process scheduled,
    it does not keep the Mac awake.
    """
    try:
        from Foundation import (NSActivityUserInitiatedAllowingIdleSystemSleep,
                                NSProcessInfo)
    except ImportError:
        return None        # not macOS, or pyobjc absent
    return NSProcessInfo.processInfo().beginActivityWithOptions_reason_(
        NSActivityUserInitiatedAllowingIdleSystemSleep,
        "PVEStats metric collection")


def fit_to_screen(webview, want_width, want_height):
    """Clamp the preferred window size to the largest attached display.

    Asking for 1230px of height on a 1080p panel would open a window taller
    than the screen, which macOS silently crops.
    """
    try:
        screens = webview.screens
        width = max(s.width for s in screens)
        height = max(s.height for s in screens)
    except Exception:
        return want_width, want_height
    return min(want_width, int(width * 0.94)), min(want_height, int(height * 0.92))


def main():
    try:
        import webview
    except ImportError:
        die("pywebview is not installed.\n"
            "  tools/uv sync\n"
            "Or run tui/target/release/pvestats-tui --serve and open its page "
            "in a browser.")
    if not DASHBOARD.is_file():
        die(f"No dashboard program at {DASHBOARD}.\n"
            "Build it with tui/make-mac-app.")

    # Held for the process lifetime; dropping it re-enables App Nap.
    activity = keep_awake()      # noqa: F841
    dashboard = Dashboard()
    url = dashboard.url()

    # Measured: the full layout is ~1160px tall between 1280 and 1500px wide,
    # and grows to ~1280px below that as columns collapse. Open large enough to
    # show everything without a scrollbar, but never larger than the display.
    #
    # Width is three quarters of that measured 1500: the two-column row still
    # holds at 1125px, and a window that does not dominate the screen is easier
    # to leave open alongside other work — which is how this gets used.
    width, height = fit_to_screen(webview, 1125, 1230)

    webview.create_window(
        "PVEStats", url,
        width=width, height=height,
        # Hard floor for drag-resizing. 700px still shows both columns — the
        # layout compresses rather than dropping panels down to ~660px.
        min_size=(700, 640),
        background_color="#0d0d0d",   # matches the page, so no white flash
    )
    try:
        webview.start()          # blocks until the window is closed
    finally:
        dashboard.stop()


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        # Anything unforeseen still reaches a dialog rather than a lost
        # traceback.
        die(f"PVEStats stopped: {type(exc).__name__}: {exc}")

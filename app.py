#!/usr/bin/env python3
"""PVEStats as a desktop window.

    .venv/bin/python app.py

Runs the collector and server in a background thread and shows the dashboard
in a native webview — WKWebView on macOS, WebView2 on Windows, WebKitGTK on
Linux. No bundled browser engine.

Closing the window stops the collector. Nothing is left running, and nothing
is installed as a service.

Use run.py instead to serve to an ordinary browser with no window.
"""

import asyncio
import errno
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

from aiohttp import web

from pvestats.collector import Collector
from pvestats.config import Config
from pvestats.server import build_app

HOST = "127.0.0.1"
STARTUP_TIMEOUT = 15


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


class Backend:
    """The collector and HTTP server, on their own event loop in a thread."""

    def __init__(self, config):
        self.config = config
        self.loop = asyncio.new_event_loop()
        self.collector = None
        self.runner = None
        self.ready = threading.Event()
        self.error = None

    def start(self):
        threading.Thread(target=self._run, daemon=True, name="backend").start()

    def _run(self):
        asyncio.set_event_loop(self.loop)
        try:
            self.loop.run_until_complete(self._serve())
            self.loop.run_forever()
        except Exception as exc:
            self.error = exc
            self.ready.set()

    async def _serve(self):
        self.collector = Collector(self.config)
        await self.collector.start()
        self.runner = web.AppRunner(build_app(self.collector), access_log=None)
        await self.runner.setup()
        await web.TCPSite(self.runner, HOST, self.config.port).start()
        self.ready.set()

    def stop(self):
        """Shut the server and collector down from outside the loop."""
        if not self.loop.is_running():
            return
        future = asyncio.run_coroutine_threadsafe(self._shutdown(), self.loop)
        try:
            future.result(timeout=10)
        except Exception:
            pass
        self.loop.call_soon_threadsafe(self.loop.stop)

    async def _shutdown(self):
        if self.runner is not None:
            await self.runner.cleanup()
        if self.collector is not None:
            await self.collector.stop()


def wait_for_server(url, deadline):
    """Block until the server answers, so the window never opens on an error.

    Asked directly, never through a proxy: urllib honours the system proxy
    settings, and a proxy cannot reach this Mac's loopback, so the app quit
    with "did not come up" while the server was running.
    """
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    while time.monotonic() < deadline:
        try:
            with opener.open(url, timeout=1):
                return True
        except (urllib.error.URLError, OSError):
            time.sleep(0.2)
    return False


def keep_awake():
    """Opt out of macOS App Nap.

    An unfocused or occluded app gets throttled — timers stretch and threads
    are descheduled — which stalls the collector and leaves real gaps in the
    ring buffers, not just a stale page. The returned token must stay
    referenced for the activity to hold.

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
            "Or use run.py to serve to an ordinary browser instead.")

    try:
        config = Config()
    except Exception as exc:
        # config.yaml and .env are edited by hand; a typo stops here.
        die(f"Could not read the configuration (config.yaml or .env): "
            f"{type(exc).__name__}: {exc}")
    # Held for the process lifetime; dropping it re-enables App Nap.
    activity = keep_awake()      # noqa: F841
    backend = Backend(config)
    backend.start()

    url = f"http://{HOST}:{config.port}/"
    backend.ready.wait(timeout=STARTUP_TIMEOUT)
    error = backend.error
    if isinstance(error, OSError) and error.errno == errno.EADDRINUSE:
        die(f"Port {config.port} is already in use. PVEStats may already be "
            f"running; check with:\n  lsof -nP -iTCP:{config.port} -sTCP:LISTEN")
    if error is not None:
        die(f"The collector failed to start: {type(error).__name__}: {error}")
    if not wait_for_server(url, time.monotonic() + STARTUP_TIMEOUT):
        backend.stop()
        die(f"The dashboard server did not come up on {url}")

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
        backend.stop()


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        # Anything unforeseen still reaches a dialog rather than a lost
        # traceback.
        die(f"PVEStats stopped: {type(exc).__name__}: {exc}")

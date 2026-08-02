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
    """Block until the server answers, so the window never opens on an error."""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1):
                return True
        except (urllib.error.URLError, OSError):
            time.sleep(0.2)
    return False


def main():
    try:
        import webview
    except ImportError:
        sys.exit(
            "pywebview is not installed.\n"
            "  .venv/bin/pip install -r requirements.txt\n"
            "Or use run.py to serve to an ordinary browser instead."
        )

    config = Config()
    backend = Backend(config)
    backend.start()

    url = f"http://{HOST}:{config.port}/"
    backend.ready.wait(timeout=STARTUP_TIMEOUT)
    if backend.error is not None:
        sys.exit(f"collector failed to start: {backend.error}")
    if not wait_for_server(url, time.monotonic() + STARTUP_TIMEOUT):
        backend.stop()
        sys.exit(f"server did not come up on {url}")

    webview.create_window(
        "PVEStats", url,
        width=1500, height=1000, min_size=(760, 600),
        background_color="#0d0d0d",   # matches the page, so no white flash
    )
    try:
        webview.start()          # blocks until the window is closed
    finally:
        backend.stop()


if __name__ == "__main__":
    main()

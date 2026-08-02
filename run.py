#!/usr/bin/env python3
"""Start the collector and the local dashboard server.

    .venv/bin/python run.py

Binds 127.0.0.1 only. Nothing is written to disk.
"""

import asyncio

from aiohttp import web

from pvestats.collector import Collector
from pvestats.config import Config
from pvestats.server import build_app

HOST = "127.0.0.1"


async def main():
    config = Config()
    collector = Collector(config)
    await collector.start()

    runner = web.AppRunner(build_app(collector), access_log=None)
    await runner.setup()
    site = web.TCPSite(runner, HOST, config.port)
    await site.start()

    # flush=True so the banner appears immediately even when redirected to a
    # log rather than a terminal.
    print(f"dashboard  http://{HOST}:{config.port}", flush=True)
    print(f"state      http://{HOST}:{config.port}/api/state", flush=True)
    for name, source in collector.sources.items():
        status = "configured" if not source.error else source.error
        print(f"  {name:<8} {status}", flush=True)
    print("ctrl-c to stop", flush=True)

    try:
        await asyncio.Event().wait()
    finally:
        await runner.cleanup()
        await collector.stop()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass

"""Local HTTP server. Binds 127.0.0.1 only; no authentication (spec §5).

Serves the static page and the collector's state. Credentials live in the
collector and never reach the browser — the page only ever talks to localhost.
"""

from pathlib import Path

import aiohttp
from aiohttp import web

from .sources import PVE_TIMEOUT

STATIC = Path(__file__).resolve().parent.parent / "static"

# rrddata accepts a fixed set of timeframes. Whitelisted rather than passed
# through, so nothing from a query string reaches the PVE API unchecked.
TIMEFRAMES = {"hour", "day", "week", "month", "year"}


async def handle_state(request):
    return web.json_response(
        request.app["collector"].snapshot(),
        headers={"Cache-Control": "no-store"},
    )


async def handle_history(request):
    """Proxy PVE's rrddata for one guest.

    Fetched on demand when a card is expanded, never on the polling loop
    (spec §3.1). This data already exists on the PVE host, so reading it adds
    no writes.
    """
    collector = request.app["collector"]
    config = collector.config

    try:
        vmid = int(request.match_info["vmid"])
    except ValueError:
        raise web.HTTPBadRequest(text="vmid must be an integer")

    timeframe = request.query.get("timeframe", "hour")
    if timeframe not in TIMEFRAMES:
        raise web.HTTPBadRequest(
            text=f"timeframe must be one of {', '.join(sorted(TIMEFRAMES))}")

    pve = collector.sources["pve"]
    guest = (pve.data.get("guests") or {}).get(vmid)
    node = (pve.data.get("node") or {}).get("name")
    if guest is None or not node:
        raise web.HTTPNotFound(text=f"vmid {vmid} is not a running guest")

    url = (f"{config.pve_base}/nodes/{node}/{guest['type']}/{vmid}/rrddata"
           f"?timeframe={timeframe}&cf=AVERAGE")
    headers = {"Authorization": config.pve_auth_header}
    try:
        async with collector.session.get(url, headers=headers, ssl=False,
                                         timeout=PVE_TIMEOUT) as response:
            response.raise_for_status()
            payload = await response.json()
    except Exception as exc:
        raise web.HTTPBadGateway(text=f"{type(exc).__name__}: {exc}")

    return web.json_response(
        {"vmid": vmid, "timeframe": timeframe, "data": payload.get("data") or []},
        headers={"Cache-Control": "no-store"},
    )


async def handle_ui_get(request):
    return web.json_response(request.app["collector"].config.ui(),
                             headers={"Cache-Control": "no-store"})


async def handle_ui_post(request):
    """Save appearance settings.

    This is the one write the dashboard performs, and it lands on the Mac —
    never on a homelab disk (spec §3). It changes nothing about any monitored
    machine, so the read-only guarantee is intact.
    """
    try:
        changes = await request.json()
    except Exception:
        raise web.HTTPBadRequest(text="expected a JSON object")
    if not isinstance(changes, dict):
        raise web.HTTPBadRequest(text="expected a JSON object")
    collector = request.app["collector"]
    try:
        saved = collector.config.save_ui(changes)
    except OSError as exc:
        raise web.HTTPInternalServerError(text=f"could not save: {exc}")
    # The history window is the one appearance setting with a server-side
    # effect: it re-lengths the ring buffers the charts read from.
    if "window" in changes:
        collector.set_history_window(saved.get("window", 180))
    return web.json_response(saved)


async def handle_index(request):
    return web.FileResponse(STATIC / "index.html",
                            headers={"Cache-Control": "no-store"})


def build_app(collector):
    app = web.Application()
    app["collector"] = collector
    app.router.add_get("/", handle_index)
    app.router.add_get("/api/state", handle_state)
    app.router.add_get("/api/ui", handle_ui_get)
    app.router.add_post("/api/ui", handle_ui_post)
    app.router.add_get("/api/history/{vmid}", handle_history)
    app.router.add_static("/static/", STATIC)
    return app

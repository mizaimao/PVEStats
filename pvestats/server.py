"""Local HTTP server. Binds 127.0.0.1 only; no authentication (spec §5).

Serves the static page and the collector's state. Credentials live in the
collector and never reach the browser — the page only ever talks to localhost.
"""

import html
from pathlib import Path

import aiohttp
from aiohttp import web

from .config import UI_DEFAULTS, clean_ui
from .sources import PVE_TIMEOUT

# Unloading blocks until any in-flight generation finishes, so it needs far
# longer than a poll. Matches the agent's own ceiling.
UNLOAD_TIMEOUT = aiohttp.ClientTimeout(total=35)

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
    # Refused before anything is written: a bad window used to be saved and
    # then stop every later launch.
    bad = sorted(k for k in changes if k in UI_DEFAULTS and k not in clean_ui(changes))
    if bad:
        raise web.HTTPBadRequest(text=f"invalid value for {', '.join(bad)}")
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


async def handle_unload(request):
    """Relay one "stop this model" request to the ml60 agent.

    The browser cannot call the agent directly — it is on another host, and
    the page has no network access beyond localhost — so this proxies it.
    Only the model name crosses; the agent decides whether to honour it.

    This is the project's only request that changes anything on a monitored
    machine. It is confined to Ollama's own memory management: the model is
    dropped from VRAM and reloads on the next request to it. Nothing is
    written to ml60's disk, no configuration changes, and it goes through the
    agent that is already there rather than opening any new path in.
    """
    # Every answer is JSON in the agent's own shape, failures included: the
    # Stop button reads the reason from `error`, and a plain-text body left
    # it with nothing but "HTTP 502".
    def failed(status, message):
        return web.json_response({"ok": False, "error": message}, status=status)

    config = request.app["collector"].config
    if not config.agent_configured:
        return failed(503, "no ml60 agent configured")
    try:
        name = (await request.json()).get("model")
    except Exception:
        return failed(400, "expected a JSON object")
    if not isinstance(name, str) or not name.strip():
        return failed(400, "no model named")

    url = config.agent_url.rstrip("/") + "/unload"
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(url, json={"model": name.strip()},
                                    timeout=UNLOAD_TIMEOUT,
                                    ssl=config.agent_ssl) as response:
                payload = await response.json()
                status = response.status
    except Exception as exc:
        return failed(502, f"{type(exc).__name__}: {exc}")
    return web.json_response(payload, status=200 if status == 200 else 502)


async def handle_index(request):
    """The page, with the saved appearance already on its <html> element.

    pywebview's private mode wipes the page's storage at every launch, so the
    first-paint cache in theme.js is always empty in the app: each launch
    painted the default theme and then faded to the saved one. Rendered here,
    the first paint is right with no client storage at all.
    """
    ui = request.app["collector"].config.ui()
    attrs = [f'data-theme="{html.escape(str(ui["theme"]), quote=True)}"',
             f'style="--glow-scale: {max(0.0, min(3.0, float(ui["glow"])))}"']
    attrs += [f'data-{name}="on"' for name in ("scanlines", "grain", "aurora") if ui.get(name)]
    page = (STATIC / "index.html").read_text().replace(
        '<html lang="en">', f'<html lang="en" {" ".join(attrs)}>', 1)
    return web.Response(text=page, content_type="text/html",
                        headers={"Cache-Control": "no-store"})


@web.middleware
async def own_page_only(request, handler):
    """Refuse requests that did not come from this dashboard's own page.

    Binding 127.0.0.1 keeps other machines out, but a browser on this Mac will
    carry a request here on behalf of any site it has open. Three checks:

    - Host must name this server. A site that points its own domain at
      127.0.0.1 still sends its own name here, so it cannot read the state.
    - Browsers say where every request comes from (Sec-Fetch-Site). A
      cross-site GET cannot read the reply, but each /api/history call still
      spends the PVE token, so anything from another site is refused.
    - Writes must come from this origin and carry JSON. Browsers send Origin
      on every cross-site POST, and a JSON body cannot go cross-site without
      a preflight, which this server never answers.

    A request with neither header is a local process (curl, a script, app.py's
    own readiness check), which could reach anything on this Mac anyway.
    """
    hosts = request.app["hosts"]
    if request.host not in hosts:
        raise web.HTTPForbidden(text="unexpected Host header")
    site = request.headers.get("Sec-Fetch-Site")
    if site is not None and site not in ("same-origin", "none"):
        raise web.HTTPForbidden(text="cross-site request refused")
    if request.method == "POST":
        origin = request.headers.get("Origin")
        if origin is not None and origin not in {f"http://{h}" for h in hosts}:
            raise web.HTTPForbidden(text="cross-origin request refused")
        if request.content_type != "application/json":
            raise web.HTTPUnsupportedMediaType(text="expected application/json")
    return await handler(request)


async def response_headers(request, response):
    # Never inside another page: a hidden frame could turn two clicks on a
    # lure into Stop and Confirm. The window loads the page top-level.
    response.headers["X-Frame-Options"] = "DENY"
    response.headers["Content-Security-Policy"] = "frame-ancestors 'none'"
    # The page's modules revalidate on every load (cheap: they carry an
    # ETag), so a browser never runs an update against stale scripts.
    if request.path.startswith("/static/"):
        response.headers["Cache-Control"] = "no-cache"


def build_app(collector):
    app = web.Application(middlewares=[own_page_only])
    port = collector.config.port
    app["hosts"] = {f"127.0.0.1:{port}", f"localhost:{port}"}
    if port == 80:
        # Browsers leave the default port out of Host and Origin.
        app["hosts"] |= {"127.0.0.1", "localhost"}
    app.on_response_prepare.append(response_headers)
    app["collector"] = collector
    app.router.add_get("/", handle_index)
    app.router.add_get("/api/state", handle_state)
    app.router.add_get("/api/ui", handle_ui_get)
    app.router.add_post("/api/ui", handle_ui_post)
    app.router.add_get("/api/history/{vmid}", handle_history)
    app.router.add_post("/api/agent/unload", handle_unload)
    app.router.add_static("/static/", STATIC)
    return app

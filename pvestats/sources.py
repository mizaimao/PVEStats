"""One independent poller per data source.

Each runs on its own interval and never blocks the others. A source that fails
sets its own `ok: false` and error string; the rest of the dashboard keeps
working (spec §2). Every outbound call has a timeout (spec §7).

All field handling here was written against captures/, not against the field
names in the design document.
"""

import asyncio
import json
import time

import aiohttp

from .state import RateTracker

PVE_INTERVAL = 10.0     # pvestatd recomputes every 10s; polling faster returns
                        # identical numbers (spec §3.1). Hard property, not a
                        # tuning choice.
AGENT_INTERVAL = 1.0    # NVML calls are microseconds; genuinely real-time.
TRUENAS_INTERVAL = 5.0

PVE_TIMEOUT = aiohttp.ClientTimeout(total=5)
AGENT_TIMEOUT = aiohttp.ClientTimeout(total=2)
TRUENAS_TIMEOUT = aiohttp.ClientTimeout(total=5)


async def run_poller(source, interval, poll):
    """Call `poll` forever on a fixed interval, absorbing every failure."""
    while True:
        started = time.monotonic()
        try:
            await poll()
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            source.fail(f"timeout after {interval}s")
        except Exception as exc:
            source.fail(f"{type(exc).__name__}: {exc}")
        # Drift-free: subtract however long the poll actually took.
        await asyncio.sleep(max(0.0, interval - (time.monotonic() - started)))


# --------------------------------------------------------------------------
# Proxmox VE
# --------------------------------------------------------------------------

class PveSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source
        self.rates = RateTracker()
        self.node = config.pve_node

    async def poll(self, session):
        headers = {"Authorization": self.config.pve_auth_header}
        base = self.config.pve_base
        # ssl=False: the PVE certificate is self-signed. Not a thing to fix
        # (spec §3.1).
        kwargs = {"headers": headers, "ssl": False, "timeout": PVE_TIMEOUT}

        if not self.node:
            async with session.get(f"{base}/nodes", **kwargs) as response:
                response.raise_for_status()
                nodes = (await response.json()).get("data") or []
            if not nodes:
                raise RuntimeError("no nodes returned")
            self.node = nodes[0]["node"]

        async with session.get(f"{base}/cluster/resources?type=vm", **kwargs) as response:
            response.raise_for_status()
            resources = (await response.json()).get("data") or []

        async with session.get(f"{base}/nodes/{self.node}/status", **kwargs) as response:
            response.raise_for_status()
            node_status = (await response.json()).get("data") or {}

        now = time.monotonic()
        guests = {}
        # Running guests only. Stopped ones report nothing worth a card, and
        # their stale counters would pollute the rate tracker.
        running = [g for g in resources if g.get("status") == "running"
                   and not g.get("template")]

        for guest in running:
            vmid = guest.get("vmid")
            if vmid is None:
                continue
            key = f"{vmid}"
            cpu_fraction = guest.get("cpu")  # 0.0-1.0, NOT a percentage
            entry = {
                "vmid": vmid,
                "name": guest.get("name"),
                "type": guest.get("type"),
                "status": guest.get("status"),
                "uptime": guest.get("uptime"),
                "cpu_pct": cpu_fraction * 100 if cpu_fraction is not None else None,
                "maxcpu": guest.get("maxcpu"),
                "mem": guest.get("mem"),
                "maxmem": guest.get("maxmem"),
                "memhost": guest.get("memhost"),
                "view": self.config.view_for(vmid),
                # Cumulative counters converted to bytes/sec. None until the
                # second poll, since a rate needs two samples.
                "net_in_bps": self.rates.rate(f"{key}.netin", guest.get("netin"), now),
                "net_out_bps": self.rates.rate(f"{key}.netout", guest.get("netout"), now),
                "disk_read_bps": self.rates.rate(f"{key}.diskread", guest.get("diskread"), now),
                "disk_write_bps": self.rates.rate(f"{key}.diskwrite", guest.get("diskwrite"), now),
            }
            # maxdisk comes back as 0 for some guests, in which case there is
            # nothing meaningful to show. PVE reports allocation, never
            # in-guest usage (spec §3.1), so it is labelled accordingly.
            if guest.get("maxdisk"):
                entry["disk_allocated"] = guest.get("disk")
                entry["maxdisk"] = guest.get("maxdisk")
            guests[vmid] = entry
            self.source.push(f"cpu.{vmid}", entry["cpu_pct"])

        # Stop tracking counters for guests that stopped, so a later restart
        # does not diff against a months-old sample.
        live = {str(g["vmid"]) for g in running if g.get("vmid") is not None}
        for tracked in {k.split(".")[0] for k in self.rates._previous}:
            if tracked not in live:
                self.rates.forget(f"{tracked}.")

        memory = node_status.get("memory") or {}
        node_cpu = node_status.get("cpu")
        node = {
            "name": self.node,
            "cpu_pct": node_cpu * 100 if node_cpu is not None else None,
            "mem": memory.get("used"),
            "maxmem": memory.get("total"),
            "loadavg": [float(x) for x in (node_status.get("loadavg") or [])],
            "uptime": node_status.get("uptime"),
            "kversion": node_status.get("kversion"),
            "pveversion": node_status.get("pveversion"),
            "cores": (node_status.get("cpuinfo") or {}).get("cores"),
            "cpus": (node_status.get("cpuinfo") or {}).get("cpus"),
            "guests_running": len(running),
            "guests_total": len([g for g in resources if not g.get("template")]),
        }
        self.source.push("node.cpu", node["cpu_pct"])

        self.source.succeed({"node": node, "guests": guests})


# --------------------------------------------------------------------------
# ml60 agent
# --------------------------------------------------------------------------

class AgentSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source

    async def poll(self, session):
        async with session.get(self.config.agent_url, timeout=AGENT_TIMEOUT) as response:
            response.raise_for_status()
            payload = await response.json()

        gpu = payload.get("gpu")
        if gpu is None:
            # NVML failed on ml60 but Ollama data may still be present.
            raise RuntimeError(payload.get("error") or "agent reported no GPU")

        models = []
        for model in payload.get("models") or []:
            models.append({
                "name": model.get("name"),
                "size": model.get("size"),
                "size_vram": model.get("size_vram"),
                # Converted here so the browser never has to parse Ollama's
                # timestamp, which carries nanosecond precision and a local
                # offset rather than the documented Z form.
                "unload_in": _seconds_until(model.get("expires_at")),
            })

        procs = []
        for proc in gpu.get("procs") or []:
            name = proc.get("name") or "?"
            procs.append({
                "pid": proc.get("pid"),
                # Ollama's worker reports an absolute path; show the leaf.
                "name": name.rsplit("/", 1)[-1],
                "vram": proc.get("vram"),
            })

        self.source.push("gpu.util", gpu.get("util"))
        self.source.push("gpu.vram_used", gpu.get("vram_used"))
        self.source.push("gpu.power", gpu.get("power"))
        self.source.push("gpu.temp", gpu.get("temp"))

        self.source.succeed({**gpu, "procs": procs, "models": models})


def _seconds_until(timestamp):
    """Seconds from now until an ISO-8601 instant, or None."""
    if not timestamp:
        return None
    from datetime import datetime, timezone
    try:
        when = datetime.fromisoformat(timestamp)
    except ValueError:
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return max(0, round((when - datetime.now(timezone.utc)).total_seconds()))


# --------------------------------------------------------------------------
# TrueNAS — JSON-RPC 2.0 over WebSocket
# --------------------------------------------------------------------------

class TrueNasSource:
    """Holds one authenticated WebSocket open and polls over it.

    Reconnects on any failure. The connection is only ever used by this single
    task, so request ids need no locking.
    """

    def __init__(self, config, source):
        self.config = config
        self.source = source
        self.ws = None
        self.counter = 0
        self.identifiers = {}   # graph name -> [identifier, ...]

    async def call(self, method, params):
        self.counter += 1
        request_id = self.counter
        await self.ws.send_json({"jsonrpc": "2.0", "id": request_id,
                                 "method": method, "params": params})
        while True:
            message = await self.ws.receive(timeout=TRUENAS_TIMEOUT.total)
            if message.type is not aiohttp.WSMsgType.TEXT:
                raise RuntimeError(f"websocket closed ({message.type})")
            payload = json.loads(message.data)
            if payload.get("id") != request_id:
                continue  # server notification; not ours
            if "error" in payload:
                raise RuntimeError(str(payload["error"])[:200])
            return payload.get("result")

    async def connect(self, session):
        self.ws = await session.ws_connect(self.config.truenas_ws_url,
                                           ssl=False, max_msg_size=0,
                                           heartbeat=30)
        self.counter = 0
        if await self.call("auth.login_with_api_key",
                           [self.config.truenas_api_key]) is not True:
            raise RuntimeError("api key rejected")

        # Identifiers are discovered, never invented. Disk identifiers are
        # full descriptive strings ("sdb | Type: HDD | Model: … | Serial: …");
        # passing a bare device name returns no series and no error.
        graphs = await self.call("reporting.netdata_graphs", [])
        self.identifiers = {g["name"]: g.get("identifiers") or []
                            for g in graphs if g.get("name") in ("disk", "interface")}

    async def poll(self, session):
        if self.ws is None or self.ws.closed:
            await self.connect(session)

        targets = [{"name": "interface", "identifier": i}
                   for i in self.identifiers.get("interface", [])]
        targets += [{"name": "disk", "identifier": i}
                    for i in self.identifiers.get("disk", [])]

        now = int(time.time())
        series = await self.call("reporting.netdata_get_data",
                                 [targets, {"start": now - 10, "end": now}])
        pools = await self.call("pool.query", [])

        interfaces, disks = {}, {}
        for entry in series or []:
            latest = _last_sample(entry.get("data"))
            if latest is None:
                continue
            identifier = entry.get("identifier") or ""
            # "sdb | Type: HDD | …" -> "sdb"
            label = identifier.split("|")[0].strip()
            if entry.get("name") == "interface":
                # netdata reports Kilobits/s; normalise to bytes/sec.
                interfaces[label] = {"rx_bps": latest[1] * 125,
                                     "tx_bps": latest[2] * 125}
            else:
                # netdata reports Kibibytes/s; normalise to bytes/sec.
                disks[label] = {"read_bps": latest[1] * 1024,
                                "write_bps": latest[2] * 1024}

        for name, value in interfaces.items():
            self.source.push(f"if.{name}.rx", value["rx_bps"])
            self.source.push(f"if.{name}.tx", value["tx_bps"])

        self.source.succeed({
            "interfaces": interfaces,
            "disks": disks,
            "pools": [{
                "name": p.get("name"),
                "status": p.get("status"),
                "healthy": p.get("healthy"),
                "size": p.get("size"),
                "allocated": p.get("allocated"),
                "free": p.get("free"),
            } for p in pools or []],
        })

    async def close(self):
        if self.ws is not None and not self.ws.closed:
            await self.ws.close()


def _last_sample(data):
    """Most recent [time, a, b] row whose values are all present."""
    for row in reversed(data or []):
        if len(row) >= 3 and row[1] is not None and row[2] is not None:
            return row
    return None

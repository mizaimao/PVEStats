"""The router's own agent (router/ in this repo), on the router's LAN address:
its counters since boot, made into rates here from one reading to the next,
as the terminal dashboard does (tui/src/sources/router.rs)."""

from ..state import clock
from .base import ROUTER_TIMEOUT


class RouterSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source
        # The last reading's counters, by name, and when it was taken.
        self._last = None

    async def poll(self, session):
        async with session.get(self.config.router_url, timeout=ROUTER_TIMEOUT) as response:
            response.raise_for_status()
            payload = await response.json()
        data = self._read(_object(payload))
        self.source.push("wan.down", data["wan_down_bps"])
        self.source.push("wan.up", data["wan_up_bps"])
        self.source.succeed(data)

    def _read(self, payload):
        """One reading, with rates against the last one."""
        now = clock()
        last, counters = self._last, {}

        def rate(key, value):
            # Per second since the last reading; None for the first, or for a
            # counter that went backwards (the router rebooted).
            if _num(value) is None:
                return None
            counters[key] = value
            if last is None or key not in last[1]:
                return None
            seconds, previous = now - last[0], last[1][key]
            return (value - previous) / seconds if seconds > 0 and value >= previous else None

        cpu = _object(payload.get("cpu"))
        busy = rate("cpu.busy", cpu.get("busy"))
        total = rate("cpu.total", cpu.get("total"))
        cpu_pct = min(100.0, max(0.0, busy / total * 100)) if busy is not None and total else None
        wan = _object(payload.get("wan"))
        wan_down_bps = rate("wan.rx", wan.get("rx"))
        wan_up_bps = rate("wan.tx", wan.get("tx"))
        # A port's or a band's rx is what its devices sent: up, from them.
        ports = []
        for p in map(_object, _list(payload.get("ports"))):
            name = _text(p.get("name")) or "?"
            ports.append({
                "name": name,
                "up": p.get("up") is True,
                "wan": p.get("wan") is True,
                "down_bps": rate(f"port.{name}.tx", p.get("tx")),
                "up_bps": rate(f"port.{name}.rx", p.get("rx")),
            })
        bands = []
        for b in map(_object, _list(payload.get("bands"))):
            name = _text(b.get("name")) or "?"
            bands.append({
                "name": name,
                "on": b.get("on") is True,
                "devices": _num(b.get("devices")),
                "down_bps": rate(f"band.{name}.tx", b.get("tx")),
                "up_bps": rate(f"band.{name}.rx", b.get("rx")),
            })
        mem = _object(payload.get("mem"))
        mem_total, available = _num(mem.get("total")), _num(mem.get("available"))
        data = {
            "model": _text(payload.get("model")),
            "uptime": _num(payload.get("uptime")),
            "cpu_pct": cpu_pct,
            "mem_used": max(mem_total - available, 0)
                        if mem_total is not None and available is not None else None,
            "mem_total": mem_total,
            "temp": _num(_object(payload.get("temp")).get("cpu")),
            "wan_up": wan.get("up") is True,
            "wan_ip": _text(wan.get("ip")),
            "wan_down_bps": wan_down_bps,
            "wan_up_bps": wan_up_bps,
            "ports": ports,
            "wired_devices": _num(payload.get("wired_devices")),
            "bands": bands,
        }
        self._last = (now, counters)
        return data


def _num(v):
    """A JSON number as it came, or None."""
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def _text(v):
    return v if isinstance(v, str) else ""


def _object(v):
    return v if isinstance(v, dict) else {}


def _list(v):
    return v if isinstance(v, list) else []

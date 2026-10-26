"""TrueNAS's own agent (nas/ in this repo), on TrueNAS's LAN address: its
counters since boot, made into rates here from one reading to the next, timed
by TrueNAS's uptime, as the terminal dashboard does
(tui/src/sources/nas_agent.rs). The NAS card's only source: TrueNAS's API
cost it 25 ms of CPU a request, the agent 0.41 ms (nas/README.md)."""

from ..state import RateTracker
from .base import NAS_AGENT_TIMEOUT

# A pool's capacity is read every 30 s; figures older than this mean the
# agent's last reads of it failed.
OLD_POOL_SECONDS = 120


class NasAgentSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source
        # Counters by name ("cpu.busy", "nic.ens18.rx", "disk.sdb.read").
        # Disks and NICs come and go too rarely to need pruning.
        self._rates = RateTracker()

    async def poll(self, session):
        async with session.get(self.config.nas_agent_url, timeout=NAS_AGENT_TIMEOUT) as response:
            response.raise_for_status()
            payload = await response.json()
        data = self._read(_object(payload))
        for name, nic in data["interfaces"].items():
            self.source.push(f"if.{name}.rx", nic["rx_bps"])
            self.source.push(f"if.{name}.tx", nic["tx_bps"])
        self.source.succeed(data)

    def _read(self, payload):
        """One reading, with rates against the last one, in the shape the
        page's NAS card reads. The pool TrueNAS runs from, and its disk, are
        left off."""
        uptime = _num(payload.get("uptime"))

        def rate(key, value):
            # None without TrueNAS's clock, and for a first reading.
            return None if uptime is None else self._rates.rate(key, _num(value), uptime)

        cpu = _object(payload.get("cpu"))
        busy = rate("cpu.busy", cpu.get("busy"))
        total = rate("cpu.total", cpu.get("total"))
        cpu_pct = min(100.0, max(0.0, busy / total * 100)) if busy is not None and total else None
        mem = _object(payload.get("mem"))
        # The agent sends 0 when it could not read /proc/meminfo.
        mem_total = _num(mem.get("total")) or None
        available = _num(mem.get("available"))
        mem_used = max(mem_total - available, 0) if mem_total and available is not None else None

        interfaces = {}
        for n in map(_object, _list(payload.get("nics"))):
            name = _text(n.get("name"))
            if not name:
                continue
            driver = _text(n.get("driver"))
            # Proxmox's virtual NICs are virtio; any other driver is a real
            # card passed through, and names it.
            role = "virtual" if driver == "virtio_net" else "unknown" if not driver else "physical"
            interfaces[name] = {
                "rx_bps": rate(f"nic.{name}.rx", n.get("rx")),
                "tx_bps": rate(f"nic.{name}.tx", n.get("tx")),
                "role": role,
                "media": driver if role == "physical" else "",
            }

        pools_in = [_object(p) for p in _list(payload.get("pools"))]
        boot = {_text(p.get("name")) for p in pools_in if p.get("boot") is True}
        disks = {}
        for d in map(_object, _list(payload.get("disks"))):
            name = _text(d.get("name"))
            if not name or d.get("pool") in boot:
                continue
            disks[name] = {
                "read_bps": rate(f"disk.{name}.read", d.get("read")),
                "write_bps": rate(f"disk.{name}.write", d.get("write")),
                "size": _num(d.get("size")),
            }

        # Capacity as its root dataset counts it (used + available), what
        # TrueNAS's Storage page shows; ZFS's kstat state word as its status.
        pools = []
        for p in pools_in:
            if p.get("boot") is True:
                continue
            state = _text(p.get("state")) or None
            used, free = _num(p.get("used")), _num(p.get("available"))
            age = _num(p.get("age"))
            pools.append({
                "name": _text(p.get("name")) or "?",
                "status": state,
                "healthy": state == "ONLINE",
                "used_bytes": used,
                "usable": used + free if used is not None and free is not None else None,
                "old": age is not None and age > OLD_POOL_SECONDS,
            })

        return {
            "interfaces": interfaces,
            # sdb..sdf in the order the drives are labelled.
            "disks": dict(sorted(disks.items())),
            "pools": pools,
            "inside": {
                "cpu_pct": cpu_pct,
                "mem_used": mem_used,
                "mem_total": mem_total,
                "uptime": uptime,
                "ip": _text(payload.get("ip")) or None,
            },
        }


def _num(v):
    """A JSON number as it came, or None."""
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def _text(v):
    return v if isinstance(v, str) else ""


def _object(v):
    return v if isinstance(v, dict) else {}


def _list(v):
    return v if isinstance(v, list) else []

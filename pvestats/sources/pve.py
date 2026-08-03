"""Proxmox VE: guest inventory, host status, and guest-agent addresses."""

import asyncio
import re
import time

from ..state import RateTracker
from .base import PVE_TIMEOUT

# --------------------------------------------------------------------------
# Proxmox VE
# --------------------------------------------------------------------------

# Virtual and container interfaces the guest agent reports alongside the real
# one. Filtering by name is sturdier than guessing at address ranges — 172.16/12
# is a legitimate LAN range as well as Docker's default pool.
VIRTUAL_NIC = re.compile(r"^(lo|docker|br-|veth|virbr|cni|kube|flannel|tap)")

# Guest addresses change rarely and each lookup is a separate API call per
# guest, so they refresh on a slow cycle rather than every poll.
IP_REFRESH_EVERY = 6        # x PVE_INTERVAL = once a minute


class PveSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source
        self.rates = RateTracker()
        self.node = config.pve_node
        self.ips = {}           # vmid -> [address, ...]
        self._ip_countdown = 0

    async def refresh_ips(self, session, guests, headers, base):
        """Ask each running guest's agent for its addresses.

        Best effort: a guest without the agent, or with it stopped, simply has
        no address rather than failing the poll.
        """
        async def one(guest):
            vmid, kind = guest.get("vmid"), guest.get("type", "qemu")
            if vmid is None or kind != "qemu":
                return
            url = f"{base}/nodes/{self.node}/{kind}/{vmid}/agent/network-get-interfaces"
            try:
                async with session.get(url, headers=headers, ssl=False,
                                       timeout=PVE_TIMEOUT) as response:
                    if response.status != 200:
                        return
                    payload = (await response.json()).get("data") or {}
            except Exception:
                return
            found = []
            for nic in payload.get("result") or []:
                if VIRTUAL_NIC.match(nic.get("name") or ""):
                    continue
                for entry in nic.get("ip-addresses") or []:
                    address = entry.get("ip-address")
                    if (entry.get("ip-address-type") == "ipv4" and address
                            and not address.startswith("127.")):
                        found.append(address)
            if found:
                self.ips[vmid] = found

        await asyncio.gather(*(one(g) for g in guests), return_exceptions=True)

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

        # Running guests only. Stopped ones report nothing worth a card, and
        # their stale counters would pollute the rate tracker.
        running_raw = [g for g in resources if g.get("status") == "running"
                       and not g.get("template")]
        if self._ip_countdown <= 0:
            await self.refresh_ips(session, running_raw, headers, base)
            self._ip_countdown = IP_REFRESH_EVERY
        self._ip_countdown -= 1

        now = time.monotonic()
        guests = {}
        running = running_raw

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
                "addresses": self.ips.get(vmid) or [],
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

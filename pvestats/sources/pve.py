"""Proxmox VE: guest inventory, host status, and guest-agent addresses."""

import asyncio
import re

from ..state import RateTracker
from .base import PVE_TIMEOUT

# --------------------------------------------------------------------------
# Proxmox VE
# --------------------------------------------------------------------------

# Virtual and container interfaces the guest agent reports alongside the real
# one. Filtering by name is sturdier than guessing at address ranges — 172.16/12
# is a legitimate LAN range as well as Docker's default pool.
VIRTUAL_NIC = re.compile(r"^(lo|docker|br-|veth|virbr|cni|kube|flannel|tap)")

# Guest addresses barely change, and each lookup is a separate API call per
# guest. So they are fetched once at startup and then only when a guest needs
# it — newly seen, or rebooted (uptime went backwards). The backstop catches a
# DHCP lease change on a guest that never restarts.
IP_BACKSTOP_EVERY = 90      # x PVE_INTERVAL = every 15 minutes

# Filesystem usage also comes from the guest agent, one call per guest. It
# moves slowly, so it refreshes on its own slow cycle.
FS_REFRESH_EVERY = 30       # x PVE_INTERVAL = every 5 minutes

# A guest whose agent has not answered yet (still booting, or none at all) is
# asked again at this pace rather than on every poll: often enough that a
# freshly started guest fills in within a minute, rarely enough that a guest
# with no agent is not an extra API call and pveproxy log line every 10s.
MISSING_RETRY_EVERY = 6     # x PVE_INTERVAL = every minute

# Pseudo-filesystems that would otherwise crowd out the real ones.
SKIP_FS = {"tmpfs", "devtmpfs", "overlay", "squashfs", "efivarfs", "ramfs"}

# PVE proxies disk IO only for storage it manages. A guest with passed-through
# storage reports a handful of bytes since boot while doing real work, so the
# counters are worse than useless — they read as an idle disk. Anything under
# this after an hour of uptime is treated as "not visible to the host".
DISK_IO_FLOOR = 16 * 1024 ** 2      # 16 MiB since boot


def _disk_io_visible(guest):
    """False when PVE's disk counters are not measuring this guest.

    Storage passed straight through to a guest bypasses the hypervisor, so its
    counters sit near zero however busy the disk actually is. Plotting that
    would show a permanently idle disk, which is worse than showing nothing.
    """
    uptime = guest.get("uptime") or 0
    total = (guest.get("diskread") or 0) + (guest.get("diskwrite") or 0)
    if uptime < 3600:
        return True         # too early to judge
    return total >= DISK_IO_FLOOR


class PveSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source
        self.rates = RateTracker()
        self.node = config.pve_node
        self.ips = {}           # vmid -> [address, ...]
        self.seen_uptime = {}   # vmid -> last uptime, for reboot detection
        self._backstop = 0
        self.filesystems = {}   # vmid -> {"used": int, "total": int, "mount": str}
        self._fs_countdown = 0
        self._polls = 0
        # vmid -> poll number before which a guest with no answer yet is not
        # asked again (MISSING_RETRY_EVERY).
        self._ip_retry = {}
        self._fs_retry = {}

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

    async def refresh_filesystems(self, session, guests, headers, base):
        """Read in-guest filesystem usage from the agent.

        `/cluster/resources` reports `disk` as 0 for every VM, so for a VM
        the agent is the only source of real usage. (Containers are the
        exception: there `disk` is the real usage, used directly in poll().)
        Best effort per guest.
        """
        async def one(guest):
            vmid, kind = guest.get("vmid"), guest.get("type", "qemu")
            if vmid is None or kind != "qemu":
                return
            url = f"{base}/nodes/{self.node}/{kind}/{vmid}/agent/get-fsinfo"
            try:
                async with session.get(url, headers=headers, ssl=False,
                                       timeout=PVE_TIMEOUT) as response:
                    if response.status != 200:
                        return
                    payload = (await response.json()).get("data") or {}
            except Exception:
                return
            # The largest real filesystem stands in for "the guest's disk" —
            # on these guests that is always root.
            best = None
            for fs in payload.get("result") or []:
                total, used = fs.get("total-bytes"), fs.get("used-bytes")
                if not total or fs.get("type") in SKIP_FS:
                    continue
                if best is None or total > best["total"]:
                    best = {"used": used, "total": total,
                            "mount": fs.get("mountpoint")}
            if best:
                self.filesystems[vmid] = best
            else:
                self.filesystems.pop(vmid, None)

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
        self._polls += 1
        polls = self._polls
        live_ids = {g.get("vmid") for g in running_raw}

        # Forget guests that are no longer running, so a later start re-reads.
        for table in (self.ips, self.filesystems, self.seen_uptime,
                      self._ip_retry, self._fs_retry):
            for gone in [v for v in table if v not in live_ids]:
                del table[gone]

        # Only ask about guests that actually need an answer.
        self._backstop -= 1
        due = self._backstop <= 0
        if due:
            self._backstop = IP_BACKSTOP_EVERY
        ask_ips = []
        for guest in running_raw:
            vmid, uptime = guest.get("vmid"), guest.get("uptime")
            if vmid is None:
                continue
            was = self.seen_uptime.get(vmid)
            if was is not None and uptime is not None and uptime < was:
                # Rebooted in place. The old address and usage may not hold,
                # and its agent is still starting: ask until it answers.
                for table in (self.ips, self.filesystems, self._ip_retry, self._fs_retry):
                    table.pop(vmid, None)
            self.seen_uptime[vmid] = uptime
            missing = vmid not in self.ips and self._ip_retry.get(vmid, 0) <= polls
            if due or missing:
                ask_ips.append(guest)
        if ask_ips:
            await self.refresh_ips(session, ask_ips, headers, base)
            for guest in ask_ips:
                if guest["vmid"] not in self.ips:
                    self._ip_retry[guest["vmid"]] = polls + MISSING_RETRY_EVERY

        # All of them on the slow cycle; in between, only guests with no usage
        # yet (newly started or rebooted), at the retry pace.
        self._fs_countdown -= 1
        if self._fs_countdown <= 0:
            ask_fs = running_raw
            self._fs_countdown = FS_REFRESH_EVERY
        else:
            ask_fs = [g for g in running_raw if g.get("type", "qemu") == "qemu"
                      and g.get("vmid") not in self.filesystems
                      and self._fs_retry.get(g.get("vmid"), 0) <= polls]
        if ask_fs:
            await self.refresh_filesystems(session, ask_fs, headers, base)
            for guest in ask_fs:
                if guest.get("vmid") not in self.filesystems:
                    self._fs_retry[guest.get("vmid")] = polls + MISSING_RETRY_EVERY

        guests = {}
        running = running_raw

        for guest in running:
            vmid = guest.get("vmid")
            if vmid is None:
                continue
            key = f"{vmid}"
            cpu_fraction = guest.get("cpu")  # 0.0-1.0, NOT a percentage
            # The counters are pvestatd's last snapshot, republished every
            # ~10s, not a reading taken now. Timing them by the Mac's clock
            # made a poll that saw the same snapshot twice read 0 and the
            # next read double. The guest's uptime comes from that same
            # snapshot, so it times them exactly; an unchanged uptime is a
            # repeat. It also ignores the Mac sleeping, and how long this
            # poll's other calls took.
            snapshot = guest.get("uptime")
            if snapshot is not None:
                rates = {name: self.rates.rate(f"{key}.{name}", guest.get(name), snapshot)
                         for name in ("netin", "netout", "diskread", "diskwrite")}
            else:
                rates = dict.fromkeys(("netin", "netout", "diskread", "diskwrite"))
            # A container's disk is the real usage of its root filesystem,
            # with no agent involved.
            filesystem = self.filesystems.get(vmid)
            if guest.get("type") == "lxc" and guest.get("maxdisk"):
                filesystem = {"used": guest.get("disk"), "total": guest.get("maxdisk"),
                              "mount": "/"}
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
                "filesystem": filesystem,
                # Whether the host can actually see this guest's disk traffic.
                "disk_io_visible": _disk_io_visible(guest),
                # Cumulative counters converted to bytes/sec. None until the
                # second snapshot, since a rate needs two.
                "net_in_bps": rates["netin"],
                "net_out_bps": rates["netout"],
                "disk_read_bps": rates["diskread"],
                "disk_write_bps": rates["diskwrite"],
            }
            # PVE's disk/maxdisk are not passed on as such: for a VM `disk` is
            # always 0 (usage comes from its agent, above), and for a container
            # it is real usage, already in `filesystem`. Nothing read the old
            # disk_allocated field, which said "allocation" for both.
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

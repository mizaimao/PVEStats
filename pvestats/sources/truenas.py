"""TrueNAS over JSON-RPC 2.0 on a WebSocket (not REST — see connect())."""

import asyncio
import json
import time

import aiohttp

from .base import TRUENAS_TIMEOUT

# --------------------------------------------------------------------------
# TrueNAS — JSON-RPC 2.0 over WebSocket
# --------------------------------------------------------------------------

# Disks, interfaces and their identifiers change rarely (a disk swapped, a
# NIC added), but they do change, and the socket can stay up for weeks.
DISCOVER_EVERY = 60     # x TRUENAS_INTERVAL = every 5 minutes


class RpcError(RuntimeError):
    """TrueNAS answered, with an error: the connection itself is fine."""


class TrueNasSource:
    """Holds one authenticated WebSocket open and polls over it.

    Reconnects after any failure except a method error. The connection is
    only ever used by this single task, so request ids need no locking.
    """

    def __init__(self, config, source):
        self.config = config
        self.source = source
        self.ws = None
        self.counter = 0
        self.identifiers = {}   # graph name -> [identifier, ...]
        self.nics = {}          # interface name -> {role, media, addresses}
        self.disk_sizes = {}    # device name -> bytes
        self.pools = []         # last pool.query result
        self.usable = {}        # pool name -> (used, usable) bytes
        self._pool_countdown = 0
        self._discover_countdown = 0

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
                raise RpcError(str(payload["error"])[:200])
            return payload.get("result")

    async def connect(self, session):
        # A reconnect must re-read the pools, not reuse a stale list.
        self._pool_countdown = 0
        # Bounded like every other call (spec §7). ws_connect has no timeout
        # of its own for the upgrade request, so the session default applied
        # (30s to connect, 300s in all), and a NAS that was down read
        # "waiting" for a minute.
        self.ws = await asyncio.wait_for(
            session.ws_connect(self.config.truenas_ws_url, ssl=False,
                               max_msg_size=0, heartbeat=30),
            TRUENAS_TIMEOUT.total)
        self.counter = 0
        try:
            await self._handshake()
        except Exception:
            # poll() reconnects only when the socket is closed, so a socket
            # left open after a failed login or a timed-out discovery call
            # would stay half set up for good: no interfaces, no disks.
            await self.close()
            raise

    async def _handshake(self):
        """Log in and discover what to poll, on a freshly opened socket."""
        if await self.call("auth.login_with_api_key",
                           [self.config.truenas_api_key]) is not True:
            raise RuntimeError("api key rejected")
        await self._discover()

    async def _discover(self):
        """Find the disks and interfaces to poll, and what each one is."""
        self._discover_countdown = DISCOVER_EVERY
        # Identifiers are discovered, never invented. Disk identifiers are
        # full descriptive strings ("sdb | Type: HDD | Model: … | Serial: …");
        # passing a bare device name returns no series and no error.
        graphs = await self.call("reporting.netdata_graphs", [])
        self.identifiers = {g["name"]: g.get("identifiers") or []
                            for g in graphs if g.get("name") in ("disk", "interface")}

        # Interface roles, derived rather than hardcoded by name. Proxmox
        # stamps BC:24:11 on the virtio NICs it creates; a passed-through card
        # reports a real media subtype ("2500Mb/s Twisted Pair") where virtio
        # reports "Unknown Other".
        self.nics = {}
        for iface in await self.call("interface.query", []):
            state = iface.get("state") or {}
            mac = (state.get("hardware_link_address") or "").upper()
            media = state.get("active_media_subtype") or ""
            if mac.startswith("BC:24:11"):
                role = "virtual"
            elif media and media.lower() != "unknown other":
                role = "physical"
            else:
                role = "unknown"
            self.nics[iface.get("name")] = {
                "role": role,
                "media": media,
                "addresses": [a.get("address") for a in (state.get("aliases") or [])
                              if a.get("type") == "INET"],
            }

        # Disk capacities, joined onto the throughput rows by device name.
        self.disk_sizes = {d.get("name"): d.get("size")
                           for d in await self.call("disk.query", [])}

    async def poll(self, session):
        if self.ws is None or self.ws.closed:
            await self.connect(session)
        try:
            await self._read()
        except RpcError:
            raise
        except Exception:
            # A timeout or a dropped connection: the socket may be no good,
            # and poll() reconnects only once it is closed. A method error is
            # left alone; reconnecting would not help, and each login is an
            # audit entry on the NAS.
            await self.close()
            raise

    async def _read(self):
        self._discover_countdown -= 1
        if self._discover_countdown <= 0:
            await self._discover()

        targets = [{"name": "interface", "identifier": i}
                   for i in self.identifiers.get("interface", [])]
        targets += [{"name": "disk", "identifier": i}
                    for i in self.identifiers.get("disk", [])]

        now = int(time.time())
        series = await self.call("reporting.netdata_get_data",
                                 [targets, {"start": now - 10, "end": now}])

        # Capacity and health move on the scale of hours, so re-reading them
        # every 5s costs an RPC per poll for numbers that have not changed.
        self._pool_countdown -= 1
        if self._pool_countdown <= 0 or not self.pools:
            self.pools = await self.call("pool.query", []) or []
            self.usable = await self._usable_space(self.pools)
            self._pool_countdown = 6      # x TRUENAS_INTERVAL = every 30s
        pools = self.pools

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
                                     "tx_bps": latest[2] * 125,
                                     **(self.nics.get(label) or {})}
            else:
                # netdata reports Kibibytes/s; normalise to bytes/sec.
                disks[label] = {"read_bps": latest[1] * 1024,
                                "write_bps": latest[2] * 1024,
                                "size": self.disk_sizes.get(label)}

        for name, value in interfaces.items():
            self.source.push(f"if.{name}.rx", value["rx_bps"])
            self.source.push(f"if.{name}.tx", value["tx_bps"])

        self.source.succeed({
            "interfaces": interfaces,
            # netdata returns identifiers newest-first; sort so sdb..sdf reads
            # in the order the drives are actually labelled.
            "disks": dict(sorted(disks.items())),
            "pools": [{
                "name": p.get("name"),
                "status": p.get("status"),
                "healthy": p.get("healthy"),
                "size": p.get("size"),
                "allocated": p.get("allocated"),
                "free": p.get("free"),
                # What the pool can hold and holds, from its root dataset.
                **dict(zip(("used_bytes", "usable"),
                           self.usable.get(p.get("name"), (None, None)))),
            } for p in reversed(pools or [])],
        })

    async def _usable_space(self, pools):
        """Pool name -> (used, used + available) bytes, from root datasets.

        zpool's size counts parity, so a RAIDZ2 pool's "size" is about twice
        what it can store (CompoundZ2FS: 58.2 TiB raw, 28.1 TiB usable); its
        root dataset's used + available is what TrueNAS's Storage page shows.
        A method error leaves this empty, and the card falls back to raw.
        """
        names = [p.get("name") for p in pools if p.get("name")]
        if not names:
            return {}
        try:
            datasets = await self.call(
                "pool.dataset.query",
                [[["id", "in", names]], {"extra": {"retrieve_children": False}}]) or []
        except RpcError:
            return {}
        usable = {}
        for d in datasets:
            used = (d.get("used") or {}).get("parsed")
            available = (d.get("available") or {}).get("parsed")
            if isinstance(used, int) and isinstance(available, int):
                usable[d.get("id")] = (used, used + available)
        return usable

    async def close(self):
        if self.ws is not None and not self.ws.closed:
            await self.ws.close()


def _last_sample(data):
    """Most recent [time, a, b] row whose values are all present."""
    for row in reversed(data or []):
        if len(row) >= 3 and row[1] is not None and row[2] is not None:
            return row
    return None

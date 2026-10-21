"""Wires the sources together and assembles the snapshot served as JSON.

One asyncio task per source, each on its own interval. A slow or dead source
never stalls another (spec §2).
"""

import asyncio

import aiohttp

from . import sources
from .state import Source


class Collector:
    def __init__(self, config):
        self.config = config
        self.sources = {
            "pve": Source("pve", sources.PVE_INTERVAL),
            "agent": Source("agent", sources.AGENT_INTERVAL),
            "truenas": Source("truenas", sources.TRUENAS_INTERVAL),
        }
        self.session = None
        self._tasks = []
        self._truenas = None

    async def start(self):
        self.session = aiohttp.ClientSession()
        self.set_history_window(self.config.ui().get("window", 180))

        if self.config.pve_configured:
            pve = sources.PveSource(self.config, self.sources["pve"])
            self._spawn(self.sources["pve"], sources.PVE_INTERVAL,
                        lambda: pve.poll(self.session))
        else:
            self.sources["pve"].fail("not configured — see .env")

        if self.config.agent_configured:
            agent = sources.AgentSource(self.config, self.sources["agent"])
            self._spawn(self.sources["agent"], sources.AGENT_INTERVAL,
                        lambda: agent.poll(self.session))
        else:
            self.sources["agent"].fail("not configured — see .env")

        if self.config.truenas_configured:
            self._truenas = sources.TrueNasSource(self.config, self.sources["truenas"])
            self._spawn(self.sources["truenas"], sources.TRUENAS_INTERVAL,
                        lambda: self._truenas.poll(self.session))
        else:
            self.sources["truenas"].fail("not configured — see .env")

    def _spawn(self, source, interval, poll):
        self._tasks.append(
            asyncio.create_task(sources.run_poller(source, interval, poll),
                                name=f"poll-{source.name}")
        )

    async def stop(self):
        for task in self._tasks:
            task.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)
        if self._truenas is not None:
            await self._truenas.close()
        if self.session is not None:
            await self.session.close()

    def set_history_window(self, seconds):
        """Re-length every ring buffer. Charts derive their span from this."""
        for source in self.sources.values():
            source.resize(max(30, min(3600, int(seconds))))

    def snapshot(self):
        """The whole state, as served by GET /api/state.

        Contains no credentials — only observed values (spec §6).
        """
        pve = self.sources["pve"]
        agent = self.sources["agent"]
        nas = self.sources["truenas"]

        guests = dict(pve.data.get("guests") or {})

        # ml60's card merges two sources at two rates: PVE for CPU and RAM at
        # 10s, the agent for the GPU at 1s. The fast panels must not stall
        # waiting on the slow poll (spec §4.2), so they are attached
        # independently of whether the PVE entry updated.
        for vmid, host in self.config.hosts.items():
            guest = guests.get(vmid)
            if guest is None:
                # Until PVE first answers there is no list of running guests,
                # but the gpu and nas cards have sources of their own and must
                # not wait for PVE's (spec §2, §4.2): they get a placeholder
                # whose PVE fields read "–". Once PVE has answered, a guest
                # missing from it is not running, and its card goes.
                if pve.updated_at is not None or host.get("view") not in ("gpu", "nas"):
                    continue
                guest = {"vmid": vmid, "name": host.get("name") or str(vmid),
                         "view": host.get("view"), "addresses": []}
            else:
                # A copy: the entry belongs to the PVE source's last result.
                guest = dict(guest)
            guests[vmid] = guest
            # The status always travels, even with no data yet — otherwise the
            # card has no way to say *why* it is empty, and a dead source looks
            # identical to one that simply reads zero (spec §7).
            if host.get("view") == "gpu":
                guest["gpu_status"] = agent.status()
                if agent.data:
                    guest["gpu"] = agent.data
            elif host.get("view") == "nas":
                guest["nas_status"] = nas.status()
                if nas.data:
                    data = dict(nas.data)
                    labels = host.get("nic_labels") or {}
                    if labels:
                        data["interfaces"] = {
                            name: ({**nic, "label": labels[name]} if name in labels
                                   else nic)
                            for name, nic in (data.get("interfaces") or {}).items()
                        }
                    guest["nas"] = data

        return {
            "sources": {name: source.status() for name, source in self.sources.items()},
            "node": pve.data.get("node"),
            "guests": sorted(guests.values(), key=lambda g: g["vmid"]),
            "history": {
                "pve": pve.history(),
                "agent": agent.history(),
                "truenas": nas.history(),
            },
        }

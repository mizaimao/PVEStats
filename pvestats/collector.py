"""Wires the sources together and assembles the snapshot served as JSON.

One asyncio task per source, each on its own interval. A slow or dead source
never stalls another (spec §2).
"""

import asyncio

import aiohttp

from . import sources
from .state import Source, clock


class Collector:
    def __init__(self, config):
        self.config = config
        self.sources = {
            "pve": Source("pve", sources.PVE_INTERVAL),
            "agent": Source("agent", sources.AGENT_INTERVAL),
            "truenas": Source("truenas", sources.TRUENAS_INTERVAL),
        }
        # Optional: only a configured router gets a source, a status dot and
        # a card.
        if config.router_configured:
            self.sources["router"] = Source("router", sources.ROUTER_INTERVAL)
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

        if "router" in self.sources:
            router = sources.RouterSource(self.config, self.sources["router"])
            self._spawn(self.sources["router"], sources.ROUTER_INTERVAL,
                        lambda: router.poll(self.session))

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
                # A name in config.yaml wins over the VM's name in Proxmox,
                # which can be years out of date (vmid 100 is still FreeNAS).
                if host.get("name"):
                    guest["name"] = host["name"]
            guests[vmid] = guest
            # The status always travels, even with no data yet — otherwise the
            # card has no way to say *why* it is empty, and a dead source looks
            # identical to one that simply reads zero (spec §7).
            if host.get("view") == "gpu":
                guest["gpu_status"] = agent.status()
                if agent.data:
                    gpu = dict(agent.data)
                    ram, ram_at = gpu.pop("ram", None), gpu.pop("ram_at", None)
                    guest["gpu"] = gpu
                    # ml60's memory from inside it, in place of PVE's (which
                    # counts the disk cache as used) while under three polls
                    # old; after that the card goes back to PVE's.
                    if ram and ram_at is not None and clock() - ram_at <= sources.AGENT_INTERVAL * 3:
                        guest["ram"] = ram
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

        router = self.sources.get("router")
        history = {
            "pve": pve.history(),
            "agent": agent.history(),
            "truenas": nas.history(),
        }
        if router is not None:
            history["router"] = router.history()
        return {
            "sources": {name: source.status() for name, source in self.sources.items()},
            "node": pve.data.get("node"),
            "guests": sorted(guests.values(), key=lambda g: g["vmid"]),
            # Last known good, like every other card's: a failing router
            # keeps its numbers, greyed, rather than blanking.
            "router": (router.data or None) if router is not None else None,
            "history": history,
        }

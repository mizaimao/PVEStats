"""Shared poller plumbing: intervals, timeouts, and the failure-absorbing loop.

Intervals are properties of the sources, not preferences. PVE's 10s is fixed
by pvestatd's own recompute cycle (spec §3.1); the agent's 1s is genuinely
real-time because NVML reads take microseconds.
"""

import asyncio
import time

import aiohttp

PVE_INTERVAL = 10.0
AGENT_INTERVAL = 1.0
# TrueNAS's own agent reads the kernel as it is asked: 0.41 ms of its CPU a
# poll (nas/README.md).
NAS_AGENT_INTERVAL = 2.0
ROUTER_INTERVAL = 1.0

PVE_TIMEOUT = aiohttp.ClientTimeout(total=5)
AGENT_TIMEOUT = aiohttp.ClientTimeout(total=2)
NAS_AGENT_TIMEOUT = aiohttp.ClientTimeout(total=2)
ROUTER_TIMEOUT = aiohttp.ClientTimeout(total=2)


async def run_poller(source, interval, poll):
    """Call `poll` forever on a fixed interval, absorbing every failure."""
    while True:
        started = time.monotonic()
        # Before new samples land, account for any that were missed — a stall
        # should read as a gap, not as compressed time.
        source.note_poll(interval)
        try:
            await poll()
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            # How long the poll actually waited. The limit that fired is a
            # request's own timeout (5s per PVE request, 2s for each agent),
            # never the poll interval. A poll makes several requests in turn,
            # so the total can be more than one limit.
            source.fail(f"timed out after {time.monotonic() - started:.0f}s")
        except Exception as exc:
            source.fail(f"{type(exc).__name__}: {exc}")
        # One slot per series per poll, so a failed poll is drawn as a gap.
        source.end_poll()
        # Drift-free: subtract however long the poll actually took.
        await asyncio.sleep(max(0.0, interval - (time.monotonic() - started)))

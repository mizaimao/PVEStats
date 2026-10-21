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
TRUENAS_INTERVAL = 5.0

PVE_TIMEOUT = aiohttp.ClientTimeout(total=5)
AGENT_TIMEOUT = aiohttp.ClientTimeout(total=2)
TRUENAS_TIMEOUT = aiohttp.ClientTimeout(total=5)


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
            source.fail(f"timeout after {interval}s")
        except Exception as exc:
            source.fail(f"{type(exc).__name__}: {exc}")
        # One slot per series per poll, so a failed poll is drawn as a gap.
        source.end_poll()
        # Drift-free: subtract however long the poll actually took.
        await asyncio.sleep(max(0.0, interval - (time.monotonic() - started)))

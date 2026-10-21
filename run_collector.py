#!/usr/bin/env python3
"""Run the collector without a server and print snapshots.

Temporary harness for phase 3 — the real entry point arrives with the HTTP
server. Useful on its own for checking that every source is live:

    .venv/bin/python run_collector.py            # 3 snapshots, 12s apart
    .venv/bin/python run_collector.py --once
"""

import argparse
import asyncio
import json

from pvestats.collector import Collector
from pvestats.config import Config


def num(value, spec, scale=1):
    """A number formatted, or "–" when a source sent none.

    An offline pool has a null size, NVML can fail to report memory, and a
    GPU or NAS card shown before PVE answers has no PVE fields at all: any of
    them used to end the run with a TypeError or KeyError.
    """
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return "–"
    return format(value / scale, spec)


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--once", action="store_true",
                        help="one snapshot after the first full PVE poll")
    parser.add_argument("--count", type=int, default=3)
    parser.add_argument("--full", action="store_true",
                        help="print the entire snapshot, history included")
    args = parser.parse_args()

    collector = Collector(Config())
    await collector.start()
    try:
        for index in range(1 if args.once else args.count):
            # Long enough for a 10s PVE poll to land, and for rates to have
            # two samples to diff.
            await asyncio.sleep(12)
            snapshot = collector.snapshot()
            print(f"\n=== snapshot {index + 1} ===")
            if args.full:
                print(json.dumps(snapshot, indent=2))
                continue
            for name, status in snapshot["sources"].items():
                flag = "ok " if status["ok"] else "ERR"
                print(f"  {flag} {name:<8} age={status['age']}s "
                      f"{status['error'] or ''}")
            node = snapshot["node"]
            if node:
                print(f"  node {node.get('name')}: cpu={num(node.get('cpu_pct'), '.1f')}% "
                      f"mem={num(node.get('mem'), '.1f', 2**30)}/"
                      f"{num(node.get('maxmem'), '.1f', 2**30)} GiB "
                      f"load={node.get('loadavg')} "
                      f"guests={node.get('guests_running')}/{node.get('guests_total')}")
            for guest in snapshot["guests"]:
                line = (f"  {guest['vmid']:>5} {str(guest.get('name')):<14} "
                        f"{str(guest.get('view')):<6} cpu={num(guest.get('cpu_pct'), '5.1f')}% "
                        f"mem={num(guest.get('mem'), '5.1f', 2**30)}/"
                        f"{num(guest.get('maxmem'), '.0f', 2**30)}G")
                if guest.get("net_in_bps") is not None:
                    line += (f" net={num(guest['net_in_bps'], '.1f', 1e6)}/"
                             f"{num(guest.get('net_out_bps'), '.1f', 1e6)} MB/s")
                print(line)
                if "gpu" in guest:
                    gpu = guest["gpu"]
                    print(f"        gpu {num(gpu.get('util'), '.0f')}% "
                          f"vram={num(gpu.get('vram_used'), '.1f', 2**30)}/"
                          f"{num(gpu.get('vram_total'), '.1f', 2**30)}G "
                          f"{num(gpu.get('temp'), '.0f')}C "
                          f"{num(gpu.get('power'), '.0f')}/{num(gpu.get('power_limit'), '.0f')}W "
                          f"models={[m.get('name') for m in gpu.get('models') or []]}")
                if "nas" in guest:
                    nas = guest["nas"]
                    for name, iface in (nas.get("interfaces") or {}).items():
                        print(f"        if {name}: rx={num(iface.get('rx_bps'), '.1f', 1e6)} "
                              f"tx={num(iface.get('tx_bps'), '.1f', 1e6)} MB/s")
                    for pool in nas.get("pools") or []:
                        size, allocated = pool.get("size"), pool.get("allocated")
                        used = (num(allocated / size * 100, ".0f")
                                if size and isinstance(allocated, (int, float)) else "–")
                        print(f"        pool {pool.get('name')}: {pool.get('status')} "
                              f"{used}% of {num(size, '.1f', 2**40)} TiB")
    finally:
        await collector.stop()


if __name__ == "__main__":
    asyncio.run(main())

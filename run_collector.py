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
                print(f"  node {node['name']}: cpu={node['cpu_pct']:.1f}% "
                      f"mem={node['mem'] / 2**30:.1f}/{node['maxmem'] / 2**30:.1f} GiB "
                      f"load={node['loadavg']} "
                      f"guests={node['guests_running']}/{node['guests_total']}")
            for guest in snapshot["guests"]:
                line = (f"  {guest['vmid']:>5} {guest['name']:<14} "
                        f"{guest['view']:<6} cpu={guest['cpu_pct']:5.1f}% "
                        f"mem={guest['mem'] / 2**30:5.1f}/{guest['maxmem'] / 2**30:.0f}G")
                if guest.get("net_in_bps") is not None:
                    line += (f" net={guest['net_in_bps'] / 1e6:.1f}/"
                             f"{guest['net_out_bps'] / 1e6:.1f} MB/s")
                print(line)
                if "gpu" in guest:
                    gpu = guest["gpu"]
                    print(f"        gpu {gpu['util']}% "
                          f"vram={gpu['vram_used'] / 2**30:.1f}/"
                          f"{gpu['vram_total'] / 2**30:.1f}G "
                          f"{gpu['temp']}C {gpu['power']}/{gpu['power_limit']}W "
                          f"models={[m['name'] for m in gpu['models']]}")
                if "nas" in guest:
                    nas = guest["nas"]
                    for name, iface in nas["interfaces"].items():
                        print(f"        if {name}: rx={iface['rx_bps'] / 1e6:.1f} "
                              f"tx={iface['tx_bps'] / 1e6:.1f} MB/s")
                    for pool in nas["pools"]:
                        used = pool["allocated"] / pool["size"] * 100
                        print(f"        pool {pool['name']}: {pool['status']} "
                              f"{used:.0f}% of {pool['size'] / 2**40:.1f} TiB")
    finally:
        await collector.stop()


if __name__ == "__main__":
    asyncio.run(main())

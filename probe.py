#!/usr/bin/env python3
"""Dump raw responses from every data source into captures/.

Spec §7: the collector's parsing is written against these captures, not against
the field names in the design document. Where the two disagree, the live
response wins.

Read-only. GET requests only, no writes to any homelab disk.

    tools/uv run probe.py              # every source
    tools/uv run probe.py pve          # named sources only

Only the standard library, but run it through tools/uv like everything else
here. TrueNAS is read from its own agent (nas/) and is no longer probed here.
"""

import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).parent
CAPTURES = ROOT / "captures"

# Generous relative to the collector's 2-5s budgets (spec §7). A probe that
# times out teaches us nothing; a probe that waits tells us the source is slow.
TIMEOUT = 15

INSECURE = ssl._create_unverified_context()  # PVE is self-signed


def load_env():
    """Minimal .env reader. Avoids a python-dotenv dependency."""
    env = {}
    path = ROOT / ".env"
    if not path.exists():
        sys.exit("No .env found. Copy .env.example to .env and fill it in.")
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        env[key.strip()] = value.strip().strip("'\"")
    return env


def fetch(url, headers=None, insecure=True, context=None):
    """GET a URL. Returns a record; never raises."""
    record = {"url": url, "status": None, "elapsed_ms": None, "error": None}
    request = urllib.request.Request(url, headers=headers or {}, method="GET")
    started = time.monotonic()
    try:
        ctx = context or (INSECURE if insecure and url.startswith("https") else None)
        with urllib.request.urlopen(request, timeout=TIMEOUT, context=ctx) as response:
            record["status"] = response.status
            raw = response.read()
    except urllib.error.HTTPError as exc:
        record["status"] = exc.code
        raw = exc.read()
    except Exception as exc:
        record["error"] = f"{type(exc).__name__}: {exc}"
        record["elapsed_ms"] = round((time.monotonic() - started) * 1000)
        return record

    record["elapsed_ms"] = round((time.monotonic() - started) * 1000)
    try:
        record["body"] = json.loads(raw)
    except (ValueError, UnicodeDecodeError):
        text = raw.decode("utf-8", "replace")
        record["body_is_json"] = False
        record["body_text_head"] = text[:2000]
        record["body_bytes"] = len(raw)
    return record


def save(name, payload):
    CAPTURES.mkdir(exist_ok=True)
    path = CAPTURES / f"{name}.json"
    path.write_text(json.dumps(payload, indent=2, sort_keys=True))
    print(f"  wrote {path.relative_to(ROOT)}")


def summarise(label, record):
    if record.get("error"):
        print(f"  {label}: FAILED — {record['error']}")
    else:
        print(f"  {label}: {record['status']} ({record['elapsed_ms']} ms)")


# --------------------------------------------------------------------------
# Proxmox VE (spec §3.1)
# --------------------------------------------------------------------------

def probe_pve(env):
    host = env.get("PVE_HOST")
    token_id = env.get("PVE_TOKEN_ID")
    secret = env.get("PVE_TOKEN_SECRET")
    if not (host and token_id and secret):
        print("  skipped: PVE_HOST / PVE_TOKEN_ID / PVE_TOKEN_SECRET not all set")
        return

    base = f"https://{host}:8006/api2/json"
    headers = {"Authorization": f"PVEAPIToken={token_id}={secret}"}
    out = {}

    # Every guest in one call. The primary source.
    out["cluster_resources_vm"] = fetch(f"{base}/cluster/resources?type=vm", headers)
    summarise("/cluster/resources?type=vm", out["cluster_resources_vm"])

    out["nodes"] = fetch(f"{base}/nodes", headers)
    summarise("/nodes", out["nodes"])

    # Node name: explicit if configured, otherwise take the first one reported.
    node = env.get("PVE_NODE") or ""
    if not node:
        listing = (out["nodes"].get("body") or {}).get("data") or []
        if listing:
            node = listing[0].get("node", "")
            print(f"  discovered node: {node}")

    if node:
        out["node_status"] = fetch(f"{base}/nodes/{node}/status", headers)
        summarise(f"/nodes/{node}/status", out["node_status"])
    else:
        print("  no node name — set PVE_NODE in .env to capture host status")

    # One rrddata sample, to pin down the real shape before building history.
    guests = (out["cluster_resources_vm"].get("body") or {}).get("data") or []
    running = [g for g in guests if g.get("status") == "running" and g.get("vmid")]
    if node and running:
        target = running[0]
        vmid = target["vmid"]
        kind = target.get("type", "qemu")  # qemu or lxc
        url = (
            f"{base}/nodes/{node}/{kind}/{vmid}/rrddata"
            "?timeframe=hour&cf=AVERAGE"
        )
        out["rrddata_sample"] = fetch(url, headers)
        out["rrddata_sample"]["_probed_guest"] = {
            "vmid": vmid,
            "name": target.get("name"),
            "type": kind,
        }
        summarise(f"rrddata for vmid {vmid}", out["rrddata_sample"])

    save("pve", out)

    # Inventory, so the guest list is readable without opening the dump.
    if guests:
        print("\n  guests:")
        for g in sorted(guests, key=lambda x: x.get("vmid", 0)):
            print(
                f"    {g.get('vmid'):>5}  {g.get('name', '?'):<20}"
                f"  {g.get('type', '?'):<5}  {g.get('status', '?')}"
            )


# --------------------------------------------------------------------------
# ml60 agent (spec §3.2) — only answers once the agent is deployed
# --------------------------------------------------------------------------

def probe_agent(env):
    url = env.get("ML60_AGENT_URL")
    if not url:
        print("  skipped: ML60_AGENT_URL not set")
        return
    # Caddy on ml60 serves the agent with a home-CA certificate, which the
    # default trust store does not know. Verified, never skipped.
    context = (ssl.create_default_context(cafile=str(ROOT / "certs" / "home-root.crt"))
               if url.startswith("https") else None)
    record = fetch(url, insecure=False, context=context)
    summarise("agent", record)
    if record.get("error"):
        print("  (expected until the agent is built and deployed — phase 2)")
    save("agent", {"agent_root": record})


SOURCES = {"pve": probe_pve, "agent": probe_agent}


def main():
    env = load_env()
    wanted = sys.argv[1:] or list(SOURCES)
    unknown = [name for name in wanted if name not in SOURCES]
    if unknown:
        sys.exit(f"Unknown source(s): {', '.join(unknown)}. "
                 f"Choose from: {', '.join(SOURCES)}")
    for name in wanted:
        print(f"\n=== {name} ===")
        SOURCES[name](env)
    print()


if __name__ == "__main__":
    main()

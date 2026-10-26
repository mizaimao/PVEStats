# Homelab dashboard — design specification

## 1. Purpose

A local dashboard, running on a macOS workstation, showing live status of a
Proxmox VE host and its guests. Different guests need different panels.

Explicit design constraints, in priority order:

1. **Nothing may be installed on the PVE host.** Read-only API access only.
   *Exception 2026-10-26, with Frank's go-ahead:* one read-only agent,
   `pvestats-pve` (`pve/`), a systemd service under a dynamic user with no
   capabilities and a read-only filesystem. It reads the host's and VMs'
   counters from world-readable kernel files every 2 s, beside the API every
   30 s, for less load on the host than the API every 10 s (measured,
   pve/README.md). Removal: pve/README.md.
2. **No SSH** anywhere in the data path.
3. **No writes to any homelab disk.** All persistence, if any, happens on the Mac.
4. Read-only credentials only. No root, no write-capable tokens.
5. One agent is permitted, on `ml60`, because its GPU is PCIe-passthrough and
   therefore invisible to the hypervisor.
   *Update 2026-10-26:* a second, on the router (`router/`), installed with
   Frank's go-ahead: Asuswrt-Merlin has only its admin account, so the router
   serves its own counters rather than the dashboard holding that login. It
   reads only, logs nothing and writes nothing to flash. Both dashboards poll
   it once a second when `ROUTER_AGENT_URL` is set, and turn its counters into
   rates themselves.
   *Update 2026-10-26 (later):* and one on TrueNAS (`nas/`), installed with
   Frank's go-ahead in place of its API: it runs as an unprivileged user,
   reads only, writes nothing, and costs a sixtieth of the API a request.
   Every agent's README ends with how to remove it completely: `router/`,
   `agent/` (ml60), `pve/`, `nas/`.

## 2. Architecture

```
┌─────────────── macOS ───────────────┐
│                                     │
│   collector  ──poll──┐              │
│      │               │              │
│      ├─ state (RAM)  │              │
│      │               │              │
│   http server :8080  │              │
│      │               │              │
│   browser page ──────┘              │
└─────────────────────┬───────────────┘
                      │
          ┌───────────┼───────────┬──────────────┐
          ▼           ▼           ▼              ▼
     PVE :8006   ml60 :9836   ml60 :11434   truenas :443
     (REST API)   (agent)      (Ollama)      (REST API)
```

**Why a local server rather than a pure browser page:** browsers block
cross-origin requests to the PVE API, and putting an API token in client-side
JavaScript would expose it. The collector holds all credentials and does all
outbound calls; the page only ever talks to `localhost`.

Suggested stack: Python 3.11+ with `aiohttp` (or Node with `express` +
`node-fetch`). No database. No build step. A single static HTML/CSS/JS page.

### Components

**Collector.** Independent async tasks, one per source, each on its own
interval. Writes into a shared in-memory state dict. Never blocks the HTTP
server. A failing source must not affect the others — it sets its own
`ok: false` and `error` string and the rest of the dashboard keeps working.

**State.** Plain dict of latest values, plus a fixed-length ring buffer
(`collections.deque(maxlen=...)`) per metric for sparklines. Memory only.
Discarded on restart. Sized for ~10 minutes of history at the relevant poll
rate.

**HTTP server.** Serves the static page and `GET /api/state` returning the
whole state as JSON. Binds `127.0.0.1` only.

**Page.** Polls `/api/state` every 1s. Renders host cards. No framework
required; vanilla JS is sufficient. Must degrade gracefully when a source is
`ok: false` — grey out that card, show the error, do not blank the page.

## 3. Data sources

### 3.1 Proxmox VE — all guests, host

Base: `https://<pve-host>:8006/api2/json`

Auth header, on every request:
```
Authorization: PVEAPIToken=dash@pve!web=<secret>
```

**The certificate is self-signed.** Disable verification (`ssl=False` in
aiohttp, `verify=False` in requests). Do not attempt to fix this.

| Endpoint | Purpose |
|---|---|
| `/cluster/resources?type=vm` | All guests, one call. Primary source. |
| `/nodes/{node}/status` | Host CPU, memory, loadavg, uptime, kernel |
| `/nodes/{node}/qemu/{vmid}/rrddata?timeframe=hour&cf=AVERAGE` | History, on demand |

**Field semantics — get these right:**

- `cpu` — fraction 0.0–1.0. Multiply by 100 for percent. **Not** already a
  percentage.
- `maxcpu` — assigned core count.
- `mem`, `maxmem`, `disk`, `maxdisk` — bytes.
- `netin`, `netout` — **cumulative** byte counters since guest boot. To get a
  rate, subtract the previous sample and divide by elapsed seconds. They reset
  to zero when the guest restarts, so clamp negative deltas to zero.
- `status` — `running` | `stopped`.
- `uptime` — seconds.
- `disk` for QEMU guests reports allocated, not in-guest used. Do not label it
  "disk used" — label it "allocated". Real filesystem usage would require
  `qemu-guest-agent`, which is out of scope.

**Poll interval: 10s.** Proxmox's `pvestatd` recomputes these values every 10
seconds. Polling faster returns identical numbers and only adds load and log
lines. This is a hard property of PVE, not a tuning choice.

**History:** `rrddata` returns arrays of `{time, cpu, mem, maxmem, netin,
netout, diskread, diskwrite}`. Timeframes: `hour`, `day`, `week`, `month`,
`year`. This data already exists on the PVE host — reading it adds no writes.
Fetch on demand when a user expands a card, not on the polling loop.

### 3.2 ml60 agent — GPU and loaded models

`GET http://ml60:9836/` — no auth (LAN only).

*Update 2026-10-14:* the agent now listens on ml60's 127.0.0.1:9836 only, and
Caddy publishes it as `https://pvestats.ml60.lan/` with a certificate from
Frank Home CA, like every other ml60 service. The collector verifies it
against `certs/home-root.crt`.

This agent is to be written as part of this project and deployed to ml60. It
reads NVML directly (library call, not a `nvidia-smi` subprocess) and merges in
Ollama's loaded-model list.

Response shape:

```json
{
  "gpu": {
    "util": 87,
    "mem_util": 42,
    "vram_used": 51539607552,
    "vram_total": 103079215104,
    "temp": 71,
    "power": 412.5,
    "power_limit": 600.0,
    "clock_sm": 2100,
    "fan": 55,
    "procs": [
      {"pid": 1234, "name": "ollama", "vram": 51539607552}
    ]
  },
  "models": [
    {"name": "qwen3.6:27b", "size": 30000000000, "size_vram": 30000000000,
     "expires_at": "2026-08-02T12:34:56Z"}
  ]
}
```

`models` comes from Ollama's `GET /api/ps` on `127.0.0.1:11434`, called from
inside the agent so the dashboard makes one request instead of two.
`size_vram` is bytes.

*Update 2026-10-26:* the Rust agent adds `"ram": {"total": …, "available":
…, "cache": …}`, ml60's memory in bytes from its own `/proc/meminfo`, or
null. PVE's figure for a VM counts the guest's disk cache as used (95% on
ml60 while its programs used 12%); used = total − available and cache =
buffers + page cache + reclaimable slab, as `free` counts them.

*Update 2026-10-26 (later):* and `"host": {"cpu": {"busy", "total"},
"cpus", "uptime", "net": {"iface", "rx", "tx"}, "disk": {"total", "used"},
"ip"}`, read from inside ml60 per request (see agent/README.md), so the
terminal dashboard's ml60 card takes its CPU, network, disk, uptime and
address from the agent, once a second, and needs PVE only as a fallback.

Agent requirements:
- `pip install nvidia-ml-py` — the official binding. Not the abandoned `pynvml`
  package on PyPI.
- Override `BaseHTTPRequestHandler.log_message` to a no-op. Without this, the
  default handler writes a stderr line per request; at 1 Hz that is ~86,400
  journal lines per day on ml60's disk, which violates constraint 3.
- `nvmlDeviceGetComputeRunningProcesses` needs permission to see other users'
  processes. Run the service as the same user as Ollama, or as root.
- If Ollama runs in Docker, process PIDs are namespaced and `procs[].name` will
  not say "ollama". Correlate VRAM by the `models` array instead of by process
  name.
- Wrap the Ollama call in a short timeout and a try/except. If Ollama is down,
  return `"models": []` and still serve GPU data.
- Ship a systemd unit. Do not use socket activation — NVML re-initialisation on
  each wake costs more than the ~20 MB the process idles at.

**Poll interval: 1s.** NVML calls are microseconds; this is genuinely
real-time.

### 3.3 TrueNAS — NIC and disk I/O

Base: `https://<truenas-host>/api/v2.0`
Auth: `Authorization: Bearer <api-key>` (generated in the TrueNAS UI, not the
shell).

*Update 2026-10-21:* as built, TrueNAS is read over JSON-RPC 2.0 on a
WebSocket, `wss://<truenas-host>/api/current`, logging in with
`auth.login_with_api_key` and a read-only (READONLY_ADMIN) key. REST is not
used: TrueNAS never applied roles to it, so it answers 403 to that same
read-only key, and REST is removed in TrueNAS 26. A `curl` check against
`/api/v2.0` therefore fails with the key the dashboard uses; test with
`tools/uv run probe.py truenas` instead. The same applies to the §2 diagram's
"truenas :443 (REST API)".

**Why this source exists at all:** the NAS's network interfaces are PCIe
passthrough. The hypervisor has no counters for a passed-through device, so
`netin`/`netout` from the PVE API are meaningless for this guest. The numbers
must come from inside TrueNAS.

*Update 2026-10-14:* the NAS no longer uses a passed-through NIC; its traffic
goes over the Proxmox bridge on a virtio NIC, so PVE's `netin`/`netout` are
valid for it again. This source still earns its place: per-disk I/O and pool
health exist only inside TrueNAS.

*Update 2026-10-26:* TrueNAS's API is retired. Both dashboards read TrueNAS's
own agent (`nas/` in this repo, `NAS_AGENT_URL`), which serves CPU, memory,
address, uptime, NICs, disks and pools from the kernel and ZFS, every 2 s.
The API cost TrueNAS 25 ms of CPU a request; the agent costs 0.41 ms
(`nas/README.md`). The rest of this section is the API's history.

**Endpoint discovery is required.** TrueNAS's reporting endpoints have moved
between releases. Do not hardcode a path from memory. Fetch the interactive API
documentation at `https://<truenas-host>/api/docs`, find the current reporting
namespace, and use what it says. Verify with `curl` before writing code against
it.

Needed: per-interface throughput, per-disk I/O, pool health and capacity.

**Poll interval: 5s.**

## 4. Views

### 4.1 PVE host card

CPU %, memory used/total, 1/5/15 load average, uptime, guest count
(running/total). Sparkline for CPU.

### 4.2 ml60

Merged from two sources — PVE for CPU/RAM, the agent for everything else.

- CPU % and RAM (PVE, 10s). *Update 2026-10-26:* both dashboards take RAM
  from the agent's `ram` (1s) while it is under three polls old, with the
  cache shown beside it, and PVE's figure otherwise. The collector passes
  it to the page as the guest's `ram`. The terminal dashboard takes CPU,
  network, disk, uptime and address from the agent's `host` the same way.
- GPU utilisation %, large and prominent (agent, 1s)
- VRAM used / total, with a bar (agent, 1s)
- Temperature, power draw vs limit, fan % (agent, 1s)
- Loaded models: name, VRAM, time until unload (agent, 1s)
- GPU processes: pid, name, VRAM (agent, 1s)

The 1s and 10s data live on the same card. Do not stall the fast panels waiting
for the slow poll.

### 4.3 TrueNAS

- CPU % and RAM (PVE, 10s)
- Per-interface RX/TX throughput (TrueNAS API, 5s)
- Per-disk read/write (TrueNAS API, 5s)
- Pool health and capacity (TrueNAS API, 5s)

*Update 2026-10-26:* all of these, CPU and RAM included, come from TrueNAS's
own agent every 2 s (see 3.3); PVE is not asked for this card.

Do not display PVE's `netin`/`netout` on this card — see 3.3. (Since
2026-10-14 they would be valid, but the card reads throughput from inside
TrueNAS, which also splits it per interface.)

### 4.4 All other guests

CPU %, RAM used/total, status, uptime. Nothing else. These should be compact —
a grid of small cards, not full-width panels.

### 4.5 Layout

Host cards driven by a config file, so adding a guest or changing its panel set
is an edit rather than a code change:

```yaml
hosts:
  - vmid: 100
    name: ml60
    view: gpu
  - vmid: 101
    name: truenas
    view: nas
  - vmid: 102
    name: hermes
    view: basic
```

`view` selects the panel set. Unknown VMIDs returned by PVE but absent from the
config default to `basic`. Addresses (the agent URL, the TrueNAS host) live in
`.env` with the credentials, not here.

## 5. Non-goals

- No alerting, no notification delivery.
- No historical storage. The PVE `rrddata` endpoint covers history that matters.
- No control actions. Read-only, including in the UI — no start/stop buttons.
  One exception, added 2026-08-20: the ml60 card can unload a loaded Ollama
  model (two clicks), through the agent. It frees VRAM only; nothing is
  written to disk, and the model reloads on its next request. Setting
  `PVESTATS_AGENT_ALLOW_UNLOAD=0` on the agent turns it off.
- No authentication on the local server. It binds `127.0.0.1`.
- No Docker, no build pipeline, no bundler.

## 6. Manual setup, performed by the operator

These are prerequisites, not tasks for the build.

**PVE token:**
```bash
pveum user add dash@pve
pveum aclmod / -user dash@pve -role PVEAuditor
pveum user token add dash@pve web --privsep 0
```
`PVEAuditor` is read-only. `--privsep 0` makes the token inherit exactly that
and nothing more.

**TrueNAS:** no credentials since 2026-10-26: its own agent answers on the
LAN without one (`nas/README.md`).

**Credentials** go in a `.env` file on the Mac, git-ignored, read by the
collector at startup. Never in the served page, never in the repo.

## 7. Implementation notes

- Every outbound call gets a timeout (2s for the agent, 5s for PVE and
  TrueNAS). A hung source must not stall the collector.
- On failure, keep the last known values and mark the source stale with an age
  in seconds. Do not show zeros — a blank card and a stale reading look very
  different to a reader.
- Round every displayed number. Bytes to GiB with one decimal; percentages to
  integers.
- Uptime as `3d 14h`, not seconds.
- Dark background. This will sit on a second monitor for long periods.
- The first thing to build is a script that dumps raw JSON from each source to
  a file. Write the parsing against real captured responses, not against the
  field names in this document. Where they disagree, the live response wins.

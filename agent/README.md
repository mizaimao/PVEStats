# pvestats-agent

Runs on ml60. Serves GPU telemetry and Ollama's loaded-model list as JSON on
`127.0.0.1:9836`, no auth. Caddy on ml60 publishes it to the LAN as
`https://pvestats.ml60.lan/`, with a certificate from Frank Home CA.

It also takes one write: `POST /unload` with `{"model": "<name>"}` sets
`keep_alive: 0` on that model, which is what `ollama stop` does, so the
dashboard's Stop button can free VRAM. Nothing is written to disk, and the
model reloads on its next request. It is on by default and, like `GET /`,
reaches the whole LAN through Caddy with no auth. It takes only a JSON body
of at most 4 KiB and refuses anything a browser sends cross-site (any
request with an `Origin` header), so a web page cannot use it. Set
`PVESTATS_AGENT_ALLOW_UNLOAD=0` to refuse it (403); the dashboard then hides
its Stop buttons.

This was the one agent the design permitted, because ml60's GPU is PCIe
passthrough and therefore invisible to the hypervisor (spec §3.2); the
router's is the second.

## The Rust agent

`rust/` is the agent as a small Rust program, in place of `agent.py`, with
the same replies (field for field, so the app and the dashboard need no
change) and these differences:

- **It also reports ml60's own memory**, as `"ram": {"total", "available",
  "cache"}` in bytes from `/proc/meminfo` (null if unreadable), the way
  `free` counts it: used is total minus available, and cache is buffers,
  page cache and reclaimable slab. PVE counts a VM's disk cache as used,
  so it showed ml60 at 95% while its programs used about 12%. Both
  dashboards show this figure while it is fresh; clients that don't know
  the field ignore it.
- **And the rest of what ml60's card shows**, as `"host"`, so the terminal
  dashboard needs nothing from Proxmox for it: CPU busy and total jiffies
  (`/proc/stat`), the number of cores, uptime, the default route's interface
  and its byte counters (`/proc/net/dev`), the largest local filesystem's
  total and used bytes (statvfs on mounts backed by a block device, so no
  NAS shares; total is used plus what users may still fill, as `df`'s Use%
  and Proxmox's guest agent count it, leaving out ext4's 5% kept for root),
  and ml60's LAN address (a UDP socket connected towards the gateway, which
  sends nothing). Counters are totals; the dashboard makes rates. CPU,
  uptime and network are read per request; disk and address by a thread of
  their own every 10 s, so a hung disk cannot hold up a request. Any part
  that cannot be read is null.

- **The GPU is read by one thread**, about once a second and only while
  someone has asked in the last 15 s; requests are served its latest
  reading. No client sets how often NVML is read, and the PCIe counters'
  ~20 ms sampling windows (two of them, ~40 ms a request in `agent.py`) are
  off the request path. NVML is shut down only after a device-level error,
  and tried again no sooner than 30 s later.
- **Ollama's list** is kept by one thread too, with `agent.py`'s cache: 30 s,
  or 5 s while a stopped model is still listed or Ollama is down. A reply
  that is not the expected shape counts as Ollama down instead of breaking
  the whole answer.
- **At most 16 connections**, more closed at once; each request read with a
  2 s deadline and a 16 KiB cap on its head, one request per connection.
- **Unload**: the same checks in the same order and with the same answers,
  then more: one unload at a time (409), not the same model again within
  10 s of stopping it (429), only a model that is loaded (404, from the
  cached list while it is fresh, so a loop of such requests never reaches
  Ollama), and at most one request a second sent on to Ollama (429). A 200
  that did not unload now says why in `error`, and a 502 carries Ollama's
  own reason.
- **NVML trouble** shows as such: a reading that has not come for 2.5 s is
  served as `gpu: null` with the reason, never as the last good one. A
  passing error in a minor figure (fan, clock, PCIe, power, processes)
  leaves that figure null for one reading; NVML is shut down only when the
  device, its utilisation or its memory cannot be read, and tried again
  after 1 s, then twice as long each time, up to 30 s.
- `procs[].name` is the last part of the path, as the dashboard shows it.
- The `error` text reads `NVML: <reason>`.

`agent.py` stays here as the way back until the Rust agent has run on ml60
for a while.

## Building

    agent/build

Tests and builds it on the build machine from `tui/deploy.conf`, into
`agent/bin/pvestats-agent` (git-ignored). NVIDIA's library is loaded at run
time, which needs a glibc build, so ml60 needs glibc 2.34 or newer (Ubuntu
22.04, Debian 12 and later); the build's last line says which version the
program asks for.

## Installing on ml60, in place of agent.py (Frank, with sudo)

From the Mac, after `agent/build`:

    scp agent/bin/pvestats-agent agent/pvestats-agent.service ml60.lan:/tmp/

Then on ml60:

    sudo install -m 755 /tmp/pvestats-agent /opt/pvestats-agent/pvestats-agent
    sudo cp /tmp/pvestats-agent.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl restart pvestats-agent
    sleep 1
    systemctl status pvestats-agent --no-pager | head -5
    curl -s http://127.0.0.1:9836/ | head -c 300; echo
    rm /tmp/pvestats-agent /tmp/pvestats-agent.service

A drop-in you made for `agent.py` (`User=`, `Environment=`) carries over
unchanged. Back to `agent.py`, should it ever be needed: the venv and
`agent.py` stay in `/opt/pvestats-agent/`, so a drop-in puts it back:

    sudo systemctl edit pvestats-agent
    # [Service]
    # ExecStart=
    # ExecStart=/opt/pvestats-agent/.venv/bin/python /opt/pvestats-agent/agent.py
    sudo systemctl restart pvestats-agent

Then publish it through Caddy, if it is not already. Add this block to
`/opt/ml60-stack/Caddyfile` (owned by frank, no sudo) and restart Caddy,
which briefly drops every ml60 HTTPS service:

    pvestats.ml60.lan {
    	tls internal
    	reverse_proxy 127.0.0.1:9836
    }

From the Mac, `curl -s https://pvestats.ml60.lan/` should answer.

The unit is replaced by every update, so never edit the installed unit for
this host: put local settings (`User=`, `Environment=` such as the unload
off switch) in a drop-in, which updates leave alone:

    sudo systemctl edit pvestats-agent     # writes .../pvestats-agent.service.d/override.conf
    systemctl cat pvestats-agent           # shows the unit and the drop-in together

A setting in the drop-in wins over the unit's own line.

The first install of `agent.py`, for the record: copy `agent.py`,
`requirements.txt` and `pvestats-agent.service` to `/opt/pvestats-agent/`,
build the venv there with `sudo python3 -m venv /opt/pvestats-agent/.venv`
(Debian and Ubuntu need `python3-venv` first) and
`sudo /opt/pvestats-agent/.venv/bin/pip install -r /opt/pvestats-agent/requirements.txt`,
then install the unit as above.

## Before enabling: check the service user

The unit ships with `User=ollama`. It must match whichever user Ollama
actually runs as, or NVML cannot see Ollama's compute processes and `procs[]`
comes back empty while every other field looks fine — a quiet failure.

    systemctl show ollama -p User

If it differs, set `User=`/`Group=` to match (or `root`) in a drop-in, not in
the unit file, which updates replace: after installing the unit and running
`daemon-reload`, run `sudo systemctl edit pvestats-agent` and add

    [Service]
    User=<that user>
    Group=<that user>

then `sudo systemctl restart pvestats-agent`.

`/opt` matters too: the unit sets `ProtectHome=true`, so installing under a
home directory will not start.

## Removing it completely (Frank, with sudo)

Everything the agent put on ml60: `/opt/pvestats-agent/` (the program, and
the old `agent.py` with its venv), the unit and any drop-in made for it, and
one block in Caddy's config. It runs as the existing `ollama` user, so no
user or group of its own is left behind. On ml60:

    sudo systemctl disable --now pvestats-agent
    sudo rm -f /etc/systemd/system/pvestats-agent.service
    sudo rm -rf /etc/systemd/system/pvestats-agent.service.d
    sudo systemctl daemon-reload
    sudo rm -rf /opt/pvestats-agent
    systemctl status pvestats-agent 2>&1 | head -1; ls /opt/pvestats-agent 2>&1

The last line should say the unit and the folder are gone. Then delete the
`pvestats.ml60.lan { … }` block from `/opt/ml60-stack/Caddyfile` (owned by
frank, no sudo) and restart Caddy the way `update_all.sh` does; left in
place, the name only answers 502. The `*.ml60.lan` DNS rewrite is shared
with every ml60 service and stays. Then empty `ML60_AGENT_URL` in `.env`
and run `tui/deploy`: ml60's card goes back to Proxmox's figures, with no
GPU.

## Troubleshooting

`active (running)` with an uptime of milliseconds means it is restarting
over and over (`Restart=always`). `journalctl -u pvestats-agent -n 30` has
the real error: a `GLIBC_2.xx not found` line means ml60's glibc is older
than the build's (see Building); `cannot listen` means something else holds
the port.

`warning: NVML unavailable at startup` means the driver's
`libnvidia-ml.so.1` could not be loaded or initialised; the agent keeps
serving the models and says why in `error`, and tries NVML again every 30 s.

Empty `procs[]` with every other field populated means the unit's `User=` does
not match Ollama's. NVML cannot see other users' compute processes.

Success on ml60's own `curl` but a failure from the Mac is Caddy: check the
`pvestats.ml60.lan` block. A certificate error on the Mac means the collector
is not reading `certs/home-root.crt`.

## Configuration

All optional, set with `Environment=` lines in a drop-in (`sudo systemctl
edit pvestats-agent`, under `[Service]`), so an update keeps them:

| Variable | Default |
|---|---|
| `PVESTATS_AGENT_BIND` | `127.0.0.1` |
| `PVESTATS_AGENT_PORT` | `9836` |
| `PVESTATS_GPU_INDEX` | `0` |
| `OLLAMA_URL` | `http://127.0.0.1:11434/api/ps` (http only: Ollama is on loopback) |
| `PVESTATS_AGENT_ALLOW_UNLOAD` | `1`; `0`, `false`, `no` or `off` refuse `POST /unload` |
| `OLLAMA_GENERATE_URL` | `OLLAMA_URL` with `/api/ps` swapped for `/api/generate` |

## Disk writes

The agent persists nothing. The NVML handle, the latest GPU reading and the
cached Ollama model list live in memory and are rebuilt on start. What is
left is incidental, and the unit closes it:

| Source | Status |
|---|---|
| Per-request access log | none — at 1 Hz this alone would be ~86,400 lines/day |
| stdout | `StandardOutput=null` — both startup messages go to stderr, so nothing is lost |
| stderr | kept: two lines per start |
| Core files | `LimitCORE=0` — a crash loop would otherwise write one each time |
| `/tmp`, `/var/tmp` | `TemporaryFileSystem=` — a fresh tmpfs each, memory-backed (`PrivateTmp=true` would keep them in the host's on-disk `/var/tmp`) |
| Anything else | `ProtectSystem=strict` makes the filesystem read-only to the unit |

For zero journal writes, set `StandardError=null` too — the cost is that a
crash becomes silent apart from the exit code in `systemctl status`.

System-wide, `Storage=volatile` in `/etc/systemd/journald.conf` keeps *all*
journal data in `/run` (tmpfs) rather than on disk. That affects every service
on ml60, not just this one, so it is your call rather than something the unit
should assume.

## Notes

If Ollama runs in Docker, its PIDs are namespaced, so `procs[].name` will not
say "ollama". This is expected; VRAM is attributed through the `models` array
rather than by matching process names.

Metrics are read individually, and one the board does not offer
(`NOT_SUPPORTED`, `NOT_FOUND`, `NO_PERMISSION`, `FUNCTION_NOT_FOUND`), such as
fan speed on a fanless datacenter board, comes back `null` instead of costing
the whole payload.

Any other NVML error (a GPU lost from the bus, for instance) fails the read:
the agent still serves `models`, reports the reason in a top-level `error`
field so the dashboard can say why the GPU panel is empty, shuts NVML down,
and initialises it afresh no sooner than 30 s later.

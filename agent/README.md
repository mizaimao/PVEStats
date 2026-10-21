# pvestats-agent

Runs on ml60. Serves GPU telemetry and Ollama's loaded-model list as JSON on
`127.0.0.1:9836`, no auth. Caddy on ml60 publishes it to the LAN as
`https://pvestats.ml60.lan/`, with a certificate from Frank Home CA.

It also takes one write: `POST /unload` with `{"model": "<name>"}` sets
`keep_alive: 0` on that model, which is what `ollama stop` does, so the
dashboard's Stop button can free VRAM. Nothing is written to disk, and the
model reloads on its next request. It is on by default and, like `GET /`,
reaches the whole LAN through Caddy with no auth. Set
`PVESTATS_AGENT_ALLOW_UNLOAD=0` to refuse it (403); the dashboard then hides
its Stop buttons.

This is the one agent the design permits, because ml60's GPU is PCIe
passthrough and therefore invisible to the hypervisor (spec §3.2).

## Deploy

Debian and Ubuntu ship `ensurepip` separately, so `python3 -m venv` fails on a
stock install. Match the version to the system Python:

    sudo apt install -y python3-venv     # or python3.13-venv, etc.

Copy `agent.py` and `requirements.txt` to ml60 under `/opt/pvestats-agent/`,
then:

    sudo python3 -m venv /opt/pvestats-agent/.venv
    sudo /opt/pvestats-agent/.venv/bin/pip install -r /opt/pvestats-agent/requirements.txt

    sudo cp pvestats-agent.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable --now pvestats-agent

Verify:

    curl -s http://localhost:9836/ | python3 -m json.tool

Then publish it through Caddy. Add this block to `/opt/ml60-stack/Caddyfile`
(owned by frank, no sudo) and restart Caddy, which briefly drops every ml60
HTTPS service:

    pvestats.ml60.lan {
    	tls internal
    	reverse_proxy 127.0.0.1:9836
    }

From the Mac, `curl -s https://pvestats.ml60.lan/` should answer.

## Before enabling: check the service user

The unit ships with `User=ollama`. It must match whichever user Ollama
actually runs as, or NVML cannot see Ollama's compute processes and `procs[]`
comes back empty while every other field looks fine — a quiet failure.

    systemctl show ollama -p User

Set the unit's `User=`/`Group=` to match, or use `root`.

`/opt` matters too: the unit sets `ProtectHome=true`, so installing under a
home directory will not start.

## Troubleshooting

`systemctl status` showing `Main PID: NNNN ((python))` — name in double
parentheses — means systemd could not execute the interpreter at all, usually
a missing or half-built venv. With `Restart=always` the unit flaps rather than
failing outright, so `active (running)` with an uptime of milliseconds is the
same symptom. `journalctl -u pvestats-agent -n 30` has the real error.

`ModuleNotFoundError: No module named 'pynvml'` means the venv exists but is
empty — `python3 -m venv` builds its directory structure before running
`ensurepip`, so a failure there leaves a working `bin/python` with no `pip`
and no packages. Delete the venv and rebuild it; do not try to repair it.

Empty `procs[]` with every other field populated means the unit's `User=` does
not match Ollama's. NVML cannot see other users' compute processes.

Success on ml60's own `curl` but a failure from the Mac is Caddy: check the
`pvestats.ml60.lan` block. A certificate error on the Mac means the collector
is not reading `certs/home-root.crt`.

## Configuration

All optional, set via systemd `Environment=`:

| Variable | Default |
|---|---|
| `PVESTATS_AGENT_BIND` | `127.0.0.1` |
| `PVESTATS_AGENT_PORT` | `9836` |
| `PVESTATS_GPU_INDEX` | `0` |
| `OLLAMA_URL` | `http://127.0.0.1:11434/api/ps` |
| `PVESTATS_AGENT_ALLOW_UNLOAD` | `1`; `0` refuses `POST /unload` |
| `OLLAMA_GENERATE_URL` | `OLLAMA_URL` with `/api/ps` swapped for `/api/generate` |

## Disk writes

The agent persists nothing. The NVML handle and the cached Ollama model list
live in memory and are rebuilt on start; every response is assembled per
request. What is left is incidental, and the unit closes it:

| Source | Status |
|---|---|
| Per-request access log | disabled — at 1 Hz this alone would be ~86,400 lines/day |
| Python bytecode (`.pyc`) | `PYTHONDONTWRITEBYTECODE=1`, and `ProtectSystem=strict` blocks it anyway |
| stdout | `StandardOutput=null` — both startup messages go to stderr, so nothing is lost |
| stderr | kept: two lines per start, plus a traceback if it dies |
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
field so the dashboard can say why the GPU panel is empty, and initialises
NVML afresh on the next request.

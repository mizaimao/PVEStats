# PVEStats

Local dashboard for a Proxmox VE host and its guests. Runs on a macOS
workstation, binds `127.0.0.1` only.

Design: [homelab-dashboard-spec.md](homelab-dashboard-spec.md).

Nothing is installed on the PVE host, nothing goes over SSH, nothing is written
to any homelab disk. Read-only credentials throughout. The one exception is an
agent on `ml60`, whose GPU is PCIe passthrough and therefore invisible to the
hypervisor.

## Layout

    probe.py       dump raw source responses to captures/ (run this first)
    config.yaml    which guest gets which panel set
    .env           credentials, git-ignored
    captures/      raw source dumps, git-ignored

## Setup

Prerequisites are performed by the operator, not the build — see spec §6 for
the PVE token and TrueNAS API key.

    python3 -m venv .venv && source .venv/bin/activate
    pip install -r requirements.txt
    cp .env.example .env      # then fill it in

## Running it

Double-click `make-app.command` once. It builds `~/Applications/PVEStats.app`
with this repo's path baked in — open that from Finder and keep it in the Dock.

Launching it starts the collector, opens the dashboard in a chrome-less
window, and shuts the collector down when you close that window. It is not a
launch agent and not a daemon; nothing runs when the window is shut.

`launch.command` does the same thing and is double-clickable on its own if you
would rather skip the app bundle. Re-run `make-app.command` if you move the
repo.

Without Google Chrome the dashboard opens as an ordinary tab in your default
browser instead — Safari has no app-window mode — and the Terminal window has
to stay open to keep the collector alive.

To run it in a terminal instead:

    .venv/bin/python run.py        # http://127.0.0.1:8080

## Probe first

Parsing is written against real captured responses, not against field names in
the spec. Where they disagree, the live response wins.

    python3 probe.py              # every source
    python3 probe.py pve          # one source

Needs no venv — stdlib only.

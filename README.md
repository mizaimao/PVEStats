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

## Probe first

Parsing is written against real captured responses, not against field names in
the spec. Where they disagree, the live response wins.

    python3 probe.py              # every source
    python3 probe.py pve          # one source

Needs no venv — stdlib only.

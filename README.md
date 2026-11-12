# PVEStats

Local dashboard for a Proxmox VE host and its guests. Runs on a macOS
workstation, binds `127.0.0.1` only.

Design: [homelab-dashboard-spec.md](homelab-dashboard-spec.md).

Nothing goes over SSH and nothing is written to any homelab disk. Read-only
credentials throughout. Four small read-only agents (Rust) serve what
Proxmox's API cannot tell, or tells at more cost: on `ml60`, whose GPU is PCIe
passthrough and therefore invisible to the hypervisor, on the router, on the
Proxmox host and on TrueNAS. Each one's README says what it may do and how
to remove it completely. The one thing the dashboard changes anywhere: the
page's Stop button asks ml60's agent to unload a model.

## Layout

    tui/           the dashboard program (Rust): polls every source, and draws
                   in a terminal or, with --serve, serves the page below
    static/        the page: the Mac app's window shows it
    app/           the Mac app's window (Rust), PVEStats.app
    agent/         ml60's agent: GPU, loaded models, the machine's own figures
    pve/           the Proxmox host's agent: the host's and VMs' live figures
    nas/           TrueNAS's agent: interfaces, disks, pools
    router/        the router's agent: WAN, ports, radios
    tools/         uv for the Python tools, and the build scripts' helpers
    certs/         the home CA's root, which ml60's agent is verified against
    probe.py       dump raw source responses to captures/ (run this first)
    config.yaml    which guest gets which panel set
    .env           credentials, git-ignored
    captures/      raw source dumps, git-ignored

## Setup

Prerequisites are performed by the operator, not the build — see spec §6 for
the PVE token. TrueNAS needs no credential: its own agent answers.

    cp .env.example .env      # then fill it in

Python is only for `probe.py` (below) and redrawing the icon. It comes from
`tools/uv`, which keeps uv, the Python it manages and its cache in `.tools/`
(git-ignored). Nothing depends on pyenv, Homebrew or the system Python.
`uv.lock` pins every installed package by hash; `.python-version` picks
3.13. Add or change a dependency in `pyproject.toml`, then `tools/uv lock`
and commit `uv.lock` with it. Never pip.

## Running it

**macOS.** Build `PVEStats.app` in this folder on a Mac, macserver (or the
laptop with Frank's OK), then double-click it here or drag it to the Dock:

    app/make-app

It is one program, `app/` (Rust): it starts the dashboard program (`tui/`,
inside the app) with `--serve` and shows the page it serves in a window,
drawn by WKWebView, the web engine macOS already has. Closing the window or
quitting ends the dashboard program too. Not a launch agent, not a daemon:
nothing runs when the window is shut. The app reads `.env` and
`config.yaml` from the folder it sits in, and writes nothing outside it: its
window is a private one, so WebKit keeps the page's data in memory.

**Elsewhere,** or to an ordinary browser instead of a window:

    tui/target/release/pvestats-tui --serve     # http://127.0.0.1:8080

## Probe first

Parsing is written against real captured responses, not against field names in
the spec. Where they disagree, the live response wins.

    tools/uv run probe.py         # Proxmox's API and ml60's agent
    tools/uv run probe.py pve     # one of the two

It needs only the standard library. The other agents (router, Proxmox host,
TrueNAS) are not probed: each answers plain JSON to `curl` on its URL in
`.env`.

## In a terminal

`tui/` is the same dashboard as a terminal program in Rust, for an always-on
screen such as a spare machine's text console. It reads the same `.env` and
`config.yaml`. See [tui/README.md](tui/README.md).

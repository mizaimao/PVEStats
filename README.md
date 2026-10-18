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

Python and every package come from `tools/uv`, which keeps uv, the Python it
manages and its cache in `.tools/` (git-ignored). Nothing depends on pyenv,
Homebrew or the system Python; `uv.lock` pins every package by hash, and
`.python-version` picks 3.13.

    tools/uv sync             # fetches uv, Python and packages on first run
    cp .env.example .env      # then fill it in

Add or change a dependency in `pyproject.toml`, then `tools/uv lock` and
commit `uv.lock` with it. Never pip.

## Running it

    .venv/bin/python app.py

Opens the dashboard in a native window and stops the collector when you close
it. Not a launch agent, not a daemon — nothing runs when the window is shut.

The window uses whatever webview the OS already provides — WKWebView on macOS,
WebView2 on Windows, WebKitGTK on Linux. No browser engine is bundled and no
particular browser needs to be installed.

**macOS.** Double-click `launch.command`. Or run `make-app.command` once to
build `PVEStats.app` in this folder, and double-click that instead — it is
git-ignored and nothing is written outside the project. Re-run it if you move
the repo.

`tools/uv` pins the Apple silicon build of uv. Elsewhere, install uv your own
way and run `uv sync`.

**Windows.** `uv run app.py`. WebView2 ships with Windows 11 and current
Windows 10; otherwise install the Evergreen runtime from Microsoft.

**Linux.** Needs the WebKitGTK bindings, which no package index supplies:

    sudo apt install python3-gi python3-gi-cairo gir1.2-webkit2-4.1   # Debian/Ubuntu
    sudo dnf install python3-gobject webkit2gtk4.1                    # Fedora

They belong to the system Python, which a uv-managed one cannot see. Build
the venv from the system Python with `--system-site-packages` instead.

To serve to an ordinary browser instead of a window:

    .venv/bin/python run.py        # http://127.0.0.1:8080

## Probe first

Parsing is written against real captured responses, not against field names in
the spec. Where they disagree, the live response wins.

    python3 probe.py              # every source
    python3 probe.py pve          # one source

Needs no venv — stdlib only.

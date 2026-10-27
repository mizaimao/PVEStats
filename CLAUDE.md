# PVEStats

Frank's rules for this project are in his knowledgebase
(`~/Projects/knowledgebase/projects.md`, section PVEStats). Design:
`homelab-dashboard-spec.md`.

## Python

- Everything Python goes through `tools/uv` (uv, its Python and its cache
  all inside `.tools/`). Never pip, pyenv, Homebrew's or the system Python
  (Frank, 2026-10-18). Dependencies live in `pyproject.toml`; commit
  `uv.lock` with any change to them.

## Git

- Always commit. Commit each change once it is done and tested, without
  asking first (Frank, 2026-10-14).
- Push to origin after each commit, without asking (Frank, 2026-10-25).

## Terminal dashboard (tui/)

- After any session that changes `tui/` or `config.yaml`, run `tui/deploy`
  before reporting back; no need to ask first (Frank, 2026-10-25). It tests
  and builds on the build machine, copies the repo to the machine whose
  screen shows the dashboard, and restarts it there. It never uses sudo. A
  failing test stops it before anything is copied.
- The machines are named only in `tui/deploy.conf`, which is git-ignored.
  Keep `tui/deploy` and `tui/deploy.conf.example` free of Frank's host
  names, users and addresses. If the file is missing, copy the example; the
  values are in Frank's knowledgebase (`hosts.md`), or ask him.
- `tui/deploy --check` prints the machines and tests both ssh logins.
- Installing the screen machine's service the first time needs sudo: hand
  Frank the commands in `tui/README.md`.
- `tui/screenshot [file.png]` saves what the screen machine's monitor
  shows, pointer included, and says whether the kiosk has a pointing
  device. Check visual changes with it rather than asking Frank to look.
- `tui/kiosk off | on | status` switches the kiosk's monitor off and
  pauses the dashboard (no machine is polled), or back on. It runs
  `tui/kiosk-remote` on the screen machine, the gatekeeper moose-mcp's key
  is limited to (MOO-200). A deploy never copies `tui/kiosk-remote` and
  warns when the kiosk's copy differs; after a change is reviewed and
  committed, `tui/deploy --gatekeeper` tests it and puts it there. A deploy turns
  it back on, so check `tui/kiosk status` before deploying while Frank has
  it off, and ask him (Frank, 2026-10-26: he turns it off to measure power).
- `tui: demo: true` in the screen machine's copy of config.yaml puts
  made-up models and GPU processes on ml60's card; the next deploy, which
  copies config.yaml over, takes it off again.

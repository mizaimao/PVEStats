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

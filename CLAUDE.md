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

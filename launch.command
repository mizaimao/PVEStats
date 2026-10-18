#!/bin/bash
# Double-click this in Finder to run PVEStats.
#
# Opens the dashboard in a native window and stops the collector when that
# window closes. Not a launch agent, not a daemon — it exists only while the
# window is open.

cd "$(dirname "$0")" || exit 1

# Started from PVEStats.app there is no Terminal, so anything echoed here
# vanishes and the app simply never opens. Without one, say it in a dialog.
fail() {
  echo "$1"
  if [ -t 0 ]; then
    read -r -p "Press return to close."
  else
    osascript -e 'on run argv' \
      -e 'display alert "PVEStats could not start" message (item 1 of argv) as critical' \
      -e 'end run' "$1" >/dev/null 2>&1
  fi
  exit 1
}

if [ ! -f .env ]; then
  fail "No .env found. Copy .env.example to .env and fill it in."
fi

# Python and every package come from tools/uv, inside this repo. This brings
# .venv in line with uv.lock, using a Python uv downloaded into .tools/python,
# and replaces a .venv built from any other Python. Instant when nothing has
# changed; only the very first launch needs the network.
if ! problem=$(tools/uv sync --frozen --quiet 2>&1); then
  fail "Setting up Python failed.

$(printf '%s\n' "$problem" | tail -5)"
fi

exec .venv/bin/python app.py

#!/bin/bash
# Double-click this in Finder to run PVEStats.
#
# Opens the dashboard in a native window and stops the collector when that
# window closes. Not a launch agent, not a daemon — it exists only while the
# window is open.

cd "$(dirname "$0")" || exit 1

if [ ! -f .env ]; then
  echo "No .env found. Copy .env.example to .env and fill it in."
  read -r -p "Press return to close."
  exit 1
fi

if [ ! -x .venv/bin/python ]; then
  echo "Creating virtualenv…"
  python3 -m venv .venv || { read -r -p "venv failed. Press return."; exit 1; }
fi

# Cheap check that dependencies are present; installs only when something is
# missing, so normal launches stay instant.
if ! .venv/bin/python -c "import aiohttp, yaml, dotenv, webview" 2>/dev/null; then
  echo "Installing dependencies…"
  .venv/bin/pip install -q -r requirements.txt || {
    read -r -p "pip install failed. Press return."; exit 1; }
fi

exec .venv/bin/python app.py

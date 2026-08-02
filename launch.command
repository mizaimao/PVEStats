#!/bin/bash
# Double-click this in Finder to run PVEStats.
#
# Starts the collector, opens the dashboard in its own chrome-less window, and
# shuts the collector down when that window closes. Nothing is left running.
#
# Not a launch agent, not a daemon — it exists only while the window is open.

cd "$(dirname "$0")" || exit 1

PORT="$(sed -n 's/^DASHBOARD_PORT=//p' .env 2>/dev/null | tr -d '[:space:]')"
PORT="${PORT:-8080}"
URL="http://127.0.0.1:${PORT}/"

if [ ! -f .env ]; then
  echo "No .env found. Copy .env.example to .env and fill it in."
  read -r -p "Press return to close."
  exit 1
fi

if [ ! -x .venv/bin/python ]; then
  echo "Creating virtualenv…"
  python3 -m venv .venv || { read -r -p "venv failed. Press return."; exit 1; }
  .venv/bin/pip install -q -r requirements.txt || {
    read -r -p "pip install failed. Press return."; exit 1; }
fi

# Reuse an instance that is already up rather than failing on a bound port.
if curl -sf -o /dev/null --max-time 1 "$URL"; then
  echo "PVEStats is already running on ${PORT}."
  OWNS_SERVER=0
else
  .venv/bin/python run.py &
  SERVER_PID=$!
  OWNS_SERVER=1
  # Stop the collector however this script exits — normal close, ctrl-c, or error.
  trap 'kill "$SERVER_PID" 2>/dev/null' EXIT INT TERM

  printf "Starting collector"
  for _ in $(seq 1 40); do
    if curl -sf -o /dev/null --max-time 1 "$URL"; then break; fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo " — collector exited. Run .venv/bin/python run.py to see why."
      read -r -p "Press return to close."
      exit 1
    fi
    printf "."
    sleep 0.25
  done
  echo " ready on ${PORT}."
fi

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

if [ -x "$CHROME" ]; then
  # --app gives a window with no tabs or address bar. A dedicated profile makes
  # this its own process, so closing the window ends this script — and does not
  # touch or depend on your normal browsing session.
  "$CHROME" \
    --app="$URL" \
    --user-data-dir="$HOME/Library/Application Support/PVEStats/chrome" \
    --no-first-run \
    --no-default-browser-check \
    --window-size=1500,1000 \
    >/dev/null 2>&1
else
  # No Chrome: fall back to the default browser. Safari has no app mode, so the
  # dashboard opens as an ordinary tab and this window has to stay open.
  open "$URL"
  if [ "$OWNS_SERVER" = "1" ]; then
    echo "Dashboard opened in your browser."
    echo "Close this Terminal window (or press ctrl-c) to stop the collector."
    wait "$SERVER_PID"
  fi
fi

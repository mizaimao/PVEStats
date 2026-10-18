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

# Needs Python 3.11 or newer. PVEStats.app starts with launchd's bare PATH,
# where python3 is Apple's 3.9; pyenv's shims honour .python-version instead.
[ -d "$HOME/.pyenv/shims" ] && PATH="$HOME/.pyenv/shims:$PATH"
new_enough() { "$1" -c 'import sys; sys.exit(sys.version_info < (3, 11))' 2>/dev/null; }

if [ -x .venv/bin/python ] && ! new_enough .venv/bin/python; then
  fail ".venv runs $(.venv/bin/python --version 2>&1); PVEStats needs 3.11 or newer. Remove .venv and launch again to rebuild it."
fi

if [ ! -x .venv/bin/python ]; then
  if ! new_enough python3; then
    fail "python3 is $(python3 --version 2>&1); PVEStats needs 3.11 or newer."
  fi
  echo "Creating virtualenv…"
  python3 -m venv .venv || fail "Creating the virtualenv failed."
fi

# Every source is HTTPS and pip needs ssl too. A pyenv Python built against
# Homebrew's OpenSSL loses it when Homebrew removes that version, and
# reinstalling packages cannot fix that, so name the cause.
if ! problem=$(.venv/bin/python -c "import ssl" 2>&1); then
  fail "Python's ssl module does not load, so nothing can connect.

$(printf '%s\n' "$problem" | grep -m1 'Error')

If the missing library is Homebrew's OpenSSL, reinstall that version, e.g. brew install openssl@3."
fi

# Cheap check that dependencies are present; installs only when something is
# missing, so normal launches stay instant.
if ! .venv/bin/python -c "import aiohttp, yaml, dotenv, webview" 2>/dev/null; then
  echo "Installing dependencies…"
  .venv/bin/pip install -q -r requirements.txt || fail "pip install failed."
fi

exec .venv/bin/python app.py

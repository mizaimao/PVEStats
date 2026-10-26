"""Credentials from .env, card layout from config.yaml.

Credentials are read once at startup and never leave the collector — they are
not in the served page and not in /api/state (spec §6).
"""

import functools
import json
import math
import os
import ssl
import tempfile
from pathlib import Path

import yaml
from dotenv import load_dotenv

ROOT = Path(__file__).resolve().parent.parent

# Appearance defaults live in config.yaml where they can be hand-edited with
# comments intact. Changes made in the UI are written here instead, because
# round-tripping config.yaml through PyYAML would strip every comment in it.
UI_STATE = ROOT / "ui.json"

# Root of Frank Home CA. Caddy on ml60 serves the agent with a certificate
# from this CA, which the Mac's Python does not trust on its own. A public
# certificate, so it is committed; it expires 2036-10-08.
HOME_CA = ROOT / "certs" / "home-root.crt"

UI_DEFAULTS = {
    "theme": "ice",
    "glow": 1.0,
    # Seconds of history kept per source, and so the span of every chart.
    "window": 180,
    # Textural overlays, each independently switchable.
    "scanlines": False,
    "grain": False,
    "aurora": False,
}


def clean_ui(values):
    """The known appearance settings whose values have the default's type.

    config.yaml and ui.json are edited by hand, and /api/ui takes whatever a
    local client sends. A value of the wrong type is dropped, so the default
    stands, rather than being saved, or stopping the next launch the way a
    null or "5m" window did. Not a dict at all means nothing usable.
    """
    if not isinstance(values, dict):
        return {}
    clean = {}
    for key, default in UI_DEFAULTS.items():
        if key not in values:
            continue
        value = values[key]
        if isinstance(default, bool):
            ok = isinstance(value, bool)
        elif isinstance(default, (int, float)):
            # bool is an int in Python, and NaN or infinity cannot size a
            # buffer.
            ok = (isinstance(value, (int, float)) and not isinstance(value, bool)
                  and math.isfinite(value))
        else:
            ok = isinstance(value, type(default))
        if ok:
            clean[key] = value
    return clean


class Config:
    def __init__(self, env_path=None, config_path=None):
        load_dotenv(env_path or ROOT / ".env")

        self.pve_host = os.getenv("PVE_HOST", "").strip()
        self.pve_node = os.getenv("PVE_NODE", "").strip() or None
        self.pve_token_id = os.getenv("PVE_TOKEN_ID", "").strip()
        self.pve_token_secret = os.getenv("PVE_TOKEN_SECRET", "").strip()

        self.truenas_host = os.getenv("TRUENAS_HOST", "").strip()
        self.truenas_api_key = os.getenv("TRUENAS_API_KEY", "").strip()

        self.agent_url = os.getenv("ML60_AGENT_URL", "").strip()
        # Optional: with none, the page has no router card at all.
        self.router_url = os.getenv("ROUTER_AGENT_URL", "").strip()
        # `DASHBOARD_PORT=` with nothing after it reads as "", not unset.
        self.port = int(os.getenv("DASHBOARD_PORT", "").strip() or "8080")

        raw = yaml.safe_load((config_path or ROOT / "config.yaml").read_text())
        # `hosts:` with every entry commented out reads as None, not [].
        self.hosts = {int(h["vmid"]): h for h in ((raw or {}).get("hosts") or [])}
        self.ui_defaults = {**UI_DEFAULTS, **clean_ui((raw or {}).get("ui"))}

    def _saved_ui(self):
        """Just what the settings panel has changed, as stored in ui.json."""
        try:
            return clean_ui(json.loads(UI_STATE.read_text()))
        except (OSError, ValueError):
            return {}   # absent or unreadable — nothing changed here

    def ui(self):
        """Appearance settings: config.yaml defaults, overridden by ui.json."""
        return {**self.ui_defaults, **self._saved_ui()}

    def save_ui(self, changes):
        """Persist appearance settings, keeping unknown keys and bad values out.

        ui.json holds only the settings changed here. Writing the whole merged
        set pinned config.yaml's values too, so later edits to its ui block
        were silently ignored.

        Written atomically so a crash mid-write cannot leave a truncated file
        that would then be silently ignored on next start.
        """
        saved = {**self._saved_ui(), **clean_ui(changes)}
        handle, temporary = tempfile.mkstemp(dir=str(ROOT), suffix=".tmp")
        try:
            with os.fdopen(handle, "w") as fh:
                json.dump(saved, fh, indent=2)
            os.replace(temporary, UI_STATE)
        except BaseException:
            Path(temporary).unlink(missing_ok=True)
            raise
        return {**self.ui_defaults, **saved}

    @property
    def pve_auth_header(self):
        return f"PVEAPIToken={self.pve_token_id}={self.pve_token_secret}"

    @property
    def pve_base(self):
        return f"https://{self.pve_host}:8006/api2/json"

    @functools.cached_property
    def agent_ssl(self):
        """TLS for agent requests: trust the home CA root and nothing else.

        True (aiohttp's default checks) for a plain-http URL, where it is
        unused. Verification is never switched off.
        """
        if not self.agent_url.startswith("https://"):
            return True
        return ssl.create_default_context(cafile=str(HOME_CA))

    @property
    def truenas_ws_url(self):
        # JSON-RPC over WebSocket, not REST — TrueNAS never wired role-based
        # access control into its REST framework, so a read-only key is
        # refused there regardless of configuration.
        return f"wss://{self.truenas_host}/api/current"

    def view_for(self, vmid):
        """Panel set for a guest. Unknown VMIDs default to `basic` (§4.5)."""
        return self.hosts.get(vmid, {}).get("view", "basic")

    @property
    def pve_configured(self):
        return bool(self.pve_host and self.pve_token_id and self.pve_token_secret)

    @property
    def truenas_configured(self):
        return bool(self.truenas_host and self.truenas_api_key)

    @property
    def agent_configured(self):
        return bool(self.agent_url)

    @property
    def router_configured(self):
        return bool(self.router_url)

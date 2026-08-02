"""Credentials from .env, card layout from config.yaml.

Credentials are read once at startup and never leave the collector — they are
not in the served page and not in /api/state (spec §6).
"""

import os
from pathlib import Path

import yaml
from dotenv import load_dotenv

ROOT = Path(__file__).resolve().parent.parent


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
        self.port = int(os.getenv("DASHBOARD_PORT", "8080"))

        raw = yaml.safe_load((config_path or ROOT / "config.yaml").read_text())
        self.hosts = {int(h["vmid"]): h for h in (raw or {}).get("hosts", [])}

    @property
    def pve_auth_header(self):
        return f"PVEAPIToken={self.pve_token_id}={self.pve_token_secret}"

    @property
    def pve_base(self):
        return f"https://{self.pve_host}:8006/api2/json"

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

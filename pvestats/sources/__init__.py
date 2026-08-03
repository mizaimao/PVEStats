"""One independent poller per data source.

Each runs on its own interval and never blocks the others. A source that fails
sets its own `ok: false` and error string; the rest of the dashboard keeps
working (spec §2). Every outbound call has a timeout (spec §7).

All field handling was written against captures/, not against the field names
in the design document — where they disagreed, the live response won.
"""

from .base import (
    AGENT_INTERVAL,
    AGENT_TIMEOUT,
    PVE_INTERVAL,
    PVE_TIMEOUT,
    TRUENAS_INTERVAL,
    TRUENAS_TIMEOUT,
    run_poller,
)
from .agent import AgentSource
from .pve import PveSource
from .truenas import TrueNasSource

__all__ = [
    "AGENT_INTERVAL", "AGENT_TIMEOUT", "PVE_INTERVAL", "PVE_TIMEOUT",
    "TRUENAS_INTERVAL", "TRUENAS_TIMEOUT", "run_poller",
    "AgentSource", "PveSource", "TrueNasSource",
]

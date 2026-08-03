"""The ml60 GPU agent (spec §3.2)."""

from .base import AGENT_TIMEOUT

# --------------------------------------------------------------------------
# ml60 agent
# --------------------------------------------------------------------------

class AgentSource:
    def __init__(self, config, source):
        self.config = config
        self.source = source

    async def poll(self, session):
        async with session.get(self.config.agent_url, timeout=AGENT_TIMEOUT) as response:
            response.raise_for_status()
            payload = await response.json()

        gpu = payload.get("gpu")
        if gpu is None:
            # NVML failed on ml60 but Ollama data may still be present.
            raise RuntimeError(payload.get("error") or "agent reported no GPU")

        models = []
        for model in payload.get("models") or []:
            models.append({
                "name": model.get("name"),
                "size": model.get("size"),
                "size_vram": model.get("size_vram"),
                # Converted here so the browser never has to parse Ollama's
                # timestamp, which carries nanosecond precision and a local
                # offset rather than the documented Z form.
                "unload_in": _seconds_until(model.get("expires_at")),
            })

        procs = []
        for proc in gpu.get("procs") or []:
            name = proc.get("name") or "?"
            procs.append({
                "pid": proc.get("pid"),
                # Ollama's worker reports an absolute path; show the leaf.
                "name": name.rsplit("/", 1)[-1],
                "vram": proc.get("vram"),
            })

        self.source.push("gpu.util", gpu.get("util"))
        self.source.push("gpu.vram_used", gpu.get("vram_used"))
        # VRAM as a percentage too, so utilisation and memory can share one
        # 0-100 plot the way nvtop draws them.
        if gpu.get("vram_total"):
            self.source.push("gpu.vram_pct",
                             gpu["vram_used"] / gpu["vram_total"] * 100)
        self.source.push("gpu.power", gpu.get("power"))
        self.source.push("gpu.temp", gpu.get("temp"))

        self.source.succeed({**gpu, "procs": procs, "models": models})


def _seconds_until(timestamp):
    """Seconds from now until an ISO-8601 instant, or None."""
    if not timestamp:
        return None
    from datetime import datetime, timezone
    try:
        when = datetime.fromisoformat(timestamp)
    except ValueError:
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return max(0, round((when - datetime.now(timezone.utc)).total_seconds()))

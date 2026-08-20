#!/usr/bin/env python3
"""GPU and loaded-model agent for ml60 (spec §3.2).

ml60's GPU is PCIe passthrough, so the hypervisor cannot see it. This is the
one agent the design permits. It reads NVML directly — a library call, not an
`nvidia-smi` subprocess — and merges in Ollama's loaded-model list so the
dashboard makes one request instead of two.

Serves GET / on :9836. No auth; LAN only.

Requires `nvidia-ml-py` (the official binding). The import name is `pynvml`,
but do NOT install the abandoned `pynvml` package from PyPI — it is a different
project and its API has drifted.
"""

import json
import os
import sys
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pynvml as nvml

BIND = os.environ.get("PVESTATS_AGENT_BIND", "0.0.0.0")
PORT = int(os.environ.get("PVESTATS_AGENT_PORT", "9836"))
GPU_INDEX = int(os.environ.get("PVESTATS_GPU_INDEX", "0"))
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://127.0.0.1:11434/api/ps")

# The one write path in this project. Everything else here — and everything on
# the Proxmox and TrueNAS side — is strictly read-only by design (spec §1), so
# this is deliberately narrow: it can set keep_alive=0 on a named model and
# nothing else. It cannot pull, run, delete or configure anything.
#
# Set PVESTATS_AGENT_ALLOW_UNLOAD=0 to close it off entirely; the agent then
# refuses with 403 and the dashboard hides the buttons.
ALLOW_UNLOAD = os.environ.get("PVESTATS_AGENT_ALLOW_UNLOAD", "1") != "0"
OLLAMA_GENERATE_URL = os.environ.get(
    "OLLAMA_GENERATE_URL",
    OLLAMA_URL.rsplit("/api/", 1)[0] + "/api/generate" if "/api/" in OLLAMA_URL
    else "http://127.0.0.1:11434/api/generate",
)

# An unload waits for any in-flight generation to finish before the model is
# released, so this gets far longer than the poll timeout above.
UNLOAD_TIMEOUT = 30.0

# The dashboard allows 2s for this whole request (spec §7), so Ollama gets a
# fraction of that. A stalled Ollama must not cost us the GPU numbers.
OLLAMA_TIMEOUT = 1.0

# Ollama's loaded-model list changes only when a model loads or unloads, but
# the dashboard polls at 1 Hz. Caching briefly cuts ~86k requests a day to the
# inference server down to a few thousand, with no visible staleness.
OLLAMA_CACHE_SECONDS = 5.0

_handle = None
_models_cache = (0.0, [])


def gpu_handle():
    """Initialise NVML once and cache the device handle.

    Deliberately not re-initialised per request: NVML init is far more
    expensive than the ~20 MB the process idles at, which is also why the
    systemd unit does not use socket activation.
    """
    global _handle
    if _handle is None:
        nvml.nvmlInit()
        _handle = nvml.nvmlDeviceGetHandleByIndex(GPU_INDEX)
    return _handle


def attempt(fn, default=None):
    """Read one metric, tolerating NVML_ERROR_NOT_SUPPORTED.

    Support varies by board — datacenter cards report no fan speed, some
    report no enforced power limit. One unsupported metric must not cost us
    the entire GPU payload.
    """
    try:
        return fn()
    except Exception:
        return default


def process_name(pid):
    """Best-effort process name.

    If Ollama runs in Docker its PIDs are namespaced and this will not say
    "ollama" (spec §3.2). That is expected — VRAM is attributed through the
    `models` array, never by matching on this string.
    """
    name = attempt(lambda: nvml.nvmlSystemGetProcessName(pid))
    if isinstance(name, bytes):
        name = name.decode("utf-8", "replace")
    if name:
        return name
    try:
        with open(f"/proc/{pid}/comm") as fh:
            return fh.read().strip()
    except OSError:
        return "?"


def gpu_stats():
    handle = gpu_handle()

    util = attempt(lambda: nvml.nvmlDeviceGetUtilizationRates(handle))
    mem = attempt(lambda: nvml.nvmlDeviceGetMemoryInfo(handle))
    power_mw = attempt(lambda: nvml.nvmlDeviceGetPowerUsage(handle))
    limit_mw = attempt(lambda: nvml.nvmlDeviceGetEnforcedPowerLimit(handle))
    if limit_mw is None:
        limit_mw = attempt(lambda: nvml.nvmlDeviceGetPowerManagementLimit(handle))

    procs = []
    # Needs permission to see other users' processes — hence running as the
    # same user as Ollama (spec §3.2). Without it this silently returns [].
    for proc in attempt(lambda: nvml.nvmlDeviceGetComputeRunningProcesses(handle), []):
        used = getattr(proc, "usedGpuMemory", None)
        procs.append({
            "pid": proc.pid,
            "name": process_name(proc.pid),
            # NVML reports an unsigned sentinel when per-process VRAM is
            # unavailable; surface that as null rather than a nonsense number.
            "vram": used if isinstance(used, int) and used < 2**63 else None,
        })

    # PCIe link state and throughput. Throughput counters are KB/s over the
    # last sampling interval, not cumulative, so they need no rate maths.
    pcie = {
        "gen": attempt(lambda: nvml.nvmlDeviceGetCurrPcieLinkGeneration(handle)),
        "width": attempt(lambda: nvml.nvmlDeviceGetCurrPcieLinkWidth(handle)),
        "max_gen": attempt(lambda: nvml.nvmlDeviceGetMaxPcieLinkGeneration(handle)),
        "max_width": attempt(lambda: nvml.nvmlDeviceGetMaxPcieLinkWidth(handle)),
        "tx_kbps": attempt(lambda: nvml.nvmlDeviceGetPcieThroughput(
            handle, nvml.NVML_PCIE_UTIL_TX_BYTES)),
        "rx_kbps": attempt(lambda: nvml.nvmlDeviceGetPcieThroughput(
            handle, nvml.NVML_PCIE_UTIL_RX_BYTES)),
    }

    return {
        "util": util.gpu if util else None,
        "mem_util": util.memory if util else None,
        "pcie": pcie,
        "vram_used": mem.used if mem else None,
        "vram_total": mem.total if mem else None,
        "temp": attempt(
            lambda: nvml.nvmlDeviceGetTemperature(handle, nvml.NVML_TEMPERATURE_GPU)
        ),
        "power": round(power_mw / 1000, 1) if power_mw is not None else None,
        "power_limit": round(limit_mw / 1000, 1) if limit_mw is not None else None,
        "clock_sm": attempt(
            lambda: nvml.nvmlDeviceGetClockInfo(handle, nvml.NVML_CLOCK_SM)
        ),
        "fan": attempt(lambda: nvml.nvmlDeviceGetFanSpeed(handle)),
        "procs": procs,
    }


def loaded_models():
    """Ollama's currently-resident models, cached briefly. Never raises."""
    global _models_cache
    cached_at, cached = _models_cache
    now = time.monotonic()
    if now - cached_at < OLLAMA_CACHE_SECONDS:
        return cached
    try:
        with urllib.request.urlopen(OLLAMA_URL, timeout=OLLAMA_TIMEOUT) as response:
            data = json.loads(response.read())
    except Exception:
        # Ollama down; GPU data is still worth serving. Cache the empty result
        # too, so a dead Ollama is not retried at full poll rate.
        _models_cache = (now, [])
        return []

    models = []
    for entry in data.get("models", []):
        models.append({
            "name": entry.get("name") or entry.get("model"),
            "size": entry.get("size"),
            "size_vram": entry.get("size_vram"),
            "expires_at": entry.get("expires_at"),
            "context_length": entry.get("context_length"),
        })
    _models_cache = (now, models)
    return models


def unload_model(name):
    """Ask Ollama to drop a model from memory.

    There is no dedicated unload endpoint: `keep_alive: 0` on an empty
    generate request is the documented mechanism, and is exactly what
    `ollama stop` does. A successful call comes back with
    done_reason == "unload".
    """
    global _models_cache
    body = json.dumps({"model": name, "keep_alive": 0}).encode()
    request = urllib.request.Request(
        OLLAMA_GENERATE_URL, data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(request, timeout=UNLOAD_TIMEOUT) as response:
        result = json.loads(response.read() or b"{}")
    # The cached list still shows the model as resident; drop it so the next
    # poll reflects what just happened instead of waiting out the cache.
    _models_cache = (0.0, [])
    return {"ok": result.get("done_reason") == "unload", "result": result}


def build_payload():
    payload = {"gpu": None, "models": loaded_models(),
               "can_unload": ALLOW_UNLOAD, "error": None}
    try:
        payload["gpu"] = gpu_stats()
    except Exception as exc:
        # Serve models anyway, and let the dashboard show why the GPU panel is
        # empty rather than showing zeros.
        payload["error"] = f"{type(exc).__name__}: {exc}"
    return payload


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        """No-op. Spec §3.2.

        The default handler writes a stderr line per request. At 1 Hz that is
        ~86,400 journal lines per day on ml60's disk, which would violate the
        no-writes-to-homelab-disk constraint.
        """

    def reply(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path.split("?")[0] != "/unload":
            self.send_error(404)
            return
        if not ALLOW_UNLOAD:
            self.reply(403, {"ok": False, "error": "unload disabled on this agent"})
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
            name = json.loads(self.rfile.read(length) or b"{}").get("model")
        except Exception:
            self.reply(400, {"ok": False, "error": "malformed request"})
            return
        if not isinstance(name, str) or not name.strip():
            self.reply(400, {"ok": False, "error": "no model named"})
            return
        try:
            self.reply(200, unload_model(name.strip()))
        except Exception as exc:
            self.reply(502, {"ok": False, "error": f"{type(exc).__name__}: {exc}"})

    def do_GET(self):
        if self.path.split("?")[0] != "/":
            self.send_error(404)
            return
        self.reply(200, build_payload())


def main():
    # Fail loudly at startup if the GPU is unreachable, so a broken deployment
    # is obvious in `systemctl status` instead of only in the JSON.
    try:
        gpu_handle()
    except Exception as exc:
        print(f"warning: NVML unavailable at startup: {exc}", file=sys.stderr)

    server = ThreadingHTTPServer((BIND, PORT), Handler)
    server.daemon_threads = True
    print(f"pvestats-agent listening on {BIND}:{PORT}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        attempt(nvml.nvmlShutdown)


if __name__ == "__main__":
    main()

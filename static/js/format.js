/* Number and duration formatting. Spec §7: round everything on screen. */

export const GiB = 1024 ** 3;
export const MiB = 1024 ** 2;

/* ---------------- formatting (spec §7: round everything) ---------------- */

export const fmt = {
  pct: v => (v == null ? "–" : Math.round(v)),
  // With its sign, and a bare dash when missing rather than "–%".
  pctText: v => (v == null ? "–" : `${Math.round(v)}%`),
  gib: v => (v == null ? "–" : (v / GiB).toFixed(1)),
  rate: v => (v == null ? "–" : (v / MiB).toFixed(1)),
  // Bytes per second as internet speeds are quoted: in bits, in the unit that
  // suits. As the terminal dashboard's bits_auto.
  bits(v) {
    if (v == null) return "–";
    const bits = v * 8;
    if (bits >= 1e9) return `${(bits / 1e9).toFixed(2)} Gbit/s`;
    if (bits >= 1e6) return `${(bits / 1e6).toFixed(1)} Mbit/s`;
    return `${(bits / 1e3).toFixed(0)} kbit/s`;
  },
  int: v => (v == null ? "–" : Math.round(v).toLocaleString()),

  uptime(seconds) {
    if (seconds == null) return "–";
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    if (d) return `${d}d ${h}h`;
    if (h) return `${h}h ${m}m`;
    return `${m}m`;
  },

  countdown(seconds) {
    if (seconds == null) return "–";
    // keep_alive -1 (or OLLAMA_KEEP_ALIVE=-1) puts the expiry ~292 years out;
    // `ollama ps` calls that Forever.
    if (seconds > 20 * 365 * 86400) return "never";
    // Ollama moves a model's expiry only when a request finishes, so a model
    // still listed well past it is busy, not about to leave. The margin
    // covers the agent's 30 s model cache plus a poll.
    if (seconds < -40) return "in use";
    if (seconds <= 0) return "now";
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    if (h) return `${h}h ${m}m`;
    if (m) return `${m}m`;
    return `${Math.round(seconds)}s`;
  },
};

/* Severity for a ratio against a limit. Drives meter colour only — every
   meter also carries its numeric caption, so colour is never the sole
   channel. */
export function severity(ratio) {
  if (ratio == null) return "var(--series-1)";
  if (ratio >= 0.9) return "var(--critical)";
  if (ratio >= 0.8) return "var(--warning)";
  return "var(--series-1)";
}

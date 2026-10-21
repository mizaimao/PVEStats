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

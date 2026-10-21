/* ---------------- tiny DOM helper ---------------- */

export function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

export const SVG_NS = "http://www.w3.org/2000/svg";
export function svgEl(tag, attrs) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, v);
  return node;
}

/* ---------------- animated numbers ----------------
 * Values change once a second; jumping straight to the new figure reads as a
 * flicker, while a short ease makes the change legible. Only the numeric part
 * animates, and only when the surrounding text is unchanged — "5d 18h" or
 * "64.1 / 95.6" are left alone rather than being counted through nonsense.
 */

const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)");
const NUMERIC = /^(-?\d+(?:\.\d+)?)(\D*)$/;
const DURATION = 320;

export function setAnimated(node, text, previous) {
  // One animation per node: each call supersedes any still running. Loops
  // used to stack up while the window was hidden (frames are held, polling
  // is not), then all ran at once and left older values on screen.
  const token = (node._animation || 0) + 1;
  node._animation = token;
  const to = NUMERIC.exec(String(text));
  const from = NUMERIC.exec(String(previous ?? ""));
  // Same trailing unit, both numeric, motion allowed, page visible —
  // otherwise just set it.
  if (!to || !from || to[2] !== from[2] || REDUCED.matches || document.hidden) {
    node.nodeValue = text;
    return;
  }
  const start = parseFloat(from[1]);
  const end = parseFloat(to[1]);
  if (start === end) { node.nodeValue = text; return; }
  const decimals = (to[1].split(".")[1] || "").length;
  const t0 = performance.now();

  const step = now => {
    if (node._animation !== token) return;     // a newer value took over
    const p = Math.min(1, (now - t0) / DURATION);
    const eased = 1 - (1 - p) ** 3;          // ease-out cubic
    node.nodeValue = (start + (end - start) * eased).toFixed(decimals) + to[2];
    if (p < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

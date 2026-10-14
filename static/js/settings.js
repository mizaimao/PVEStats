/* ---------------- settings panel ----------------
 * Opening and closing only. The controls inside are wired up in theme.js;
 * the poll-rate rows are informational.
 */

export function initSettings() {
  const button = document.getElementById("settings");
  const panel = document.getElementById("settings-panel");
  const close = document.getElementById("settings-close");
  if (!button || !panel) return;

  const setOpen = open => {
    panel.hidden = !open;
    button.setAttribute("aria-expanded", String(open));
  };
  button.addEventListener("click", () => setOpen(panel.hidden));
  close.addEventListener("click", () => setOpen(false));
  document.addEventListener("keydown", e => {
    if (e.key !== "Escape") return;
    setOpen(false);
    e.preventDefault();     // otherwise AppKit beeps at the unconsumed Escape
  });
  document.addEventListener("click", e => {
    if (!panel.hidden && !panel.contains(e.target) && !button.contains(e.target)) {
      setOpen(false);
    }
  });
}

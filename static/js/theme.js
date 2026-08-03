/* Theme selection.
 *
 * Themes are pure token overrides in style.css — this only stamps the name on
 * <html> and remembers it. Applied before first paint from main.js so there is
 * no flash of the default theme.
 */

const KEY = "pvestats.theme";
const DEFAULT = "ice";
const THEMES = new Set(["midnight", "neon", "amber", "synthwave",
                        "matrix", "ice", "ember", "nord"]);

export function currentTheme() {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved && THEMES.has(saved)) return saved;
  } catch {
    // Private mode or storage disabled — fall through to the default.
  }
  return DEFAULT;
}

export function applyTheme(name) {
  const theme = THEMES.has(name) ? name : DEFAULT;
  // Always stamped, including midnight — :root carries midnight's values, so
  // the attribute is a no-op there but keeps the select and the DOM in step.
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    // Not persisting is survivable; the theme still applies for this session.
  }
  return theme;
}

export function initTheme() {
  const select = document.getElementById("set-theme");
  const active = applyTheme(currentTheme());
  if (!select) return;
  select.value = active;
  select.addEventListener("change", () => applyTheme(select.value));
}

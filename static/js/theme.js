/* Theme selection.
 *
 * Themes are pure token overrides in style.css — this only stamps the name on
 * <html> and remembers it. Applied before first paint from main.js so there is
 * no flash of the default theme.
 */

const KEY = "pvestats.theme";
const THEMES = new Set(["midnight", "neon", "amber", "nord"]);

export function currentTheme() {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved && THEMES.has(saved)) return saved;
  } catch {
    // Private mode or storage disabled — fall through to the default.
  }
  return "midnight";
}

export function applyTheme(name) {
  const theme = THEMES.has(name) ? name : "midnight";
  // midnight is the :root default, so it needs no attribute.
  if (theme === "midnight") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
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

/* Appearance: theme and glow strength.
 *
 * Both persist server-side via /api/ui, so they survive a reinstall and are
 * shared by any browser pointed at the collector. The server also renders
 * them onto <html>, so the first paint is right before any round trip;
 * localStorage is only a fallback cache for that.
 */

const CACHE = "pvestats.appearance";
const DEFAULTS = { theme: "ice", glow: 1, window: 180,
                   scanlines: false, grain: false, aurora: false };
// Keyboard 1-8 map to these, in order. The rest are dropdown-only.
export const THEMES_ORDERED = ["ice", "midnight", "neon", "amber",
                               "synthwave", "matrix", "ember", "glass"];
const THEMES = new Set([...THEMES_ORDERED,
                        "vista", "win7", "snowleopard", "nord"]);

function cached() {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(CACHE) || "{}") };
  } catch {
    return { ...DEFAULTS };
  }
}

export function apply(settings) {
  const root = document.documentElement;
  const { theme, glow, scanlines, grain, aurora } = { ...DEFAULTS, ...settings };

  // Crossfade only while switching; a standing transition would make every
  // live value lag behind its own colour.
  const changing = root.dataset.theme && root.dataset.theme !== theme;
  if (changing) {
    root.classList.add("theming");
    setTimeout(() => root.classList.remove("theming"), 420);
  }

  root.dataset.theme = THEMES.has(theme) ? theme : DEFAULTS.theme;
  const scale = Number.isFinite(+glow) ? Math.max(0, Math.min(3, +glow)) : 1;
  root.style.setProperty("--glow-scale", String(scale));
  for (const [name, on] of [["scanlines", scanlines], ["grain", grain],
                            ["aurora", aurora]]) {
    if (on) root.dataset[name] = "on";
    else delete root.dataset[name];
  }

  const stored = { theme: root.dataset.theme, glow: scale,
                   scanlines: !!scanlines, grain: !!grain, aurora: !!aurora };
  try {
    localStorage.setItem(CACHE, JSON.stringify(stored));
  } catch {
    // Cache is an optimisation, not a requirement.
  }
  return stored;
}

/* The saved appearance the server rendered onto <html> (handle_index). It
   wins over the localStorage cache, which pywebview's private mode empties
   at every launch: starting from it, the first paint is already right. */
function rendered() {
  const root = document.documentElement;
  if (!root.dataset.theme) return {};
  const glow = parseFloat(root.style.getPropertyValue("--glow-scale"));
  return {
    theme: root.dataset.theme,
    ...(Number.isFinite(glow) ? { glow } : {}),
    scanlines: root.dataset.scanlines === "on",
    grain: root.dataset.grain === "on",
    aurora: root.dataset.aurora === "on",
  };
}

/* The full settings as this page knows them, and the ones changed here
   since it loaded. */
let current = { ...cached(), ...rendered() };
const changedHere = {};

/* Apply a change and save only that change.

   Posting every setting on every pick used to save whatever the controls
   happened to show for the others: a group with no chip lit (a value from
   config.yaml that is not a chip, or the settings not loaded yet) read as
   its first chip, so picking a theme could switch glow off and cut the
   history window to 60s. The server merges partial changes. */
async function save(change) {
  Object.assign(changedHere, change);
  current = { ...current, ...change };
  apply(current);
  try {
    await fetch("/api/ui", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(change),
    });
  } catch {
    // Already applied locally; it will simply not persist.
  }
}

/* A chip group: one chip marked, and a callback with the picked value. */
function group(id, onPick) {
  const root = document.getElementById(id);
  if (!root) return null;
  const buttons = [...root.querySelectorAll("button")];
  const api = {
    set(value) {
      const wanted = String(value);
      buttons.forEach(b =>
        b.setAttribute("aria-checked", String(b.value === wanted)));
    },
  };
  buttons.forEach(b => b.addEventListener("click", () => {
    api.set(b.value);
    onPick(b.value);
  }));
  return api;
}

let groups = {};

/* Set the theme from outside the panel — used by the keyboard shortcuts. */
export function selectTheme(name) {
  if (groups.theme) groups.theme.set(name);
  save({ theme: name });
}

export async function initAppearance() {
  groups = {
    theme: group("set-theme", value => save({ theme: value })),
    glow: group("set-glow", value => save({ glow: +value })),
    window: group("set-window", value => save({ window: +value })),
    scanlines: document.getElementById("fx-scanlines"),
    grain: document.getElementById("fx-grain"),
    aurora: document.getElementById("fx-aurora"),
  };
  for (const name of ["scanlines", "grain", "aurora"]) {
    const box = groups[name];
    if (box) box.addEventListener("change", () => save({ [name]: box.checked }));
  }

  // Paint immediately from cache, then reconcile with the server.
  apply(current);

  try {
    const response = await fetch("/api/ui", { cache: "no-store" });
    if (response.ok) {
      // A pick made while this was in flight is newer than the reply.
      current = { ...current, ...(await response.json()), ...changedHere };
      apply(current);
    }
  } catch {
    // Offline or starting up — the cached values stand.
  }

  if (groups.theme) groups.theme.set(document.documentElement.dataset.theme);
  if (groups.glow) groups.glow.set(current.glow ?? 1);
  if (groups.window) groups.window.set(current.window ?? 180);
  for (const name of ["scanlines", "grain", "aurora"]) {
    if (groups[name]) groups[name].checked = !!current[name];
  }
}

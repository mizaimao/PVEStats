/* Appearance: theme and glow strength.
 *
 * Both persist server-side via /api/ui, so they survive a reinstall and are
 * shared by any browser pointed at the collector. localStorage is kept only
 * as a first-paint cache — waiting for a round trip before styling would
 * flash the default theme.
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

async function save(settings) {
  apply(settings);
  try {
    await fetch("/api/ui", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(settings),
    });
  } catch {
    // Already applied locally; it will simply not persist.
  }
}

/* A chip group behaves like a <select> for our purposes: read the checked
   value, mark one checked, and call back on click. */
function group(id, onPick) {
  const root = document.getElementById(id);
  if (!root) return null;
  const buttons = [...root.querySelectorAll("button")];
  const api = {
    get value() {
      const on = buttons.find(b => b.getAttribute("aria-checked") === "true");
      return on ? on.value : buttons[0] && buttons[0].value;
    },
    set(value) {
      const wanted = String(value);
      buttons.forEach(b =>
        b.setAttribute("aria-checked", String(b.value === wanted)));
    },
  };
  buttons.forEach(b => b.addEventListener("click", () => {
    api.set(b.value);
    onPick();
  }));
  return api;
}

let groups = {};

/* Set the theme from outside the panel — used by the keyboard shortcuts. */
export function selectTheme(name) {
  if (groups.theme) {
    groups.theme.set(name);
    save(readAll());
  } else {
    apply({ ...cached(), theme: name });
  }
}

function readAll() {
  return {
    theme: groups.theme ? groups.theme.value : cached().theme,
    glow: groups.glow ? +groups.glow.value : cached().glow,
    window: groups.window ? +groups.window.value : cached().window,
    scanlines: !!(groups.scanlines && groups.scanlines.checked),
    grain: !!(groups.grain && groups.grain.checked),
    aurora: !!(groups.aurora && groups.aurora.checked),
  };
}

export async function initAppearance() {
  const onPick = () => save(readAll());

  groups = {
    theme: group("set-theme", onPick),
    glow: group("set-glow", onPick),
    window: group("set-window", onPick),
    scanlines: document.getElementById("fx-scanlines"),
    grain: document.getElementById("fx-grain"),
    aurora: document.getElementById("fx-aurora"),
  };

  // Paint immediately from cache, then reconcile with the server.
  let settings = cached();
  apply(settings);

  try {
    const response = await fetch("/api/ui", { cache: "no-store" });
    if (response.ok) {
      settings = { ...settings, ...(await response.json()) };
      apply(settings);
    }
  } catch {
    // Offline or starting up — the cached values stand.
  }

  if (groups.theme) groups.theme.set(document.documentElement.dataset.theme);
  if (groups.glow) groups.glow.set(settings.glow ?? 1);
  if (groups.window) groups.window.set(settings.window ?? 180);
  for (const name of ["scanlines", "grain", "aurora"]) {
    const box = groups[name];
    if (box) {
      box.checked = !!settings[name];
      box.addEventListener("change", onPick);
    }
  }
}

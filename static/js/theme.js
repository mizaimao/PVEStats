/* Appearance: theme and glow strength.
 *
 * Both persist server-side via /api/ui, so they survive a reinstall and are
 * shared by any browser pointed at the collector. localStorage is kept only
 * as a first-paint cache — waiting for a round trip before styling would
 * flash the default theme.
 */

const CACHE = "pvestats.appearance";
const DEFAULTS = { theme: "ice", glow: 1,
                   scanlines: false, grain: false, aurora: false };
const THEMES = new Set(["midnight", "neon", "amber", "synthwave",
                        "matrix", "ice", "ember", "nord"]);

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

export async function initAppearance() {
  const controls = {
    theme: document.getElementById("set-theme"),
    glow: document.getElementById("set-glow"),
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

  const read = () => ({
    theme: controls.theme ? controls.theme.value : settings.theme,
    glow: controls.glow ? +controls.glow.value : settings.glow,
    scanlines: !!(controls.scanlines && controls.scanlines.checked),
    grain: !!(controls.grain && controls.grain.checked),
    aurora: !!(controls.aurora && controls.aurora.checked),
  });

  if (controls.theme) controls.theme.value = document.documentElement.dataset.theme;
  if (controls.glow) controls.glow.value = String(settings.glow ?? 1);
  for (const name of ["scanlines", "grain", "aurora"]) {
    if (controls[name]) controls[name].checked = !!settings[name];
  }
  for (const node of Object.values(controls)) {
    if (node) node.addEventListener("change", () => save(read()));
  }
}

/* Entry point: polls /api/state and routes each guest to its card.
 *
 * Cards are built once and mutated thereafter — a full re-render each tick
 * would flash and destroy hover state.
 */

import { el } from "./dom.js";
import { basicCard, gpuCard, nasCard, nodeCard } from "./cards.js";
import { initSettings } from "./settings.js";
import { initAppearance, selectTheme, THEMES_ORDERED } from "./theme.js";

const POLL_MS = 1000;

/* ---------------- source strip ---------------- */

function renderSources(sources) {
  const host = document.getElementById("sources");
  host.textContent = "";
  for (const [name, status] of Object.entries(sources)) {
    const item = el("span", "src");
    // Not ok with no error is a source that has not answered yet: neutral.
    if (!status.ok) item.classList.add(status.error ? "is-down" : "is-waiting");
    else if (status.stale) item.classList.add("is-stale");
    const dot = el("span", "dot");
    const label = el("span", null, name);
    // Counting down to the next poll reads as "something is about to happen";
    // counting up just says how stale things are.
    const remaining = status.age == null || !status.interval
      ? null
      : Math.max(0, Math.ceil(status.interval - status.age));
    const age = el("span", "age",
      status.age == null ? "never" : `${remaining}s`);
    item.append(dot, label, age);
    // The dot says that something is wrong; hovering says what.
    if (status.error) item.title = status.error;
    host.appendChild(item);
  }
}

/* ---------------- main loop ---------------- */

const cards = new Map();
let entered = 0;

function controllerFor(guest) {
  if (guest.view === "gpu") return gpuCard(guest);
  if (guest.view === "nas") return nasCard(guest);
  return basicCard(guest);
}

/* The page refresh rate is derived, not chosen: polling faster than the
   fastest source only redraws numbers that have not changed. */
function refreshInterval(sources) {
  const intervals = Object.values(sources)
    .map(s => s.interval)
    .filter(v => typeof v === "number" && v > 0);
  return intervals.length ? Math.min(...intervals) : 1;
}

/* While PVE has given no data the host card cannot exist, so a note in its
   place says why, instead of an empty column and a red dot (spec §2: show the
   error, do not blank the page). Gone once PVE answers. */
function updatePveNote(state) {
  let note = document.getElementById("pve-note");
  if (state.node || !standby.done) {
    if (note) note.remove();
    return;
  }
  if (!note) {
    note = el("p", "source-note");
    note.id = "pve-note";
    document.getElementById("col-left").prepend(note);
  }
  const pve = (state.sources || {}).pve || {};
  note.textContent = pve.error
    ? `Proxmox VE has not answered: ${pve.error}`
    : "Waiting for Proxmox VE's first answer…";
}

function render(state) {
  updateStandby(state);
  updatePveNote(state);
  renderSources(state.sources);

  const derived = document.getElementById("set-page-derived");
  if (derived) derived.textContent = `${refreshInterval(state.sources)}s`;
  nodeCard(state.node, state.history.pve, state.sources.pve);

  const left = document.getElementById("col-left");
  const right = document.getElementById("col-right");
  const basics = document.getElementById("basics");
  const seen = new Set();

  for (const guest of state.guests) {
    seen.add(guest.vmid);
    let card = cards.get(guest.vmid);
    if (!card) {
      card = controllerFor(guest);
      cards.set(guest.vmid, card);
      // NAS joins the host in the left column; the GPU card owns the right.
      const host = guest.view === "gpu" ? right
                 : guest.view === "nas" ? left
                 : basics;
      // Position in the arrival order drives the entrance stagger.
      card.el.style.setProperty("--enter", String(entered++));
      host.appendChild(card.el);
    }
    card.update(guest, state.history, state.sources.pve);
  }

  // A guest that stopped loses its card rather than freezing at its last value.
  for (const [vmid, card] of cards) {
    if (!seen.has(vmid)) { card.el.remove(); cards.delete(vmid); }
  }
}

async function tick() {
  const offline = document.getElementById("offline");
  try {
    const response = await fetch("/api/state", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    render(await response.json());
    offline.hidden = true;
    document.body.style.opacity = "";
  } catch (err) {
    // Hold the previous render rather than flashing an empty page.
    offline.hidden = false;
    document.body.style.opacity = "0.6";
  }
}

initAppearance();

/* Standby: held until the collector has produced something worth drawing.
 *
 * The first PVE poll can be a full 10s away, so without this the first thing
 * every launch shows is an empty dashboard. It names each source and its state
 * rather than showing a bare spinner — if one is slow or dead, that is the
 * thing worth knowing while waiting.
 */
const SOURCE_LABELS = {
  pve: "Proxmox VE",
  agent: "ml60 GPU agent",
  truenas: "TrueNAS",
};
const STANDBY_GIVE_UP_MS = 25000;

const standby = {
  el: document.getElementById("standby"),
  note: document.getElementById("standby-note"),
  list: document.getElementById("standby-list"),
  rows: new Map(),
  done: false,
  since: performance.now(),
};

function updateStandby(state) {
  if (standby.done || !standby.el) return;

  for (const [name, status] of Object.entries(state.sources || {})) {
    let row = standby.rows.get(name);
    if (!row) {
      row = { li: el("li"), dot: el("span", "dot"),
              label: el("span", null, SOURCE_LABELS[name] || name),
              detail: el("span", "detail") };
      row.li.append(row.dot, row.label, row.detail);
      standby.list.appendChild(row.li);
      standby.rows.set(name, row);
    }
    const ready = status.ok;
    // A source that is merely "not configured" is settled, not pending —
    // waiting on it would hang the screen forever.
    const failed = !status.ok && !!status.error;
    row.li.classList.toggle("is-ready", ready);
    row.li.classList.toggle("is-failed", failed && !ready);
    row.detail.textContent = ready ? "ready"
                           : failed ? String(status.error).split(":")[0]
                           : "waiting…";
  }

  const haveSomething = state.node && (state.guests || []).length > 0;
  // Every source has answered or failed for good, so there is nothing left
  // to wait for: show the page, with the failures said on it.
  const settled = Object.values(state.sources || {})
    .every(s => s.ok || s.error);
  const waitedLongEnough = performance.now() - standby.since > STANDBY_GIVE_UP_MS;

  if (haveSomething || settled || waitedLongEnough) {
    standby.done = true;
    standby.note.textContent = "Ready";
    standby.el.hidden = true;
  }
}

/* Keyboard: `s` toggles settings, 1-8 pick a theme. Ignored while a control
   has focus, so typing in the panel does not fire them. */
/* Focus mode: collapse to the GPU card, which is what this dashboard exists
   for. Session-only — a launch always starts on the full view. */
const focusToggle = document.getElementById("focus-toggle");

function setFocus(on) {
  const root = document.documentElement;
  const apply = () => {
    if (on) root.dataset.focus = "gpu";
    else delete root.dataset.focus;
    if (focusToggle) focusToggle.setAttribute("aria-pressed", String(on));
  };
  // View Transitions morphs the pane between layouts for free; without it the
  // switch is instant, which is fine.
  if (document.startViewTransition
      && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    document.startViewTransition(apply);
  } else {
    apply();
  }
}

function toggleFocus() {
  setFocus(document.documentElement.dataset.focus !== "gpu");
}

if (focusToggle) focusToggle.addEventListener("click", toggleFocus);

/* Keys the browser and assistive tech need; everything else is swallowed.

   An unconsumed key event travels up to AppKit, which beeps when nothing in
   the responder chain wants it — that is the click heard on every keystroke
   in the window. There are no text fields here, so absorbing the rest costs
   nothing and silences it. */
const PASS_THROUGH = new Set([
  "Tab", "Enter", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
  "Home", "End", "PageUp", "PageDown",
]);

document.addEventListener("keydown", event => {
  if (event.metaKey || event.ctrlKey || event.altKey) return;
  const tag = (event.target.tagName || "").toLowerCase();
  if (["input", "select", "textarea"].includes(tag)) return;
  if (PASS_THROUGH.has(event.key)) return;

  if (event.key === "s") {
    document.getElementById("settings").click();
  } else if (event.key === "f") {
    toggleFocus();
  } else {
    const index = "12345678".indexOf(event.key);
    if (index !== -1 && THEMES_ORDERED[index]) selectTheme(THEMES_ORDERED[index]);
  }
  // Consumed either way — an unhandled key is exactly what causes the beep.
  event.preventDefault();
});
initSettings();
/* Polls regardless of focus. Pausing while hidden saved a little CPU but the
   window is meant to be glanceable from across the room, and coming back to a
   chart that stopped moving is worse than the saving is worth. */
tick();
setInterval(tick, POLL_MS);

/* Entry point: polls /api/state and routes each guest to its card.
 *
 * Cards are built once and mutated thereafter — a full re-render each tick
 * would flash and destroy hover state.
 */

import { el } from "./dom.js";
import { basicCard, gpuCard, nasCard, nodeCard, routerCard } from "./cards.js";
import { initSettings } from "./settings.js";
import { initAppearance, selectTheme, THEMES_ORDERED } from "./theme.js";

const POLL_MS = 1000;

/* ---------------- source strip ---------------- */

/* Built once per source and updated in place: a dot recreated every second
   restarted its 2 s pulse each time, which made it a 1 Hz blink. */
const strip = new Map();     // source name -> { item, age }

function renderSources(sources) {
  const host = document.getElementById("sources");
  for (const [name, status] of Object.entries(sources)) {
    let entry = strip.get(name);
    if (!entry) {
      entry = { item: el("span", "src"), age: el("span", "age") };
      entry.item.append(el("span", "dot"), el("span", null, name), entry.age);
      host.appendChild(entry.item);
      strip.set(name, entry);
    }
    // Not ok with no error is a source that has not answered yet: neutral.
    const state = !status.ok ? (status.error ? " is-down" : " is-waiting")
                : status.stale ? " is-stale" : "";
    if (entry.item.className !== `src${state}`) entry.item.className = `src${state}`;
    // A healthy source counts down to its next poll: "something is about to
    // happen". One that is failing says how long ago it last answered; the
    // countdown used to sit at "0s" for it, as if it had just updated.
    let text;
    if (status.age == null) text = "never";
    else if (status.ok && !status.stale && status.interval) {
      text = `${Math.max(0, Math.ceil(status.interval - status.age))}s`;
    } else {
      text = `${Math.round(status.age)}s ago`;
    }
    if (entry.age.textContent !== text) entry.age.textContent = text;
    // The dot says that something is wrong; hovering says what.
    entry.item.title = status.error || "";
  }
}

/* ---------------- main loop ---------------- */

const cards = new Map();
let router = null;           // the router's card, once its source exists
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

/* Cards sit in VMID order, the order the server sends them in. A card that
   arrives later (a guest started, or stopped and started again) used to be
   appended at the end of its column. */
function insertByVmid(host, node, vmid) {
  const after = [...host.children].find(c => c.dataset.vmid && +c.dataset.vmid > vmid);
  if (after) host.insertBefore(node, after);
  else host.appendChild(node);
}

function render(state) {
  // Cards made while the standby is still up come in staggered as it lifts.
  // Later ones come in at once: their delays used to keep growing, and
  // replayed each time focus mode showed their column again.
  const staggering = !standby.done;
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
    if (card && card.view !== guest.view) {
      // Its view changed (config.yaml edited, server restarted under run.py):
      // a different kind of card.
      card.el.remove();
      card = null;
    }
    if (!card) {
      card = controllerFor(guest);
      card.view = guest.view;
      card.el.dataset.vmid = String(guest.vmid);
      cards.set(guest.vmid, card);
      // NAS joins the host in the left column; the GPU card owns the right.
      const host = guest.view === "gpu" ? right
                 : guest.view === "nas" ? left
                 : basics;
      card.el.style.setProperty("--enter", staggering ? String(entered++) : "0");
      insertByVmid(host, card.el, guest.vmid);
    }
    card.update(guest, state.history, state.sources.pve);
  }

  // A guest that stopped loses its card rather than freezing at its last value.
  for (const [vmid, card] of cards) {
    if (!seen.has(vmid)) { card.el.remove(); cards.delete(vmid); }
  }

  // The router, when one is configured: last in the left column, under the
  // NAS, which can arrive after it.
  const routerStatus = state.sources.router;
  if (routerStatus) {
    if (!router) {
      router = routerCard();
      router.el.style.setProperty("--enter", staggering ? String(entered++) : "0");
    }
    if (left.lastElementChild !== router.el) left.appendChild(router.el);
    router.update(state.router, (state.history || {}).router, routerStatus);
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
    // The standby's own give-up runs only on a successful render, so a
    // collector that died before its first full poll left "Connecting to
    // sources…" up for good, over the banner that explains it.
    if (!standby.done && performance.now() - standby.since > STANDBY_GIVE_UP_MS) {
      standby.done = true;
      standby.el.hidden = true;
    }
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
  router: "Router agent",
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

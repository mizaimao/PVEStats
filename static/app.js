/* PVEStats dashboard.
 *
 * Polls /api/state once a second and updates cards in place. Nodes are built
 * once and mutated thereafter — a full re-render each tick would flash and
 * destroy hover state.
 *
 * A source that fails greys its own card and shows the error; it never blanks
 * the page (spec §2).
 */

const POLL_MS = 1000;
const GiB = 1024 ** 3;
const MiB = 1024 ** 2;

/* ---------------- formatting (spec §7: round everything) ---------------- */

const fmt = {
  pct: v => (v == null ? "–" : Math.round(v)),
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
function severity(ratio) {
  if (ratio == null) return "var(--series-1)";
  if (ratio >= 0.9) return "var(--critical)";
  if (ratio >= 0.8) return "var(--warning)";
  return "var(--series-1)";
}

/* ---------------- tiny DOM helper ---------------- */

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

const SVG_NS = "http://www.w3.org/2000/svg";
function svgEl(tag, attrs) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, v);
  return node;
}

/* ---------------- stat tile ---------------- */

function tile(label, { hero = false, unit = "" } = {}) {
  const root = el("div", hero ? "tile hero" : "tile");
  const value = el("div", "value");
  const number = document.createTextNode("–");
  value.appendChild(number);
  if (unit) {
    const u = el("span", "unit", unit);
    value.appendChild(u);
  }
  root.append(el("div", "label", label), value);
  return { el: root, set: v => { number.nodeValue = v; } };
}

/* ---------------- meter ---------------- */

/* `graded` opts into severity colouring. Off by default and deliberately not
   used for guest memory: PVE reports cache and balloon inside `mem`, so a
   healthy guest routinely sits above 90% and a red bar there would be crying
   wolf. ZFS pool capacity is graded, where >80% is a real concern. */
function meter(label, { graded = false, compact = false } = {}) {
  const root = el("div", compact ? "meter-row is-compact" : "meter-row");
  const readout = el("b", null, "–");
  if (!compact) {
    const caption = el("div", "caption");
    caption.append(el("span", null, label), readout);
    root.append(caption);
  }
  const track = el("div", "meter");
  const fill = el("span");
  // Explicit zero: an unset width on a block element renders as a full bar,
  // so a source with no data yet would read as 100%.
  fill.style.width = "0%";
  track.appendChild(fill);
  root.append(track);
  return {
    el: root,
    set(used, total, text) {
      const ratio = total ? Math.min(1, used / total) : 0;
      track.style.setProperty("--meter-color",
        graded ? severity(total ? used / total : null) : "var(--series-1)");
      fill.style.width = `${(ratio * 100).toFixed(1)}%`;
      readout.textContent = text;
    },
  };
}

/* ---------------- sparkline ----------------
 * Rendered in real pixel coordinates rather than a stretched viewBox, so the
 * 2px stroke and the end dot stay circular at any width.
 */

class Spark {
  /* `domain` fixes the y-scale. Percentages get [0, 100] — auto-scaling a
     percentage makes a host idling at 2% look like it is thrashing, and with
     no y-axis on a sparkline the reader has no way to tell. Series with no
     natural ceiling (throughput) auto-scale and print their peak instead, so
     the scale is still readable. */
  constructor(series, { height = 38, format = fmt.pct, unit = "",
                        domain = null, caption = "" } = {}) {
    this.series = series;                 // [{key, color, label}]
    this.height = height;
    this.format = format;
    this.unit = unit;
    this.domain = domain;
    this.width = 240;
    this.data = [];

    this.el = el("div", "spark");

    // Caption and peak sit above the plot, never over it — a label drawn on
    // top of the data is unreadable the moment the series climbs.
    this.head = el("div", "spark-head");
    this.peak = el("span", "peak");
    this.head.append(el("span", null, caption), this.peak);
    if (caption || !domain) this.el.appendChild(this.head);

    this.svg = svgEl("svg", { preserveAspectRatio: "none" });
    this.el.appendChild(this.svg);

    // Hairline, solid, recessive — and only where the scale is fixed, so the
    // reader can see that a flat line near the floor means genuinely idle
    // rather than a broken chart.
    this.gridlines = domain
      ? [0.5, 0].map(() => {
          const line = svgEl("line", { stroke: "var(--grid)", "stroke-width": "1" });
          this.svg.appendChild(line);
          return line;
        })
      : [];

    this.parts = series.map(s => {
      const area = svgEl("path", { fill: s.color, "fill-opacity": "0.10" });
      const line = svgEl("polyline", {
        fill: "none", stroke: s.color, "stroke-width": "2",
        "stroke-linejoin": "round", "stroke-linecap": "round",
      });
      // 2px ring in the surface colour keeps the end dot legible where it
      // crosses another line.
      const dot = svgEl("circle", {
        r: "4", fill: s.color, stroke: "var(--surface)", "stroke-width": "2",
      });
      this.svg.append(area, line, dot);
      return { area, line, dot };
    });

    this.tip = el("div", "tip");
    this.el.appendChild(this.tip);
    this.svg.addEventListener("mousemove", e => this.hover(e));

    new ResizeObserver(entries => {
      const w = Math.max(40, Math.round(entries[0].contentRect.width));
      if (w !== this.width) { this.width = w; this.draw(); }
    }).observe(this.el);
  }

  update(history) {
    this.data = this.series.map(s => (history && history[s.key]) || []);
    this.draw();
  }

  scale() {
    if (this.domain) return this.domain[1];
    let max = 0;
    for (const values of this.data) for (const v of values) if (v > max) max = v;
    return max || 1;
  }

  draw() {
    const h = this.height;
    const w = this.width;
    this.svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    this.svg.setAttribute("height", h);
    const max = this.scale();
    const pad = 3;
    if (!this.domain) {
      this.peak.textContent = `peak ${this.format(max)}${this.unit}`;
    }
    // Gridlines at the midpoint and the ceiling of a fixed domain.
    this.gridlines.forEach((line, i) => {
      const y = (pad + (h - pad * 2) * (i === 0 ? 0.5 : 0)).toFixed(1);
      line.setAttribute("x1", 0); line.setAttribute("x2", w);
      line.setAttribute("y1", y); line.setAttribute("y2", y);
    });

    this.data.forEach((values, i) => {
      const { area, line, dot } = this.parts[i];
      if (values.length < 2) {
        line.setAttribute("points", "");
        area.setAttribute("d", "");
        dot.setAttribute("r", "0");
        return;
      }
      const step = w / (values.length - 1);
      const y = v => h - pad - (v / max) * (h - pad * 2);
      const points = values.map((v, j) => `${(j * step).toFixed(1)},${y(v).toFixed(1)}`);
      line.setAttribute("points", points.join(" "));
      area.setAttribute("d", `M0,${h} L${points.join(" L")} L${w},${h} Z`);
      dot.setAttribute("r", "4");
      dot.setAttribute("cx", w.toFixed(1));
      dot.setAttribute("cy", y(values[values.length - 1]).toFixed(1));
    });
  }

  /* Hover enhances; it never gates a value — the current reading is always
     printed in the tile beside the sparkline. */
  hover(event) {
    const values = this.data[0] || [];
    if (values.length < 2) { this.tip.style.opacity = 0; return; }
    const rect = this.svg.getBoundingClientRect();
    const ratio = (event.clientX - rect.left) / rect.width;
    const index = Math.max(0, Math.min(values.length - 1,
      Math.round(ratio * (values.length - 1))));
    const parts = this.data
      .map((s, i) => `${this.series[i].label}: ${this.format(s[index])}${this.unit}`)
      .join("  ");
    const secondsAgo = values.length - 1 - index;
    this.tip.textContent = secondsAgo ? `${parts}  ·  ${secondsAgo} samples ago` : parts;
    this.tip.style.left = `${ratio * 100}%`;
  }
}

function legend(series) {
  const root = el("div", "legend");
  for (const s of series) {
    const item = el("span");
    const swatch = el("i");
    swatch.style.background = s.color;
    item.append(swatch, document.createTextNode(s.label));
    root.appendChild(item);
  }
  return root;
}

/* ---------------- table ---------------- */

function dataTable(caption, columns) {
  const table = el("table", "data");
  table.appendChild(el("caption", null, caption));
  const head = el("tr");
  for (const c of columns) head.appendChild(el("th", c.num ? "num" : null, c.label));
  const thead = document.createElement("thead");
  thead.appendChild(head);
  const tbody = document.createElement("tbody");
  table.append(thead, tbody);
  return {
    el: table,
    set(rows) {
      tbody.textContent = "";
      if (!rows.length) {
        const tr = el("tr");
        const td = el("td", "empty", "none");
        td.colSpan = columns.length;
        tr.appendChild(td);
        tbody.appendChild(tr);
        return;
      }
      for (const row of rows) {
        const tr = el("tr");
        row.forEach((cell, i) => {
          const td = el("td", columns[i].num ? "num" : "name");
          if (cell instanceof Node) td.appendChild(cell); else td.textContent = cell;
          tr.appendChild(td);
        });
        tbody.appendChild(tr);
      }
    },
  };
}

/* ---------------- cards ---------------- */

function cardShell(title, subtitle) {
  const root = el("section", "card");
  const h = el("h2", null, title);
  const sub = el("div", "sub", subtitle || "");
  const error = el("div", "card-error");
  error.hidden = true;
  root.append(h, sub);
  return { el: root, sub, error, title: h };
}

function baseGuestPanels(shell) {
  const tiles = el("div", "tiles");
  const cpu = tile("CPU", { unit: "%" });
  const up = tile("Uptime");
  tiles.append(cpu.el, up.el);
  const mem = meter("Memory");
  shell.el.append(tiles, mem.el);
  return { cpu, up, mem };
}

function applyStale(shell, status) {
  const stale = !status || !status.ok;
  shell.el.classList.toggle("is-stale", stale);
  if (stale && status && status.error) {
    shell.error.hidden = false;
    shell.error.textContent =
      `${status.error}${status.age != null ? ` · last good ${Math.round(status.age)}s ago` : ""}`;
    if (!shell.error.isConnected) shell.el.appendChild(shell.error);
  } else {
    shell.error.hidden = true;
  }
}

/* --- 4.4 basic guest --- */

function basicCard(guest) {
  const shell = cardShell(guest.name, `vmid ${guest.vmid}`);
  const panels = baseGuestPanels(shell);
  return {
    el: shell.el,
    update(g, history, status) {
      panels.cpu.set(fmt.pct(g.cpu_pct));
      panels.up.set(fmt.uptime(g.uptime));
      panels.mem.set(g.mem, g.maxmem,
        `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);
      applyStale(shell, status);
    },
  };
}

/* --- 4.2 ml60 --- */

function gpuCard(guest) {
  const shell = cardShell(guest.name, `vmid ${guest.vmid} · GPU`);

  const heroWrap = el("div", "tiles");
  const util = tile("GPU utilisation", { hero: true, unit: "%" });
  heroWrap.appendChild(util.el);

  const spark = new Spark([{ key: "gpu.util", color: "var(--series-1)", label: "GPU" }],
    { format: fmt.pct, unit: "%", domain: [0, 100] });

  const vram = meter("VRAM");

  const tiles = el("div", "tiles");
  const temp = tile("Temp", { unit: "°C" });
  const power = tile("Power", { unit: "W" });
  const fan = tile("Fan", { unit: "%" });
  const clock = tile("SM clock", { unit: "MHz" });
  const cpu = tile("CPU", { unit: "%" });
  const up = tile("Uptime");
  tiles.append(temp.el, power.el, fan.el, clock.el, cpu.el, up.el);

  const mem = meter("Memory");

  const models = dataTable("Loaded models", [
    { label: "Model" }, { label: "VRAM", num: true }, { label: "Unloads in", num: true },
  ]);
  const procs = dataTable("GPU processes", [
    { label: "Process" }, { label: "PID", num: true }, { label: "VRAM", num: true },
  ]);

  shell.el.append(heroWrap, spark.el, vram.el, tiles, mem.el, models.el, procs.el);

  return {
    el: shell.el,
    update(g, history, status) {
      // PVE panels (10s) update regardless of the agent's state, and the
      // agent panels (1s) never wait on PVE — §4.2.
      cpu.set(fmt.pct(g.cpu_pct));
      up.set(fmt.uptime(g.uptime));
      mem.set(g.mem, g.maxmem, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);

      const gpu = g.gpu;
      if (gpu) {
        util.set(fmt.pct(gpu.util));
        temp.set(fmt.pct(gpu.temp));
        power.set(gpu.power == null ? "–" : Math.round(gpu.power));
        fan.set(gpu.fan == null ? "–" : fmt.pct(gpu.fan));
        clock.set(fmt.int(gpu.clock_sm));
        vram.set(gpu.vram_used, gpu.vram_total,
          `${fmt.gib(gpu.vram_used)} / ${fmt.gib(gpu.vram_total)} GiB`);
        models.set((gpu.models || []).map(m => [
          m.name || "?", `${fmt.gib(m.size_vram)} GiB`, fmt.countdown(m.unload_in),
        ]));
        procs.set((gpu.procs || []).map(p => [
          p.name || "?", String(p.pid), p.vram == null ? "–" : `${fmt.gib(p.vram)} GiB`,
        ]));
        spark.update(history.agent);
      } else {
        // Agent unreachable. Clear its panels explicitly rather than leaving
        // whatever was last drawn — the card must not imply live readings.
        [util, temp, power, fan, clock].forEach(t => t.set("–"));
        vram.set(0, 0, "–");
        models.set([]);
        procs.set([]);
      }
      // Stale styling follows the agent, since that is what this card leads on.
      applyStale(shell, g.gpu_status || status);
    },
  };
}

/* --- 4.3 TrueNAS --- */

function nasCard(guest) {
  const shell = cardShell(guest.name, `vmid ${guest.vmid} · NAS`);

  const tiles = el("div", "tiles");
  const cpu = tile("CPU", { unit: "%" });
  const up = tile("Uptime");
  tiles.append(cpu.el, up.el);
  const mem = meter("Memory");
  shell.el.append(tiles, mem.el);

  // Interface throughput is read from inside TrueNAS, never from PVE's
  // counters (spec §3.3). Two series, so a legend is mandatory.
  const ifSeries = [];
  const ifSpark = el("div");
  const ifTable = dataTable("Interfaces", [
    { label: "Interface" }, { label: "RX MiB/s", num: true }, { label: "TX MiB/s", num: true },
  ]);
  const disks = dataTable("Disks", [
    { label: "Disk" }, { label: "Read MiB/s", num: true }, { label: "Write MiB/s", num: true },
  ]);
  const pools = dataTable("Pools", [
    { label: "Pool" }, { label: "Status" }, { label: "Capacity" }, { label: "Used", num: true },
  ]);
  shell.el.append(ifSpark, ifTable.el, disks.el, pools.el);

  let spark = null;

  return {
    el: shell.el,
    update(g, history, status) {
      cpu.set(fmt.pct(g.cpu_pct));
      up.set(fmt.uptime(g.uptime));
      mem.set(g.mem, g.maxmem, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);

      const nas = g.nas;
      if (nas) {
        const names = Object.keys(nas.interfaces || {});
        // Build the throughput sparkline once the interface names are known.
        if (!spark && names.length) {
          const colours = ["var(--series-1)", "var(--series-2)"];
          names.slice(0, 2).forEach((name, i) => ifSeries.push({
            key: `if.${name}.rx`, color: colours[i], label: name,
          }));
          spark = new Spark(ifSeries, {
            format: fmt.rate, unit: " MiB/s", caption: "Receive throughput",
          });
          ifSpark.append(spark.el, legend(ifSeries));
        }
        if (spark) spark.update(history.truenas);

        ifTable.set(names.map(n => [
          n, fmt.rate(nas.interfaces[n].rx_bps), fmt.rate(nas.interfaces[n].tx_bps),
        ]));
        disks.set(Object.entries(nas.disks || {}).map(([n, d]) => [
          n, fmt.rate(d.read_bps), fmt.rate(d.write_bps),
        ]));
        pools.set((nas.pools || []).map(p => {
          const badge = el("span", "pool-status");
          const dot = el("span", "dot");
          dot.style.background = p.healthy ? "var(--good)" : "var(--critical)";
          // Icon plus label — pool state never rides on colour alone.
          badge.append(dot, document.createTextNode(p.status || "?"));
          const used = p.size ? Math.round((p.allocated / p.size) * 100) : null;
          // Graded: a ZFS pool past 80% is a genuine performance concern.
          const bar = meter("", { graded: true, compact: true });
          bar.set(p.allocated, p.size, "");
          return [p.name, badge, bar.el,
            used == null ? "–" : `${used}% of ${(p.size / 1024 ** 4).toFixed(1)} TiB`];
        }));
      } else {
        ifTable.set([]);
        disks.set([]);
        pools.set([]);
      }
      applyStale(shell, g.nas_status || status);
    },
  };
}

/* --- 4.1 PVE host --- */

const nodeCard = (() => {
  const root = document.getElementById("node-card");
  let built = null;

  return function update(node, history, status) {
    if (!node) return;
    root.hidden = false;
    if (!built) {
      const title = el("h2", null, `Proxmox host · ${node.name}`);
      const sub = el("div", "sub");
      const tiles = el("div", "tiles");
      const cpu = tile("CPU", { unit: "%" });
      const load = tile("Load 1 / 5 / 15");
      const up = tile("Uptime");
      const guests = tile("Guests running");
      tiles.append(cpu.el, load.el, up.el, guests.el);
      const mem = meter("Memory");
      const spark = new Spark([{ key: "node.cpu", color: "var(--series-1)", label: "CPU" }],
        { format: fmt.pct, unit: "%", domain: [0, 100] });
      const error = el("div", "card-error");
      error.hidden = true;
      root.append(title, sub, tiles, mem.el, spark.el, error);
      built = { sub, cpu, load, up, guests, mem, spark, error };
    }
    // Trim pve-manager's build hash — it is noise at a glance.
    const version = (node.pveversion || "").split("/").slice(0, 2).join(" ");
    built.sub.textContent = `${node.cores} cores / ${node.cpus} threads · ${version}`;
    built.cpu.set(fmt.pct(node.cpu_pct));
    built.load.set((node.loadavg || []).map(v => v.toFixed(2)).join(" / "));
    built.up.set(fmt.uptime(node.uptime));
    built.guests.set(`${node.guests_running} / ${node.guests_total}`);
    built.mem.set(node.mem, node.maxmem,
      `${fmt.gib(node.mem)} / ${fmt.gib(node.maxmem)} GiB`);
    built.spark.update(history);
    applyStale({ el: root, error: built.error }, status);
  };
})();

/* ---------------- source strip ---------------- */

function renderSources(sources) {
  const host = document.getElementById("sources");
  host.textContent = "";
  for (const [name, status] of Object.entries(sources)) {
    const item = el("span", "src");
    if (!status.ok) item.classList.add("is-down");
    else if (status.stale) item.classList.add("is-stale");
    const dot = el("span", "dot");
    const label = el("span", null, name);
    const age = el("span", "age",
      status.age == null ? "never" : `${status.age.toFixed(0)}s`);
    item.append(dot, label, age);
    host.appendChild(item);
  }
}

/* ---------------- main loop ---------------- */

const cards = new Map();

function controllerFor(guest) {
  if (guest.view === "gpu") return gpuCard(guest);
  if (guest.view === "nas") return nasCard(guest);
  return basicCard(guest);
}

function render(state) {
  renderSources(state.sources);
  nodeCard(state.node, state.history.pve, state.sources.pve);

  const feature = document.getElementById("feature");
  const basics = document.getElementById("basics");
  const seen = new Set();

  for (const guest of state.guests) {
    seen.add(guest.vmid);
    let card = cards.get(guest.vmid);
    if (!card) {
      card = controllerFor(guest);
      cards.set(guest.vmid, card);
      (guest.view === "basic" ? basics : feature).appendChild(card.el);
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

tick();
setInterval(tick, POLL_MS);

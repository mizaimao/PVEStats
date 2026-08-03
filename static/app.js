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
  const caption = el("div", "label", label);
  root.append(caption, value);
  return {
    el: root,
    set: v => { number.nodeValue = v; },
    setLabel: text => { caption.textContent = text; },
  };
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

/* ---------------- arc gauge ----------------
 * A 270-degree arc, open at the bottom. `nested` adds a second inner ring so
 * one glyph can carry two ratios — CPU outside, memory inside.
 *
 * Worth knowing: arcs are harder to compare across cards than aligned bars,
 * because there is no shared baseline. Fine for a single prominent value,
 * weaker for scanning a grid.
 */

const ARC_SWEEP = 0.75;   // 270 of 360 degrees

function arcRing(svg, radius, width, colour) {
  const circumference = 2 * Math.PI * radius;
  const common = {
    cx: 50, cy: 50, r: radius, fill: "none", "stroke-width": width,
    "stroke-linecap": "round", transform: "rotate(135 50 50)",
  };
  const track = svgEl("circle", {
    ...common,
    stroke: `color-mix(in oklab, ${colour} 22%, var(--surface))`,
    "stroke-dasharray": `${circumference * ARC_SWEEP} ${circumference}`,
  });
  const fill = svgEl("circle", {
    ...common, stroke: colour, "stroke-dasharray": `0 ${circumference}`,
    // Butt, not round: a round cap paints a full-width dot even at near-zero
    // length, so an idle guest at 0.3% grew a blob that read as a real value.
    // Square at the origin is also the correct treatment for a value scale.
    "stroke-linecap": "butt",
  });
  svg.append(track, fill);
  return value => fill.setAttribute(
    "stroke-dasharray",
    `${circumference * ARC_SWEEP * Math.max(0, Math.min(1, value || 0))} ${circumference}`);
}

function arcGauge(label, { nested = false, sublabel = "" } = {}) {
  const root = el("div", nested ? "arc is-nested" : "arc");
  const svg = svgEl("svg", { viewBox: "0 0 100 100" });
  root.appendChild(svg);

  const outer = arcRing(svg, 41, 8, "var(--series-1)");
  const inner = nested ? arcRing(svg, 29, 7, "var(--series-2)") : null;

  const centre = el("div", "arc-centre");
  const value = el("div", "arc-value", "–");
  const caption = el("div", "arc-label", label);
  centre.append(value, caption);
  root.appendChild(centre);

  const foot = el("div", "arc-foot", sublabel);
  if (sublabel || nested) root.appendChild(foot);

  return {
    el: root,
    set(ratio, text, innerRatio, footText) {
      outer(ratio);
      if (inner) inner(innerRatio);
      value.textContent = text;
      if (footText != null) foot.textContent = footText;
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
                        domain = null, caption = "", axis = false,
                        xTicks = false, secondsPerSample = 1 } = {}) {
    this.series = series;                 // [{key, color, label}]
    this.height = height;
    this.format = format;
    this.unit = unit;
    this.domain = domain;
    this.axis = axis;
    // Right-hand gutter reserved for axis labels, sized to the unit — a fixed
    // 46px clipped " MiB/s" to " MiB/".
    this.gutter = axis ? 44 + unit.length * 4 : 0;
    this.xTicks = xTicks;
    this.secondsPerSample = secondsPerSample;
    // Room under the plot for the time labels.
    this.footer = xTicks ? 15 : 0;
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
    // Gridlines mark the midpoint and the ceiling of a fixed domain, with
    // labels when `axis` is on — otherwise a flat trace is unreadable, since
    // nothing says what the height means.
    this.gridFractions = [0.5, 1];
    // Gridlines whenever an axis is wanted, not only for fixed domains — an
    // auto-scaled throughput chart is just as unreadable without them.
    this.gridlines = (domain || axis)
      ? this.gridFractions.map(() => {
          const line = svgEl("line", { stroke: "var(--grid)", "stroke-width": "1" });
          this.svg.appendChild(line);
          return line;
        })
      : [];
    this.axisLabels = axis
      ? this.gridFractions.map(() => {
          const text = svgEl("text", {
            "text-anchor": "start", "dominant-baseline": "middle",
            fill: "var(--ink-muted)", "font-size": "10",
          });
          this.svg.appendChild(text);
          return text;
        })
      : [];

    // Vertical time ticks. Built lazily on first draw, since how many there
    // are depends on the buffer capacity the server reports.
    this.xTickNodes = [];

    this.parts = series.map(s => {
      // Taller charts cover far more area, so the same opacity reads as a
      // solid block rather than a wash.
      const area = svgEl("path", {
        fill: s.color, "fill-opacity": height > 60 ? "0.06" : "0.10",
      });
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

  update(history, capacity) {
    this.data = this.series.map(s => (history && history[s.key]) || []);
    // The x-axis spans the buffer's full capacity, not the samples collected
    // so far. Without this the line stretches as history accumulates and the
    // same reading appears to move.
    if (capacity) this.capacity = capacity;
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
    const plot = Math.max(20, w - this.gutter);   // trace area, minus axis gutter
    const body = h - this.footer;                 // plot area, minus time labels
    this.svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    this.svg.setAttribute("height", h);
    const max = this.scale();
    const pad = 3;
    this.drawTimeTicks(plot, body, h);
    // The peak duplicates the topmost axis label, so only show it when there
    // is no axis to read the scale from.
    if (!this.domain && !this.axis) {
      this.peak.textContent = `peak ${this.format(max)}${this.unit}`;
    }
    this.gridlines.forEach((line, i) => {
      const frac = this.gridFractions[i];
      const y = (body - pad - frac * (body - pad * 2)).toFixed(1);
      line.setAttribute("x1", 0); line.setAttribute("x2", plot);
      line.setAttribute("y1", y); line.setAttribute("y2", y);
      const text = this.axisLabels[i];
      if (text) {
        text.setAttribute("x", plot + 6);
        // Keep the label inside the box — the topmost gridline sits at y=pad,
        // and a centred label there is half clipped by the svg edge.
        text.setAttribute("y", Math.min(Math.max(+y, 7), body - 5));
        text.textContent = `${this.format(max * frac)}${this.unit}`;
      }
    });

    this.data.forEach((values, i) => {
      const { area, line, dot } = this.parts[i];
      if (values.length < 2) {
        line.setAttribute("points", "");
        area.setAttribute("d", "");
        dot.setAttribute("r", "0");
        return;
      }
      // Fixed step across the buffer's capacity, with the newest sample pinned
      // to the right edge, so the trace fills in from the right and then
      // scrolls rather than stretching.
      const span = Math.max(this.capacity || values.length, values.length);
      const step = plot / (span - 1);
      const x0 = plot - (values.length - 1) * step;
      const y = v => body - pad - (v / max) * (body - pad * 2);
      const points = values.map((v, j) =>
        `${(x0 + j * step).toFixed(1)},${y(v).toFixed(1)}`);
      line.setAttribute("points", points.join(" "));
      area.setAttribute("d",
        `M${x0.toFixed(1)},${body} L${points.join(" L")} L${plot.toFixed(1)},${body} Z`);
      dot.setAttribute("r", "4");
      dot.setAttribute("cx", plot.toFixed(1));
      dot.setAttribute("cy", y(values[values.length - 1]).toFixed(1));
    });
  }

  /* Time ticks along the bottom: the x-axis spans the whole ring buffer, so
     the span is capacity x sample interval. Without these a flat trace gives
     no sense of how far back it reaches. */
  drawTimeTicks(plot, body, h) {
    if (!this.xTicks || !this.capacity) return;
    const spanMin = (this.capacity * this.secondsPerSample) / 60;
    const step = spanMin <= 5 ? 1 : spanMin <= 15 ? 2 : 5;
    const marks = [];
    for (let t = 0; t <= spanMin + 0.001; t += step) marks.push(t);

    while (this.xTickNodes.length < marks.length) {
      const line = svgEl("line", { stroke: "var(--grid)", "stroke-width": "1" });
      const text = svgEl("text", {
        "text-anchor": "middle", fill: "var(--ink-muted)", "font-size": "10",
      });
      // Behind the data: inserted before the series parts.
      this.svg.insertBefore(line, this.svg.firstChild);
      this.svg.appendChild(text);
      this.xTickNodes.push({ line, text });
    }
    this.xTickNodes.forEach((node, i) => {
      if (i >= marks.length) {
        node.line.setAttribute("stroke-opacity", "0");
        node.text.textContent = "";
        return;
      }
      const minutesAgo = marks[i];
      const x = plot * (1 - minutesAgo / spanMin);
      node.line.setAttribute("stroke-opacity", "1");
      node.line.setAttribute("x1", x.toFixed(1));
      node.line.setAttribute("x2", x.toFixed(1));
      node.line.setAttribute("y1", 0);
      node.line.setAttribute("y2", body);
      node.text.setAttribute("x", Math.min(Math.max(x, 12), plot - 8).toFixed(1));
      node.text.setAttribute("y", h - 3);
      node.text.textContent = minutesAgo === 0 ? "now" : `-${minutesAgo}m`;
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
  // Two bars rather than arcs: aligned baselines make the grid scannable, and
  // the two metrics read the same way.
  const cpu = meter("CPU");
  const mem = meter("Memory");
  shell.el.append(cpu.el, mem.el);

  return {
    el: shell.el,
    update(g, history, status) {
      shell.sub.textContent = `vmid ${g.vmid} · up ${fmt.uptime(g.uptime)}`;
      cpu.set(g.cpu_pct || 0, 100, `${fmt.pct(g.cpu_pct)}%`);
      mem.set(g.mem, g.maxmem,
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

  // One plot for both, nvtop style: utilisation and VRAM share a 0-100 scale,
  // so they belong on the same axis instead of two stacked charts.
  const gpuSeries = [
    { key: "gpu.util", color: "var(--series-1)", label: "GPU utilisation" },
    { key: "gpu.vram_pct", color: "var(--series-2)", label: "VRAM used" },
  ];
  const spark = new Spark(gpuSeries,
    { format: fmt.pct, unit: "%", domain: [0, 100], axis: true, height: 312,
      xTicks: true, secondsPerSample: 1,
      caption: "GPU and VRAM, last 10 minutes" });
  const gpuLegend = legend(gpuSeries);

  const vramNow = tile("VRAM in use", { unit: " GiB" });

  const tiles = el("div", "tiles");
  const temp = tile("Temp", { unit: "°C" });
  const power = tile("Power", { unit: "W" });
  const fan = tile("Fan", { unit: "%" });
  const clock = tile("SM clock", { unit: "MHz" });
  const cpu = tile("CPU", { unit: "%" });
  const pcieLink = tile("PCIe link");
  const pcieRate = tile("PCIe rx/tx MiB/s");
  tiles.append(temp.el, power.el, fan.el, clock.el,
               pcieLink.el, pcieRate.el, cpu.el);

  const mem = meter("Memory");

  const models = dataTable("Loaded models", [
    { label: "Model" }, { label: "VRAM", num: true }, { label: "Unloads in", num: true },
  ]);
  const procs = dataTable("GPU processes", [
    { label: "Process" }, { label: "PID", num: true }, { label: "VRAM", num: true },
  ]);

  vramNow.el.classList.add("after-chart");
  models.el.classList.add("divided");
  procs.el.classList.add("divided");
  shell.el.append(heroWrap, spark.el, gpuLegend, vramNow.el, tiles, mem.el,
                  models.el, procs.el);

  return {
    el: shell.el,
    update(g, history, status) {
      // PVE panels (10s) update regardless of the agent's state, and the
      // agent panels (1s) never wait on PVE — §4.2.
      cpu.set(fmt.pct(g.cpu_pct));
      shell.sub.textContent = `vmid ${g.vmid} · GPU · up ${fmt.uptime(g.uptime)}`;
      mem.set(g.mem, g.maxmem, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);

      const gpu = g.gpu;
      if (gpu) {
        util.set(fmt.pct(gpu.util));
        temp.set(fmt.pct(gpu.temp));
        power.set(gpu.power == null ? "–" : Math.round(gpu.power));
        fan.set(gpu.fan == null ? "–" : fmt.pct(gpu.fan));
        clock.set(fmt.int(gpu.clock_sm));
        vramNow.set(`${fmt.gib(gpu.vram_used)} / ${fmt.gib(gpu.vram_total)}`);
        const pcie = gpu.pcie || {};
        // One string, e.g. "1.0 x16". NVIDIA parks the link at Gen1 when idle
        // and renegotiates under load, so a low current generation is normal
        // — the ceiling goes in the label rather than being flagged as a fault.
        pcieLink.set(pcie.gen ? `${pcie.gen}.0 x${pcie.width}` : "–");
        pcieLink.setLabel(pcie.max_gen ? `PCIe · max ${pcie.max_gen}.0` : "PCIe");
        // One decimal: the link idles well under 1 MiB/s, which rounded to "1".
        pcieRate.set(pcie.rx_kbps == null ? "–"
          : `${(pcie.rx_kbps / 1024).toFixed(1)} / ${(pcie.tx_kbps / 1024).toFixed(1)}`);
        models.set((gpu.models || []).map(m => [
          m.name || "?", `${fmt.gib(m.size_vram)} GiB`, fmt.countdown(m.unload_in),
        ]));
        procs.set((gpu.procs || []).map(p => [
          p.name || "?", String(p.pid), p.vram == null ? "–" : `${fmt.gib(p.vram)} GiB`,
        ]));
        spark.update(history.agent, g.gpu_status && g.gpu_status.capacity);
      } else {
        // Agent unreachable. Clear its panels explicitly rather than leaving
        // whatever was last drawn — the card must not imply live readings.
        [util, temp, power, fan, clock, vramNow, pcieLink, pcieRate]
          .forEach(t => t.set("–"));
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

  const row = el("div", "arc-row");
  const cpu = arcGauge("CPU");
  const mem = arcGauge("Memory");
  row.append(cpu.el, mem.el);
  shell.el.append(row);

  // Interface throughput is read from inside TrueNAS, never from PVE's
  // counters (spec §3.3). Two series, so a legend is mandatory.
  const ifSeries = [];
  const ifSpark = el("div");
  const ifTable = dataTable("Interfaces", [
    { label: "Interface" }, { label: "Role" },
    { label: "RX MiB/s", num: true }, { label: "TX MiB/s", num: true },
  ]);
  const disks = dataTable("Disks", [
    { label: "Disk" }, { label: "Size", num: true },
    { label: "Read MiB/s", num: true }, { label: "Write MiB/s", num: true },
  ]);
  const pools = dataTable("Pools", [
    { label: "Pool" }, { label: "Status" }, { label: "Capacity" }, { label: "Used", num: true },
  ]);
  shell.el.append(ifSpark, ifTable.el, disks.el, pools.el);

  let spark = null;

  return {
    el: shell.el,
    update(g, history, status) {
      cpu.set((g.cpu_pct || 0) / 100, `${fmt.pct(g.cpu_pct)}%`);
      mem.set(g.maxmem ? g.mem / g.maxmem : 0,
              `${fmt.pct(g.maxmem ? (g.mem / g.maxmem) * 100 : null)}%`,
              null, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);
      shell.sub.textContent = `vmid ${g.vmid} · NAS · up ${fmt.uptime(g.uptime)}`;

      const nas = g.nas;
      if (nas) {
        const names = Object.keys(nas.interfaces || {});
        // Build the throughput sparkline once the interface names are known.
        if (!spark && names.length) {
          const colours = ["var(--series-1)", "var(--series-2)"];
          names.slice(0, 2).forEach((name, i) => ifSeries.push({
            key: `if.${name}.rx`, color: colours[i],
            // Role travels in the legend, so the two lines are identifiable
            // without cross-referencing the table.
            label: `${name} · ${(nas.interfaces[name] || {}).label
                || (nas.interfaces[name] || {}).role || "?"}`,
          }));
          spark = new Spark(ifSeries, {
            format: fmt.rate, unit: " MiB/s", height: 96, axis: true,
            xTicks: true, secondsPerSample: 5,
            caption: "Receive throughput, last 10 minutes",
          });
          ifSpark.append(spark.el, legend(ifSeries));
        }
        if (spark) spark.update(history.truenas, g.nas_status && g.nas_status.capacity);

        ifTable.set(names.map(n => {
          const nic = nas.interfaces[n];
          const role = nic.label
                     || (nic.role === "virtual" ? "virtual (virtio)"
                       : nic.role === "physical" ? `physical · ${nic.media || "passthrough"}`
                       : "unknown");
          return [n, role, fmt.rate(nic.rx_bps), fmt.rate(nic.tx_bps)];
        }));
        disks.set(Object.entries(nas.disks || {}).map(([n, d]) => [
          n,
          d.size ? `${(d.size / 1024 ** 4).toFixed(1)} TiB` : "–",
          fmt.rate(d.read_bps), fmt.rate(d.write_bps),
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
      const row = el("div", "arc-row");
      const mem = arcGauge("Memory");
      const load = tile("Load 1 / 5 / 15");
      const up = tile("Uptime");
      const guests = tile("Guests running");
      row.append(mem.el, load.el, up.el, guests.el);

      // Labelled: the top gridline is 100% of the host's cores, the middle
      // one 50%. Previously this was an unlabelled trace with no way to tell
      // what a given height meant.
      const spark = new Spark(
        [{ key: "node.cpu", color: "var(--series-1)", label: "CPU" }],
        { format: fmt.pct, unit: "%", domain: [0, 100], axis: true, height: 84,
          caption: "Host CPU, last 10 minutes" });

      const error = el("div", "card-error");
      error.hidden = true;
      root.append(title, sub, row, spark.el, error);
      built = { sub, load, up, guests, mem, spark, error };
    }
    // Trim pve-manager's build hash — it is noise at a glance.
    const version = (node.pveversion || "").split("/").slice(0, 2).join(" ");
    built.sub.textContent = `${node.cores} cores / ${node.cpus} threads · ${version}`;
    built.load.set((node.loadavg || []).map(v => v.toFixed(2)).join(" / "));
    built.up.set(fmt.uptime(node.uptime));
    built.guests.set(`${node.guests_running} / ${node.guests_total}`);
    built.mem.set(node.maxmem ? node.mem / node.maxmem : 0,
      `${fmt.pct(node.maxmem ? (node.mem / node.maxmem) * 100 : null)}%`,
      null, `${fmt.gib(node.mem)} / ${fmt.gib(node.maxmem)} GiB`);
    built.spark.update(history, status && status.capacity);
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

/* The page refresh rate is derived, not chosen: polling faster than the
   fastest source only redraws numbers that have not changed. */
function refreshInterval(sources) {
  const intervals = Object.values(sources)
    .map(s => s.interval)
    .filter(v => typeof v === "number" && v > 0);
  return intervals.length ? Math.min(...intervals) : 1;
}

function render(state) {
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

/* ---------------- settings panel ----------------
 * Opens and closes only. Every control inside is disabled until there is
 * something to configure.
 */

(function settingsPanel() {
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
    if (e.key === "Escape") setOpen(false);
  });
  document.addEventListener("click", e => {
    if (!panel.hidden && !panel.contains(e.target) && !button.contains(e.target)) {
      setOpen(false);
    }
  });
})();

tick();
setInterval(tick, POLL_MS);

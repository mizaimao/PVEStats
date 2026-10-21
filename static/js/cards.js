import { fmt, GiB, MiB } from "./format.js";
import { el, svgEl } from "./dom.js";
import { tile, meter, arcGauge, Spark, legend, dataTable, statPair, statLine }
  from "./widgets.js";

/* ---------------- cards ---------------- */

/* Subtitle parts as separate spans so a long one wraps onto its own line at
   narrow widths instead of overflowing the card. */
export function setSub(shell, parts) {
  const wanted = parts.filter(Boolean);
  if (shell._sub && shell._sub.join("\u0000") === wanted.join("\u0000")) return;
  shell._sub = wanted;
  shell.sub.textContent = "";
  wanted.forEach((part, i) => {
    // The separator lives inside the following span, so it wraps with its own
    // text instead of dangling at the end of the previous line.
    const node = el("span");
    if (i) node.appendChild(el("span", "sep", "· "));
    node.appendChild(document.createTextNode(part));
    shell.sub.appendChild(node);
  });
}

export function cardShell(title, subtitle) {
  const root = el("section", "card");
  const h = el("h2", null, title);
  const sub = el("div", "sub", subtitle || "");
  const error = el("div", "card-error");
  error.hidden = true;
  root.append(h, sub);
  return { el: root, sub, error, title: h };
}

export function baseGuestPanels(shell) {
  const tiles = el("div", "tiles");
  const cpu = tile("CPU", { unit: "%" });
  const up = tile("Uptime");
  tiles.append(cpu.el, up.el);
  const mem = meter("Memory");
  shell.el.append(tiles, mem.el);
  return { cpu, up, mem };
}

export function applyStale(shell, status) {
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

/* Third row shared by every guest card except the NAS, whose own card reads
   these from inside TrueNAS instead. */
function ioRow({ diskIo = true } = {}) {
  const root = el("div", "trio");
  const disk = diskIo ? statPair("Disk IO", "read", "write", "MiB/s") : null;
  const net = statPair("Network", "in", "out", "MiB/s");
  const usage = statLine("Disk usage");
  if (disk) root.appendChild(disk.el);
  root.append(net.el, usage.el);

  return {
    el: root,
    update(g) {
      if (disk) {
        // Storage passed through to a guest bypasses the hypervisor, so its
        // counters sit near zero however busy the disk is. Drop the panel
        // rather than show numbers that mean nothing.
        const usable = g.disk_io_visible !== false;
        disk.el.hidden = !usable;
        if (usable) disk.set(fmt.rate(g.disk_read_bps), fmt.rate(g.disk_write_bps));
      }
      net.set(fmt.rate(g.net_in_bps), fmt.rate(g.net_out_bps));

      // PVE sends null when the guest agent gave no usage. A card PVE has
      // not reported yet (a placeholder) has no field at all: no data, not
      // no agent.
      const fs = g.filesystem;
      usage.set(fs
        ? `${Math.round((fs.used / fs.total) * 100)}% · `
          + `${fmt.gib(fs.used)} / ${fmt.gib(fs.total)} GiB`
        : fs === null ? "no guest agent" : "–");
    },
  };
}

/* --- 4.4 basic guest --- */

export function basicCard(guest) {
  const shell = cardShell(guest.name, `vmid ${guest.vmid}`);
  // Two bars rather than arcs: aligned baselines make the grid scannable, and
  // the two metrics read the same way.
  const cpu = meter("CPU");
  const mem = meter("Memory");
  const io = ioRow();
  shell.el.append(cpu.el, mem.el, io.el);

  return {
    el: shell.el,
    update(g, history, status) {
      setSub(shell, [`vmid ${g.vmid}`, (g.addresses || [])[0],
                     `up ${fmt.uptime(g.uptime)}`]);
      cpu.set(g.cpu_pct || 0, 100, fmt.pctText(g.cpu_pct));
      mem.set(g.mem, g.maxmem,
        `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);
      io.update(g);
      applyStale(shell, status);
    },
  };
}

/* --- 4.2 ml60 --- */

export function gpuCard(guest) {
  const shell = cardShell(guest.name, `vmid ${guest.vmid} · GPU`);

  // Headline row: the three figures worth reading first, side by side.
  const heroWrap = el("div", "gpu-headline");
  const util = tile("GPU utilization", { hero: true, unit: "%" });
  const vramNow = tile("VRAM in use", { unit: " GiB" });
  let vramBand = null;
  const pcieRate = tile("PCIe rx/tx MiB/s");
  heroWrap.append(util.el, vramNow.el, pcieRate.el);

  // One plot for both, nvtop style: utilization and VRAM share a 0-100 scale,
  // so they belong on the same axis instead of two stacked charts.
  const gpuSeries = [
    { key: "gpu.util", color: "var(--series-1)", label: "GPU utilization" },
    { key: "gpu.vram_pct", color: "var(--series-2)", label: "VRAM used" },
  ];
  const spark = new Spark(gpuSeries,
    { format: fmt.pct, unit: "%", domain: [0, 100], axis: true, height: 312,
      xTicks: true, secondsPerSample: 1,
      caption: "GPU and VRAM" });
  const gpuLegend = legend(gpuSeries);

  const tiles = el("div", "tiles");
  const temp = tile("Temp", { unit: "°C" });
  const power = tile("Power", { unit: "W" });
  const fan = tile("Fan", { unit: "%" });
  const clock = tile("SM clock", { unit: "MHz" });
  const pcieLink = tile("PCIe link");
  tiles.append(temp.el, power.el, fan.el, clock.el, pcieLink.el);

  // Guest CPU and memory side by side, both as bars so they read the same way.
  const usage = el("div", "bar-pair");
  const cpu = meter("CPU");
  const mem = meter("Memory");
  usage.append(cpu.el, mem.el);
  // ml60's storage is passed through, so the hypervisor never sees its disk
  // traffic — the panel would only ever read zero.
  const io = ioRow({ diskIo: false });

  const models = dataTable("Loaded models", [
    { label: "Model" }, { label: "Processor" },
    { label: "VRAM", num: true }, { label: "Unloads in", num: true },
    { label: "", num: true },
  ]);
  const procs = dataTable("GPU processes", [
    { label: "Process" }, { label: "PID", num: true }, { label: "VRAM", num: true },
  ]);

  tiles.classList.add("after-chart");
  models.el.classList.add("divided");
  procs.el.classList.add("divided");
  shell.el.append(heroWrap, spark.el, gpuLegend, tiles, usage, io.el,
                  models.el, procs.el);

  return {
    el: shell.el,
    update(g, history, status) {
      // PVE panels (10s) update regardless of the agent's state, and the
      // agent panels (1s) never wait on PVE — §4.2.
      cpu.set(g.cpu_pct || 0, 100, fmt.pctText(g.cpu_pct));
      setSub(shell, [`vmid ${g.vmid}`, "GPU", (g.addresses || [])[0],
                     `up ${fmt.uptime(g.uptime)}`]);
      mem.set(g.mem, g.maxmem, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);
      io.update(g);

      const gpu = g.gpu;
      if (gpu) {
        util.set(fmt.pct(gpu.util));
        temp.set(fmt.pct(gpu.temp));
        power.set(gpu.power == null ? "–" : Math.round(gpu.power));
        fan.set(gpu.fan == null ? "–" : fmt.pct(gpu.fan));
        clock.set(fmt.int(gpu.clock_sm));
        vramNow.set(`${fmt.gib(gpu.vram_used)} / ${fmt.gib(gpu.vram_total)}`);

        // VRAM filling up is the failure that actually bites on this box: a
        // model that no longer fits gets evicted or spills to host memory.
        const vramRatio = gpu.vram_total ? gpu.vram_used / gpu.vram_total : 0;
        const band = vramRatio >= 0.9 ? "critical"
                   : vramRatio >= 0.8 ? "warning" : "ok";
        if (band !== vramBand) {
          vramBand = band;
          const colour = band === "critical" ? "var(--critical)"
                       : band === "warning" ? "var(--warning)"
                       : "var(--series-2)";
          vramNow.el.classList.toggle("is-critical", band === "critical");
          vramNow.el.classList.toggle("is-warning", band === "warning");
          // The VRAM trace changes with it, so the chart agrees with the tile.
          spark.setSeriesColor(1, colour);
          gpuLegend.setColor(1, colour);
        }
        const pcie = gpu.pcie || {};
        // One string, e.g. "1.0 x16". NVIDIA parks the link at Gen1 when idle
        // and renegotiates under load, so a low current generation is normal
        // — the ceiling goes in the label rather than being flagged as a fault.
        pcieLink.set(pcie.gen ? `${pcie.gen}.0 x${pcie.width}` : "–");
        pcieLink.setLabel(pcie.max_gen ? `PCIe · max ${pcie.max_gen}.0` : "PCIe");
        // One decimal: the link idles well under 1 MiB/s, which rounded to "1".
        pcieRate.set(pcie.rx_kbps == null ? "–"
          : `${(pcie.rx_kbps / 1024).toFixed(1)} / ${(pcie.tx_kbps / 1024).toFixed(1)}`);
        if (!modelsBusy()) {
          models.set((gpu.models || []).map(m => {
            // Where the model actually runs. A model that spilled to system RAM
            // is not slightly slower, it is an order of magnitude slower, and
            // GPU utilization alone will not tell you.
            const proc = el("span", m.offloaded ? "offloaded" : null,
                            m.processor || "unknown");
            // Absent unless the agent says it will honour an unload, so an
            // agent with the write path closed off shows no buttons at all.
            const stop = gpu.can_unload && m.name ? unloadButton(m.name) : el("span");
            return [m.name || "?", proc, `${fmt.gib(m.size_vram)} GiB`,
                    fmt.countdown(m.unload_in), stop];
          }));
          // Visible, not only in the button's tooltip.
          const note = unloadFailureNote();
          models.setCaption(note ? `Loaded models · ${note}` : "Loaded models");
        }
        procs.set((gpu.procs || []).map(p => [
          p.name || "?", String(p.pid), p.vram == null ? "–" : `${fmt.gib(p.vram)} GiB`,
        ]));
        spark.update(history.agent, g.gpu_status && g.gpu_status.capacity);
      } else {
        // Agent unreachable. Clear its panels explicitly rather than leaving
        // whatever was last drawn — the card must not imply live readings.
        // CPU and memory are PVE's and were set above, so they stay.
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

/* --- unloading a model ---------------------------------------------------
 *
 * The only control in this dashboard that changes anything on a monitored
 * machine. Everything else is read-only by design, so this is deliberately
 * two-step: the first click arms, the second releases. Dropping a 40 GiB
 * model by misclick would mean waiting out a reload.
 *
 * It is recoverable rather than destructive — Ollama reloads the model on the
 * next request to it — and it goes no further than Ollama's own memory
 * management. Nothing is written to ml60's disk.
 */
const ARM_MS = 4000;
const FAILED_MS = 8000;      // long enough to read why
const STOPPED_MS = 10000;    // until the agent's list drops the model
const armed = new Map();     // model name -> when the arming lapses
const unloading = new Set();
// Outcomes live here, not on the button: the table is rebuilt every tick, so
// an outcome kept on the node went with it, and "Failed" showed for under a
// second before a fresh "Stop" replaced it, reason and all.
const failures = new Map();  // model name -> { message, until }
const stopped = new Map();   // model name -> until

/* The models table is rebuilt on every tick. While a button is armed or
   working, that rebuild would replace the node mid-click, so it is held off
   until the interaction settles; a countdown frozen for a second is a much
   smaller problem than a button that swallows presses. An arming that lapses
   needs no timer of its own: the next rebuild draws a plain "Stop". */
function modelsBusy() {
  const now = performance.now();
  for (const [name, until] of armed) if (until <= now) armed.delete(name);
  return armed.size > 0 || unloading.size > 0;
}

/* Why the most recent unload failed, while that is still news. */
function unloadFailureNote() {
  const now = performance.now();
  let latest = null;
  for (const [name, failure] of failures) {
    if (failure.until <= now) failures.delete(name);
    else if (!latest || failure.until > latest.until) latest = { name, ...failure };
  }
  return latest && `stopping ${latest.name} failed: ${latest.message}`;
}

function unloadButton(name) {
  const btn = el("button", "stop", "Stop");
  btn.type = "button";
  btn.title = `Unload ${name} from VRAM — it reloads on the next request to it`;

  // Rebuilt every tick, so start from the model's last outcome.
  const now = performance.now();
  const failure = failures.get(name);
  if (failure && failure.until > now) {
    btn.className = "stop is-failed";
    btn.textContent = "Failed";
    btn.title = failure.message;
  } else if ((stopped.get(name) || 0) > now) {
    btn.textContent = "Stopped";
    btn.disabled = true;
  }

  btn.addEventListener("click", async () => {
    if (!((armed.get(name) || 0) > performance.now())) {
      failures.delete(name);
      armed.set(name, performance.now() + ARM_MS);
      btn.className = "stop is-armed";
      btn.textContent = "Confirm";
      return;
    }
    armed.delete(name);
    unloading.add(name);
    btn.className = "stop is-busy";
    btn.textContent = "Stopping…";
    btn.disabled = true;
    try {
      const response = await fetch("/api/agent/unload", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: name }),
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok || !result.ok) throw new Error(result.error || `HTTP ${response.status}`);
      // The next poll dropping the row is the real confirmation that Ollama
      // let go of the model; until then it reads "Stopped".
      stopped.set(name, performance.now() + STOPPED_MS);
      btn.textContent = "Stopped";
    } catch (err) {
      const message = String(err.message || err);
      failures.set(name, { message, until: performance.now() + FAILED_MS });
      btn.className = "stop is-failed";
      btn.textContent = "Failed";
      btn.title = message;
      btn.disabled = false;
    } finally {
      unloading.delete(name);
    }
  });
  return btn;
}

/* --- 4.3 TrueNAS --- */

export function nasCard(guest) {
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
      cpu.set((g.cpu_pct || 0) / 100, fmt.pctText(g.cpu_pct));
      mem.set(g.maxmem ? g.mem / g.maxmem : 0,
              fmt.pctText(g.maxmem ? (g.mem / g.maxmem) * 100 : null),
              null, `${fmt.gib(g.mem)} / ${fmt.gib(g.maxmem)} GiB`);
      setSub(shell, [`vmid ${g.vmid}`, "NAS", (g.addresses || [])[0],
                     `up ${fmt.uptime(g.uptime)}`]);

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
            xTicks: true, secondsPerSample: 5, minScale: 2 * MiB,
            caption: "Receive throughput",
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

export const nodeCard = (() => {
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
          // PVE polls every 10s; without this the caption and any time ticks
          // would read the buffer as one sample per second.
          xTicks: true, secondsPerSample: 10, caption: "Host CPU" });

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
      fmt.pctText(node.maxmem ? (node.mem / node.maxmem) * 100 : null),
      null, `${fmt.gib(node.mem)} / ${fmt.gib(node.maxmem)} GiB`);
    built.spark.update(history, status && status.capacity);
    // The card has no CPU tile (spec §4.1 lists CPU % first), so the current
    // figure goes on the chart's caption line rather than only under hover.
    built.spark.setReadout(fmt.pctText(node.cpu_pct));
    applyStale({ el: root, error: built.error }, status);
  };
})();

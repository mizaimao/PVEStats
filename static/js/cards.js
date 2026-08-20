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

      const fs = g.filesystem;
      usage.set(fs
        ? `${Math.round((fs.used / fs.total) * 100)}% · `
          + `${fmt.gib(fs.used)} / ${fmt.gib(fs.total)} GiB`
        : "no guest agent");
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
      cpu.set(g.cpu_pct || 0, 100, `${fmt.pct(g.cpu_pct)}%`);
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
      cpu.set(g.cpu_pct || 0, 100, `${fmt.pct(g.cpu_pct)}%`);
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
        models.set((gpu.models || []).map(m => {
          // Where the model actually runs. A model that spilled to system RAM
          // is not slightly slower, it is an order of magnitude slower, and
          // GPU utilisation alone will not tell you.
          const proc = el("span", m.offloaded ? "offloaded" : null,
                          m.processor || "unknown");
          return [m.name || "?", proc,
                  `${fmt.gib(m.size_vram)} GiB`, fmt.countdown(m.unload_in)];
        }));
        procs.set((gpu.procs || []).map(p => [
          p.name || "?", String(p.pid), p.vram == null ? "–" : `${fmt.gib(p.vram)} GiB`,
        ]));
        spark.update(history.agent, g.gpu_status && g.gpu_status.capacity);
      } else {
        // Agent unreachable. Clear its panels explicitly rather than leaving
        // whatever was last drawn — the card must not imply live readings.
        [util, temp, power, fan, clock, vramNow, pcieLink, pcieRate]
          .forEach(t => t.set("–"));
        cpu.set(0, 100, "–");
        models.set([]);
        procs.set([]);
      }
      // Stale styling follows the agent, since that is what this card leads on.
      applyStale(shell, g.gpu_status || status);
    },
  };
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
      cpu.set((g.cpu_pct || 0) / 100, `${fmt.pct(g.cpu_pct)}%`);
      mem.set(g.maxmem ? g.mem / g.maxmem : 0,
              `${fmt.pct(g.maxmem ? (g.mem / g.maxmem) * 100 : null)}%`,
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
      `${fmt.pct(node.maxmem ? (node.mem / node.maxmem) * 100 : null)}%`,
      null, `${fmt.gib(node.mem)} / ${fmt.gib(node.maxmem)} GiB`);
    built.spark.update(history, status && status.capacity);
    applyStale({ el: root, error: built.error }, status);
  };
})();

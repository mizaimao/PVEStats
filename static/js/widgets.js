import { fmt, severity, GiB, MiB } from "./format.js";
import { el, svgEl, setAnimated } from "./dom.js";

/* ---------------- stat tile ---------------- */

export function tile(label, { hero = false, unit = "" } = {}) {
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
  let shown = "–";
  return {
    el: root,
    set: v => { setAnimated(number, v, shown); shown = String(v); },
    setLabel: text => { caption.textContent = text; },
  };
}

/* ---------------- meter ---------------- */

/* `graded` opts into severity colouring. Off by default and deliberately not
   used for guest memory: PVE reports cache and balloon inside `mem`, so a
   healthy guest routinely sits above 90% and a red bar there would be crying
   wolf. ZFS pool capacity is graded, where >80% is a real concern. */
export function meter(label, { graded = false, compact = false } = {}) {
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
  fill.appendChild(el("i"));      // clipped sweep layer; see .meter > span > i
  track.appendChild(fill);
  root.append(track);
  let lastBand = null;
  return {
    el: root,
    set(used, total, text) {
      const ratio = total ? Math.min(1, used / total) : 0;
      const colour = graded ? severity(total ? used / total : null)
                            : "var(--series-1)";
      track.style.setProperty("--meter-color", colour);
      fill.style.width = `${(ratio * 100).toFixed(1)}%`;
      readout.textContent = text;

      // Flare only on the way up, and only for graded meters: crossing into a
      // worse band is news, sitting in one is not.
      if (graded) {
        const band = colour.includes("critical") ? 2
                   : colour.includes("warning") ? 1 : 0;
        if (lastBand !== null && band > lastBand) {
          track.classList.remove("flare");
          void track.offsetWidth;                 // restart the animation
          track.classList.add("flare");
          setTimeout(() => track.classList.remove("flare"), 2400);
        }
        lastBand = band;
      }
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

export const ARC_SWEEP = 0.75;   // 270 of 360 degrees

export function arcRing(svg, radius, width, colour) {
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
  // The glow is drawn, not filtered: WKWebView's SVG renderer ignores CSS
  // filter functions on SVG children, so the drop-shadow these arcs used
  // never showed in the app. Three wider, fainter copies of the arc under
  // it, as the charts draw theirs; widths and opacity come from the glow
  // tokens, so a glow of 0 leaves nothing.
  const halos = ["a", "b", "c"].map(n => svgEl("circle", {
    ...common, stroke: colour, "stroke-dasharray": `0 ${circumference}`,
    "stroke-linecap": "butt", class: `arc-halo arc-halo-${n}`, style: `--w: ${width}px`,
  }));
  svg.append(track, halos[2], halos[1], halos[0], fill);
  return value => {
    const dash = `${circumference * ARC_SWEEP * Math.max(0, Math.min(1, value || 0))} ${circumference}`;
    fill.setAttribute("stroke-dasharray", dash);
    halos.forEach(h => h.setAttribute("stroke-dasharray", dash));
  };
}

export function arcGauge(label, { nested = false, sublabel = "" } = {}) {
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

  const number = document.createTextNode("–");
  value.textContent = "";
  value.appendChild(number);
  let shown = "–";

  return {
    el: root,
    set(ratio, text, innerRatio, footText) {
      outer(ratio);
      if (inner) inner(innerRatio);
      setAnimated(number, text, shown);
      shown = String(text);
      // Attached on first use: the foot used to be added only for a sublabel
      // or a nested ring, so memory's "used / total GiB" went into a detached
      // node and never showed.
      if (footText != null) {
        foot.textContent = footText;
        if (foot.parentNode !== root) root.appendChild(foot);
      }
    },
  };
}

/* ---------------- paired readout ----------------
 * Two related figures stacked — read/write, in/out. Kept as plain numbers:
 * these are rates with no ceiling, so a bar would need an arbitrary scale.
 */

export function statPair(title, firstLabel, secondLabel, unit = "") {
  const root = el("div", "pair");
  root.appendChild(el("div", "pair-title", `${title}${unit ? ` ${unit}` : ""}`));
  const rows = [firstLabel, secondLabel].map(name => {
    const row = el("div", "pair-row");
    const value = el("b", null, "–");
    row.append(el("span", null, name), value);
    root.appendChild(row);
    return value;
  });
  return {
    el: root,
    set(a, b) {
      rows[0].textContent = a;
      rows[1].textContent = b;
    },
  };
}

/* A title over a single line of text. */
export function statLine(title) {
  const root = el("div", "pair");
  root.appendChild(el("div", "pair-title", title));
  const value = el("div", "pair-value", "–");
  root.appendChild(value);
  return { el: root, set: text => { value.textContent = text; } };
}

/* ---------------- sparkline ----------------
 * Rendered in real pixel coordinates rather than a stretched viewBox, so the
 * 2px stroke and the end dot stay circular at any width.
 */

export class Spark {
  /* `domain` fixes the y-scale. Percentages get [0, 100] — auto-scaling a
     percentage makes a host idling at 2% look like it is thrashing, and with
     no y-axis on a sparkline the reader has no way to tell. Series with no
     natural ceiling (throughput) auto-scale and print their peak instead, so
     the scale is still readable. */
  constructor(series, { height = 38, format = fmt.pct, unit = "",
                        domain = null, caption = "", axis = false,
                        xTicks = false, secondsPerSample = 1,
                        minScale = 0 } = {}) {
    this.series = series;                 // [{key, color, label}]
    this.height = height;
    this.format = format;
    this.unit = unit;
    this.domain = domain;
    this.axis = axis;
    // Floor for the auto-scale. Without it an idle series collapses to a
    // 1-byte range and both axis labels round to the same "0.0".
    this.minScale = minScale;
    // Right-hand gutter reserved for axis labels, sized to the unit — a fixed
    // 46px clipped " MiB/s" to " MiB/".
    this.gutter = axis ? 44 + unit.length * 4 : 0;
    this.reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    this._drawn = false;
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
    this.captionNode = el("span", null, caption);
    this.baseCaption = caption;
    this.head.append(this.captionNode, this.peak);
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

    // Unique per instance: gradient ids are document-global, so two charts
    // sharing one id would silently borrow each other's fade.
    this.uid = `sp${Math.random().toString(36).slice(2, 9)}`;
    const defs = svgEl("defs", {});
    this.svg.appendChild(defs);

    this.parts = series.map((s, i) => {
      /* The glow is drawn, not filtered.

         CSS filter on SVG children proved unreliable: the filter region comes
         from the element's bounding box, which is zero-height for a flat
         trace, and WebKit and Blink disagree about the rest. So the halo is
         three real strokes of the same path at increasing width and falling
         opacity, sitting under the line. No filters, no bounding boxes, and it
         renders identically in every engine. Widths and opacities come from
         the glow tokens, so a scale of 0 collapses them to nothing. */
      const halo = ["halo-a", "halo-b", "halo-c"].map(name =>
        svgEl("path", { class: `halo ${name}`, fill: "none", stroke: s.color }));

      // Taller charts cover far more area, so the same opacity reads as a
      // solid block rather than a wash.
      const area = svgEl("path", {
        fill: s.color, "fill-opacity": height > 60 ? "0.06" : "0.10",
      });
      // A path, not a polyline: nulls mark polls that never happened, and a
      // path can start a new subpath so the break is visible instead of the
      // line being drawn straight across missing time.
      const line = svgEl("path", {
        fill: "none", stroke: s.color, "stroke-width": "2",
        "stroke-linejoin": "round", "stroke-linecap": "round",
      });
      // 2px ring in the surface colour keeps the end dot legible where it
      // crosses another line.
      const dot = svgEl("circle", {
        r: "4", fill: s.color, stroke: "var(--surface)", "stroke-width": "2",
      });
      // Comet trail: the last stretch of the line redrawn over itself with a
      // gradient that fades to nothing, so the eye is pulled to `now`.
      const gradientId = `${this.uid}-${i}`;
      // In drawing coordinates, set from the tail on every draw. The default
      // (the path's bounding box) ignores the gradient when the box has no
      // height, so a flat tail, an idle GPU or NIC, lost its comet entirely.
      const gradient = svgEl("linearGradient", {
        id: gradientId, gradientUnits: "userSpaceOnUse",
        x1: "0", x2: "1", y1: "0", y2: "0",
      });
      const stopFrom = svgEl("stop", {
        offset: "0", "stop-color": s.color, "stop-opacity": "0" });
      // Brightens toward the leading end so "now" is the hottest point.
      const stopTo = svgEl("stop", {
        offset: "1", "stop-color": "var(--ink)", "stop-opacity": "0.75" });
      gradient.append(stopFrom, stopTo);
      defs.appendChild(gradient);
      // Same 2px weight as the line: a fatter comet made the trace visibly
      // thicker for its last stretch, which read as a rendering fault rather
      // than emphasis. The gradient and the glow carry it instead.
      const comet = svgEl("path", {
        fill: "none", stroke: `url(#${gradientId})`, "stroke-width": "2",
        "stroke-linecap": "round", "stroke-linejoin": "round",
      });

      // Order matters: wash, then halo widest-first, then the crisp line.
      this.svg.append(area, halo[2], halo[1], halo[0], line, comet, dot);
      return { halo, area, line, comet, dot, gradient, stopFrom, stopTo };
    });

    this.tip = el("div", "tip");
    this.el.appendChild(this.tip);
    // Remembered so update() can re-read the sample under a resting pointer;
    // the trace scrolls beneath it every tick.
    this.pointerX = null;
    this.svg.addEventListener("mousemove", e => { this.pointerX = e.clientX; this.hover(e); });
    this.svg.addEventListener("mouseleave", () => { this.pointerX = null; });

    new ResizeObserver(entries => {
      const w = Math.max(40, Math.round(entries[0].contentRect.width));
      if (w !== this.width) { this.width = w; this.draw(); }
    }).observe(this.el);
  }

  /* The current reading, printed at the right of the caption line. For a
     chart whose card has no tile for its value, so the figure is always on
     screen rather than only under the pointer. Uses the slot the auto-scaled
     charts print their peak in, which fixed-axis charts leave empty. */
  setReadout(text) {
    this.peak.classList.add("now");
    this.peak.textContent = text;
  }

  /* Recolour a series in place — used when a value crosses into a status
     band, so the trace itself carries the warning rather than only a figure
     elsewhere on the card. */
  setSeriesColor(index, colour) {
    const series = this.series[index];
    const part = this.parts[index];
    if (!series || !part || series.color === colour) return;
    series.color = colour;
    part.line.setAttribute("stroke", colour);
    part.halo.forEach(h => h.setAttribute("stroke", colour));
    part.area.setAttribute("fill", colour);
    part.dot.setAttribute("fill", colour);
    part.stopFrom.setAttribute("stop-color", colour);
  }

  /* Draw-in: the line sweeps on once, the first time there is anything to
     draw. Re-running it on every tick would make the chart unreadable. */
  playDrawIn() {
    if (this._drawn || this.reduced) return;
    const paths = this.parts.map(p => p.line).filter(p => p.getAttribute("d"));
    if (!paths.length) return;
    this._drawn = true;
    for (const path of paths) {
      // Measured in units of the path itself (pathLength 1): the first draw
      // can happen at the 240px placeholder width, and the ResizeObserver
      // redraws at the real width before the first paint, so a dash measured
      // in pixels slid along the longer line and then popped.
      path.setAttribute("pathLength", "1");
      path.style.strokeDasharray = "1";
      path.style.strokeDashoffset = "1";
      path.getBoundingClientRect();               // force layout before animating
      path.style.transition = "stroke-dashoffset 900ms ease-out";
      path.style.strokeDashoffset = "0";
      setTimeout(() => {
        path.style.transition = path.style.strokeDasharray =
          path.style.strokeDashoffset = "";
        path.removeAttribute("pathLength");
      }, 1000);
    }
  }

  update(history, capacity) {
    this.data = this.series.map(s => (history && history[s.key]) || []);
    // The x-axis spans the buffer's full capacity, not the samples collected
    // so far. Without this the line stretches as history accumulates and the
    // same reading appears to move.
    if (capacity) this.capacity = capacity;
    // The caption states the real span rather than a baked-in figure, so it
    // stays true when the history window changes.
    if (this.baseCaption && this.capacity) {
      const minutes = (this.capacity * this.secondsPerSample) / 60;
      const span = minutes >= 1
        ? `${minutes % 1 ? minutes.toFixed(1) : minutes} min`
        : `${Math.round(minutes * 60)}s`;
      this.captionNode.textContent = `${this.baseCaption}, last ${span}`;
    }
    this.draw();
    this.playDrawIn();
    if (this.pointerX != null) this.hover({ clientX: this.pointerX });
  }

  scale() {
    if (this.domain) return this.domain[1];
    let max = 0;
    for (const values of this.data) {
      for (const v of values) if (v != null && v > max) max = v;
    }
    return Math.max(max, this.minScale) || 1;
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
      const { area, line, comet, dot, halo } = this.parts[i];
      if (values.length < 2) {
        line.setAttribute("d", "");
        area.setAttribute("d", "");
        comet.setAttribute("d", "");
        halo.forEach(h => h.setAttribute("d", ""));
        dot.setAttribute("r", "0");
        return;
      }
      const { step, x0 } = this.xLayout(values.length);
      const y = v => body - pad - (v / max) * (body - pad * 2);

      // Walk the series, splitting into runs separated by gaps.
      const runs = [];
      let run = null;
      values.forEach((v, j) => {
        if (v == null) { run = null; return; }
        const point = [x0 + j * step, y(v)];
        if (run) run.push(point);
        else runs.push(run = [point]);
      });

      // A run of one point (a reading between two gaps) is a zero-length
      // segment, which the round cap draws as a dot; a lone "M" is never
      // stroked, so it used to vanish.
      const stroke = runs.map(r => {
        const pts = r.length === 1 ? [r[0], r[0]] : r;
        return "M" + pts.map(([x, yy]) => `${x.toFixed(1)},${yy.toFixed(1)}`).join(" L");
      }).join(" ");
      line.setAttribute("d", stroke);
      halo.forEach(h => h.setAttribute("d", stroke));
      // One closed shape per run, so the wash does not span a gap either.
      area.setAttribute("d", runs.map(r =>
        `M${r[0][0].toFixed(1)},${body} `
        + r.map(([x, yy]) => `L${x.toFixed(1)},${yy.toFixed(1)}`).join(" ")
        + ` L${r[r.length - 1][0].toFixed(1)},${body} Z`).join(" "));

      // Trail over the tail of the newest run only — a comet across a gap
      // would imply data that is not there.
      const newest = runs.length ? runs[runs.length - 1] : null;
      if (newest && newest.length > 1) {
        const tail = newest.slice(-Math.min(newest.length, 14));
        comet.setAttribute("d",
          "M" + tail.map(([x, yy]) => `${x.toFixed(1)},${yy.toFixed(1)}`).join(" L"));
        this.parts[i].gradient.setAttribute("x1", tail[0][0].toFixed(1));
        this.parts[i].gradient.setAttribute("x2", tail[tail.length - 1][0].toFixed(1));
      } else {
        comet.setAttribute("d", "");
      }

      // The end dot marks the newest reading, which after a stall is the end
      // of the newest run rather than the right edge.
      const last = newest ? newest[newest.length - 1] : null;
      dot.setAttribute("r", last ? "4" : "0");
      if (last) {
        dot.setAttribute("cx", last[0].toFixed(1));
        dot.setAttribute("cy", last[1].toFixed(1));
      }
    });
  }

  /* Time ticks along the bottom: the x-axis spans the whole ring buffer, so
     the span is capacity x sample interval. Without these a flat trace gives
     no sense of how far back it reaches. */
  drawTimeTicks(plot, body, h) {
    if (!this.xTicks || !this.capacity) return;
    // capacity samples span capacity - 1 intervals (xLayout), so the left
    // edge is that old; counting capacity intervals drifted the ticks by up
    // to a whole sample, which is 10s on the PVE chart.
    const spanMin = ((this.capacity - 1) * this.secondsPerSample) / 60;
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

  /* Where samples sit horizontally: a fixed step across the buffer's
     capacity, with the newest sample pinned to the right edge of the plot, so
     the trace fills in from the right and then scrolls rather than
     stretching. Shared by draw() and hover() so the tooltip always reads the
     sample under the cursor. */
  xLayout(count) {
    const plot = Math.max(20, this.width - this.gutter);
    const span = Math.max(this.capacity || count, count);
    const step = plot / (span - 1);
    return { step, x0: plot - (count - 1) * step };
  }

  /* Hover enhances; it never gates a value — the current reading is always
     printed in the tile beside the sparkline.

     Hiding sets opacity inline; showing clears it, so the stylesheet's
     `.spark:hover .tip` rule applies again. A lingering inline 0 would hide
     the tip for good. */
  hover(event) {
    const values = this.data[0] || [];
    const rect = this.svg.getBoundingClientRect();
    const hide = () => { this.tip.style.opacity = "0"; };
    if (values.length < 2 || !rect.width) return hide();

    const scale = rect.width / this.width;          // drawing units to pixels
    const { step, x0 } = this.xLayout(values.length);
    const x = (event.clientX - rect.left) / scale;
    // Left of the oldest sample is time not collected yet.
    if (x < x0 - step / 2) return hide();
    const index = Math.min(values.length - 1, Math.round((x - x0) / step));
    const samplesAgo = values.length - 1 - index;

    // Series can differ in length, and each is right-aligned, so count back
    // from each one's newest sample rather than reusing the first's index.
    const parts = this.data
      .map((s, i) => {
        const v = s[s.length - 1 - samplesAgo];
        return `${this.series[i].label}: `
          + (v == null ? "no data" : `${this.format(v)}${this.unit}`);
      })
      .join("  ");
    this.tip.textContent = samplesAgo ? `${parts}  ·  ${samplesAgo} samples ago` : parts;
    this.tip.style.opacity = "";
    // Pinned to the sample, not the cursor, so it points at what it reads,
    // but kept inside the chart: centred on the newest GPU sample it ran off
    // the window and left the page scrolling sideways.
    const offset = rect.left - this.el.getBoundingClientRect().left;
    const half = this.tip.offsetWidth / 2;
    const centre = offset + (x0 + index * step) * scale;
    const room = this.el.clientWidth || rect.width;
    this.tip.style.left = `${Math.min(Math.max(centre, half), room - half).toFixed(1)}px`;
  }
}

export function legend(series) {
  const root = el("div", "legend");
  const swatches = series.map(s => {
    const item = el("span");
    const swatch = el("i");
    swatch.style.background = s.color;
    item.append(swatch, document.createTextNode(s.label));
    root.appendChild(item);
    return swatch;
  });
  // The legend is the dependable identity channel, so it has to track a
  // recoloured series rather than keep pointing at the old hue.
  root.setColor = (index, colour) => {
    if (swatches[index]) swatches[index].style.background = colour;
  };
  return root;
}

/* ---------------- table ---------------- */

export function dataTable(caption, columns) {
  const table = el("table", "data");
  const captionNode = el("caption", null, caption);
  table.appendChild(captionNode);
  const head = el("tr");
  for (const c of columns) head.appendChild(el("th", c.num ? "num" : null, c.label));
  const thead = document.createElement("thead");
  thead.appendChild(head);
  const tbody = document.createElement("tbody");
  table.append(thead, tbody);
  let keys = null;      // row keys of the body as drawn, when given
  return {
    el: table,
    setCaption(text) {
      if (captionNode.textContent !== text) captionNode.textContent = text;
    },
    sameKeys(next) {
      return !!keys && next.length === keys.length && next.every((k, i) => k === keys[i]);
    },
    /* With `rowKeys` matching the rows already drawn, cells change in place:
       a node already in its cell stays there (a button keeps a press or
       focus, a bar keeps its animation) and only changed text is written.
       Anything else rebuilds the body. */
    set(rows, rowKeys = null) {
      if (rowKeys && rows.length && this.sameKeys(rowKeys)) {
        rows.forEach((row, r) => {
          const tr = tbody.children[r];
          row.forEach((cell, i) => {
            const td = tr.children[i];
            if (cell instanceof Node) {
              if (td.firstChild !== cell || td.childNodes.length !== 1) {
                td.textContent = "";
                td.appendChild(cell);
              }
            } else if (td.textContent !== String(cell)) {
              td.textContent = cell;
            }
          });
        });
        return;
      }
      keys = rowKeys;
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

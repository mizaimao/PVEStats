//! The parts a card is built from, and how each one lays out and draws.
//!
//! A card is a list of parts. Each part has a fixed height for a given
//! width, and charts also take a share of whatever height is left over, so
//! one description serves the full 1920x1080 console and a narrow split.
//! Every write is clipped to the part's own area.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::theme::Look;
use crate::state::Series;

pub struct Pen<'a> {
    pub buf: &'a mut Buffer,
    pub look: &'a Look,
}

impl Pen<'_> {
    fn inside(&self, clip: Rect, x: u16, y: u16) -> Option<Rect> {
        let clip = clip.intersection(self.buf.area);
        (x >= clip.left() && x < clip.right() && y >= clip.top() && y < clip.bottom()).then_some(clip)
    }

    /// Writes a line at (x, y), cut at the right edge of `clip`. Returns
    /// the column after it.
    pub fn line(&mut self, clip: Rect, x: u16, y: u16, line: &Line) -> u16 {
        let Some(clip) = self.inside(clip, x, y) else { return x };
        self.buf.set_line(x, y, line, clip.right() - x).0
    }

    pub fn text(&mut self, clip: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
        self.line(clip, x, y, &Line::styled(text.to_string(), style))
    }

    pub fn cell(&mut self, clip: Rect, x: u16, y: u16, symbol: &str, style: Style) {
        if self.inside(clip, x, y).is_some() {
            if let Some(cell) = self.buf.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }

    pub fn fill_bg(&mut self, area: Rect, color: Option<Color>) {
        if let Some(color) = color {
            self.buf.set_style(area.intersection(self.buf.area), Style::default().bg(color));
        }
    }

    /// Greys out everything in `area`, for readings that are no longer live.
    pub fn dim(&mut self, area: Rect) {
        let area = area.intersection(self.buf.area);
        let muted = self.look.p.muted;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = self.buf.cell_mut((x, y)) {
                    cell.set_fg(muted);
                    if cell.bg != Color::Reset && Some(cell.bg) != self.look.p.surface {
                        cell.set_bg(self.look.p.surface.unwrap_or(Color::Reset));
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ parts

pub enum Part {
    Gap,
    Text(Line<'static>),
    Meter(Meter),
    Chart(Chart),
    /// A label, a short graph and the latest value, btop's per-core style.
    Spark(Spark),
    /// A value in digits five rows tall, then its unit in plain text.
    Big(String, String, Style),
    /// A muted label over its value.
    Tile(String, Line<'static>),
    /// Short lines set side by side, wrapped onto as many rows as needed.
    Chips(Vec<Line<'static>>),
    Table(Table),
    /// Children side by side: (fixed width or 0 for a share of the rest).
    Row(Vec<(u16, Part)>),
    /// Children side by side when each gets at least this many columns,
    /// otherwise stacked.
    Flow(u16, Vec<Part>),
    Stack(Vec<Part>),
    /// Greyed out: a reading from a source that is not answering.
    Dim(Box<Part>),
}

const GAP: u16 = 2;

impl Part {
    /// Rows needed at this width, before any share of spare rows.
    pub fn height(&self, width: u16) -> u16 {
        match self {
            Part::Gap | Part::Text(_) | Part::Meter(_) => 1,
            Part::Chart(c) => c.min_rows,
            Part::Spark(s) => s.rows,
            Part::Big(..) => 5,
            Part::Tile(..) => 2,
            Part::Chips(chips) => chip_lines(chips, width).len().max(1) as u16,
            Part::Table(t) => t.caption.is_some() as u16 + 1 + t.rows.len().max(1) as u16,
            Part::Row(children) => {
                let widths = row_widths(children, width);
                children.iter().zip(widths).map(|((_, p), w)| p.height(w)).max().unwrap_or(0)
            }
            Part::Flow(min, children) => match flow_widths(*min, children.len(), width) {
                Some(w) => children.iter().map(|p| p.height(w)).max().unwrap_or(0),
                None => children.iter().map(|p| p.height(width)).sum(),
            },
            Part::Stack(children) => children.iter().map(|p| p.height(width)).sum(),
            Part::Dim(inner) => inner.height(width),
        }
    }

    /// Weight for a share of spare rows; only charts grow.
    pub fn flex(&self) -> u16 {
        match self {
            Part::Chart(c) => c.weight,
            Part::Row(children) => children.iter().map(|(_, p)| p.flex()).max().unwrap_or(0),
            Part::Flow(_, children) | Part::Stack(children) => children.iter().map(Part::flex).sum(),
            Part::Dim(inner) => inner.flex(),
            _ => 0,
        }
    }

    pub fn render(&self, pen: &mut Pen, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let p = pen.look.p;
        match self {
            Part::Gap => {}
            Part::Text(line) => {
                pen.line(area, area.x, area.y, line);
            }
            Part::Meter(m) => m.render(pen, area),
            Part::Chart(c) => c.render(pen, area),
            Part::Spark(s) => s.render(pen, area),
            Part::Big(value, unit, style) => big(pen, area, value, unit, *style),
            Part::Tile(label, value) => {
                pen.text(area, area.x, area.y, label, Style::default().fg(p.muted));
                pen.line(area, area.x, area.y + 1, value);
            }
            Part::Chips(chips) => {
                for (i, line) in chip_lines(chips, area.width).iter().enumerate() {
                    pen.line(area, area.x, area.y + i as u16, line);
                }
            }
            Part::Table(t) => t.render(pen, area),
            Part::Row(children) => {
                let mut x = area.x;
                for ((_, child), w) in children.iter().zip(row_widths(children, area.width)) {
                    child.render(pen, Rect { x, width: w, ..area });
                    x += w + GAP;
                }
            }
            Part::Flow(min, children) => match flow_widths(*min, children.len(), area.width) {
                Some(w) => {
                    let mut x = area.x;
                    for child in children {
                        child.render(pen, Rect { x, width: w, ..area });
                        x += w + GAP;
                    }
                }
                None => stack(pen, area, children),
            },
            Part::Stack(children) => stack(pen, area, children),
            Part::Dim(inner) => {
                inner.render(pen, area);
                pen.dim(area);
            }
        }
    }
}

fn row_widths(children: &[(u16, Part)], width: u16) -> Vec<u16> {
    let gaps = GAP * children.len().saturating_sub(1) as u16;
    let fixed: u16 = children.iter().map(|(w, _)| *w).sum();
    let shares = children.iter().filter(|(w, _)| *w == 0).count().max(1) as u16;
    let spare = width.saturating_sub(fixed + gaps);
    children.iter().map(|(w, _)| if *w > 0 { *w } else { spare / shares }).collect()
}

fn flow_widths(min: u16, n: usize, width: u16) -> Option<u16> {
    let n = n.max(1) as u16;
    let each = width.saturating_sub(GAP * (n - 1)) / n;
    (each >= min).then_some(each)
}

/// Splits `total` rows among parts: each gets its own height, and the
/// rows left over go to the parts that grow, by weight. Short of rows, the
/// last parts get less, or none.
pub fn share(heights: &[u16], flexes: &[u16], total: u16) -> Vec<u16> {
    let need: u16 = heights.iter().sum();
    let mut out = heights.to_vec();
    if need <= total {
        let spare = total - need;
        let weight: u16 = flexes.iter().sum();
        if weight > 0 {
            let mut given = 0;
            let last = flexes.iter().rposition(|f| *f > 0).unwrap_or(0);
            for (i, f) in flexes.iter().enumerate() {
                if *f == 0 {
                    continue;
                }
                let add = if i == last { spare - given } else { spare * f / weight };
                out[i] += add;
                given += add;
            }
        }
    } else {
        let mut left = total;
        for h in &mut out {
            *h = (*h).min(left);
            left -= *h;
        }
    }
    out
}

/// Rows for each part. Charts take two thirds of any spare rows and the
/// gaps below the first chart the other third, so a chart does not crowd
/// what follows it. With no gap below a chart it takes all of them; with no
/// chart, the gaps do.
fn layout_rows(parts: &[Part], width: u16, total: u16) -> Vec<u16> {
    let heights: Vec<u16> = parts.iter().map(|p| p.height(width)).collect();
    let flexes: Vec<u16> = parts.iter().map(Part::flex).collect();
    let need: u16 = heights.iter().sum();
    if need >= total {
        return share(&heights, &flexes, total);
    }
    let spare = total - need;
    let first = flexes.iter().position(|f| *f > 0);
    let gaps: Vec<usize> = (first.map_or(0, |f| f + 1)..parts.len())
        .filter(|&i| matches!(parts[i], Part::Gap))
        .collect();
    let to_charts = match (first, gaps.is_empty()) {
        (None, _) => 0,
        (Some(_), true) => spare,
        (Some(_), false) => (spare * 2).div_ceil(3),
    };
    let mut out = share(&heights, &flexes, need + to_charts);
    spread(&mut out, &gaps, spare - to_charts);
    out
}

/// Adds `rows` to the entries at `at`, as evenly as they divide.
pub fn spread(heights: &mut [u16], at: &[usize], rows: u16) {
    let n = at.len() as u16;
    for (k, &i) in at.iter().enumerate() {
        heights[i] += rows / n + u16::from((k as u16) < rows % n);
    }
}

fn stack(pen: &mut Pen, area: Rect, parts: &[Part]) {
    let mut y = area.y;
    for (part, h) in parts.iter().zip(layout_rows(parts, area.width, area.height)) {
        part.render(pen, Rect { y, height: h, ..area });
        y += h;
    }
}

fn chip_lines(chips: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for chip in chips {
        let w = chip.width();
        if used > 0 && used + 3 + w > width as usize {
            lines.push(Line::from(std::mem::take(&mut current)));
            used = 0;
        }
        if used > 0 {
            current.push(Span::raw("   "));
            used += 3;
        }
        current.extend(chip.spans.iter().cloned());
        used += w;
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

// ----------------------------------------------------------------- meters

pub struct Meter {
    pub label: String,
    pub label_w: u16,
    /// 0..1; None draws an empty track.
    pub ratio: Option<f64>,
    pub color: Color,
    pub value: String,
    pub value_w: u16,
}

impl Meter {
    fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        pen.text(area, area.x, area.y, &self.label, Style::default().fg(p.muted));
        let value_w = self.value_w.max(self.value.chars().count() as u16).min(area.width);
        let bar_x = area.x + self.label_w;
        let bar_w = area.width.saturating_sub(self.label_w + value_w + 1);
        if bar_w > 0 {
            let line = bar(pen.look, self.ratio, bar_w, self.color);
            pen.line(area, bar_x, area.y, &line);
        }
        let value = crate::format::right(&self.value, value_w as usize);
        pen.text(area, area.right() - value_w, area.y, &value, Style::default().fg(p.ink));
    }
}

/// A horizontal bar as a line of `width` cells: a row of small squares,
/// btop style, lit up to the ratio. Full blocks on a shaded track on the
/// console, whose font has no small square worth the name.
pub fn bar(look: &Look, ratio: Option<f64>, width: u16, color: Color) -> Line<'static> {
    let lit = (ratio.unwrap_or(0.0).clamp(0.0, 1.0) * width as f64).round() as usize;
    let rest = (width as usize).saturating_sub(lit);
    let (on, off) = if look.console_glyphs { ("█", "░") } else { ("■", "■") };
    Line::from(vec![
        Span::styled(on.repeat(lit), Style::default().fg(color)),
        Span::styled(off.repeat(rest), Style::default().fg(look.p.grid)),
    ])
}

// ----------------------------------------------------------------- charts

pub struct Trace {
    pub data: Vec<Option<f64>>,
    /// Series 1 (0) or 2 (1), for its colour.
    pub series: usize,
    pub label: String,
}

impl Trace {
    pub fn from(data: Option<&Series>, series: usize, label: &str) -> Trace {
        Trace {
            data: data.map(|s| s.iter().copied().collect()).unwrap_or_default(),
            series,
            label: label.to_string(),
        }
    }

    fn peak(&self) -> f64 {
        self.data.iter().flatten().fold(0.0f64, |a, &b| a.max(b))
    }
}

/// The first trace is drawn as a filled area, any others as lines over it.
pub struct Chart {
    pub traces: Vec<Trace>,
    /// Samples across the full width: the ring buffer's length, so the
    /// x-axis holds still while history fills.
    pub capacity: usize,
    /// A fixed top (100 for percentages), or None to scale to the data.
    pub top: Option<f64>,
    /// The scaled top never goes below this, so noise stays flat.
    pub floor: f64,
    pub axis: fn(f64) -> String,
    pub seconds_per_sample: f64,
    pub caption: String,
    pub min_rows: u16,
    pub weight: u16,
}

impl Chart {
    fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        let rows = area.height.saturating_sub(1);
        let peak = self.traces.iter().map(Trace::peak).fold(0.0, f64::max);
        let top = self.top.unwrap_or_else(|| nice(peak.max(self.floor)));
        let label_w = (self.axis)(top).chars().count().max((self.axis)(0.0).chars().count()) as u16;
        let plot_area = Rect {
            x: area.x + label_w + 1,
            y: area.y,
            width: area.width.saturating_sub(label_w + 1),
            height: rows,
        };
        let muted = Style::default().fg(p.muted);
        let grid = Style::default().fg(p.grid);

        // Axis: labels at the top and bottom, and on the row holding the
        // halfway mark when the chart is tall enough to spare one.
        let mid = (rows >= 6).then(|| rows - 1 - rows / 2);
        for r in 0..rows {
            let y = area.y + r;
            let label = if r == 0 {
                Some((self.axis)(top))
            } else if r == rows - 1 {
                Some((self.axis)(0.0))
            } else if Some(r) == mid {
                Some((self.axis)(top / 2.0))
            } else {
                None
            };
            if let Some(label) = &label {
                pen.text(area, area.x, y, &crate::format::right(label, label_w as usize), muted);
                if r != 0 && r != rows - 1 {
                    for x in plot_area.left()..plot_area.right() {
                        pen.cell(area, x, y, "·", grid);
                    }
                }
            }
            pen.cell(area, area.x + label_w, y, if label.is_some() { "┤" } else { "│" }, grid);
        }
        plot(pen, plot_area, &self.traces, self.capacity, top);

        // Bottom row: the span on the left, the legend, "now" on the right.
        let y = area.bottom() - 1;
        let span = span(self.capacity as f64 * self.seconds_per_sample);
        let now_x = area.right().saturating_sub(3);
        let left = Rect { width: now_x.saturating_sub(area.x + 1), ..area };
        let x = pen.text(left, plot_area.x, y, &span, muted);
        let mut legend = vec![Span::raw("  "), Span::styled(self.caption.clone(), Style::default().fg(p.ink2))];
        let area_swatch = if pen.look.console_glyphs { "█" } else { "⣿" };
        for (i, trace) in self.traces.iter().enumerate() {
            if trace.label.is_empty() {
                continue;
            }
            legend.push(Span::raw("  "));
            let swatch = if i == 0 { area_swatch } else { "─" };
            legend.push(Span::styled(swatch, Style::default().fg(pen.look.flat(trace.series))));
            legend.push(Span::styled(format!(" {}", trace.label), muted));
        }
        pen.line(left, x, y, &Line::from(legend));
        pen.text(area, now_x, y, "now", muted);
    }
}

/// btop's per-core row: a label, a short graph of the recent past scaled to
/// its own peak, and the latest value. With two rows or more, the peak is
/// printed under the value, so the graph's height can be read.
pub struct Spark {
    pub label: Line<'static>,
    pub label_w: u16,
    pub trace: Trace,
    pub capacity: usize,
    pub floor: f64,
    pub value: String,
    /// Formats the peak for the second row.
    pub peak: fn(f64) -> String,
    pub value_w: u16,
    pub rows: u16,
}

impl Spark {
    fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        pen.line(area, area.x, area.y, &self.label);
        let value_x = area.right().saturating_sub(self.value_w);
        let value = crate::format::right(&self.value, self.value_w as usize);
        pen.text(area, value_x, area.y, &value, Style::default().fg(p.ink).add_modifier(Modifier::BOLD));
        let peak = self.trace.peak();
        if area.height > 1 {
            let text = format!("peak {}", (self.peak)(peak));
            let text = crate::format::right(&text, self.value_w as usize);
            pen.text(area, value_x, area.y + 1, &text, Style::default().fg(p.muted));
        }
        let graph = Rect {
            x: area.x + self.label_w,
            width: area.width.saturating_sub(self.label_w + self.value_w + 1),
            ..area
        };
        plot(pen, graph, std::slice::from_ref(&self.trace), self.capacity, nice(peak.max(self.floor)));
    }
}

/// Draws traces into `area`: the first as a filled area, the rest as lines
/// over it. Braille dots, two samples across and four levels up per cell,
/// shaded from the foot up; on the console, whole cells and shades.
fn plot(pen: &mut Pen, area: Rect, traces: &[Trace], capacity: usize, top: f64) {
    let area = area.intersection(pen.buf.area);
    if area.width == 0 || area.height == 0 || top <= 0.0 {
        return;
    }
    if pen.look.console_glyphs {
        plot_cells(pen, area, traces, capacity, top);
    } else {
        plot_braille(pen, area, traces, capacity, top);
    }
}

/// Braille dot bits, by side of the cell and by level from its foot.
const DOTS: [[u8; 4]; 2] = [[0x40, 0x04, 0x02, 0x01], [0x80, 0x20, 0x10, 0x08]];

fn plot_braille(pen: &mut Pen, area: Rect, traces: &[Trace], capacity: usize, top: f64) {
    let look = pen.look;
    let (w, rows) = (area.width as usize, area.height as usize);
    let (across, levels) = (w * 2, rows * 4);
    // Per cell, counted from the bottom row: its dots, and the line series
    // passing through it, if any.
    let mut dots = vec![0u8; w * rows];
    let mut line = vec![None; w * rows];
    let set = |dots: &mut [u8], sub: usize, level: usize| {
        dots[(level / 4) * w + sub / 2] |= DOTS[sub % 2][level % 4];
    };
    if let Some(first) = traces.first() {
        for sub in 0..across {
            let Some(v) = column_value(&first.data, capacity, sub, across) else { continue };
            let mut height = (v / top * levels as f64).round() as usize;
            // Any reading above zero shows, however small.
            if v > 0.0 {
                height = height.max(1);
            }
            for level in 0..height.min(levels) {
                set(&mut dots, sub, level);
            }
        }
    }
    for trace in traces.iter().skip(1) {
        for sub in 0..across {
            let Some(v) = column_value(&trace.data, capacity, sub, across) else { continue };
            let level = ((v / top * levels as f64).floor() as usize).min(levels - 1);
            set(&mut dots, sub, level);
            line[(level / 4) * w + sub / 2] = Some(trace.series);
        }
    }
    let series = traces.first().map_or(0, |t| t.series);
    for row in 0..rows {
        let shade = look.ramp(series, row as f64 / (rows - 1).max(1) as f64);
        for x in 0..w {
            let bits = dots[row * w + x];
            if bits == 0 {
                continue;
            }
            let color = line[row * w + x].map_or(shade, |s| look.flat(s));
            let symbol = char::from_u32(0x2800 + bits as u32).unwrap_or(' ').to_string();
            pen.cell(area, area.x + x as u16, area.bottom() - 1 - row as u16, &symbol, Style::default().fg(color));
        }
    }
}

fn plot_cells(pen: &mut Pen, area: Rect, traces: &[Trace], capacity: usize, top: f64) {
    let look = pen.look;
    let rows = area.height;
    for c in 0..area.width {
        let x = area.x + c;
        let column = |t: &Trace| column_value(&t.data, capacity, c as usize, area.width as usize);
        let mut filled = 0.0;
        if let Some(first) = traces.first() {
            if let Some(v) = column(first) {
                filled = v / top * rows as f64;
                area_column(pen, area, x, filled, look.flat(first.series));
            }
        }
        for trace in traces.iter().skip(1) {
            let Some(v) = column(trace) else { continue };
            let level = ((v / top * rows as f64).floor() as u16).min(rows - 1);
            let mut style = Style::default().fg(look.flat(trace.series));
            if (level as f64) < filled.floor() {
                style = style.bg(look.p.s1_under);
            }
            pen.cell(area, x, area.bottom() - 1 - level, "─", style);
        }
    }
}

/// The highest sample under column `c` of `width`, with the newest sample
/// at the right edge and `capacity` samples across.
fn column_value(data: &[Option<f64>], capacity: usize, c: usize, width: usize) -> Option<f64> {
    let (capacity, width) = (capacity.max(1), width.max(1));
    let lo = c * capacity / width;
    let hi = ((c + 1) * capacity / width).max(lo + 1);
    let offset = capacity.saturating_sub(data.len());
    (lo..hi)
        .filter_map(|slot| slot.checked_sub(offset).and_then(|i| data.get(i)).copied().flatten())
        .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v))))
}

fn area_column(pen: &mut Pen, plot: Rect, x: u16, height: f64, color: Color) {
    const SHADES: [&str; 2] = ["░", "▒"];
    let style = Style::default().fg(color);
    let height = height.clamp(0.0, plot.height as f64);
    let full = height.floor() as u16;
    for r in 0..full {
        pen.cell(plot, x, plot.bottom() - 1 - r, "█", style);
    }
    if full < plot.height {
        let step = ((height - full as f64) * (SHADES.len() + 1) as f64).floor() as usize;
        // Any reading above zero shows, however small.
        let step = if step == 0 && full == 0 && height > 0.0 { 1 } else { step };
        if step > 0 {
            pen.cell(plot, x, plot.bottom() - 1 - full, SHADES[step - 1], style);
        }
    }
}

/// 1, 2 or 5 times a power of ten, at or above `v`. MiB-scaled for byte
/// rates, since that is how they are labelled.
fn nice(v: f64) -> f64 {
    let unit = crate::format::MIB;
    let v = (v / unit).max(1e-9);
    let base = 10f64.powf(v.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * base).find(|s| *s >= v * 0.999).unwrap_or(10.0 * base);
    step * unit
}

fn span(seconds: f64) -> String {
    if seconds < 120.0 {
        format!("{seconds:.0}s ago")
    } else if seconds < 7200.0 {
        format!("{:.0}m ago", seconds / 60.0)
    } else {
        format!("{:.0}h ago", seconds / 3600.0)
    }
}

// ------------------------------------------------------------ big digits

/// Heavy box-drawing strokes, four columns by five rows.
const STROKES: [(char, [&str; 5]); 11] = [
    ('0', ["┏━━┓", "┃  ┃", "┃  ┃", "┃  ┃", "┗━━┛"]),
    ('1', ["╺━┓ ", "  ┃ ", "  ┃ ", "  ┃ ", "╺━┻╸"]),
    ('2', ["╺━━┓", "   ┃", "┏━━┛", "┃   ", "┗━━╸"]),
    ('3', ["╺━━┓", "   ┃", " ━━┫", "   ┃", "╺━━┛"]),
    ('4', ["╻  ╻", "┃  ┃", "┗━━┫", "   ┃", "   ╹"]),
    ('5', ["┏━━╸", "┃   ", "┗━━┓", "   ┃", "╺━━┛"]),
    ('6', ["┏━━╸", "┃   ", "┣━━┓", "┃  ┃", "┗━━┛"]),
    ('7', ["╺━━┓", "   ┃", "   ┃", "   ┃", "   ╹"]),
    ('8', ["┏━━┓", "┃  ┃", "┣━━┫", "┃  ┃", "┗━━┛"]),
    ('9', ["┏━━┓", "┃  ┃", "┗━━┫", "   ┃", "╺━━┛"]),
    ('–', ["    ", "    ", "╺━━╸", "    ", "    "]),
];

/// Block pixels for the console, whose font has few of those strokes:
/// three by five, each pixel two cells wide.
const BLOCKS: [(char, [&str; 5]); 11] = [
    ('0', ["###", "# #", "# #", "# #", "###"]),
    ('1', [" # ", "## ", " # ", " # ", "###"]),
    ('2', ["###", "  #", "###", "#  ", "###"]),
    ('3', ["###", "  #", " ##", "  #", "###"]),
    ('4', ["# #", "# #", "###", "  #", "  #"]),
    ('5', ["###", "#  ", "###", "  #", "###"]),
    ('6', ["###", "#  ", "###", "# #", "###"]),
    ('7', ["###", "  #", "  #", "  #", "  #"]),
    ('8', ["###", "# #", "###", "# #", "###"]),
    ('9', ["###", "# #", "###", "  #", "###"]),
    ('–', ["   ", "   ", "###", "   ", "   "]),
];

fn glyph_width(look: &Look) -> u16 {
    if look.console_glyphs {
        7
    } else {
        5
    }
}

/// Columns taken by `digits` big digits and a short unit after them.
pub fn big_width(look: &Look, digits: usize, unit: &str) -> u16 {
    digits as u16 * glyph_width(look) + unit.chars().count() as u16
}

fn big(pen: &mut Pen, area: Rect, value: &str, unit: &str, style: Style) {
    let mut x = area.x;
    for ch in value.chars() {
        let font = if pen.look.console_glyphs { &BLOCKS } else { &STROKES };
        if let Some((_, rows)) = font.iter().find(|(c, _)| *c == ch) {
            for (r, row) in rows.iter().enumerate() {
                let cells: String = if pen.look.console_glyphs {
                    row.chars().map(|c| if c == '#' { "██" } else { "  " }).collect()
                } else {
                    row.to_string()
                };
                pen.text(area, x, area.y + r as u16, &cells, style);
            }
        }
        x += glyph_width(pen.look);
    }
    // The unit sits small on the baseline.
    pen.text(area, x, area.y + 4, unit, style);
}

// ----------------------------------------------------------------- tables

pub struct Col {
    pub title: &'static str,
    /// Columns wide, or 0 to take what the others leave.
    pub width: u16,
    pub right: bool,
}

pub struct Table {
    pub caption: Option<Line<'static>>,
    pub cols: Vec<Col>,
    pub rows: Vec<Vec<Line<'static>>>,
    pub empty: &'static str,
}

impl Table {
    fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        let mut y = area.y;
        if let Some(caption) = &self.caption {
            pen.line(area, area.x, y, caption);
            y += 1;
        }
        let fixed: u16 = self.cols.iter().map(|c| c.width).sum();
        let gaps = GAP * self.cols.len().saturating_sub(1) as u16;
        let fills = self.cols.iter().filter(|c| c.width == 0).count().max(1) as u16;
        let fill = (area.width.saturating_sub(fixed + gaps) / fills).max(4);
        let widths: Vec<u16> = self.cols.iter().map(|c| if c.width == 0 { fill } else { c.width }).collect();

        let header = Style::default().fg(p.muted);
        let mut x = area.x;
        for (col, w) in self.cols.iter().zip(&widths) {
            let title = if col.right {
                crate::format::right(col.title, *w as usize)
            } else {
                crate::format::left(col.title, *w as usize)
            };
            pen.text(area, x, y, &title, header);
            x += w + GAP;
        }
        y += 1;
        if self.rows.is_empty() {
            pen.text(area, area.x, y, self.empty, header);
        }
        for row in &self.rows {
            let mut x = area.x;
            for ((col, w), cell) in self.cols.iter().zip(&widths).zip(row) {
                let clip = Rect { x, y, width: *w, height: 1 }.intersection(area);
                let cw = cell.width() as u16;
                let at = if col.right && cw < *w { x + w - cw } else { x };
                if cw > *w && !col.right {
                    // Cut with an ellipsis rather than mid-word.
                    let text: String = cell.spans.iter().map(|s| s.content.as_ref()).collect();
                    let style = cell.spans.first().map_or(Style::default(), |s| s.style);
                    pen.text(clip, x, y, &crate::format::fit(&text, *w as usize), style);
                } else {
                    pen.line(clip, at, y, cell);
                }
                x += w + GAP;
            }
            y += 1;
        }
    }
}

// ------------------------------------------------------------------ cards

pub struct Card {
    pub title: String,
    pub right: String,
    /// Not live: the card is greyed and its border says why, or "waiting".
    pub stale: bool,
    pub error: Option<String>,
    pub body: Vec<Part>,
}

impl Card {
    pub fn height(&self, width: u16) -> u16 {
        2 + self.body.iter().map(|p| p.height(width.saturating_sub(4))).sum::<u16>()
    }

    pub fn flex(&self) -> u16 {
        self.body.iter().map(Part::flex).sum()
    }

    /// Whether spare rows can open up space between its sections.
    pub fn has_gaps(&self) -> bool {
        self.body.iter().any(|p| matches!(p, Part::Gap))
    }

    pub fn render(&self, pen: &mut Pen, area: Rect) {
        let area = area.intersection(pen.buf.area);
        if area.height < 2 || area.width < 4 {
            return;
        }
        let p = pen.look.p;
        pen.fill_bg(area, p.surface);
        let border = Style::default().fg(if self.stale { p.muted } else { p.grid });
        let title_style = Style::default()
            .fg(if self.stale { p.muted } else { p.ink })
            .add_modifier(Modifier::BOLD);
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(Line::styled(format!(" {} ", self.title), title_style))
            .title_top(Line::styled(format!(" {} ", self.right), Style::default().fg(p.muted)).right_aligned());
        if self.stale {
            let (text, color) = match &self.error {
                Some(e) => (e.clone(), p.critical),
                None => ("waiting…".to_string(), p.muted),
            };
            block = block.title_bottom(Line::styled(format!(" {text} "), Style::default().fg(color)));
        }
        let inner = block.inner(area);
        block.render(area, pen.buf);
        let body = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        stack(pen, body, &self.body);
        if self.stale {
            pen.dim(body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_hold_the_newest_sample_right() {
        let data = vec![Some(1.0), Some(2.0), Some(3.0)];
        // Four slots, three samples: the first slot is empty.
        assert_eq!(column_value(&data, 4, 0, 4), None);
        assert_eq!(column_value(&data, 4, 3, 4), Some(3.0));
        // Two columns over four slots take the higher of each pair.
        assert_eq!(column_value(&data, 4, 0, 2), Some(1.0));
        assert_eq!(column_value(&data, 4, 1, 2), Some(3.0));
    }

    #[test]
    fn spare_rows_go_to_charts() {
        assert_eq!(share(&[1, 4, 2], &[0, 1, 0], 10), vec![1, 7, 2]);
        assert_eq!(share(&[1, 4, 2], &[0, 1, 1], 9), vec![1, 5, 3]);
        assert_eq!(share(&[3, 3, 3], &[0, 0, 0], 5), vec![3, 2, 0]);
    }

    #[test]
    fn charts_leave_a_third_below() {
        let chart = Part::Chart(Chart {
            traces: Vec::new(),
            capacity: 10,
            top: Some(100.0),
            floor: 0.0,
            axis: |v| format!("{v}"),
            seconds_per_sample: 1.0,
            caption: String::new(),
            min_rows: 4,
            weight: 1,
        });
        let parts = vec![Part::Gap, chart, Part::Gap, Part::Text(Line::raw("x"))];
        // Ten spare rows: seven to the chart, three to the gap below it.
        assert_eq!(layout_rows(&parts, 40, 17), vec![1, 11, 4, 1]);
    }

    #[test]
    fn scales() {
        let mib = crate::format::MIB;
        assert_eq!(nice(1.5 * mib), 2.0 * mib);
        assert_eq!(nice(2.0 * mib), 2.0 * mib);
        assert_eq!(nice(31.0 * mib), 50.0 * mib);
        assert_eq!(span(180.0), "3m ago");
        assert_eq!(span(90.0), "90s ago");
    }
}

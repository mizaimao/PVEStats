//! The screen: a header of source dots, then the cards, laid out for the
//! width there is. Three columns on a full 1920x1080 console (240 columns),
//! two in a wide split, one (scrolled) in a narrow one.

pub mod cards;
pub mod draw;
pub mod theme;

use std::collections::BTreeMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::{Config, View};
use crate::sources::{lock, Shared};
use crate::state::{clock, Status};
use cards::Slot;
use draw::{share, Card, Pen};
use theme::Look;

/// Widths at which the layout gains a column.
const THREE_COLUMNS: u16 = 150;
const TWO_COLUMNS: u16 = 96;
/// Narrowest guest card; the grid fits as many across as allow this.
const GUEST_WIDTH: u16 = 38;

/// How long the standby screen waits for every source before showing the
/// dashboard anyway, with the failures said on it.
const STANDBY_GIVE_UP: f64 = 25.0;

/// Against burn-in: the whole screen steps one cell around a small square,
/// one step every this many seconds, so no edge sits on the same pixels for
/// long. It keeps one column and one row free to move into.
const ORBIT_STEP: u64 = 240;
const ORBIT: [(u16, u16); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

pub struct Screen {
    /// Only the GPU card, which is what this dashboard exists for.
    pub focus: bool,
    /// First row shown when a single column is taller than the screen.
    pub scroll: u16,
    /// Step the screen around against burn-in.
    pub orbit: bool,
    started: f64,
    standby_done: bool,
}

impl Screen {
    pub fn new() -> Self {
        Screen { focus: false, scroll: 0, orbit: true, started: clock(), standby_done: false }
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, shared: &Shared, config: &Config, look: &Look) {
        let pve = lock(&shared.pve);
        let agent = lock(&shared.agent);
        let nas = lock(&shared.nas);
        let (sp, sa, sn) = (pve.status(), agent.status(), nas.status());
        let mut pen = Pen { buf, look };
        pen.fill_bg(area, look.p.plane);
        let area = if self.orbit && area.width > 1 && area.height > 1 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let (dx, dy) = ORBIT[(now / ORBIT_STEP % ORBIT.len() as u64) as usize];
            Rect { x: area.x + dx, y: area.y + dy, width: area.width - 1, height: area.height - 1 }
        } else {
            area
        };
        if area.height < 3 {
            return;
        }
        header(&mut pen, Rect { height: 1, ..area }, &[("pve", &sp), ("agent", &sa), ("truenas", &sn)]);
        let body = Rect { y: area.y + 1, height: area.height - 1, ..area };

        // Standby: held until there is something worth drawing. The first
        // PVE poll can be a full 10s away, so without it every start would
        // show an empty dashboard.
        if !self.standby_done {
            let have = pve.data.as_ref().is_some_and(|d| !d.guests.is_empty());
            // A source that is merely "not configured" is settled, not
            // pending: waiting on it would hold the screen forever.
            let settled = [&sp, &sa, &sn].iter().all(|s| s.ok || s.error.is_some());
            if have || settled || clock() - self.started > STANDBY_GIVE_UP {
                self.standby_done = true;
            } else {
                standby(&mut pen, body, &[("Proxmox VE", &sp), ("ml60 GPU agent", &sa), ("TrueNAS", &sn)]);
                return;
            }
        }

        // Guests in VMID order. Until PVE first answers there is no list of
        // running guests, but the GPU and NAS cards get a placeholder; once
        // PVE has answered, a guest missing from it is not running.
        // A name in config.yaml wins over the VM's name in Proxmox, which can
        // be years out of date.
        let mut slots: BTreeMap<u64, Slot> = BTreeMap::new();
        if let Some(data) = pve.data.as_ref() {
            for (&vmid, guest) in &data.guests {
                let name = config
                    .hosts
                    .get(&vmid)
                    .and_then(|h| h.name.clone())
                    .or_else(|| guest.name.clone())
                    .unwrap_or_else(|| vmid.to_string());
                slots.insert(vmid, Slot { vmid, name, view: config.view_for(vmid), guest: Some(guest) });
            }
        }
        if pve.updated_at.is_none() {
            for (&vmid, host) in &config.hosts {
                if host.view != View::Basic && !slots.contains_key(&vmid) {
                    let name = host.name.clone().unwrap_or_else(|| vmid.to_string());
                    slots.insert(vmid, Slot { vmid, name, view: host.view, guest: None });
                }
            }
        }

        let host_card = match pve.data.as_ref() {
            Some(d) => cards::host(look, &d.node, &pve, &sp),
            None => cards::pve_note(look, &sp),
        };
        let of = |view: View| slots.values().filter(move |s| s.view == view);
        let gpu_cards: Vec<Card> = of(View::Gpu).map(|s| cards::gpu(look, s, &agent, &sa, &sp)).collect();
        let nas_cards: Vec<Card> =
            of(View::Nas).map(|s| cards::nas(look, s, config.hosts.get(&s.vmid), &nas, &sn, &sp)).collect();
        let guests: Vec<&Slot> = of(View::Basic).collect();
        let grid = Grid { slots: &guests, status: &sp, look, across: None };

        if self.focus && !gpu_cards.is_empty() {
            column(&mut pen, body, &gpu_cards, None);
            return;
        }
        let w = body.width;
        if w >= THREE_COLUMNS {
            // The GPU card in the middle, host and NAS on its left, guests
            // on its right.
            let left = (w - 2) / 3;
            let mid = (w - left - 2) / 2;
            let right = w - left - mid - 2;
            let mut left_cards = vec![host_card];
            left_cards.extend(nas_cards);
            column(&mut pen, Rect { width: left, ..body }, &left_cards, None);
            column(&mut pen, Rect { x: body.x + left + 1, width: mid, ..body }, &gpu_cards, None);
            // Two guests a row, compact, to leave room below them.
            let pairs = Grid { across: Some(2), ..grid };
            column(&mut pen, Rect { x: body.x + left + mid + 2, width: right, ..body }, &[], Some(&pairs));
        } else if w >= TWO_COLUMNS {
            let left = (w - 1) / 2;
            let mut left_cards = vec![host_card];
            left_cards.extend(gpu_cards);
            column(&mut pen, Rect { width: left, ..body }, &left_cards, None);
            column(&mut pen, Rect { x: body.x + left + 1, width: w - left - 1, ..body }, &nas_cards, Some(&grid));
        } else {
            let mut all = gpu_cards;
            all.push(host_card);
            all.extend(nas_cards);
            self.scrolled(&mut pen, body, &all, &grid);
        }
    }

    /// One column that may be taller than the screen: drawn whole off
    /// screen, then the rows from `scroll` down copied in.
    fn scrolled(&mut self, pen: &mut Pen, area: Rect, cards: &[Card], grid: &Grid) {
        let w = area.width;
        let mut height: u16 = cards.iter().map(|c| c.height(w)).sum::<u16>() + grid.height(w, false);
        // Charts get a readable size even when that means scrolling.
        height += cards.iter().map(|c| c.flex() * 4).sum::<u16>();
        let height = height.max(area.height);
        self.scroll = self.scroll.min(height - area.height);
        let full = Rect { x: area.x, y: 0, width: w, height };
        let mut off = Buffer::empty(full);
        let mut off_pen = Pen { buf: &mut off, look: pen.look };
        off_pen.fill_bg(full, pen.look.p.plane);
        column(&mut off_pen, full, cards, Some(grid));
        for row in 0..area.height {
            for x in area.left()..area.right() {
                if let (Some(src), Some(dst)) = (off.cell((x, row + self.scroll)), pen.buf.cell_mut((x, area.y + row))) {
                    *dst = src.clone();
                }
            }
        }
    }
}

struct Grid<'a> {
    slots: &'a [&'a Slot<'a>],
    status: &'a Status,
    look: &'a Look,
    /// Cards across, or None for as many as fit at full size.
    across: Option<u16>,
}

impl Grid<'_> {
    fn columns(&self, width: u16) -> u16 {
        let fit = self.across.unwrap_or((width + 1) / (GUEST_WIDTH + 1));
        fit.clamp(1, self.slots.len().max(1) as u16)
    }

    fn card_width(&self, width: u16) -> u16 {
        let cols = self.columns(width);
        (width + 1) / cols - 1
    }

    /// A card too narrow for the full five lines is always compact.
    fn compact(&self, width: u16, short: bool) -> bool {
        short || self.card_width(width) < GUEST_WIDTH
    }

    fn height(&self, width: u16, short: bool) -> u16 {
        if self.slots.is_empty() {
            return 0;
        }
        let rows = self.slots.len().div_ceil(self.columns(width) as usize) as u16;
        rows * self.card_height(width, short)
    }

    fn card_height(&self, width: u16, short: bool) -> u16 {
        let compact = self.compact(width, short);
        self.slots
            .iter()
            .map(|s| cards::basic(self.look, s, self.status, compact).height(self.card_width(width)))
            .max()
            .unwrap_or(0)
    }

    fn render(&self, pen: &mut Pen, area: Rect, short: bool) {
        if self.slots.is_empty() || area.height == 0 {
            return;
        }
        let compact = self.compact(area.width, short);
        let cols = self.columns(area.width) as usize;
        let card_w = self.card_width(area.width);
        let card_h = self.card_height(area.width, short).max(1);
        let fit = (area.height / card_h) as usize * cols;
        // Short of room, the last place says how many did not fit.
        let shown = if self.slots.len() > fit { fit.saturating_sub(1) } else { self.slots.len() };
        for (i, slot) in self.slots.iter().take(shown).enumerate() {
            let (row, col) = ((i / cols) as u16, (i % cols) as u16);
            let rect = Rect { x: area.x + col * (card_w + 1), y: area.y + row * card_h, width: card_w, height: card_h };
            cards::basic(pen.look, slot, self.status, compact).render(pen, rect);
        }
        if shown < self.slots.len() && fit > 0 {
            let (row, col) = ((shown / cols) as u16, (shown % cols) as u16);
            let text = format!("+{} more guests", self.slots.len() - shown);
            let style = Style::default().fg(pen.look.p.muted);
            pen.text(area, area.x + col * (card_w + 1) + 2, area.y + row * card_h + card_h / 2, &text, style);
        }
    }
}

/// Rows for each card in a column. Cards with charts get two thirds of the
/// spare rows, as far as their charts can grow, and cards without charts the
/// rest, to space out their sections; failing those, the charted cards that
/// can space out theirs take it.
fn card_rows(cards: &[Card], width: u16, total: u16) -> Vec<u16> {
    let heights: Vec<u16> = cards.iter().map(|c| c.height(width)).collect();
    let flexes: Vec<u16> = cards.iter().map(Card::flex).collect();
    let need: u16 = heights.iter().sum();
    if need >= total {
        return share(&heights, &flexes, total);
    }
    let mut out = heights.clone();
    let mut spare = total - need;
    let open: Vec<usize> = (0..cards.len()).filter(|&i| flexes[i] == 0 && cards[i].max_extra().is_none()).collect();
    let charted: Vec<usize> = (0..cards.len()).filter(|&i| flexes[i] > 0).collect();
    let for_charts = if open.is_empty() { spare } else { (spare * 2).div_ceil(3) };
    let weight = charted.iter().map(|&i| flexes[i]).sum::<u16>().max(1);
    for &i in &charted {
        let cap = cards[i].max_extra().unwrap_or(u16::MAX);
        let give = (for_charts * flexes[i] / weight).min(cap).min(spare);
        out[i] += give;
        spare -= give;
    }
    let takers = if open.is_empty() {
        charted.into_iter().filter(|&i| cards[i].max_extra().is_none()).collect()
    } else {
        open
    };
    draw::spread(&mut out, &takers, spare);
    out
}

/// Cards stacked top to bottom with a blank row between each, then the
/// guest grid. Spare rows go to the cards; short of rows, guests go
/// compact, then some are left out.
fn column(pen: &mut Pen, area: Rect, cards: &[Card], grid: Option<&Grid>) {
    let w = area.width;
    let dividers = (cards.len() + usize::from(grid.is_some())).saturating_sub(1) as u16;
    let fixed: u16 = cards.iter().map(|c| c.height(w)).sum::<u16>() + dividers;
    let (short, grid_h) = match grid {
        None => (false, 0),
        Some(g) => {
            let full = g.height(w, false);
            let small = g.height(w, true);
            if fixed + full <= area.height {
                (false, full)
            } else if fixed + small <= area.height {
                (true, small)
            } else {
                (true, area.height.saturating_sub(fixed))
            }
        }
    };
    let mut y = area.y;
    for (card, h) in cards.iter().zip(card_rows(cards, w, area.height.saturating_sub(grid_h + dividers))) {
        card.render(pen, Rect { y, height: h, ..area });
        y += h + 1;
    }
    if let Some(g) = grid {
        let y = if cards.is_empty() { area.y } else { y };
        g.render(pen, Rect { y, height: area.bottom().saturating_sub(y), ..area }, short);
    }
}

fn header(pen: &mut Pen, area: Rect, sources: &[(&str, &Status)]) {
    let p = pen.look.p;
    let mut spans = vec![Span::styled(" PVEStats", Style::default().fg(p.ink).add_modifier(Modifier::BOLD))];
    for (name, status) in sources {
        let (color, age) = source_state(pen.look, status);
        spans.push(Span::raw("   "));
        spans.push(Span::styled("● ", Style::default().fg(color)));
        spans.push(Span::styled(format!("{name} "), Style::default().fg(p.ink2)));
        spans.push(Span::styled(age, Style::default().fg(p.muted)));
    }
    pen.line(area, area.x, area.y, &Line::from(spans));
    let right = format!("f focus · q quit   {} ", local_time());
    let x = area.right().saturating_sub(right.chars().count() as u16);
    pen.text(area, x, area.y, &right, Style::default().fg(p.muted));
}

/// Dot colour, and the age beside it. A healthy source counts down to its
/// next poll; a failing one says how long ago it last answered.
fn source_state(look: &Look, s: &Status) -> (ratatui::style::Color, String) {
    let p = look.p;
    let color = if !s.ok {
        if s.error.is_some() { p.critical } else { p.muted }
    } else if s.stale {
        p.warning
    } else {
        p.good
    };
    let age = match s.age {
        None => "never".to_string(),
        Some(age) if s.ok && !s.stale => format!("{}s", (s.interval - age).ceil().max(0.0)),
        Some(age) => format!("{}s ago", age.round()),
    };
    (color, age)
}

/// Names each source and its state rather than a bare spinner: if one is
/// slow or dead, that is the thing worth knowing while waiting.
fn standby(pen: &mut Pen, area: Rect, sources: &[(&str, &Status)]) {
    let p = pen.look.p;
    let w = 46.min(area.width);
    let h = (sources.len() as u16 + 4).min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let inner = Rect { x, y, width: w, height: h };
    pen.text(inner, x, y, "PVEStats", Style::default().fg(p.ink).add_modifier(Modifier::BOLD));
    pen.text(inner, x, y + 1, "Connecting to sources…", Style::default().fg(p.muted));
    for (i, (label, s)) in sources.iter().enumerate() {
        let (color, detail) = if s.ok {
            (p.good, "ready".to_string())
        } else if let Some(e) = &s.error {
            (p.critical, e.split(':').next().unwrap_or(e).to_string())
        } else {
            (p.muted, "waiting…".to_string())
        };
        let line = Line::from(vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(crate::format::left(label, 18), Style::default().fg(p.ink2)),
            Span::styled(detail, Style::default().fg(p.muted)),
        ]);
        pen.line(inner, x, y + 3 + i as u16, &line);
    }
}

fn local_time() -> String {
    // SAFETY: time and localtime_r only write the values they are given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

//! The screen: the cards, laid out for the width there is. Three columns on
//! a full-screen terminal (150 columns or more), two in a wide split, one
//! (scrolled) in a narrow one. The source dots are a small panel of their
//! own at the top of the first column, so the others start at the top row.

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
use cards::{Size, Slot};
use draw::{share, Card, Part, Pen};
use theme::Look;

/// Widths at which the layout gains a column.
const THREE_COLUMNS: u16 = 150;
const TWO_COLUMNS: u16 = 96;
/// Most guest cards side by side.
const GUESTS_ACROSS: u16 = 3;

/// How long the standby screen waits for every source before showing the
/// dashboard anyway, with the failures said on it.
const STANDBY_GIVE_UP: f64 = 25.0;

/// Against burn-in: the whole screen steps one cell around a small square,
/// one step every this many seconds, so no edge sits on the same pixels for
/// long. It keeps one column and one row free to move into.
const ORBIT_STEP: u64 = 240;
const ORBIT: [(u16, u16); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

pub struct Tour {
    pub index: usize,
    pub next_in: u64,
}

pub struct Screen {
    /// Only the GPU card, which is what this dashboard exists for.
    pub focus: bool,
    /// First row shown when a single column is taller than the screen.
    pub scroll: u16,
    /// Step the screen around against burn-in.
    pub orbit: bool,
    /// The theme tour, while one is on: which theme, and seconds to the next.
    pub tour: Option<Tour>,
    started: f64,
    standby_done: bool,
}

impl Screen {
    pub fn new() -> Self {
        Screen { focus: false, scroll: 0, orbit: false, tour: None, started: clock(), standby_done: false }
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, shared: &Shared, config: &Config, look: &Look) {
        let pve = lock(&shared.pve);
        let agent = lock(&shared.agent);
        let nas = lock(&shared.nas);
        let (sp, sa, sn) = (pve.status(), agent.status(), nas.status());
        let mut pen = Pen { buf, look };
        pen.fill_bg(area, look.p.plane);
        let body = if self.orbit && area.width > 1 && area.height > 1 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let (dx, dy) = ORBIT[(now / ORBIT_STEP % ORBIT.len() as u64) as usize];
            Rect { x: area.x + dx, y: area.y + dy, width: area.width - 1, height: area.height - 1 }
        } else {
            area
        };
        if body.height < 3 {
            return;
        }

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

        let names: Vec<&str> = theme::names().collect();
        let theme_now = self.tour.as_ref().map(|t| names[t.index % names.len()]);
        let host_card = match pve.data.as_ref() {
            Some(d) => cards::host(look, &d.node, &pve, &sp),
            None => cards::pve_note(look, &sp),
        };
        let top = with_status(look, &[("pve", &sp), ("agent", &sa), ("truenas", &sn)], theme_now, host_card);
        let of = |view: View| slots.values().filter(move |s| s.view == view);
        let gpu_cards: Vec<Card> = of(View::Gpu).map(|s| cards::gpu(look, s, &agent, &sa, &sp)).collect();
        let nas_cards: Vec<Card> =
            of(View::Nas).map(|s| cards::nas(look, s, config.hosts.get(&s.vmid), &nas, &sn, &sp)).collect();
        let basic: Vec<&Slot> = of(View::Basic).collect();
        let guests = Guests { slots: &basic, status: &sp, look };
        let tour = self.tour.as_ref().map(|t| cards::tour(look, &names, t.index % names.len(), t.next_in));

        if self.focus && !gpu_cards.is_empty() {
            column(&mut pen, body, &gpu_cards, None, None);
            return;
        }
        let w = body.width;
        if w >= THREE_COLUMNS {
            // Status, host, NAS and the guests on the left, the GPU in the
            // middle, and the right kept for a forecast.
            let left = (w - 2) / 3;
            let mid = (w - left - 2) / 2;
            let right = w - left - mid - 2;
            let mut left_cards = vec![top];
            left_cards.extend(nas_cards);
            column(&mut pen, Rect { width: left, ..body }, &left_cards, Some(&guests), None);
            column(&mut pen, Rect { x: body.x + left + 1, width: mid, ..body }, &gpu_cards, None, None);
            let mut right_cards = vec![cards::weather_placeholder(look)];
            right_cards.extend(tour);
            column(&mut pen, Rect { x: body.x + left + mid + 2, width: right, ..body }, &right_cards, None, None);
        } else if w >= TWO_COLUMNS {
            let left = (w - 1) / 2;
            let mut left_cards = vec![top];
            left_cards.extend(gpu_cards);
            column(&mut pen, Rect { width: left, ..body }, &left_cards, None, None);
            let right = Rect { x: body.x + left + 1, width: w - left - 1, ..body };
            column(&mut pen, right, &nas_cards, Some(&guests), tour.as_ref());
        } else {
            let mut all = vec![top];
            all.extend(gpu_cards);
            all.extend(nas_cards);
            self.scrolled(&mut pen, body, &all, &guests);
        }
    }

    /// One column that may be taller than the screen: drawn whole off
    /// screen, then the rows from `scroll` down copied in.
    fn scrolled(&mut self, pen: &mut Pen, area: Rect, cards: &[Card], guests: &Guests) {
        let w = area.width;
        let dividers = cards.len() as u16;
        let mut height: u16 = cards.iter().map(|c| c.height(w)).sum::<u16>() + dividers + guests.natural(w);
        // Charts get a readable size even when that means scrolling.
        height += cards.iter().map(|c| c.flex() * 4).sum::<u16>();
        let height = height.max(area.height);
        self.scroll = self.scroll.min(height - area.height);
        let full = Rect { x: area.x, y: 0, width: w, height };
        let mut off = Buffer::empty(full);
        let mut off_pen = Pen { buf: &mut off, look: pen.look };
        off_pen.fill_bg(full, pen.look.p.plane);
        column(&mut off_pen, full, cards, Some(guests), None);
        for row in 0..area.height {
            for x in area.left()..area.right() {
                if let (Some(src), Some(dst)) = (off.cell((x, row + self.scroll)), pen.buf.cell_mut((x, area.y + row))) {
                    *dst = src.clone();
                }
            }
        }
    }
}

/// The guests other than the GPU and NAS ones, laid out to fill the area
/// they are given: the fullest cards that fit them all, in as few columns
/// as will do, down to a list of one line each; short even of that, as
/// many lines as fit.
struct Guests<'a> {
    slots: &'a [&'a Slot<'a>],
    status: &'a Status,
    look: &'a Look,
}

enum Plan {
    Cards { size: Size, cols: u16, card_w: u16, card_h: u16 },
    List,
}

impl Guests<'_> {
    fn plan(&self, width: u16, height: u16) -> Plan {
        let n = self.slots.len();
        for (size, narrowest) in [(Size::Full, 38), (Size::Compact, 26), (Size::Small, 18), (Size::Tiny, 18)] {
            let most = ((width + 1) / (narrowest + 1)).clamp(1, GUESTS_ACROSS).min(n.max(1) as u16);
            for cols in 1..=most {
                let card_w = (width + 1) / cols - 1;
                if card_w < narrowest {
                    continue;
                }
                let card_h = self
                    .slots
                    .iter()
                    .map(|s| cards::basic(self.look, s, self.status, size).height(card_w))
                    .max()
                    .unwrap_or(0);
                if n.div_ceil(cols as usize) as u16 * card_h <= height {
                    return Plan::Cards { size, cols, card_w, card_h };
                }
            }
        }
        Plan::List
    }

    /// Rows it takes in `height` rows, or None with no guests to show.
    fn used(&self, width: u16, height: u16) -> Option<u16> {
        if self.slots.is_empty() {
            return None;
        }
        Some(match self.plan(width, height) {
            Plan::Cards { cols, card_h, .. } => self.slots.len().div_ceil(cols as usize) as u16 * card_h,
            Plan::List => (self.slots.len() as u16 + 3).min(height),
        })
    }

    /// Rows it takes with no limit on them: full cards.
    fn natural(&self, width: u16) -> u16 {
        if self.slots.is_empty() {
            return 0;
        }
        match self.plan(width, u16::MAX) {
            Plan::Cards { cols, card_h, .. } => self.slots.len().div_ceil(cols as usize) as u16 * card_h,
            Plan::List => self.slots.len() as u16 + 3,
        }
    }

    fn render(&self, pen: &mut Pen, area: Rect) {
        if self.slots.is_empty() || area.height < 3 {
            return;
        }
        match self.plan(area.width, area.height) {
            Plan::Cards { size, cols, card_w, card_h } => {
                for (i, slot) in self.slots.iter().enumerate() {
                    let (row, col) = (i as u16 / cols, i as u16 % cols);
                    let rect = Rect { x: area.x + col * (card_w + 1), y: area.y + row * card_h, width: card_w, height: card_h };
                    cards::basic(pen.look, slot, self.status, size).render(pen, rect);
                }
            }
            Plan::List => {
                // Border and header take three rows; the rest hold guests.
                let fit = (area.height - 3) as usize;
                let shown = &self.slots[..self.slots.len().min(fit)];
                let mut card = cards::guest_list(pen.look, shown, self.status);
                if shown.len() < self.slots.len() {
                    card.right = format!("{} of {}", shown.len(), self.slots.len());
                }
                card.render(pen, Rect { height: (shown.len() as u16 + 3).min(area.height), ..area });
            }
        }
    }
}

/// Rows for each card in a column. Spare rows go only to cards with charts,
/// as far as each can use them; cards without keep to their own height, so
/// what is left stays free at the foot of the column.
fn card_rows(cards: &[Card], width: u16, total: u16) -> Vec<u16> {
    let heights: Vec<u16> = cards.iter().map(|c| c.height(width)).collect();
    let flexes: Vec<u16> = cards.iter().map(Card::flex).collect();
    let need: u16 = heights.iter().sum();
    if need >= total {
        return share(&heights, &flexes, total);
    }
    let mut out = heights;
    let spare = total - need;
    let weight = flexes.iter().sum::<u16>().max(1);
    for (i, card) in cards.iter().enumerate() {
        let cap = card.max_extra().unwrap_or(u16::MAX);
        out[i] += (spare * flexes[i] / weight).min(cap);
    }
    out
}

/// Cards stacked top to bottom with a blank row between each, then the
/// guests in the rows left, then `tail` under them if it fits.
fn column(pen: &mut Pen, area: Rect, cards: &[Card], guests: Option<&Guests>, tail: Option<&Card>) {
    let w = area.width;
    let dividers = cards.len().saturating_sub(1) as u16;
    // The guests come first: they take what they need of the rows the
    // cards leave at their smallest, and charts grow only into the rest.
    let smallest: u16 = cards.iter().map(|c| c.height(w)).sum::<u16>() + dividers;
    let for_guests = match guests {
        Some(g) if !cards.is_empty() => {
            let room = area.height.saturating_sub(smallest + 1);
            g.used(w, room).map_or(0, |rows| rows + 1)
        }
        _ => 0,
    };
    let rows = card_rows(cards, w, area.height.saturating_sub(dividers + for_guests));
    let mut y = area.y;
    for (card, h) in cards.iter().zip(rows) {
        card.render(pen, Rect { y, height: h, ..area });
        y += h + 1;
    }
    let Some(g) = guests else { return };
    let top = if cards.is_empty() { area.y } else { y };
    let room = Rect { y: top, height: area.bottom().saturating_sub(top), ..area };
    g.render(pen, room);
    if let Some(card) = tail {
        let below = top + g.natural(w).min(room.height) + 1;
        if area.bottom() >= below + card.height(w) {
            card.render(pen, Rect { y: below, height: card.height(w), ..area });
        }
    }
}

/// The dashboard's name and its sources, each a dot and an age, on top of
/// the host card: one panel rather than two leaves the cards below three
/// more rows. The host's readings dim on their own when PVE is not
/// answering, with the reason on the border; the source dots never do.
/// During a theme tour the corner names the theme.
fn with_status(look: &Look, sources: &[(&str, &Status)], theme_now: Option<&str>, host: Card) -> Card {
    let p = look.p;
    let mut spans = Vec::new();
    for (i, (name, status)) in sources.iter().enumerate() {
        let (color, age) = source_state(look, status);
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled("● ", Style::default().fg(color)));
        spans.push(Span::styled(format!("{name} "), Style::default().fg(p.ink2)));
        spans.push(Span::styled(age, Style::default().fg(p.muted)));
    }
    let clock = match theme_now {
        Some(theme) => format!("theme {theme} · {}", local_time()),
        None => local_time(),
    };
    let readings = Part::Stack(host.body);
    Card {
        title: "PVEStats".into(),
        right: if host.right.is_empty() { clock } else { format!("{} · {clock}", host.right) },
        stale: false,
        error: host.error,
        body: vec![
            Part::Text(Line::from(spans)),
            if host.stale { Part::Dim(Box::new(readings)) } else { readings },
        ],
    }
}

/// Dot colour, and the age beside it. A healthy source counts down to its
/// next poll; a failing one says how long ago it last answered. The number
/// keeps two columns, so "9s" and "10s" sit in the same place.
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
        Some(age) if s.ok && !s.stale => format!("{:>2}s", (s.interval - age).ceil().max(0.0)),
        Some(age) => format!("{:>2}s ago", age.round()),
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

/// Local hour and minute.
pub fn local_hm() -> (u32, u32) {
    // SAFETY: time and localtime_r only write the values they are given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
    }
    (tm.tm_hour as u32, tm.tm_min as u32)
}

fn local_time() -> String {
    let (h, m) = local_hm();
    format!("{h:02}:{m:02}")
}

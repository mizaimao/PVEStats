//! The screen: the cards, laid out for the width there is. Three columns on
//! a full-screen terminal (150 columns or more), two in a wide split, one
//! (scrolled) in a narrow one. The source dots are a small panel of their
//! own at the top of the first column, so the others start at the top row.

pub mod cards;
pub mod draw;
pub mod theme;
pub mod weather;

use std::collections::BTreeMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::{Config, View};
use crate::sources::{lock, HostData, PveData, Shared};
use crate::state::{clock, History, Status};
use cards::Slot;
use draw::{share, Card, Part, Pen};
use theme::Look;

/// Widths at which the layout gains a column.
const THREE_COLUMNS: u16 = 150;
const TWO_COLUMNS: u16 = 96;

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
    /// Columns at the left of the top row left bare for a window's buttons,
    /// when the window's title bar is see-through (--titlebar).
    pub corner: u16,
    started: f64,
    standby_done: bool,
}

impl Screen {
    pub fn new() -> Self {
        Screen { focus: false, scroll: 0, orbit: false, tour: None, corner: 0, started: clock(), standby_done: false }
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, shared: &Shared, config: &Config, look: &Look) {
        self.draw_all(buf, area, shared, config, look);
        // Bare cells under the window's buttons, in the background the title
        // bar above them takes too.
        let corner = area.x..area.x.saturating_add(self.corner).min(area.right());
        for x in corner {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.reset();
                cell.set_bg(look.p.plane.unwrap_or(Color::Reset));
            }
        }
    }

    fn draw_all(&mut self, buf: &mut Buffer, area: Rect, shared: &Shared, config: &Config, look: &Look) {
        let pve = lock(&shared.pve);
        let host_agent = lock(&shared.host);
        let agent = lock(&shared.agent);
        let nas = lock(&shared.nas);
        let router = lock(&shared.router);
        let forecast = lock(&shared.weather);
        let (sp, sa, sn, sr) = (pve.status(), agent.status(), nas.status(), router.status());
        let sh = host_agent.status();
        // The host agent's live figures over PVE's while it answers, and its
        // 2 s history for the host's and guests' charts; PVE's otherwise.
        let live = config.host_configured() && sh.ok && !sh.stale;
        let data: Option<PveData> = pve.data.as_ref().map(|d| if live { overlay(d, host_agent.data.as_ref()) } else { d.clone() });
        let history: &dyn History = if live { &*host_agent } else { &*pve };
        let mut pen = Pen { buf, look, corner: self.corner };
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
        if let Some(data) = data.as_ref() {
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
        let host_card = match data.as_ref() {
            Some(d) => {
                // The host's figures are the agent's while it answers. After it
                // stops, PVE's are greyed while they are older than two of its
                // polls: the status it reused can be ten minutes old.
                let old = !live && d.node.status_at.is_some_and(|t| clock() - t > pve.interval * 2.0);
                let status = match (live, old) {
                    (true, _) => sh.clone(),
                    (false, true) => Status { ok: false, error: None, ..sp.clone() },
                    (false, false) => sp.clone(),
                };
                cards::host(look, &d.node, history, &status)
            }
            None => cards::pve_note(look, &sp),
        };
        // One dot per machine. Proxmox's covers both its feeds: its agent's
        // live figures and its API's names and limits.
        let proxmox = if config.host_configured() { pair_dot(look, "proxmox", &sh, &sp) } else { dot(look, "proxmox", &sp) };
        let mut dots = vec![proxmox, dot(look, "ml60", &sa), dot(look, "truenas", &sn)];
        if config.router_shown() {
            dots.push(dot(look, "router", &sr));
        }
        let top = with_status(look, &dots, theme_now, host_card);
        let of = |view: View| slots.values().filter(move |s| s.view == view);
        let gpu_cards: Vec<Card> = of(View::Gpu).map(|s| cards::gpu(look, s, &agent, &sa, &pve, &sp)).collect();
        // The router rides under the NAS in every layout.
        let mut nas_cards: Vec<Card> =
            of(View::Nas).map(|s| cards::nas(look, s, config.hosts.get(&s.vmid), &nas, &sn)).collect();
        nas_cards.extend(config.router_shown().then(|| cards::router(look, &router, &sr)));
        let basic: Vec<&Slot> = of(View::Basic).collect();
        // Guest cards grey with the source their live figures come from.
        let guests = Guests {
            slots: &basic,
            status: if live { &sh } else { &sp },
            pve: &*pve,
            host: live.then_some(()).and(host_agent.data.as_ref()).map(|d| (&*host_agent as &dyn History, d)),
        };
        let tour = self.tour.as_ref().map(|t| cards::tour(look, &names, t.index % names.len(), t.next_in));

        if self.focus && !gpu_cards.is_empty() {
            column(&mut pen, body, &gpu_cards, None, true);
            return;
        }
        let w = body.width;
        if w >= THREE_COLUMNS {
            // Status, host, NAS and router on the left, the GPU in the
            // middle, the forecast and the guests on the right. The left
            // keeps its cards to their smallest, but for the router's chart,
            // which takes any rows left at the foot. The NAS sits right
            // under the status card, with no blank row: one more row for
            // the router's chart, which then splits evenly.
            let left = (w - 2) / 3;
            let mid = (w - left - 2) / 2;
            let right = w - left - mid - 2;
            let top_h = top.height(left).min(body.height);
            top.render(&mut pen, Rect { width: left, height: top_h, ..body });
            let below = Rect { y: body.y + top_h, width: left, height: body.height - top_h, ..body };
            column(&mut pen, below, &nas_cards, None, false);
            column(&mut pen, Rect { x: body.x + left + 1, width: mid, ..body }, &gpu_cards, None, true);
            let forecast_card = weather::card(look, &forecast, &forecast.status(), config.weather_configured());
            // The guests keep a set height at the foot of the column, short
            // of squeezing the forecast; the forecast takes every row above
            // them, for more days.
            let area = Rect { x: body.x + left + mid + 2, width: right, ..body };
            let guests_h = guests.set_height(right).min(area.height.saturating_sub(forecast_card.height(right) + 1));
            let above = Rect { height: area.height - guests_h - u16::from(guests_h > 0), ..area };
            match tour {
                Some(tour) => column(&mut pen, above, &[forecast_card, tour], None, true),
                None => forecast_card.render(&mut pen, above),
            }
            guests.render(&mut pen, Rect { y: area.bottom() - guests_h, height: guests_h, ..area });
            // In the gap above the guests, once, since their cards have no
            // room for it: PVE counts a VM's disk cache as used memory.
            if guests_h > 0 && !basic.is_empty() {
                let note = "VM memory as Proxmox counts it: disk cache included";
                pen.text(area, area.x + 1, area.bottom() - guests_h - 1, note, Style::default().fg(look.p.muted));
            }
        } else if w >= TWO_COLUMNS {
            let left = (w - 1) / 2;
            let mut left_cards = vec![top];
            left_cards.extend(gpu_cards);
            column(&mut pen, Rect { width: left, ..body }, &left_cards, None, true);
            let right = Rect { x: body.x + left + 1, width: w - left - 1, ..body };
            column(&mut pen, right, &nas_cards, Some(&guests), true);
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
        let mut off_pen = Pen { buf: &mut off, look: pen.look, corner: pen.corner };
        off_pen.fill_bg(full, pen.look.p.plane);
        column(&mut off_pen, full, cards, Some(guests), true);
        for row in 0..area.height {
            for x in area.left()..area.right() {
                if let (Some(src), Some(dst)) = (off.cell((x, row + self.scroll)), pen.buf.cell_mut((x, area.y + row))) {
                    *dst = src.clone();
                }
            }
        }
    }
}

/// The guests other than the GPU and NAS ones, filling the whole area they
/// are given, however many there are. They split into rows as evenly as
/// they go; each row's cards share its width, and the rows share the
/// height, so nothing is left blank. A row with fewer cards has wider ones,
/// and those go to the guests with the most memory. Each card shows as many
/// lines as its box holds.
struct Guests<'a> {
    slots: &'a [&'a Slot<'a>],
    status: &'a Status,
    /// PVE's history, for each guest's CPU.
    pve: &'a dyn History,
    /// The host agent's history and figures while it answers: its CPU
    /// history for the guests it reports (VMs), PVE's for any other
    /// (containers, which it does not read).
    host: Option<(&'a dyn History, &'a HostData)>,
}

/// Narrowest a guest card may be: room for "CPU ■■■ 12%".
const GUEST_NARROWEST: u16 = 18;

/// A guest card's height where the guests keep a set height: its borders,
/// six lines and a three-row CPU chart.
const GUEST_ROWS: u16 = 11;

impl Guests<'_> {
    /// Rows they take at the set card height: as few rows of cards as
    /// leave each card wide enough.
    fn set_height(&self, width: u16) -> u16 {
        let n = self.slots.len();
        let rows = (1..=n).find(|&r| (width + 1) / self.counts(r)[0] as u16 > GUEST_NARROWEST).unwrap_or(n);
        rows as u16 * GUEST_ROWS
    }

    /// Cards per row, fullest rows first, for `rows` rows.
    fn counts(&self, rows: usize) -> Vec<usize> {
        let n = self.slots.len();
        (0..rows).map(|r| n / rows + usize::from(r < n % rows)).collect()
    }

    /// The number of rows that shows the most on every card: at least CPU
    /// and memory, and on a tie, fewer rows, for wider cards. None when not
    /// even that fits.
    fn rows(&self, width: u16, height: u16) -> Option<usize> {
        let n = self.slots.len();
        let mut best: Option<(usize, u16)> = None;
        for rows in 1..=n {
            let across = self.counts(rows)[0] as u16;
            let card_w = (width + 1) / across - 1;
            let card_h = height / rows as u16;
            if card_w < GUEST_NARROWEST || card_h < 4 {
                continue;
            }
            let lines = self
                .slots
                .iter()
                .map(|s| cards::guest_lines(card_w, s.guest).min(card_h - 2))
                .min()
                .unwrap_or(0);
            if best.map_or(true, |(_, most)| lines > most) {
                best = Some((rows, lines));
            }
        }
        best.map(|(rows, _)| rows)
    }

    /// Rows of guests: the widest cards (rows with fewest of them) to the
    /// guests with the most memory; rows in order of their lowest VMID, and
    /// guests in VMID order within a row.
    fn arrange(&self, rows: usize) -> Vec<Vec<&Slot<'_>>> {
        let mut counts = self.counts(rows);
        counts.sort_unstable();
        let mut by_size: Vec<&Slot> = self.slots.to_vec();
        let memory = |s: &Slot| s.guest.and_then(|g| g.maxmem).unwrap_or(0.0);
        by_size.sort_by(|a, b| memory(b).total_cmp(&memory(a)).then(a.vmid.cmp(&b.vmid)));
        let mut taken = by_size.into_iter();
        let mut out: Vec<Vec<&Slot>> = counts
            .iter()
            .map(|&count| {
                let mut row: Vec<&Slot> = taken.by_ref().take(count).collect();
                row.sort_by_key(|s| s.vmid);
                row
            })
            .collect();
        out.sort_by_key(|row| row.first().map(|s| s.vmid));
        out
    }

    /// Rows it takes with no limit on them, for a scrolled column.
    fn natural(&self, width: u16) -> u16 {
        if self.slots.is_empty() {
            return 0;
        }
        let across = ((width + 1) / 31).clamp(1, self.slots.len() as u16) as usize;
        self.slots.len().div_ceil(across) as u16 * 8
    }

    fn render(&self, pen: &mut Pen, area: Rect) {
        if self.slots.is_empty() || area.height < 3 {
            return;
        }
        let Some(rows) = self.rows(area.width, area.height) else {
            // Not even CPU and memory fit on a card: one line each.
            let fit = (area.height - 3) as usize;
            let shown = &self.slots[..self.slots.len().min(fit)];
            let mut card = cards::guest_list(pen.look, shown, self.status);
            if shown.len() < self.slots.len() {
                card.right = format!("{} of {}", shown.len(), self.slots.len());
            }
            card.render(pen, Rect { height: (shown.len() as u16 + 3).min(area.height), ..area });
            return;
        };
        let mut y = area.y;
        for (r, row) in self.arrange(rows).iter().enumerate() {
            // Rows share the height, the first ones taking any remainder.
            let h = area.height / rows as u16 + u16::from((r as u16) < area.height % rows as u16);
            let k = row.len() as u16;
            let spare = area.width + 1 - k * ((area.width + 1) / k);
            let width = |i: usize| (area.width + 1) / k - 1 + u16::from((i as u16) < spare);
            // One chart height for the whole row, so its cards' lines stay
            // level: the rows the fullest card leaves over.
            let chart = row
                .iter()
                .enumerate()
                .map(|(i, s)| h.saturating_sub(2 + cards::guest_lines(width(i), s.guest)))
                .min()
                .unwrap_or(0);
            let mut x = area.x;
            for (i, slot) in row.iter().enumerate() {
                let w = width(i);
                let history = match self.host {
                    Some((history, data)) if data.guests.contains_key(&slot.vmid) => history,
                    _ => self.pve,
                };
                cards::guest(pen.look, slot, self.status, history, chart, w, h)
                    .render(pen, Rect { x, y, width: w, height: h });
                x += w + 1;
            }
            y += h;
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
/// guests filling every row left. With guests below them, or without
/// `grow`, the cards keep to their smallest (without `grow`, the last
/// card's charts still take rows left at the foot); otherwise charts grow
/// into spare rows.
fn column(pen: &mut Pen, area: Rect, cards: &[Card], guests: Option<&Guests>, grow: bool) {
    let w = area.width;
    let dividers = cards.len().saturating_sub(1) as u16;
    let mut rows: Vec<u16> = if !grow || guests.is_some_and(|g| !g.slots.is_empty()) {
        cards.iter().map(|c| c.height(w)).collect()
    } else {
        card_rows(cards, w, area.height.saturating_sub(dividers))
    };
    if !grow && guests.is_none() {
        // Rows left at the foot go to the last card's charts, as far as
        // they grow, rather than stay blank.
        let left = area.height.saturating_sub(dividers + rows.iter().sum::<u16>());
        if let (Some(card), Some(h)) = (cards.last(), rows.last_mut()) {
            *h += left.min(card.room());
        }
    }
    let mut y = area.y;
    for (card, h) in cards.iter().zip(rows) {
        card.render(pen, Rect { y, height: h.min(area.bottom().saturating_sub(y)), ..area });
        y += h + 1;
    }
    if let Some(g) = guests {
        // After the same blank row as between cards.
        let top = if cards.is_empty() { area.y } else { y };
        g.render(pen, Rect { y: top, height: area.bottom().saturating_sub(top), ..area });
    }
}

/// One machine on the status line, and how its feed is doing.
struct Dot {
    name: &'static str,
    color: Color,
    /// Said only when something is wrong: how long since it answered.
    note: Option<String>,
}

/// Answering on schedule.
fn fine(s: &Status) -> bool {
    s.ok && !s.stale
}

/// Not answered yet, and no error yet either: the grey dot says it.
fn waiting(s: &Status) -> bool {
    !s.ok && s.error.is_none() && s.age.is_none()
}

/// Green answering, amber late, red failing, grey before a first answer.
fn color_of(look: &Look, s: &Status) -> Color {
    let p = look.p;
    if !s.ok {
        if s.error.is_some() { p.critical } else { p.muted }
    } else if s.stale {
        p.warning
    } else {
        p.good
    }
}

/// How long since a source last answered.
fn since(s: &Status) -> String {
    match s.age {
        None => "never".into(),
        Some(age) if age < 120.0 => format!("{}s ago", age.round()),
        Some(age) => format!("{} ago", crate::format::uptime(Some(age))),
    }
}

fn dot(look: &Look, name: &'static str, s: &Status) -> Dot {
    Dot { name, color: color_of(look, s), note: (!fine(s) && !waiting(s)).then(|| since(s)) }
}

/// A machine with two feeds, its agent and its API: green while neither is
/// down, amber naming the one that is while the other answers, and as the
/// agent alone when nothing answers.
fn pair_dot(look: &Look, name: &'static str, agent: &Status, api: &Status) -> Dot {
    let down = |s: &Status| !fine(s) && !waiting(s);
    let p = look.p;
    match (down(agent), down(api)) {
        (false, false) => {
            Dot { name, color: if fine(agent) || fine(api) { p.good } else { p.muted }, note: None }
        }
        (false, true) if fine(agent) => Dot { name, color: p.warning, note: Some(format!("api {}", since(api))) },
        (true, false) if fine(api) => Dot { name, color: p.warning, note: Some(format!("agent {}", since(agent))) },
        (_, true) if !down(agent) => dot(look, name, api),
        _ => dot(look, name, agent),
    }
}

/// The dashboard's name and a dot for each machine, on top of the host card:
/// one panel rather than two leaves the cards below three more rows. The
/// host's readings dim on their own when PVE is not answering, with the
/// reason on the border; the dots never do. During a theme tour the corner
/// names the theme.
fn with_status(look: &Look, dots: &[Dot], theme_now: Option<&str>, host: Card) -> Card {
    let p = look.p;
    let mut spans = Vec::new();
    // A dot and a name while all is well; how long since it answered only
    // when it is late or failing.
    for (i, d) in dots.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("● ", Style::default().fg(d.color)));
        spans.push(Span::styled(d.name, Style::default().fg(p.ink2)));
        if let Some(note) = &d.note {
            spans.push(Span::styled(format!(" {note}"), Style::default().fg(p.muted)));
        }
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

/// PVE's data with the host agent's live figures in its place where the
/// agent has them: the host's CPU, memory, load, uptime and threads, and
/// each guest's CPU, network and uptime. Names, limits, guest memory,
/// addresses and disk usage stay PVE's.
fn overlay(pve: &PveData, host: Option<&HostData>) -> PveData {
    let mut d = pve.clone();
    let Some(h) = host else { return d };
    d.node.cpu_pct = h.cpu_pct.or(d.node.cpu_pct);
    if h.maxmem.is_some() {
        (d.node.mem, d.node.maxmem) = (h.mem, h.maxmem);
    }
    if !h.loadavg.is_empty() {
        d.node.loadavg = h.loadavg.clone();
    }
    d.node.uptime = h.uptime.or(d.node.uptime);
    d.node.cpus = h.cpus.or(d.node.cpus);
    for (vmid, g) in d.guests.iter_mut() {
        let Some(hg) = h.guests.get(vmid) else { continue };
        g.cpu_pct = hg.cpu_pct.or(g.cpu_pct);
        g.net_in_bps = hg.net_in_bps.or(g.net_in_bps);
        g.net_out_bps = hg.net_out_bps.or(g.net_out_bps);
        g.uptime = hg.uptime.or(g.uptime);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::Guest;

    fn guest(memory_gib: f64) -> Guest {
        Guest {
            name: None,
            uptime: None,
            cpu_pct: None,
            maxcpu: None,
            mem: None,
            maxmem: Some(memory_gib * 1024f64.powi(3)),
            addresses: Vec::new(),
            filesystem: None,
            disk_io_visible: true,
            net_in_bps: None,
            net_out_bps: None,
            disk_read_bps: None,
            disk_write_bps: None,
        }
    }

    #[test]
    fn the_biggest_guests_get_the_wider_row() {
        let guests = [(102, 1.0), (103, 4.0), (107, 16.0), (108, 12.0), (211, 4.0)].map(|(v, m)| (v, guest(m)));
        let slots: Vec<Slot> = guests
            .iter()
            .map(|(vmid, g)| Slot { vmid: *vmid, name: vmid.to_string(), view: View::Basic, guest: Some(g) })
            .collect();
        let refs: Vec<&Slot> = slots.iter().collect();
        let status = Status { ok: true, error: None, age: None, stale: false };
        let pve: crate::state::Source<PveData> = crate::state::Source::new(10.0, 600.0);
        let g = Guests { slots: &refs, status: &status, pve: &pve, host: None };
        // 57 x 12: two rows of six, three cards and then two.
        assert_eq!(g.rows(57, 12), Some(2));
        let rows: Vec<Vec<u64>> = g.arrange(2).iter().map(|r| r.iter().map(|s| s.vmid).collect()).collect();
        assert_eq!(rows, vec![vec![102, 103, 211], vec![107, 108]]);
        // Too little room for CPU and memory on a card: the one-line list.
        assert_eq!(g.rows(57, 3), None);
    }

    #[test]
    fn proxmox_one_dot_for_its_agent_and_its_api() {
        let look = Look::new("ice", theme::Colors::Rgb, false);
        let p = look.p;
        let up = Status { ok: true, error: None, age: Some(1.0), stale: false };
        let down = Status { ok: false, error: Some("timed out".into()), age: Some(42.0), stale: true };
        let starting = Status { ok: false, error: None, age: None, stale: false };
        let seen = |d: Dot| (d.color, d.note);
        assert_eq!(seen(pair_dot(&look, "proxmox", &up, &up)), (p.good, None));
        // The API still on its first call: nothing wrong yet.
        assert_eq!(seen(pair_dot(&look, "proxmox", &up, &starting)), (p.good, None));
        assert_eq!(seen(pair_dot(&look, "proxmox", &up, &down)), (p.warning, Some("api 42s ago".into())));
        assert_eq!(seen(pair_dot(&look, "proxmox", &down, &up)), (p.warning, Some("agent 42s ago".into())));
        assert_eq!(seen(pair_dot(&look, "proxmox", &down, &down)), (p.critical, Some("42s ago".into())));
        assert_eq!(seen(pair_dot(&look, "proxmox", &starting, &down)), (p.critical, Some("42s ago".into())));
        assert_eq!(seen(pair_dot(&look, "proxmox", &starting, &starting)), (p.muted, None));
        // A single feed: a dot and its name while fine, no countdown.
        assert_eq!(seen(dot(&look, "ml60", &up)), (p.good, None));
        let long = Status { age: Some(600.0), ..down };
        assert_eq!(dot(&look, "router", &long).note.as_deref(), Some("10m ago"));
    }
}

//! The forecast card: the weather now in big figures beside an ASCII icon,
//! then the coming hours and the week, in one of three layouts:
//!
//!   A  an hourly strip, a chart of the coming days, the week as a table
//!   B  the hours a line each, as many as fit, then the week as a table
//!   C  an hourly strip, a three-hourly strip after it, the week as tiles

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::draw::{plot, Big, Card, Col, Part, Pen, Table, Trace};
use super::theme::Look;
use crate::format as fmt;
use crate::sources::agent::days_from_civil;
use crate::sources::{Day, Hour, WeatherData};
use crate::state::{Source, Status};

fn style(color: Color) -> Style {
    Style::default().fg(color)
}

fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// A WMO weather code as a word or two, a symbol and its colour.
fn sky(look: &Look, code: Option<u64>, day: bool) -> (&'static str, &'static str, Color) {
    let p = look.p;
    match code {
        Some(0) => ("Clear", if day { "☀" } else { "☾" }, if day { p.warning } else { p.ink2 }),
        Some(1) => ("Mainly clear", if day { "☀" } else { "☾" }, if day { p.warning } else { p.ink2 }),
        Some(2) => ("Partly cloudy", "☁", p.ink2),
        Some(3) => ("Cloudy", "☁", p.muted),
        Some(45 | 48) => ("Fog", "≡", p.muted),
        Some(51..=55) => ("Drizzle", "☂", p.s1),
        Some(56 | 57 | 66 | 67) => ("Freezing rain", "☂", p.s1),
        Some(61) => ("Light rain", "☂", p.s1),
        Some(63) => ("Rain", "☂", p.s1),
        Some(65) => ("Heavy rain", "☂", p.s1),
        Some(71) => ("Light snow", "❄", p.ink),
        Some(73) => ("Snow", "❄", p.ink),
        Some(75) => ("Heavy snow", "❄", p.ink),
        Some(77) => ("Snow grains", "❄", p.ink),
        Some(80..=82) => ("Showers", "☂", p.s1),
        Some(85 | 86) => ("Snow showers", "❄", p.ink),
        Some(95..=99) => ("Thunderstorm", "ϟ", p.warning),
        _ => ("–", "·", p.muted),
    }
}

/// An icon in ASCII, seven columns by three rows, for a WMO code: each row
/// with a mask beside it naming each character's colour (y sun, m moon,
/// c cloud, b rain, w snow, l lightning, f fog).
fn icon(code: Option<u64>, day: bool) -> [(&'static str, &'static str); 3] {
    const SUN: [(&str, &str); 3] = [(" \\ | / ", " y y y "), ("-- O --", "yy y yy"), (" / | \\ ", " y y y ")];
    const MOON: [(&str, &str); 3] = [("  .-.  ", "  mmm  "), (" (  (  ", " m  m  "), ("  `-'  ", "  mmm  ")];
    const SUN_CLOUD: [(&str, &str); 3] = [("\\ / .-.", "y y ccc"), ("-O-(  )", "yyycccc"), (" (__(_)", " cccccc")];
    const MOON_CLOUD: [(&str, &str); 3] = [("  _ .-.", "  m ccc"), (" ( (  )", " m cccc"), (" `-(__)", " mmcccc")];
    const CLOUD: [(&str, &str); 3] = [("  .-.  ", "  ccc  "), (" (   ) ", " ccccc "), ("(__(__)", "ccccccc")];
    const RAIN: [(&str, &str); 3] = [("  .-.  ", "  ccc  "), ("(__(__)", "ccccccc"), (" ' ' ' ", " b b b ")];
    const SNOW: [(&str, &str); 3] = [("  .-.  ", "  ccc  "), ("(__(__)", "ccccccc"), (" * * * ", " w w w ")];
    const STORM: [(&str, &str); 3] = [("  .-.  ", "  ccc  "), ("(__(__)", "ccccccc"), (" ' / ' ", " b l b ")];
    const FOG: [(&str, &str); 3] = [("_ - _ -", "fffffff"), (" _ - _ ", " fffff "), ("_ - _ -", "fffffff")];
    const NONE: [(&str, &str); 3] = [("       ", "       "); 3];
    match code {
        Some(0 | 1) => if day { SUN } else { MOON },
        Some(2) => if day { SUN_CLOUD } else { MOON_CLOUD },
        Some(3) => CLOUD,
        Some(45 | 48) => FOG,
        Some(51..=67 | 80..=82) => RAIN,
        Some(71..=77 | 85 | 86) => SNOW,
        Some(95..=99) => STORM,
        _ => NONE,
    }
}

/// An icon as three lines, coloured by its masks.
fn icon_lines(look: &Look, code: Option<u64>, day: bool) -> Vec<Line<'static>> {
    let p = look.p;
    icon(code, day)
        .iter()
        .map(|(art, mask)| {
            Line::from(
                art.chars()
                    .zip(mask.chars().chain(std::iter::repeat(' ')))
                    .map(|(ch, m)| {
                        let color = match m {
                            'y' | 'l' => p.warning,
                            'm' => p.ink2,
                            'c' => p.ink2,
                            'b' => p.s1,
                            'w' => p.ink,
                            'f' => p.muted,
                            _ => p.muted,
                        };
                        Span::styled(ch.to_string(), style(color))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn degrees(v: Option<f64>) -> String {
    v.map_or(fmt::DASH.into(), |v| format!("{}°", v.round() as i64))
}

/// "2026-10-25T07:21" as "07:21".
fn clock(time: &str) -> &str {
    time.get(11..16).unwrap_or(fmt::DASH)
}

/// "2026-10-25T22:00" as "22h".
fn hour_label(time: &str) -> String {
    time.get(11..13).map_or(String::new(), |h| format!("{h}h"))
}

/// "2026-10-25..." as "Sun".
fn weekday(date: &str) -> &'static str {
    let field = |a: usize, b: usize| date.get(a..b)?.parse::<i64>().ok();
    let (Some(y), Some(m), Some(d)) = (field(0, 4), field(5, 7), field(8, 10)) else { return "" };
    // 1970-01-01 was a Thursday.
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days_from_civil(y, m, d).rem_euclid(7) as usize]
}

/// A chance of rain, blue once it is worth noticing.
fn rain_chance(look: &Look, chance: Option<f64>) -> Line<'static> {
    let color = if chance.is_some_and(|c| c >= 30.0) { look.p.s1 } else { look.p.muted };
    Line::styled(fmt::pct(chance), style(color))
}

/// What the sky is doing, in the forecaster's words where there are some.
fn words(look: &Look, text: &Option<String>, code: Option<u64>, day: bool) -> String {
    text.clone().unwrap_or_else(|| sky(look, code, day).0.to_string())
}

pub fn card(look: &Look, weather: &Source<WeatherData>, status: &Status, configured: bool, layout: char, touring: bool) -> Card {
    let p = look.p;
    let note = |text: &str| Card {
        title: "Weather".into(),
        right: String::new(),
        stale: false,
        error: None,
        body: vec![Part::Text(Line::styled(text.to_string(), style(p.muted)))],
    };
    if !configured {
        return note("Set WEATHER_PLACE in .env: a city name, or WEATHER_LATITUDE and WEATHER_LONGITUDE.");
    }
    let Some(w) = weather.data.as_ref() else {
        let mut card = note("Waiting for the forecast…");
        card.error = status.error.clone();
        return card;
    };
    let units = if w.imperial { ("in", "mph", "°F") } else { ("mm", "km/h", "°C") };

    let mut body = vec![now(look, w, units), sun(look, w, units), Part::Gap];
    match layout {
        'b' => {
            // No gap after the list: with none below it, it runs down to the
            // week rather than leaving rows blank.
            body.push(Part::HourList(HourList { hours: w.hours.clone(), wind: units.1 }));
            body.push(week_table(look, w, units));
        }
        'c' => {
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 1, skip: 0 }));
            body.push(Part::Gap);
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 3, skip: 12 }));
            body.push(Part::Gap);
            body.push(Part::Week(WeekTiles { days: w.days.clone() }));
        }
        _ => {
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 1, skip: 0 }));
            body.push(Part::Gap);
            body.push(Part::Outlook(Outlook { hours: w.hours.clone(), min_rows: 6, max_rows: 24 }));
            body.push(Part::Gap);
            body.push(week_table(look, w, units));
        }
    }
    // While the layouts take turns, the title says which this is.
    let tour = if touring { format!(" · layout {}", layout.to_ascii_uppercase()) } else { String::new() };
    Card {
        title: format!("Weather · {}{tour}", w.place),
        right: format!("{} · as of {}", w.source, clock(&w.now.time)),
        stale: !status.ok,
        error: status.error.clone(),
        body,
    }
}

/// The weather now: its icon, the temperature in big digits, and the sky,
/// feels-like, humidity and wind beside them.
fn now(look: &Look, w: &WeatherData, (_, wind_unit, temp_unit): (&str, &str, &str)) -> Part {
    let p = look.p;
    let n = &w.now;
    let figures = Big {
        value: n.temp.map_or(fmt::DASH.into(), |t| format!("{}", t.round() as i64).replace('-', "–")),
        unit: temp_unit.into(),
        slots: 3,
        small: !look.console_glyphs,
        style: bold(p.s1),
    };
    let wind = match (n.wind, n.gusts) {
        (Some(v), Some(g)) if g > v => format!("{} {wind_unit} {} · gusts {}", v.round() as i64, n.wind_from, g.round() as i64),
        (Some(v), _) if v < 1.0 => "calm".into(),
        (Some(v), _) => format!("{} {wind_unit} {}", v.round() as i64, n.wind_from),
        _ => fmt::DASH.into(),
    };
    let art: Vec<Part> = icon_lines(look, n.code, n.day).into_iter().map(Part::Text).collect();
    Part::Row(vec![
        (7, Part::Stack(art)),
        (figures.width(look), Part::Big(figures)),
        (
            0,
            Part::Stack(vec![
                Part::Text(Line::styled(words(look, &n.text, n.code, n.day), bold(p.ink))),
                Part::Text(Line::from(vec![
                    Span::styled("feels ", style(p.muted)),
                    Span::styled(degrees(n.feels), bold(p.ink)),
                    Span::styled(" · humidity ", style(p.muted)),
                    Span::styled(fmt::pct(n.humidity), bold(p.ink)),
                ])),
                Part::Text(Line::from(vec![Span::styled("wind ", style(p.muted)), Span::styled(wind, bold(p.ink))])),
            ]),
        ),
    ])
}

/// Sunrise, sunset, and today's rain where the source gives an amount.
fn sun(look: &Look, w: &WeatherData, (rain_unit, ..): (&str, &str, &str)) -> Part {
    let p = look.p;
    let today = w.days.first();
    let mut spans = vec![
        Span::styled("sunrise ", style(p.muted)),
        Span::styled(today.map_or(fmt::DASH, |d| clock(&d.sunrise)).to_string(), style(p.ink)),
        Span::styled(" · sunset ", style(p.muted)),
        Span::styled(today.map_or(fmt::DASH, |d| clock(&d.sunset)).to_string(), style(p.ink)),
    ];
    if let Some(rain) = today.and_then(|d| d.rain) {
        spans.push(Span::styled(" · today ", style(p.muted)));
        spans.push(Span::styled(format!("{rain:.1} {rain_unit}"), style(p.ink)));
    }
    Part::Text(Line::from(spans))
}

/// The week a line a day: the sky, high and low, the chance of rain, and
/// the amount where the source gives one.
fn week_table(look: &Look, w: &WeatherData, (rain_unit, ..): (&str, &str, &str)) -> Part {
    let p = look.p;
    let amounts = w.days.iter().any(|d| d.rain.is_some());
    let rows = w
        .days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (_, symbol, color) = sky(look, d.code, true);
            let mut row = vec![
                Line::styled(if i == 0 { "Today".to_string() } else { weekday(&d.date).to_string() }, style(p.ink)),
                Line::styled(symbol, style(color)),
                Line::styled(words(look, &d.text, d.code, true), style(p.ink2)),
                Line::from(vec![
                    Span::styled(degrees(d.high), bold(p.ink)),
                    Span::styled(" / ", style(p.muted)),
                    Span::styled(degrees(d.low), style(p.ink2)),
                ]),
                rain_chance(look, d.rain_chance),
            ];
            if amounts {
                row.push(Line::styled(d.rain.map_or(fmt::DASH.into(), |r| format!("{r:.1} {rain_unit}")), style(p.ink2)));
            }
            row
        })
        .collect();
    let mut cols = vec![
        Col { title: "Day", width: 5, right: false },
        Col { title: "", width: 1, right: false },
        Col { title: "Sky", width: 0, right: false },
        Col { title: "High / low", width: 10, right: true },
        Col { title: "Rain", width: 4, right: true },
    ];
    if amounts {
        cols.push(Col { title: "Amount", width: 7, right: true });
    }
    Part::Table(Table { caption: None, cols, rows, empty: "no forecast" })
}

/// Hours a column each, `step` hours apart after the first `skip`: the
/// hour, the sky, the temperature and the chance of rain. As many columns
/// as fit.
pub struct HourStrip {
    pub hours: Vec<Hour>,
    pub step: usize,
    pub skip: usize,
}

impl HourStrip {
    pub fn render(&self, pen: &mut Pen, area: Rect) {
        const COLUMN: u16 = 5;
        let p = pen.look.p;
        let columns = (area.width / COLUMN).max(1) as usize;
        for (c, hour) in self.hours.iter().skip(self.skip).step_by(self.step.max(1)).take(columns).enumerate() {
            let x = area.x + c as u16 * COLUMN;
            let (_, symbol, color) = sky(pen.look, hour.code, hour.day);
            let label = if self.skip == 0 && c == 0 { "now".to_string() } else { hour_label(&hour.time) };
            let cells = [
                Line::styled(label, style(p.muted)),
                Line::styled(symbol, style(color)),
                Line::styled(degrees(hour.temp), bold(p.ink)),
                rain_chance(pen.look, hour.rain_chance),
            ];
            for (r, cell) in cells.iter().enumerate() {
                let w = cell.width() as u16;
                let clip = Rect { x, y: area.y + r as u16, width: COLUMN, height: 1 }.intersection(area);
                pen.line(clip, x + COLUMN.saturating_sub(w) / 2, area.y + r as u16, cell);
            }
        }
    }
}

/// The coming hours a line each, as many as there are rows for: the hour,
/// the sky, the temperature, the chance of rain, the wind and the
/// forecaster's words. A line at each midnight names the day.
pub struct HourList {
    pub hours: Vec<Hour>,
    pub wind: &'static str,
}

impl HourList {
    pub const MIN_ROWS: u16 = 6;
    pub const MAX_ROWS: u16 = 99;

    pub fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        let mut y = area.y;
        for (i, hour) in self.hours.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            if i > 0 && hour.time.get(11..13) == Some("00") {
                let name = weekday(&hour.time);
                let rule = format!("── {name} {}", "─".repeat(area.width.saturating_sub(name.len() as u16 + 4) as usize));
                pen.text(area, area.x, y, &rule, style(p.grid));
                y += 1;
                if y >= area.bottom() {
                    break;
                }
            }
            let (_, symbol, color) = sky(pen.look, hour.code, hour.day);
            let wind = hour.wind.map_or(String::new(), |v| format!("{} {} {}", hour.wind_from, v.round() as i64, self.wind));
            let line = Line::from(vec![
                Span::styled(fmt::left(&if i == 0 { "now".to_string() } else { hour_label(&hour.time) }, 5), style(p.muted)),
                Span::styled(symbol, style(color)),
                Span::raw("  "),
                Span::styled(fmt::right(&degrees(hour.temp), 4), bold(p.ink)),
                Span::raw("  "),
                Span::styled(fmt::right(&fmt::pct(hour.rain_chance), 4), rain_chance(pen.look, hour.rain_chance).spans[0].style),
                Span::raw("  "),
                Span::styled(fmt::left(&wind, 11), style(p.ink2)),
                Span::raw(" "),
                Span::styled(words(pen.look, &hour.text, hour.code, hour.day), style(p.ink2)),
            ]);
            pen.line(area, area.x, y, &line);
            y += 1;
        }
    }
}

/// The week as tiles side by side: the day, its icon in ASCII, high and
/// low, and the chance of rain.
pub struct WeekTiles {
    pub days: Vec<Day>,
}

impl WeekTiles {
    pub const ROWS: u16 = 6;

    pub fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        let n = self.days.len().max(1) as u16;
        let tile = (area.width / n).max(7);
        for (i, d) in self.days.iter().enumerate() {
            let x = area.x + i as u16 * tile;
            let column = Rect { x, width: tile, ..area }.intersection(area);
            let centre = |line: &Line| x + tile.saturating_sub(line.width() as u16) / 2;
            let name = Line::styled(if i == 0 { "Today".to_string() } else { weekday(&d.date).to_string() }, bold(p.ink));
            pen.line(column, centre(&name), area.y, &name);
            for (r, art) in icon_lines(pen.look, d.code, true).iter().enumerate() {
                pen.line(column, centre(art), area.y + 1 + r as u16, art);
            }
            let temps = Line::from(vec![
                Span::styled(degrees(d.high), bold(p.ink)),
                Span::styled("/", style(p.muted)),
                Span::styled(degrees(d.low), style(p.ink2)),
            ]);
            pen.line(column, centre(&temps), area.y + 4, &temps);
            let rain = rain_chance(pen.look, d.rain_chance);
            pen.line(column, centre(&rain), area.y + 5, &rain);
        }
    }
}

/// Temperature over the coming hours as a graph scaled to its own range,
/// the chance of rain under it, and the days marked along the foot.
pub struct Outlook {
    pub hours: Vec<Hour>,
    pub min_rows: u16,
    pub max_rows: u16,
}

impl Outlook {
    pub fn render(&self, pen: &mut Pen, area: Rect) {
        let p = pen.look.p;
        if area.height < 4 || self.hours.is_empty() {
            return;
        }
        let temps: Vec<f64> = self.hours.iter().filter_map(|h| h.temp).collect();
        let lo = temps.iter().fold(f64::MAX, |a, &b| a.min(b)).floor() - 1.0;
        let hi = temps.iter().fold(f64::MIN, |a, &b| a.max(b)).ceil() + 1.0;
        let label_w = [degrees(Some(hi)), degrees(Some(lo)), "rain".into()].iter().map(|s| s.chars().count()).max().unwrap_or(4) as u16;
        let plot_x = area.x + label_w + 1;
        let width = area.width.saturating_sub(label_w + 1);
        // A cell per hour: as many hours as fit, from now.
        let n = self.hours.len().min(width as usize);
        let rain_rows = 2;
        let temp_rows = area.height - rain_rows - 1;
        let temp_area = Rect { x: plot_x, y: area.y, width, height: temp_rows };
        let rain_area = Rect { x: plot_x, y: area.y + temp_rows, width, height: rain_rows };
        let hours = &self.hours[..n];
        // Shown oldest first and newest at the right edge, so the n hours
        // fill the width with now at its left.
        let temp = Trace { data: hours.iter().map(|h| h.temp.map(|t| t - lo)).collect(), series: 1, label: String::new() };
        let rain = Trace { data: hours.iter().map(|h| h.rain_chance).collect(), series: 0, label: String::new() };
        let muted = style(p.muted);
        let grid = style(p.grid);
        let axis = |pen: &mut Pen, y: u16, label: Option<String>| {
            if let Some(label) = &label {
                pen.text(area, area.x, y, &fmt::right(label, label_w as usize), muted);
            }
            pen.cell(area, area.x + label_w, y, if label.is_some() { "┤" } else { "│" }, grid);
        };
        for r in 0..temp_rows {
            let label = match r {
                0 => Some(degrees(Some(hi))),
                r if r == temp_rows - 1 => Some(degrees(Some(lo))),
                _ => None,
            };
            axis(pen, area.y + r, label);
        }
        for r in 0..rain_rows {
            axis(pen, rain_area.y + r, (r == rain_rows - 1).then(|| "rain".to_string()));
        }
        plot(pen, temp_area, std::slice::from_ref(&temp), n, hi - lo, false, Some(1.0));
        plot(pen, rain_area, std::slice::from_ref(&rain), n, 100.0, false, Some(1.0));

        // The foot: each day's name where it starts, and "now" at the left
        // edge when the first day's name leaves room for it.
        let y = area.bottom() - 1;
        let left = plot_x + width.saturating_sub(n as u16);
        let midnights: Vec<u16> =
            (0..n).filter(|&i| hours[i].time.get(11..13) == Some("00")).map(|i| left + i as u16).collect();
        if midnights.first().map_or(true, |&x| x >= left + 4) {
            pen.text(area, left, y, "now", muted);
        }
        for (&x, i) in midnights.iter().zip(0..) {
            let name = weekday(&hours[(x - left) as usize].time);
            if midnights.get(i + 1).map_or(true, |&next| next >= x + name.len() as u16 + 1) {
                pen.text(area, x, y, name, style(p.ink2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_hours() {
        assert_eq!(weekday("2026-10-25"), "Sun");
        assert_eq!(weekday("2026-10-26T00:00"), "Mon");
        assert_eq!(clock("2026-10-25T07:21"), "07:21");
        assert_eq!(hour_label("2026-10-25T07:00"), "07h");
    }

    #[test]
    fn icons_are_seven_by_three_with_matching_masks() {
        for code in [0, 1, 2, 3, 45, 61, 73, 95, 200] {
            for day in [true, false] {
                for (art, mask) in icon(Some(code), day) {
                    assert_eq!(art.chars().count(), 7, "{code} {day} {art:?}");
                    assert_eq!(mask.chars().count(), 7, "{code} {day} {mask:?}");
                }
            }
        }
    }
}

//! The forecast card: the weather now in big figures beside an ASCII icon,
//! then the coming hours and the week, in one of three layouts:
//!
//!   A  an hourly strip, then the week as a table
//!   B  the hours a line each, as many as fit, then the week as a table
//!   C  an hourly strip, a three-hourly strip after it, the week as tiles

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::draw::{Big, Card, Col, Part, Pen, Table};
use super::theme::Look;
use crate::format as fmt;
use crate::sources::agent::days_from_civil;
use crate::sources::{Alert, Day, Hour, WeatherData};
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

    let mut body = banner(look, &w.alerts);
    body.extend([now(look, w, units), sun(look, w, units), Part::Gap]);
    // The forecaster's own days, without the longer outlook.
    let week: Vec<Day> = w.days.iter().filter(|d| !d.outlook).cloned().collect();
    match layout {
        'b' => {
            // No gap after the list: with none below it, it runs down to the
            // week rather than leaving rows blank.
            body.push(Part::HourList(HourList { hours: w.hours.clone(), wind: units.1 }));
            body.push(Part::Table(week_table(look, &week, units)));
        }
        'c' => {
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 1, skip: 0 }));
            body.push(Part::Gap);
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 3, skip: 12 }));
            body.push(Part::Gap);
            body.push(Part::Week(WeekTiles { days: week }));
        }
        _ => {
            // Every two hours from the next one: the next 22 hours in a row.
            body.push(Part::Hours(HourStrip { hours: w.hours.clone(), step: 2, skip: 1 }));
            body.push(Part::Gap);
            // The week at least; the outlook's later days as rows allow.
            body.push(Part::Growing(week_table(look, &w.days, units), week.len().max(1)));
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

/// Alerts in force, at the very top: each one's event and when it ends,
/// coloured by severity, with its headline under it. Nothing at all when
/// there are none.
fn banner(look: &Look, alerts: &[Alert]) -> Vec<Part> {
    const SHOWN: usize = 2;
    let p = look.p;
    let mut parts = Vec::new();
    for alert in alerts.iter().take(SHOWN) {
        let color = match alert.severity.as_str() {
            "Extreme" | "Severe" => p.critical,
            "Moderate" => p.warning,
            _ => p.ink2,
        };
        let mark = if look.console_glyphs { "!" } else { "⚠" };
        let mut spans = vec![Span::styled(format!("{mark} {}", alert.event), bold(color))];
        if !alert.until.is_empty() {
            spans.push(Span::styled(" · until ", style(p.muted)));
            spans.push(Span::styled(format!("{} {}", weekday(&alert.until), clock(&alert.until)), style(color)));
        }
        parts.push(Part::Text(Line::from(spans)));
        if let Some(headline) = &alert.headline {
            parts.push(Part::Wrap(headline.clone(), style(p.muted), 2));
        }
    }
    if alerts.len() > SHOWN {
        parts.push(Part::Text(Line::styled(format!("and {} more", alerts.len() - SHOWN), style(p.muted))));
    }
    if !parts.is_empty() {
        parts.push(Part::Gap);
    }
    parts
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

/// The days a line each: the sky, high and low, the chance of rain, and
/// the amount where the source gives one. Days from the longer outlook are
/// dimmer, with their date, since their weekdays come round again.
fn week_table(look: &Look, days: &[Day], (rain_unit, ..): (&str, &str, &str)) -> Table {
    let p = look.p;
    let amounts = days.iter().any(|d| d.rain.is_some());
    let rows = days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (_, symbol, color) = sky(look, d.code, true);
            let name = match (i, d.outlook) {
                (0, _) => "Today".to_string(),
                (_, true) => format!("{} {}", weekday(&d.date), d.date.get(8..10).unwrap_or("").trim_start_matches('0')),
                _ => weekday(&d.date).to_string(),
            };
            let mut row = vec![
                Line::styled(name, style(p.ink)),
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
            if d.outlook {
                for span in row.iter_mut().flat_map(|line| line.spans.iter_mut()) {
                    span.style = style(p.muted);
                }
            }
            row
        })
        .collect();
    let mut cols = vec![
        Col { title: "Day", width: 6, right: false, tint: None },
        Col { title: "", width: 1, right: false, tint: None },
        Col { title: "Sky", width: 0, right: false, tint: None },
        Col { title: "High / low", width: 10, right: true, tint: None },
        Col { title: "Rain", width: 4, right: true, tint: None },
    ];
    if amounts {
        cols.push(Col { title: "Amount", width: 7, right: true, tint: None });
    }
    Table { caption: None, cols, rows, empty: "no forecast" }
}

/// Hours a column each, `step` hours apart after the first `skip`: the
/// hour, the sky, the temperature and the chance of rain. As many columns
/// as fit. A column standing for several hours shows the first one's
/// temperature, and the worst of them for sky and rain, so a shower in the
/// hour between is not lost.
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
        let hours = &self.hours[self.skip.min(self.hours.len())..];
        for (c, span) in hours.chunks(self.step.max(1)).take(columns).enumerate() {
            let hour = &span[0];
            let x = area.x + c as u16 * COLUMN;
            // WMO codes run roughly from clear to storm, so the highest is
            // the worst sky.
            let code = span.iter().filter_map(|h| h.code).max();
            let rain = span.iter().filter_map(|h| h.rain_chance).max_by(f64::total_cmp);
            let (_, symbol, color) = sky(pen.look, code, hour.day);
            let label = if self.skip == 0 && c == 0 { "now".to_string() } else { hour_label(&hour.time) };
            let cells = [
                Line::styled(label, style(p.muted)),
                Line::styled(symbol, style(color)),
                Line::styled(degrees(hour.temp), bold(p.ink)),
                rain_chance(pen.look, rain),
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

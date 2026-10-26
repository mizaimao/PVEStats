//! The forecast card: the weather now in big figures, the coming hours as a
//! strip of columns, temperature over the next few days with the chance of
//! rain under it, and the week a line a day.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::draw::{plot, Big, Card, Col, Part, Pen, Table, Trace};
use super::theme::Look;
use crate::format as fmt;
use crate::sources::agent::days_from_civil;
use crate::sources::{Hour, WeatherData};
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
        Some(3) => ("Overcast", "☁", p.muted),
        Some(45 | 48) => ("Fog", "≡", p.muted),
        Some(51) => ("Light drizzle", "☂", p.s1),
        Some(53 | 55) => ("Drizzle", "☂", p.s1),
        Some(56 | 57) => ("Freezing drizzle", "☂", p.s1),
        Some(61) => ("Light rain", "☂", p.s1),
        Some(63) => ("Rain", "☂", p.s1),
        Some(65) => ("Heavy rain", "☂", p.s1),
        Some(66 | 67) => ("Freezing rain", "☂", p.s1),
        Some(71) => ("Light snow", "❄", p.ink),
        Some(73) => ("Snow", "❄", p.ink),
        Some(75) => ("Heavy snow", "❄", p.ink),
        Some(77) => ("Snow grains", "❄", p.ink),
        Some(80) => ("Light showers", "☂", p.s1),
        Some(81) => ("Showers", "☂", p.s1),
        Some(82) => ("Heavy showers", "☂", p.s1),
        Some(85 | 86) => ("Snow showers", "❄", p.ink),
        Some(95) => ("Thunderstorm", "ϟ", p.warning),
        Some(96 | 99) => ("Thunderstorm, hail", "ϟ", p.warning),
        _ => ("–", "·", p.muted),
    }
}

fn degrees(v: Option<f64>) -> String {
    v.map_or(fmt::DASH.into(), |v| format!("{}°", v.round() as i64))
}

/// "2026-10-25T07:21" as "07:21".
fn clock(time: &str) -> &str {
    time.get(11..16).unwrap_or(fmt::DASH)
}

/// "2026-10-25..." as "Sun".
fn weekday(date: &str) -> &'static str {
    let field = |a: usize, b: usize| date.get(a..b)?.parse::<i64>().ok();
    let (Some(y), Some(m), Some(d)) = (field(0, 4), field(5, 7), field(8, 10)) else { return "" };
    // 1970-01-01 was a Thursday.
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days_from_civil(y, m, d).rem_euclid(7) as usize]
}

/// Where the wind blows from, to sixteen points.
fn compass(from: Option<f64>) -> &'static str {
    const POINTS: [&str; 16] =
        ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];
    from.map_or("", |d| POINTS[((d.rem_euclid(360.0) / 22.5).round() as usize) % 16])
}

pub fn card(look: &Look, weather: &Source<WeatherData>, status: &Status, configured: bool) -> Card {
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
    let (rain_unit, wind_unit, temp_unit) = if w.imperial { ("in", "mph", "°F") } else { ("mm", "km/h", "°C") };
    let today = w.days.first();

    let (words, _, _) = sky(look, w.now.code, w.now.day);
    let figure = |label: &str, value: String| {
        Line::from(vec![Span::styled(format!("{label} "), style(p.muted)), Span::styled(value, bold(p.ink))])
    };
    let wind = match (w.now.wind, w.now.gusts) {
        (Some(v), Some(g)) => format!("{} {wind_unit} {} · gusts {}", v.round() as i64, compass(w.now.wind_from), g.round() as i64),
        (Some(v), None) => format!("{} {wind_unit} {}", v.round() as i64, compass(w.now.wind_from)),
        _ => fmt::DASH.into(),
    };
    let now = Big {
        value: w.now.temp.map_or(fmt::DASH.into(), |t| format!("{}", t.round() as i64).replace('-', "–")),
        unit: temp_unit.into(),
        slots: 3,
        small: !look.console_glyphs,
        style: bold(p.s1),
    };
    let current = Part::Row(vec![
        (now.width(look), Part::Big(now)),
        (
            0,
            Part::Stack(vec![
                Part::Text(Line::styled(words, bold(p.ink))),
                Part::Text(Line::from(vec![
                    Span::styled("feels ", style(p.muted)),
                    Span::styled(degrees(w.now.feels), bold(p.ink)),
                    Span::styled(" · humidity ", style(p.muted)),
                    Span::styled(fmt::pct(w.now.humidity), bold(p.ink)),
                ])),
                Part::Text(figure("wind", wind)),
            ]),
        ),
    ]);
    let sun = Part::Text(Line::from(vec![
        Span::styled("sunrise ", style(p.muted)),
        Span::styled(today.map_or(fmt::DASH, |d| clock(&d.sunrise)).to_string(), style(p.ink)),
        Span::styled(" · sunset ", style(p.muted)),
        Span::styled(today.map_or(fmt::DASH, |d| clock(&d.sunset)).to_string(), style(p.ink)),
        Span::styled(" · today ", style(p.muted)),
        Span::styled(
            today.and_then(|d| d.rain).map_or(fmt::DASH.into(), |r| format!("{r:.1} {rain_unit}")),
            style(p.ink),
        ),
    ]));

    let rows = w
        .days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (words, symbol, color) = sky(look, d.code, true);
            vec![
                Line::styled(if i == 0 { "Today".to_string() } else { weekday(&d.date).to_string() }, style(p.ink)),
                Line::styled(symbol, style(color)),
                Line::styled(words, style(p.ink2)),
                Line::from(vec![
                    Span::styled(degrees(d.high), bold(p.ink)),
                    Span::styled(" / ", style(p.muted)),
                    Span::styled(degrees(d.low), style(p.ink2)),
                ]),
                rain_chance(look, d.rain_chance),
                Line::styled(d.rain.map_or(fmt::DASH.into(), |r| format!("{r:.1} {rain_unit}")), style(p.ink2)),
            ]
        })
        .collect();

    Card {
        title: format!("Weather · {}", w.place),
        right: format!("as of {}", clock(&w.now.time)),
        stale: !status.ok,
        error: status.error.clone(),
        body: vec![
            current,
            sun,
            Part::Gap,
            Part::Hours(HourStrip { hours: w.hours.clone() }),
            Part::Gap,
            Part::Outlook(Outlook { hours: w.hours.clone(), min_rows: 6, max_rows: 20 }),
            Part::Gap,
            Part::Table(Table {
                caption: None,
                cols: vec![
                    Col { title: "Day", width: 5, right: false },
                    Col { title: "", width: 1, right: false },
                    Col { title: "Sky", width: 0, right: false },
                    Col { title: "High / low", width: 10, right: true },
                    Col { title: "Rain", width: 4, right: true },
                    Col { title: "Amount", width: 7, right: true },
                ],
                rows,
                empty: "no forecast",
            }),
        ],
    }
}

/// A chance of rain, blue once it is worth noticing.
fn rain_chance(look: &Look, chance: Option<f64>) -> Line<'static> {
    let color = if chance.is_some_and(|c| c >= 30.0) { look.p.s1 } else { look.p.muted };
    Line::styled(fmt::pct(chance), style(color))
}

/// The coming hours, a column each: the hour, the sky, the temperature and
/// the chance of rain. As many columns as fit, spaced to cover a day.
pub struct HourStrip {
    pub hours: Vec<Hour>,
}

impl HourStrip {
    pub fn render(&self, pen: &mut Pen, area: Rect) {
        const COLUMN: u16 = 5;
        let p = pen.look.p;
        let columns = (area.width / COLUMN).max(1) as usize;
        let step = 24usize.div_ceil(columns).max(1);
        for (c, hour) in self.hours.iter().step_by(step).take(columns).enumerate() {
            let x = area.x + c as u16 * COLUMN;
            let (_, symbol, color) = sky(pen.look, hour.code, hour.day);
            let label = if c == 0 { "now".to_string() } else { clock(&hour.time).get(..2).unwrap_or("").to_string() + "h" };
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
    fn days_and_bearings() {
        assert_eq!(weekday("2026-10-25"), "Sun");
        assert_eq!(weekday("2026-10-26T00:00"), "Mon");
        assert_eq!(compass(Some(206.0)), "SSW");
        assert_eq!(compass(Some(359.0)), "N");
        assert_eq!(clock("2026-10-25T07:21"), "07:21");
    }
}

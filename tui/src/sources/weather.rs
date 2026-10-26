//! The forecast. In the United States, the National Weather Service's own
//! (api.weather.gov): the forecast its local office publishes, hourly for
//! about a week, with the latest reading from the nearest station for now.
//! Elsewhere, Open-Meteo's. Both are free and need no account or key.
//!
//! Where it is for comes from .env, beside the other private settings, so
//! it stays off GitHub: WEATHER_PLACE, a place name looked up once, or
//! WEATHER_LATITUDE and WEATHER_LONGITUDE; WEATHER_UNITS=imperial for °F,
//! mph and inches.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use super::agent::{days_from_civil, unix_seconds};
use super::{lock, Alert, Day, Hour, Now, WeatherData, WEATHER_TIMEOUT};
use crate::config::Config;
use crate::json::{int, list, num, text};
use crate::net;
use crate::state::Source;

const OPEN_METEO: &str = "https://api.open-meteo.com/v1/forecast";
const GEOCODE: &str = "https://geocoding-api.open-meteo.com/v1/search";
const NWS: &str = "https://api.weather.gov";
/// The NWS asks every client to name itself.
const AGENT: &str = "pvestats-tui (homelab dashboard)";

/// Hours kept from now on: more than the widest view shows.
const HOURS: usize = 96;

/// Where the forecast comes from, settled on the first poll.
enum Provider {
    Nws { hourly: String, forecast: String, station: Option<String> },
    OpenMeteo,
}

pub struct Weather {
    config: Arc<Config>,
    http: ureq::Agent,
    /// Latitude, longitude and name, once known.
    place: Option<(f64, f64, String)>,
    provider: Option<Provider>,
}

impl Weather {
    pub fn new(config: Arc<Config>) -> Self {
        // Public certificates: ureq's bundled roots trust them.
        let http = ureq::AgentBuilder::new().timeout(WEATHER_TIMEOUT).user_agent(AGENT).build();
        Weather { config, http, place: None, provider: None }
    }

    fn get(&self, url: &str, started: Instant) -> Result<Value, String> {
        self.http
            .get(url)
            .set("Accept", "application/geo+json, application/json")
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })
    }

    /// Coordinates and a name for the place, looked up the first time.
    fn place(&mut self, started: Instant) -> Result<(f64, f64, String), String> {
        if let Some(place) = &self.place {
            return Ok(place.clone());
        }
        let name = self.config.weather_place.clone();
        let place = match self.config.weather_at {
            Some((lat, lon)) => {
                let name = if name.is_empty() { format!("{lat:.2}, {lon:.2}") } else { name };
                (lat, lon, name)
            }
            None => {
                let url = format!("{GEOCODE}?name={}&count=1&language=en&format=json", encode(&name));
                let found = self.get(&url, started)?;
                let first = list(&found, "results").first().ok_or_else(|| format!("no place called {name}"))?;
                let lat = num(first, "latitude").ok_or("the place has no latitude")?;
                let lon = num(first, "longitude").ok_or("the place has no longitude")?;
                (lat, lon, text(first, "name").map_or(name, str::to_string))
            }
        };
        self.place = Some(place.clone());
        Ok(place)
    }

    /// The NWS where it covers the place, Open-Meteo where it does not
    /// (it answers 404 outside the United States).
    fn provider(&mut self, lat: f64, lon: f64, started: Instant) -> Result<&Provider, String> {
        if self.provider.is_none() {
            let url = format!("{NWS}/points/{lat:.4},{lon:.4}");
            let provider = match self.http.get(&url).call() {
                Ok(response) => {
                    let point: Value = response.into_json().map_err(|e| format!("unreadable reply: {e}"))?;
                    let p = point.get("properties").cloned().unwrap_or_default();
                    let field = |key: &str| text(&p, key).map(str::to_string).ok_or(format!("no {key} for the place"));
                    let stations = field("observationStations")?;
                    // The nearest station first; without one, now comes
                    // from the forecast for this hour.
                    let station = self
                        .get(&stations, started)
                        .ok()
                        .and_then(|s| list(&s, "features").first().and_then(|f| text(&f["properties"], "stationIdentifier")).map(str::to_string));
                    Provider::Nws { hourly: field("forecastHourly")?, forecast: field("forecast")?, station }
                }
                Err(ureq::Error::Status(404, _)) => Provider::OpenMeteo,
                Err(e) => return Err(net::describe(e, started)),
            };
            self.provider = Some(provider);
        }
        Ok(self.provider.as_ref().expect("set just above"))
    }

    pub fn poll(&mut self, source: &Mutex<Source<WeatherData>>) -> Result<(), String> {
        let started = Instant::now();
        let (lat, lon, name) = self.place(started)?;
        let imperial = self.config.weather_imperial;
        let mut data = match self.provider(lat, lon, started)? {
            Provider::Nws { hourly, forecast, station } => {
                let units = if imperial { "" } else { "?units=si" };
                let (hourly, forecast, station) = (format!("{hourly}{units}"), format!("{forecast}{units}"), station.clone());
                let hours = self.get(&hourly, started)?;
                let days = self.get(&forecast, started)?;
                // A station that has gone quiet leaves now to the forecast.
                let observed = station.and_then(|s| self.get(&format!("{NWS}/stations/{s}/observations/latest"), started).ok());
                let mut data = nws(&hours, &days, observed.as_ref(), name, imperial, (lat, lon))?;
                // Alerts are worth having but not worth failing the poll over.
                let url = format!("{NWS}/alerts/active?point={lat:.4},{lon:.4}&status=actual");
                data.alerts = self.get(&url, started).map(|a| alerts(&a)).unwrap_or_default();
                // The NWS stops at a week; Open-Meteo's outlook runs on past
                // it, for rows that would otherwise stand empty. Nor is that
                // worth failing the poll over.
                let url = format!(
                    "{OPEN_METEO}?latitude={lat}&longitude={lon}\
                     &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,\
                     sunrise,sunset&timezone=auto&forecast_days=16{}",
                    open_meteo_units(imperial)
                );
                if let Ok(body) = self.get(&url, started) {
                    let last = data.days.last().map(|d| d.date.clone()).unwrap_or_default();
                    let later = open_meteo_days(&body).into_iter().filter(|d| d.date > last);
                    data.days.extend(later.map(|d| Day { outlook: true, ..d }));
                }
                data
            }
            Provider::OpenMeteo => {
                let units = open_meteo_units(imperial);
                let url = format!(
                    "{OPEN_METEO}?latitude={lat}&longitude={lon}\
                     &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,\
                     wind_speed_10m,wind_direction_10m,wind_gusts_10m,is_day\
                     &hourly=temperature_2m,precipitation_probability,weather_code,is_day,\
                     relative_humidity_2m,wind_speed_10m,wind_direction_10m\
                     &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,\
                     precipitation_sum,sunrise,sunset\
                     &timezone=auto&forecast_days=7{units}"
                );
                open_meteo(&self.get(&url, started)?, name, imperial)?
            }
        };
        // `tui: demo` shows what an alert looks like, when none is in force.
        if self.config.demo && data.alerts.is_empty() {
            let tomorrow = data.days.get(1).map_or(String::new(), |d| d.date.clone());
            data.alerts.push(Alert {
                event: "Frost Advisory".into(),
                severity: "Moderate".into(),
                headline: Some("Frost Advisory issued October 25 at 3:12PM EDT until October 26 at 9:00AM EDT by NWS Sterling VA".into()),
                until: format!("{tomorrow}T09:00"),
            });
        }
        lock(source).succeed(data);
        Ok(())
    }
}

// ------------------------------------------------------------------- NWS

fn nws(hourly: &Value, forecast: &Value, observed: Option<&Value>, place: String, imperial: bool, (lat, lon): (f64, f64)) -> Result<WeatherData, String> {
    let periods = |v: &Value| v.get("properties").map(|p| list(p, "periods").to_vec()).unwrap_or_default();
    let hourly = periods(hourly);
    if hourly.is_empty() {
        return Err("no hourly forecast in the reply".into());
    }
    let pop = |p: &Value| p.get("probabilityOfPrecipitation").and_then(|v| num(v, "value"));
    let hours: Vec<Hour> = hourly
        .iter()
        .take(HOURS)
        .map(|p| Hour {
            time: text(p, "startTime").and_then(|t| t.get(..16)).unwrap_or("").to_string(),
            temp: num(p, "temperature"),
            rain_chance: pop(p).or(Some(0.0)),
            code: text(p, "icon").and_then(nws_code),
            text: text(p, "shortForecast").map(str::to_string),
            humidity: p.get("relativeHumidity").and_then(|v| num(v, "value")),
            wind: text(p, "windSpeed").and_then(top_speed),
            wind_from: text(p, "windDirection").unwrap_or("").to_string(),
            day: p.get("isDaytime").and_then(Value::as_bool).unwrap_or(true),
        })
        .collect();
    // The UTC offset the forecast's times carry, "-04:00", in minutes.
    let offset = text(&hourly[0], "startTime").and_then(utc_offset).unwrap_or(0);

    // Day and night periods, joined by the date they start on: "Tonight"
    // belongs to today, "Monday Night" to Monday.
    let mut days: Vec<Day> = Vec::new();
    for p in periods(forecast) {
        let date = text(&p, "startTime").and_then(|t| t.get(..10)).unwrap_or("").to_string();
        let daytime = p.get("isDaytime").and_then(Value::as_bool).unwrap_or(true);
        if days.last().map_or(true, |d| d.date != date) {
            let (sunrise, sunset) = sun_times(&date, lat, lon, offset);
            days.push(Day { date: date.clone(), sunrise, sunset, ..Day::default() });
        }
        let day = days.last_mut().expect("pushed just above");
        let temp = num(&p, "temperature");
        if daytime {
            day.high = temp;
            day.code = text(&p, "icon").and_then(nws_code);
            day.text = text(&p, "shortForecast").map(str::to_string);
        } else {
            day.low = temp;
            if day.text.is_none() {
                day.code = text(&p, "icon").and_then(nws_code);
                day.text = text(&p, "shortForecast").map(str::to_string);
            }
        }
        day.rain_chance = match (day.rain_chance, pop(&p)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    days.truncate(7);

    // Now: the nearest station's latest reading, where it has one, else the
    // forecast for this hour.
    let first = &hours[0];
    let mut now = Now {
        time: first.time.clone(),
        temp: first.temp,
        feels: None,
        humidity: first.humidity,
        wind: first.wind,
        gusts: None,
        wind_from: first.wind_from.clone(),
        code: first.code,
        text: first.text.clone(),
        day: first.day,
    };
    if let Some(o) = observed.and_then(|o| o.get("properties")) {
        let reading = |key: &str| o.get(key).and_then(|v| num(v, "value"));
        let temp = |c: f64| if imperial { c * 9.0 / 5.0 + 32.0 } else { c };
        let speed = |kmh: f64| if imperial { kmh / 1.609_344 } else { kmh };
        if let Some(c) = reading("temperature") {
            now.temp = Some(temp(c));
            now.humidity = reading("relativeHumidity").or(now.humidity);
            now.wind = reading("windSpeed").map(speed).or(now.wind);
            now.gusts = reading("windGust").map(speed);
            if let Some(degrees) = reading("windDirection") {
                now.wind_from = compass(degrees).to_string();
            }
            now.feels = reading("heatIndex").or(reading("windChill")).map(temp);
            if let Some(words) = text(o, "textDescription").filter(|t| !t.is_empty()) {
                now.text = Some(words.to_string());
            }
            if let Some(at) = text(o, "timestamp").and_then(unix_seconds) {
                let local = (at as i64 + offset * 60).rem_euclid(86_400);
                now.time = format!("{}T{:02}:{:02}", first.time.get(..10).unwrap_or(""), local / 3600, local % 3600 / 60);
            }
        }
    }
    if now.feels.is_none() {
        now.feels = feels_like(now.temp, now.wind, now.humidity, imperial);
    }

    Ok(WeatherData { place, imperial, source: "NWS", now, hours, days, alerts: Vec::new() })
}

/// The alerts in force, from the NWS's active ones for the point: worst
/// first, each event once however many times it was issued.
fn alerts(body: &Value) -> Vec<Alert> {
    let rank = |severity: &str| match severity {
        "Extreme" => 0,
        "Severe" => 1,
        "Moderate" => 2,
        "Minor" => 3,
        _ => 4,
    };
    let mut found: Vec<Alert> = list(body, "features")
        .iter()
        .filter_map(|f| f.get("properties"))
        .filter(|p| text(p, "status") == Some("Actual") && text(p, "messageType") != Some("Cancel"))
        .map(|p| Alert {
            event: text(p, "event").unwrap_or("Alert").to_string(),
            severity: text(p, "severity").unwrap_or("Unknown").to_string(),
            headline: text(p, "headline").map(str::to_string),
            until: text(p, "ends").or_else(|| text(p, "expires")).and_then(|t| t.get(..16)).unwrap_or("").to_string(),
        })
        .collect();
    found.sort_by_key(|a| rank(&a.severity));
    let mut seen = std::collections::HashSet::new();
    found.retain(|a| seen.insert(a.event.clone()));
    found
}

/// The NWS's condition, from its icon's address
/// (".../icons/land/day/rain,40/ovc?size=small"), as the nearest WMO code.
fn nws_code(icon: &str) -> Option<u64> {
    let path = icon.split("/land/").nth(1)?;
    let name = path.split('/').nth(1)?.split([',', '?']).next()?;
    let name = name.strip_prefix("wind_").unwrap_or(name);
    Some(match name {
        "skc" | "hot" | "cold" => 0,
        "few" => 1,
        "sct" => 2,
        "bkn" | "ovc" => 3,
        "fog" | "haze" | "smoke" | "dust" => 45,
        "rain" => 63,
        "rain_showers" | "rain_showers_hi" => 80,
        "snow" => 73,
        "blizzard" => 75,
        "rain_snow" | "rain_sleet" | "snow_sleet" | "sleet" | "fzra" | "rain_fzra" | "snow_fzra" => 66,
        "tsra" | "tsra_sct" | "tsra_hi" | "tornado" | "hurricane" | "tropical_storm" => 95,
        _ => return None,
    })
}

/// "5 to 10 mph" or "1 mph" as its top speed.
fn top_speed(text: &str) -> Option<f64> {
    text.split_whitespace().filter_map(|w| w.parse::<f64>().ok()).last()
}

/// "2026-10-25T21:00:00-04:00" as -240 minutes.
fn utc_offset(time: &str) -> Option<i64> {
    let tail = time.get(time.len().checked_sub(6)?..)?;
    let sign = match tail.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    Some(sign * (tail[1..3].parse::<i64>().ok()? * 60 + tail[4..6].parse::<i64>().ok()?))
}

/// Sunrise and sunset on a date, local, by NOAA's approximation: good to a
/// minute or two away from the poles.
fn sun_times(date: &str, lat: f64, lon: f64, offset: i64) -> (String, String) {
    let field = |a: usize, b: usize| date.get(a..b)?.parse::<i64>().ok();
    let (Some(y), Some(m), Some(d)) = (field(0, 4), field(5, 7), field(8, 10)) else {
        return (String::new(), String::new());
    };
    let n = (days_from_civil(y, m, d) - days_from_civil(y, 1, 1)) as f64;
    let g = 2.0 * std::f64::consts::PI / 365.0 * n;
    let eqtime = 229.18
        * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2.0 * g).cos() - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos()
        + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    let lat = lat.to_radians();
    let cos_ha = 90.833f64.to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
    if cos_ha.abs() > 1.0 {
        return (String::new(), String::new()); // the sun neither rises nor sets
    }
    let ha = cos_ha.acos().to_degrees();
    let at = |minutes: f64| {
        let m = (minutes + offset as f64).round().rem_euclid(1440.0) as i64;
        format!("{date}T{:02}:{:02}", m / 60, m % 60)
    };
    (at(720.0 - 4.0 * (lon + ha) - eqtime), at(720.0 - 4.0 * (lon - ha) - eqtime))
}

/// Wind chill when it is cold and windy, the heat index when it is hot and
/// close, the temperature otherwise (the NWS's own formulas, in °F).
fn feels_like(temp: Option<f64>, wind: Option<f64>, humidity: Option<f64>, imperial: bool) -> Option<f64> {
    let t = temp?;
    let f = if imperial { t } else { t * 9.0 / 5.0 + 32.0 };
    let v = wind.map(|w| if imperial { w } else { w / 1.609_344 }).unwrap_or(0.0);
    let feels = if f <= 50.0 && v >= 3.0 {
        35.74 + 0.6215 * f - 35.75 * v.powf(0.16) + 0.4275 * f * v.powf(0.16)
    } else if let (true, Some(rh)) = (f >= 80.0, humidity) {
        -42.379 + 2.049_015_23 * f + 10.143_331_27 * rh - 0.224_755_41 * f * rh - 0.006_837_83 * f * f
            - 0.054_817_17 * rh * rh
            + 0.001_228_74 * f * f * rh
            + 0.000_852_82 * f * rh * rh
            - 0.000_001_99 * f * f * rh * rh
    } else {
        f
    };
    Some(if imperial { feels } else { (feels - 32.0) * 5.0 / 9.0 })
}

/// Degrees the wind blows from, to sixteen points.
pub fn compass(degrees: f64) -> &'static str {
    const POINTS: [&str; 16] =
        ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];
    POINTS[((degrees.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

// ------------------------------------------------------------- Open-Meteo

fn open_meteo(body: &Value, place: String, imperial: bool) -> Result<WeatherData, String> {
    let current = body.get("current").ok_or("no current weather in the reply")?;
    let now = Now {
        time: text(current, "time").unwrap_or("").to_string(),
        temp: num(current, "temperature_2m"),
        feels: num(current, "apparent_temperature"),
        humidity: num(current, "relative_humidity_2m"),
        wind: num(current, "wind_speed_10m"),
        gusts: num(current, "wind_gusts_10m"),
        wind_from: num(current, "wind_direction_10m").map_or("", compass).to_string(),
        code: int(current, "weather_code"),
        text: None,
        day: num(current, "is_day") != Some(0.0),
    };

    // Columns of values, one per hour or day; one missing reads as absent.
    let at = |table: &Value, key: &str, i: usize| list(table, key).get(i).and_then(Value::as_f64);
    let label = |table: &Value, key: &str, i: usize| {
        list(table, key).get(i).and_then(Value::as_str).unwrap_or("").to_string()
    };

    let hourly = body.get("hourly").cloned().unwrap_or_default();
    // From the hour now is in: "2026-10-25T21:30" is in "2026-10-25T21:00".
    let this_hour = format!("{}:00", now.time.get(..13).unwrap_or(""));
    let times = list(&hourly, "time");
    let start = times.iter().position(|t| t.as_str().is_some_and(|t| t >= this_hour.as_str())).unwrap_or(0);
    let hours = (start..times.len())
        .take(HOURS)
        .map(|i| Hour {
            time: label(&hourly, "time", i),
            temp: at(&hourly, "temperature_2m", i),
            rain_chance: at(&hourly, "precipitation_probability", i),
            code: at(&hourly, "weather_code", i).map(|c| c as u64),
            text: None,
            humidity: at(&hourly, "relative_humidity_2m", i),
            wind: at(&hourly, "wind_speed_10m", i),
            wind_from: at(&hourly, "wind_direction_10m", i).map_or("", compass).to_string(),
            day: at(&hourly, "is_day", i) != Some(0.0),
        })
        .collect();

    let days = open_meteo_days(body);
    Ok(WeatherData { place, imperial, source: "Open-Meteo", now, hours, days, alerts: Vec::new() })
}

/// The days in an Open-Meteo reply, today first.
fn open_meteo_days(body: &Value) -> Vec<Day> {
    let daily = body.get("daily").cloned().unwrap_or_default();
    let at = |key: &str, i: usize| list(&daily, key).get(i).and_then(Value::as_f64);
    let label = |key: &str, i: usize| list(&daily, key).get(i).and_then(Value::as_str).unwrap_or("").to_string();
    (0..list(&daily, "time").len())
        .map(|i| Day {
            date: label("time", i),
            code: at("weather_code", i).map(|c| c as u64),
            text: None,
            high: at("temperature_2m_max", i),
            low: at("temperature_2m_min", i),
            rain_chance: at("precipitation_probability_max", i),
            rain: at("precipitation_sum", i),
            sunrise: label("sunrise", i),
            sunset: label("sunset", i),
            outlook: false,
        })
        .collect()
}

fn open_meteo_units(imperial: bool) -> &'static str {
    if imperial {
        "&temperature_unit=fahrenheit&wind_speed_unit=mph&precipitation_unit=inch"
    } else {
        ""
    }
}

/// A place name for a URL query: letters and digits as they are, the rest
/// percent-encoded byte by byte.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_in_urls() {
        assert_eq!(encode("New York"), "New%20York");
        assert_eq!(encode("Zürich"), "Z%C3%BCrich");
    }

    #[test]
    fn open_meteo_hours_start_at_the_current_one() {
        let body = json!({
            "current": {"time": "2026-10-25T21:30", "temperature_2m": 10.4, "weather_code": 0, "is_day": 0},
            "hourly": {
                "time": ["2026-10-25T20:00", "2026-10-25T21:00", "2026-10-25T22:00"],
                "temperature_2m": [11.0, 10.5, 9.8],
                "precipitation_probability": [0, 5, 20],
                "weather_code": [0, 0, 3],
                "is_day": [0, 0, 0]
            },
            "daily": {"time": ["2026-10-25"], "temperature_2m_max": [15.0], "temperature_2m_min": [8.0],
                      "sunrise": ["2026-10-25T07:21"], "sunset": ["2026-10-25T18:03"]}
        });
        let w = open_meteo(&body, "Here".into(), false).unwrap();
        assert_eq!(w.hours.len(), 2);
        assert_eq!(w.hours[0].time, "2026-10-25T21:00");
        assert_eq!(w.hours[1].rain_chance, Some(20.0));
        assert!(!w.now.day);
        assert_eq!(w.days[0].sunset, "2026-10-25T18:03");
    }

    #[test]
    fn nws_icons_and_words() {
        assert_eq!(nws_code("https://api.weather.gov/icons/land/night/skc?size=small"), Some(0));
        assert_eq!(nws_code("https://api.weather.gov/icons/land/day/rain,40/ovc?size=small"), Some(63));
        assert_eq!(nws_code("https://api.weather.gov/icons/land/day/wind_bkn?size=small"), Some(3));
        assert_eq!(nws_code("https://api.weather.gov/icons/land/day/tsra_hi,30?size=small"), Some(95));
        assert_eq!(top_speed("5 to 10 mph"), Some(10.0));
        assert_eq!(utc_offset("2026-10-25T21:00:00-04:00"), Some(-240));
    }

    #[test]
    fn nws_days_join_day_and_night() {
        let period = |start: &str, day: bool, t: f64, pop: f64, words: &str| json!({
            "startTime": start, "isDaytime": day, "temperature": t,
            "probabilityOfPrecipitation": {"value": pop}, "shortForecast": words,
            "icon": "https://api.weather.gov/icons/land/day/sct?size=small",
            "relativeHumidity": {"value": 60}, "windSpeed": "5 mph", "windDirection": "SW"
        });
        let hourly = json!({"properties": {"periods": [period("2026-10-25T21:00:00-04:00", false, 48.0, 0.0, "Clear")]}});
        let forecast = json!({"properties": {"periods": [
            period("2026-10-25T18:00:00-04:00", false, 35.0, 0.0, "Clear"),
            period("2026-10-26T06:00:00-04:00", true, 69.0, 10.0, "Sunny"),
            period("2026-10-26T18:00:00-04:00", false, 47.0, 30.0, "Showers"),
        ]}});
        let w = nws(&hourly, &forecast, None, "Here".into(), true, (39.04, -77.12)).unwrap();
        assert_eq!(w.days.len(), 2);
        assert_eq!((w.days[0].high, w.days[0].low), (None, Some(35.0)));
        assert_eq!((w.days[1].high, w.days[1].low, w.days[1].rain_chance), (Some(69.0), Some(47.0), Some(30.0)));
        assert_eq!(w.days[1].text.as_deref(), Some("Sunny"));
        assert_eq!(w.now.temp, Some(48.0));
        assert_eq!(w.source, "NWS");
    }

    #[test]
    fn alerts_worst_first_without_tests_or_repeats() {
        let alert = |event: &str, severity: &str, status: &str| json!({"properties": {
            "event": event, "severity": severity, "status": status, "messageType": "Alert",
            "headline": "h", "ends": "2026-10-26T09:00:00-04:00"
        }});
        let body = json!({"features": [
            alert("Frost Advisory", "Minor", "Actual"),
            alert("Test Message", "Unknown", "Test"),
            alert("Wind Advisory", "Moderate", "Actual"),
            alert("Frost Advisory", "Minor", "Actual"),
        ]});
        let found = alerts(&body);
        let events: Vec<&str> = found.iter().map(|a| a.event.as_str()).collect();
        assert_eq!(events, ["Wind Advisory", "Frost Advisory"]);
        assert_eq!(found[0].until, "2026-10-26T09:00");
    }

    #[test]
    fn sunrise_in_maryland() {
        // North Bethesda, 25 October 2026: 07:28 and 18:16 EDT by Open-Meteo.
        let (rise, set) = sun_times("2026-10-25", 39.04, -77.12, -240);
        let minutes = |t: &str| t[11..13].parse::<i64>().unwrap() * 60 + t[14..16].parse::<i64>().unwrap();
        assert!((minutes(&rise) - (7 * 60 + 28)).abs() <= 3, "{rise}");
        assert!((minutes(&set) - (18 * 60 + 16)).abs() <= 3, "{set}");
    }
}

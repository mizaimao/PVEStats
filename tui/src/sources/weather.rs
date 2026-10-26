//! The forecast, from Open-Meteo: free, with no account or key. Where it is
//! for comes from .env, beside the other private settings, so it stays off
//! GitHub: WEATHER_PLACE, a place name looked up once, or WEATHER_LATITUDE
//! and WEATHER_LONGITUDE; WEATHER_UNITS=imperial for °F, mph and inches.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use super::{lock, Day, Hour, Now, WeatherData, WEATHER_TIMEOUT};
use crate::config::Config;
use crate::json::{int, list, num, text};
use crate::net;
use crate::state::Source;

const FORECAST: &str = "https://api.open-meteo.com/v1/forecast";
const GEOCODE: &str = "https://geocoding-api.open-meteo.com/v1/search";

/// Hours kept from now on: more than the widest chart shows.
const HOURS: usize = 96;

pub struct Weather {
    config: Arc<Config>,
    http: ureq::Agent,
    /// Latitude, longitude and name, once known.
    place: Option<(f64, f64, String)>,
}

impl Weather {
    pub fn new(config: Arc<Config>) -> Self {
        // Public certificates: ureq's bundled roots trust them.
        let http = ureq::AgentBuilder::new().timeout(WEATHER_TIMEOUT).build();
        Weather { config, http, place: None }
    }

    fn get(&self, url: &str, started: Instant) -> Result<Value, String> {
        self.http
            .get(url)
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

    pub fn poll(&mut self, source: &Mutex<Source<WeatherData>>) -> Result<(), String> {
        let started = Instant::now();
        let (lat, lon, name) = self.place(started)?;
        let units = if self.config.weather_imperial {
            "&temperature_unit=fahrenheit&wind_speed_unit=mph&precipitation_unit=inch"
        } else {
            ""
        };
        let url = format!(
            "{FORECAST}?latitude={lat}&longitude={lon}\
             &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,\
             wind_speed_10m,wind_direction_10m,wind_gusts_10m,is_day\
             &hourly=temperature_2m,precipitation_probability,weather_code,is_day\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,\
             precipitation_sum,sunrise,sunset\
             &timezone=auto&forecast_days=7{units}"
        );
        let body = self.get(&url, started)?;
        let data = parse(&body, name, self.config.weather_imperial)?;
        lock(source).succeed(data);
        Ok(())
    }
}

fn parse(body: &Value, place: String, imperial: bool) -> Result<WeatherData, String> {
    let current = body.get("current").ok_or("no current weather in the reply")?;
    let now = Now {
        time: text(current, "time").unwrap_or("").to_string(),
        temp: num(current, "temperature_2m"),
        feels: num(current, "apparent_temperature"),
        humidity: num(current, "relative_humidity_2m"),
        wind: num(current, "wind_speed_10m"),
        gusts: num(current, "wind_gusts_10m"),
        wind_from: num(current, "wind_direction_10m"),
        code: int(current, "weather_code"),
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
            day: at(&hourly, "is_day", i) != Some(0.0),
        })
        .collect();

    let daily = body.get("daily").cloned().unwrap_or_default();
    let days = (0..list(&daily, "time").len())
        .map(|i| Day {
            date: label(&daily, "time", i),
            code: at(&daily, "weather_code", i).map(|c| c as u64),
            high: at(&daily, "temperature_2m_max", i),
            low: at(&daily, "temperature_2m_min", i),
            rain_chance: at(&daily, "precipitation_probability_max", i),
            rain: at(&daily, "precipitation_sum", i),
            sunrise: label(&daily, "sunrise", i),
            sunset: label(&daily, "sunset", i),
        })
        .collect();

    Ok(WeatherData { place, imperial, now, hours, days })
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
    fn hours_start_at_the_current_one() {
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
        let w = parse(&body, "Here".into(), false).unwrap();
        assert_eq!(w.hours.len(), 2);
        assert_eq!(w.hours[0].time, "2026-10-25T21:00");
        assert_eq!(w.hours[1].rain_chance, Some(20.0));
        assert!(!w.now.day);
        assert_eq!(w.days[0].sunset, "2026-10-25T18:03");
    }
}

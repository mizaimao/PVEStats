//! The ml60 GPU agent (spec §3.2), as pvestats/sources/agent.py.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::{lock, AGENT_TIMEOUT};
use crate::config::Config;
use crate::net;
use crate::state::{clock, Source};

pub struct Agent {
    config: Arc<Config>,
    http: ureq::Agent,
}

impl Agent {
    pub fn new(config: Arc<Config>) -> Self {
        // Caddy on ml60 serves the agent with a home-CA certificate.
        Agent { config, http: net::http(net::home_ca_tls(), AGENT_TIMEOUT) }
    }

    pub fn poll(&mut self, source: &Mutex<Source>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.config.agent_url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })?;

        let models: Vec<Value> = payload
            .get("models")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|m| {
                let (size, vram) = (m.get("size").cloned().unwrap_or(Value::Null), m.get("size_vram").cloned().unwrap_or(Value::Null));
                let mut model = Map::new();
                model.insert("name".into(), m.get("name").cloned().unwrap_or(Value::Null));
                model.insert("size".into(), size.clone());
                model.insert("size_vram".into(), vram.clone());
                model.insert("context_length".into(), m.get("context_length").cloned().unwrap_or(Value::Null));
                for (k, v) in placement(size.as_f64(), vram.as_f64()) {
                    model.insert(k.into(), v);
                }
                // Converted here so the page never parses Ollama's timestamp,
                // which carries nanoseconds and a local offset.
                let unload_in = m.get("expires_at").and_then(Value::as_str).and_then(seconds_until);
                model.insert("unload_in".into(), json!(unload_in));
                Value::Object(model)
            })
            .collect();
        // Absent on an agent that predates the unload endpoint, which is
        // right: the page then shows no stop buttons.
        let can_unload = payload.get("can_unload").and_then(Value::as_bool).unwrap_or(false);
        // ml60's own memory, with when it was read: the agent still sends it
        // while NVML fails, so the collector judges its age, not the source's.
        let ram = ram(payload.get("ram"));
        let at = clock();

        let Some(gpu) = payload.get("gpu").and_then(Value::as_object) else {
            // NVML failed on ml60, but the agent still serves the models for
            // exactly this case: keep them current, and still fail, so the
            // card says why its GPU readings are old.
            let mut s = lock(source);
            s.data.insert("models".into(), Value::Array(models));
            s.data.insert("can_unload".into(), json!(can_unload));
            s.data.insert("ram".into(), ram);
            s.data.insert("ram_at".into(), json!(at));
            let error = payload.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
            return Err(error.unwrap_or("agent reported no GPU").to_string());
        };

        let procs: Vec<Value> = gpu
            .get("procs")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|p| {
                let name = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or("?");
                // Ollama's worker reports an absolute path; show the leaf.
                json!({"pid": p.get("pid"), "name": name.rsplit('/').next().unwrap_or(name), "vram": p.get("vram")})
            })
            .collect();

        let mut s = lock(source);
        let field = |k: &str| gpu.get(k).cloned();
        s.push("gpu.util", field("util"));
        s.push("gpu.vram_used", field("vram_used"));
        // VRAM as a percentage too, so utilisation and memory can share one
        // 0-100 plot the way nvtop draws them.
        let used = gpu.get("vram_used").and_then(Value::as_f64);
        let total = gpu.get("vram_total").and_then(Value::as_f64).filter(|t| *t != 0.0);
        s.push("gpu.vram_pct", used.zip(total).map(|(u, t)| json!(u / t * 100.0)));
        s.push("gpu.power", field("power"));
        s.push("gpu.temp", field("temp"));

        let mut data = gpu.clone();
        data.insert("procs".into(), Value::Array(procs));
        data.insert("models".into(), Value::Array(models));
        data.insert("can_unload".into(), json!(can_unload));
        data.insert("ram".into(), ram);
        data.insert("ram_at".into(), json!(at));
        s.succeed(data);
        Ok(())
    }
}

/// ml60's memory as ml60 sees it, in bytes, or null from an agent that does
/// not send it. PVE counts a VM's disk cache as used memory.
fn ram(ram: Option<&Value>) -> Value {
    let Some(ram) = ram.and_then(Value::as_object) else { return Value::Null };
    let number = |k: &str| ram.get(k).filter(|v| v.is_number()).cloned();
    match (number("total"), number("available")) {
        (Some(total), Some(available)) if total.as_f64().is_some_and(|t| t > 0.0) => {
            json!({"total": total, "available": available, "cache": number("cache").unwrap_or(json!(0))})
        }
        _ => Value::Null,
    }
}

/// Where a loaded model lives, GPU versus system RAM, by the arithmetic of
/// `ollama ps`'s PROCESSOR column.
fn placement(size: Option<f64>, size_vram: Option<f64>) -> [(&'static str, Value); 3] {
    let none = [("processor", Value::Null), ("offloaded", Value::Null), ("gpu_fraction", Value::Null)];
    let (Some(size), Some(vram)) = (size, size_vram) else { return none };
    if size == 0.0 || vram > size {
        return none;
    }
    if vram == 0.0 {
        return [("processor", json!("100% CPU")), ("offloaded", json!(true)), ("gpu_fraction", json!(0.0))];
    }
    if vram == size {
        return [("processor", json!("100% GPU")), ("offloaded", json!(false)), ("gpu_fraction", json!(1.0))];
    }
    let cpu = python_round((size - vram) / size * 100.0);
    [
        ("processor", json!(format!("{cpu}%/{}% CPU/GPU", 100 - cpu))),
        ("offloaded", json!(true)),
        ("gpu_fraction", json!(vram / size)),
    ]
}

/// Python's round(): halves go to the even neighbour.
fn python_round(v: f64) -> i64 {
    let r = v.round();
    if (v - v.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - v.signum()) as i64
    } else {
        r as i64
    }
}

/// Seconds from now until an RFC 3339 instant. Negative once it has passed,
/// not clamped: a model still listed well past its expiry is busy.
fn seconds_until(timestamp: &str) -> Option<i64> {
    let when = unix_seconds(timestamp)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    Some(python_round(when - now))
}

/// Ollama's timestamps carry nanoseconds and a local offset
/// ("2026-10-25T14:03:12.123456789-07:00"). No offset reads as UTC.
fn unix_seconds(ts: &str) -> Option<f64> {
    let field = |from: usize, to: usize| ts.get(from..to)?.parse::<i64>().ok();
    let b = ts.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    let mut rest = &ts[19..];
    let mut fraction = 0.0;
    if let Some(digits) = rest.strip_prefix('.') {
        let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
        fraction = format!("0.{}", &digits[..end]).parse().ok()?;
        rest = &digits[end..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (h, m) = rest[1..].split_once(':')?;
            sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset) as f64 + fraction)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_as_ollama_ps() {
        assert_eq!(placement(Some(10.0), Some(10.0))[0].1, json!("100% GPU"));
        assert_eq!(placement(Some(10.0), Some(0.0))[1].1, json!(true));
        assert_eq!(placement(Some(100.0), Some(64.0))[0].1, json!("36%/64% CPU/GPU"));
        assert_eq!(placement(Some(1.0), Some(2.0))[0].1, Value::Null);
        assert_eq!((python_round(2.5), python_round(3.5), python_round(-2.5)), (2, 4, -2));
    }

    #[test]
    fn timestamps() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0.0));
        let t = unix_seconds("2026-10-25T14:03:12.5-07:00").unwrap();
        assert_eq!(t, unix_seconds("2026-10-25T21:03:12Z").unwrap() + 0.5);
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn memory() {
        let r = json!({"total": 64, "available": 56, "cache": 50});
        assert_eq!(ram(Some(&r)), r);
        assert_eq!(ram(Some(&json!({"total": 64, "available": 8})))["cache"], json!(0));
        for bad in [json!(null), json!({"total": 64}), json!({"total": 0, "available": 0}), json!({"total": true, "available": 1})] {
            assert_eq!(ram(Some(&bad)), Value::Null, "{bad}");
        }
        assert_eq!(ram(None), Value::Null);
    }
}

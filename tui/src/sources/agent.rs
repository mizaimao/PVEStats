//! The ml60 GPU agent (spec §3.2).

use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{lock, AgentData, Gpu, Model, Pcie, Proc, Ram, AGENT_TIMEOUT};
use crate::json::{int, list, num, text};
use crate::net;
use crate::state::{clock, Source};

pub struct Agent {
    url: String,
    http: ureq::Agent,
    /// Made-up models and processes on each reading (`tui: demo`).
    demo: bool,
}

impl Agent {
    pub fn new(url: String, demo: bool) -> Self {
        // Caddy on ml60 serves the agent with a home-CA certificate.
        Agent { url, http: net::http(net::home_ca_tls(), AGENT_TIMEOUT), demo }
    }

    pub fn poll(&mut self, source: &Mutex<Source<AgentData>>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| {
                if net::is_timeout(&e) {
                    net::timed_out(started)
                } else {
                    format!("unreadable reply: {e}")
                }
            })?;

        let models: Vec<Model> = list(&payload, "models")
            .iter()
            .map(|m| {
                let (processor, offloaded) = placement(num(m, "size"), num(m, "size_vram"));
                Model {
                    name: text(m, "name").unwrap_or("?").to_string(),
                    size_vram: num(m, "size_vram"),
                    processor,
                    offloaded,
                    unload_in: text(m, "expires_at").and_then(seconds_until),
                }
            })
            .collect();

        let ram = ram(&payload);
        let ram = ram.filter(|r| r.total > 0.0);
        let (ram_used, ram_cached) = (ram.as_ref().map(Ram::used_pct), ram.as_ref().map(Ram::cached_pct));

        let Some(gpu) = payload.get("gpu").filter(|g| g.is_object()) else {
            // NVML failed on ml60, but the agent still serves Ollama's models
            // for exactly this case. Keep them current, and still fail, so
            // the card says why its GPU readings are old.
            let mut source = lock(source);
            let previous = source.data.as_ref().and_then(|d| d.gpu.clone());
            source.push("ram.used", ram_used);
            source.push("ram.cached", ram_cached);
            source.data = Some(AgentData { gpu: previous, models, ram });
            return Err(text(&payload, "error").unwrap_or("agent reported no GPU").to_string());
        };

        let pcie = gpu.get("pcie").cloned().unwrap_or_default();
        let gpu = Gpu {
            util: num(gpu, "util"),
            vram_used: num(gpu, "vram_used"),
            vram_total: num(gpu, "vram_total"),
            temp: num(gpu, "temp"),
            power: num(gpu, "power"),
            power_limit: num(gpu, "power_limit"),
            clock_sm: num(gpu, "clock_sm"),
            fan: num(gpu, "fan"),
            pcie: Pcie {
                gen: int(&pcie, "gen"),
                width: int(&pcie, "width"),
                max_gen: int(&pcie, "max_gen"),
                tx_kbps: num(&pcie, "tx_kbps"),
                rx_kbps: num(&pcie, "rx_kbps"),
            },
            procs: list(gpu, "procs")
                .iter()
                .map(|p| Proc {
                    pid: int(p, "pid"),
                    // Ollama's worker reports an absolute path; show the leaf.
                    name: text(p, "name").unwrap_or("?").rsplit('/').next().unwrap_or("?").to_string(),
                    vram: num(p, "vram"),
                })
                .collect(),
        };

        let (mut gpu, mut models) = (gpu, models);
        if self.demo {
            demo(&mut gpu, &mut models);
        }
        let mut source = lock(source);
        source.push("gpu.util", gpu.util);
        source.push("gpu.vram_used", gpu.vram_used);
        // VRAM as a percentage too, so utilisation and memory can share one
        // 0-100 plot the way nvtop draws them.
        let vram_pct = match (gpu.vram_used, gpu.vram_total) {
            (Some(used), Some(total)) if total > 0.0 => Some(used / total * 100.0),
            _ => None,
        };
        source.push("gpu.vram_pct", vram_pct);
        source.push("gpu.power", gpu.power);
        source.push("gpu.temp", gpu.temp);
        source.push("ram.used", ram_used);
        // Programs and cache together: the line over the memory chart.
        source.push("ram.cached", ram_cached);
        source.succeed(AgentData { gpu: Some(gpu), models, ram });
        Ok(())
    }
}

/// ml60's own memory, from inside it: an agent from before it was added
/// sends none, and the card falls back to PVE's figure.
fn ram(payload: &Value) -> Option<Ram> {
    let r = payload.get("ram").filter(|r| r.is_object())?;
    Some(Ram { total: num(r, "total")?, available: num(r, "available")?, cache: num(r, "cache").unwrap_or(0.0), at: clock() })
}

/// Four made-up Ollama models (one spilled to the CPU, one in use, one kept
/// for good) and seven python processes, with VRAM in use to match, in place
/// of the real ones: to see how a busy ml60 lays out. The rest of the
/// reading stays real.
fn demo(gpu: &mut Gpu, models: &mut Vec<Model>) {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let model = |name: &str, gib: f64, processor: &str, offloaded: bool, unload_in: i64| Model {
        name: name.into(),
        size_vram: Some(gib * GIB),
        processor: Some(processor.into()),
        offloaded,
        unload_in: Some(unload_in),
    };
    *models = vec![
        model("qwen3:32b", 20.1, "100% GPU", false, 240),
        model("llama3.3:70b-instruct-q4_K_M", 38.4, "22%/78% CPU/GPU", true, -90),
        model("gemma3:27b", 17.2, "100% GPU", false, 1500),
        model("nomic-embed-text:latest", 0.6, "100% GPU", false, 10_000_000_000),
    ];
    gpu.procs = (0..7)
        .map(|i| Proc { pid: Some(41_200 + i * 37), name: "python3.13".into(), vram: Some((0.8 + i as f64 * 1.3) * GIB) })
        .collect();
    gpu.vram_used = Some(76.3 * GIB);
}

/// Where a loaded model actually lives, GPU versus system RAM: the
/// arithmetic behind `ollama ps`'s PROCESSOR column, so the screen agrees
/// with the command. A model that spilled to the CPU is dramatically slower,
/// and GPU utilisation alone will not show it.
fn placement(size: Option<f64>, size_vram: Option<f64>) -> (Option<String>, bool) {
    let (Some(size), Some(vram)) = (size, size_vram) else { return (None, false) };
    if size == 0.0 || vram > size {
        return (None, false);
    }
    if vram == 0.0 {
        return (Some("100% CPU".into()), true);
    }
    if vram == size {
        return (Some("100% GPU".into()), false);
    }
    let cpu = ((size - vram) / size * 100.0).round() as i64;
    (Some(format!("{cpu}%/{}% CPU/GPU", 100 - cpu)), true)
}

/// Seconds from now until an RFC 3339 instant. Negative once it has passed,
/// not clamped: a model still listed well past its expiry is busy.
fn seconds_until(timestamp: &str) -> Option<i64> {
    let when = unix_seconds(timestamp)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    Some((when - now).round() as i64)
}

/// Ollama's timestamps carry nanoseconds and a local offset
/// ("2026-10-25T14:03:12.123456789-07:00"). No offset reads as UTC.
pub(crate) fn unix_seconds(ts: &str) -> Option<f64> {
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
            let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = digits[2..].parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset) as f64 + fraction)
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
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
    fn timestamps() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(unix_seconds("2000-03-01T00:00:00Z"), Some(951_868_800.0));
        let t = unix_seconds("2026-10-25T14:03:12.5-07:00").unwrap();
        assert_eq!(t, unix_seconds("2026-10-25T21:03:12Z").unwrap() + 0.5);
        assert_eq!(unix_seconds("2026-10-25T21:03:12"), unix_seconds("2026-10-25T21:03:12+00:00"));
        assert!(unix_seconds("2318-01-01T00:00:00Z").unwrap() > 1e10);
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn placements() {
        assert_eq!(placement(Some(10.0), Some(10.0)), (Some("100% GPU".into()), false));
        assert_eq!(placement(Some(10.0), Some(0.0)), (Some("100% CPU".into()), true));
        assert_eq!(placement(Some(100.0), Some(64.0)), (Some("36%/64% CPU/GPU".into()), true));
        assert_eq!(placement(None, Some(1.0)), (None, false));
        assert_eq!(placement(Some(1.0), Some(2.0)), (None, false));
    }

    #[test]
    fn memory() {
        let r = ram(&serde_json::json!({"ram": {"total": 64, "available": 56, "cache": 57}})).unwrap();
        assert_eq!((r.total, r.available, r.cache), (64.0, 56.0, 57.0));
        assert!(clock() - r.at < 1.0);
        // An older agent, one that could not read /proc/meminfo, and a
        // reply missing what the figure needs.
        assert!(ram(&serde_json::json!({"gpu": null})).is_none());
        assert!(ram(&serde_json::json!({"ram": null})).is_none());
        assert!(ram(&serde_json::json!({"ram": {"total": 64}})).is_none());
        assert_eq!(ram(&serde_json::json!({"ram": {"total": 64, "available": 8}})).unwrap().cache, 0.0);
    }
}

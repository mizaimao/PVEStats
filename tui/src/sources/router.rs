//! The router's own agent (router/ in this repo), on the router's LAN
//! address: its counters since boot, made into rates here from one reading
//! to the next. With `tui: demo` and no router configured, made-up readings
//! show what the card looks like.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::{lock, Band, Port, RouterData, ROUTER_TIMEOUT};
use crate::json::{int, list, num, text};
use crate::net;
use crate::state::Source;

pub struct Router {
    /// None for made-up readings.
    url: Option<String>,
    http: ureq::Agent,
    /// The last reading's counters, by name, and when it was taken.
    last: Option<(Instant, HashMap<String, f64>)>,
    started: Instant,
}

impl Router {
    pub fn new(url: Option<String>) -> Self {
        // Plain HTTP on the LAN; the TLS settings go unused.
        let http = net::http(net::home_ca_tls(), ROUTER_TIMEOUT);
        Router { url, http, last: None, started: Instant::now() }
    }

    pub fn poll(&mut self, source: &Mutex<Source<RouterData>>) -> Result<(), String> {
        let data = match &self.url {
            None => demo(self.started.elapsed().as_secs_f64()),
            Some(url) => {
                let started = Instant::now();
                let payload: Value = self
                    .http
                    .get(url)
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
                self.read(&payload)
            }
        };
        let mut source = lock(source);
        source.push("wan.down", data.wan_down_bps);
        source.push("wan.up", data.wan_up_bps);
        source.succeed(data);
        Ok(())
    }

    /// One reading, with rates against the last one.
    fn read(&mut self, payload: &Value) -> RouterData {
        let now = Instant::now();
        let mut counters: HashMap<String, f64> = HashMap::new();
        let last = self.last.take();
        // Per second since the last reading; None for the first, or for a
        // counter that went backwards (the router rebooted).
        let mut rate = |key: String, value: Option<f64>| -> Option<f64> {
            let value = value?;
            counters.insert(key.clone(), value);
            let (then, before) = last.as_ref()?;
            let seconds = now.duration_since(*then).as_secs_f64();
            let previous = *before.get(&key)?;
            (seconds > 0.0 && value >= previous).then(|| (value - previous) / seconds)
        };

        let cpu = payload.get("cpu").cloned().unwrap_or_default();
        let busy = rate("cpu.busy".into(), num(&cpu, "busy"));
        let total = rate("cpu.total".into(), num(&cpu, "total"));
        let cpu_pct = match (busy, total) {
            (Some(b), Some(t)) if t > 0.0 => Some((b / t * 100.0).clamp(0.0, 100.0)),
            _ => None,
        };
        let wan = payload.get("wan").cloned().unwrap_or_default();
        let wan_down_bps = rate("wan.rx".into(), num(&wan, "rx"));
        let wan_up_bps = rate("wan.tx".into(), num(&wan, "tx"));
        // A port's or a band's rx is what its devices sent: up, from them.
        let ports = list(payload, "ports")
            .iter()
            .map(|p| {
                let name = text(p, "name").unwrap_or("?");
                Port {
                    up: p.get("up").and_then(Value::as_bool).unwrap_or(false),
                    wan: p.get("wan").and_then(Value::as_bool).unwrap_or(false),
                    down_bps: rate(format!("port.{name}.tx"), num(p, "tx")),
                    up_bps: rate(format!("port.{name}.rx"), num(p, "rx")),
                }
            })
            .collect();
        let bands = list(payload, "bands")
            .iter()
            .map(|b| {
                let name = text(b, "name").unwrap_or("?").to_string();
                Band {
                    on: b.get("on").and_then(Value::as_bool).unwrap_or(false),
                    devices: int(b, "devices"),
                    down_bps: rate(format!("band.{name}.tx"), num(b, "tx")),
                    up_bps: rate(format!("band.{name}.rx"), num(b, "rx")),
                    name,
                }
            })
            .collect();
        let mem = payload.get("mem").cloned().unwrap_or_default();
        let mem_total = num(&mem, "total");
        let data = RouterData {
            model: text(payload, "model").unwrap_or("").to_string(),
            uptime: num(payload, "uptime"),
            cpu_pct,
            mem_used: mem_total.zip(num(&mem, "available")).map(|(t, a)| (t - a).max(0.0)),
            mem_total,
            temp: payload.get("temp").and_then(|t| num(t, "cpu")),
            wan_up: wan.get("up").and_then(Value::as_bool).unwrap_or(false),
            wan_ip: text(&wan, "ip").unwrap_or("").to_string(),
            wan_down_bps,
            wan_up_bps,
            ports,
            wired_devices: int(payload, "wired_devices"),
            bands,
        };
        self.last = Some((now, counters));
        data
    }
}

/// Made-up readings for `tui: demo`, shaped like this router's.
fn demo(t: f64) -> RouterData {
    let wave = |period: f64, phase: f64| ((t / period + phase).sin() + 1.0) / 2.0;
    let down = 1.5e6 + 9.0e6 * wave(11.0, 0.0).powi(3);
    let up = 0.2e6 + 1.2e6 * wave(7.0, 1.3).powi(2);
    let port = |up_now: bool, share: f64| Port { up: up_now, wan: false, down_bps: Some(down * share), up_bps: Some(up * share) };
    let band = |name: &str, devices: u64, share: f64| Band {
        name: name.into(),
        on: true,
        devices: Some(devices),
        down_bps: Some(down * share),
        up_bps: Some(up * share),
    };
    RouterData {
        model: "GT-BE98_PRO".into(),
        uptime: Some(186_000.0 + t),
        cpu_pct: Some(3.0 + 4.0 * wave(5.0, 2.0)),
        mem_used: Some(1.21e9),
        mem_total: Some(2.10e9),
        temp: Some(69.5),
        wan_up: true,
        wan_ip: "203.0.113.7".into(),
        wan_down_bps: Some(down),
        wan_up_bps: Some(up),
        ports: vec![port(true, 0.55), port(true, 0.25), port(false, 0.0), port(true, 0.01)],
        wired_devices: Some(11),
        bands: vec![band("5 GHz", 1, 0.04), band("6 GHz-1", 1, 0.12), band("6 GHz-2", 3, 0.03), band("2.4 GHz", 0, 0.0)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reading(rx: f64, busy: f64, total: f64) -> Value {
        json!({
            "model": "GT-BE98_PRO", "uptime": 10.0,
            "cpu": {"busy": busy, "total": total},
            "mem": {"total": 2000.0, "available": 500.0},
            "temp": {"cpu": 69.7},
            "wan": {"up": true, "ip": "192.0.2.1", "rx": rx, "tx": 0},
            "ports": [{"name": "eth0", "up": true, "mbps": 10000, "rx": 5, "tx": rx}],
            "bands": [{"name": "5 GHz", "on": true, "devices": null, "rx": 1, "tx": 1}],
            "devices_age": null
        })
    }

    #[test]
    fn rates_come_from_two_readings_and_not_across_a_reboot() {
        let mut r = Router::new(Some(String::new()));
        let first = r.read(&reading(1000.0, 10.0, 100.0));
        assert_eq!(first.wan_down_bps, None);
        assert_eq!(first.cpu_pct, None);
        assert_eq!(first.mem_used, Some(1500.0));
        assert_eq!(first.bands[0].devices, None);
        // Pretend the first reading was a second ago.
        if let Some((then, _)) = r.last.as_mut() {
            *then -= std::time::Duration::from_secs(1);
        }
        let second = r.read(&reading(3000.0, 30.0, 200.0));
        let down = second.wan_down_bps.unwrap();
        assert!((1900.0..=2000.0).contains(&down), "{down}");
        assert!((second.cpu_pct.unwrap() - 20.0).abs() < 0.01);
        assert!(second.ports[0].down_bps.unwrap() > 0.0);
        if let Some((then, _)) = r.last.as_mut() {
            *then -= std::time::Duration::from_secs(1);
        }
        let rebooted = r.read(&reading(10.0, 1.0, 5.0));
        assert_eq!(rebooted.wan_down_bps, None);
        assert_eq!(rebooted.cpu_pct, None);
    }
}

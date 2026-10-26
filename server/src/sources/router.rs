//! The router's own agent (router/ in this repo), as
//! pvestats/sources/router.py: its counters since boot, made into rates
//! from one reading to the next.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::{lock, ROUTER_TIMEOUT};
use crate::config::Config;
use crate::net;
use crate::state::{clock, Source};

pub struct Router {
    config: Arc<Config>,
    http: ureq::Agent,
    rates: Rates,
}

/// The last reading's counters, by name, and when it was taken.
#[derive(Default)]
struct Rates {
    last: Option<(f64, HashMap<String, f64>)>,
}

impl Router {
    pub fn new(config: Arc<Config>) -> Self {
        // Plain HTTP on the LAN; the TLS settings go unused.
        Router { config, http: net::http(net::home_ca_tls(), ROUTER_TIMEOUT), rates: Rates::default() }
    }

    pub fn poll(&mut self, source: &Mutex<Source>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.config.router_url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })?;
        let data = self.rates.read(&payload);
        let mut s = lock(source);
        s.push("wan.down", data.get("wan_down_bps").cloned());
        s.push("wan.up", data.get("wan_up_bps").cloned());
        s.succeed(data);
        Ok(())
    }
}

impl Rates {
    /// One reading, with rates against the last one.
    fn read(&mut self, payload: &Value) -> Map<String, Value> {
        let now = clock();
        let last = self.last.take();
        let mut counters: HashMap<String, f64> = HashMap::new();
        // Per second since the last reading; None for the first, or for a
        // counter that went backwards (the router rebooted).
        let mut rate = |key: String, value: Option<&Value>| -> Option<f64> {
            let value = value.filter(|v| v.is_number())?.as_f64()?;
            counters.insert(key.clone(), value);
            let (then, before) = last.as_ref()?;
            let (seconds, previous) = (now - then, *before.get(&key)?);
            (seconds > 0.0 && value >= previous).then(|| (value - previous) / seconds)
        };

        let cpu = object(payload.get("cpu"));
        let busy = rate("cpu.busy".into(), cpu.get("busy"));
        let total = rate("cpu.total".into(), cpu.get("total"));
        let cpu_pct = busy.zip(total.filter(|t| *t != 0.0)).map(|(b, t)| (b / t * 100.0).clamp(0.0, 100.0));
        let wan = object(payload.get("wan"));
        let wan_down_bps = rate("wan.rx".into(), wan.get("rx"));
        let wan_up_bps = rate("wan.tx".into(), wan.get("tx"));
        // A port's or a band's rx is what its devices sent: up, from them.
        let ports: Vec<Value> = list(payload.get("ports"))
            .iter()
            .map(|p| {
                let p = object(Some(p));
                let name = text(p.get("name")).unwrap_or("?");
                json!({
                    "name": name,
                    "up": p.get("up") == Some(&Value::Bool(true)),
                    "wan": p.get("wan") == Some(&Value::Bool(true)),
                    "down_bps": rate(format!("port.{name}.tx"), p.get("tx")),
                    "up_bps": rate(format!("port.{name}.rx"), p.get("rx")),
                })
            })
            .collect();
        let bands: Vec<Value> = list(payload.get("bands"))
            .iter()
            .map(|b| {
                let b = object(Some(b));
                let name = text(b.get("name")).unwrap_or("?");
                json!({
                    "name": name,
                    "on": b.get("on") == Some(&Value::Bool(true)),
                    "devices": number(b.get("devices")),
                    "down_bps": rate(format!("band.{name}.tx"), b.get("tx")),
                    "up_bps": rate(format!("band.{name}.rx"), b.get("rx")),
                })
            })
            .collect();
        let mem = object(payload.get("mem"));
        let (mem_total, available) = (number(mem.get("total")), number(mem.get("available")));
        // As Python's max(total - available, 0): whole numbers stay whole.
        let mem_used = match (&mem_total, &available) {
            (Value::Number(t), Value::Number(a)) => match (t.as_i64(), a.as_i64()) {
                (Some(t), Some(a)) => json!((t - a).max(0)),
                _ => json!((t.as_f64().unwrap_or(0.0) - a.as_f64().unwrap_or(0.0)).max(0.0)),
            },
            _ => Value::Null,
        };

        let mut data = Map::new();
        data.insert("model".into(), json!(text(payload.get("model")).unwrap_or("")));
        data.insert("uptime".into(), number(payload.get("uptime")));
        data.insert("cpu_pct".into(), json!(cpu_pct));
        data.insert("mem_used".into(), mem_used);
        data.insert("mem_total".into(), mem_total);
        data.insert("temp".into(), number(object(payload.get("temp")).get("cpu")));
        data.insert("wan_up".into(), json!(wan.get("up") == Some(&Value::Bool(true))));
        data.insert("wan_ip".into(), json!(text(wan.get("ip")).unwrap_or("")));
        data.insert("wan_down_bps".into(), json!(wan_down_bps));
        data.insert("wan_up_bps".into(), json!(wan_up_bps));
        data.insert("ports".into(), Value::Array(ports));
        data.insert("wired_devices".into(), number(payload.get("wired_devices")));
        data.insert("bands".into(), Value::Array(bands));
        self.last = Some((now, counters));
        data
    }
}

/// A JSON number as it came, or null.
fn number(v: Option<&Value>) -> Value {
    v.filter(|v| v.is_number()).cloned().unwrap_or(Value::Null)
}

/// An object, or null, whose fields then all read as missing.
fn object(v: Option<&Value>) -> &Value {
    static NONE: Value = Value::Null;
    v.filter(|v| v.is_object()).unwrap_or(&NONE)
}

fn text(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

fn list(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_from_one_reading_to_the_next() {
        let mut router = Rates::default();
        let reading = |rx: u64, busy: u64| {
            json!({"model": "GT-BE98_PRO", "uptime": 100, "cpu": {"busy": busy, "total": 1000 + 2 * busy},
                   "mem": {"total": 1000, "available": 400}, "temp": {"cpu": 69.5},
                   "wan": {"up": true, "ip": "192.0.2.1", "rx": rx, "tx": 10},
                   "ports": [{"name": "eth1", "up": true, "wan": true, "rx": rx, "tx": 5}],
                   "wired_devices": 8,
                   "bands": [{"name": "5 GHz", "on": true, "devices": 2, "rx": 1, "tx": rx}]})
        };
        let first = router.read(&reading(1000, 0));
        assert_eq!((first["wan_down_bps"].clone(), first["cpu_pct"].clone()), (Value::Null, Value::Null));
        assert_eq!((first["mem_used"].clone(), first["mem_total"].clone()), (json!(600), json!(1000)));
        assert_eq!((first["temp"].clone(), first["wired_devices"].clone()), (json!(69.5), json!(8)));
        // A second later by the router's own counters: rates appear.
        router.last.as_mut().unwrap().0 -= 1.0;
        let second = router.read(&reading(3000, 500));
        let near = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() / want < 0.01;
        assert!(near(&second["wan_down_bps"], 2000.0), "{second:?}");
        assert!(near(&second["bands"][0]["down_bps"], 2000.0), "{second:?}");
        assert!(near(&second["cpu_pct"], 50.0), "{second:?}");
        assert_eq!(second["ports"][0]["wan"], json!(true));
        // A counter that went backwards (the router rebooted) is no rate.
        let third = router.read(&reading(10, 600));
        assert_eq!(third["wan_down_bps"], Value::Null);
        assert_eq!(router.read(&json!("nonsense"))["ports"], json!([]));
    }
}

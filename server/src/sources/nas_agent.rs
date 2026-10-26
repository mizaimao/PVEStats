//! TrueNAS's own agent (nas/ in this repo), as pvestats/sources/nas_agent.py
//! and the terminal dashboard's tui/src/sources/nas_agent.rs: its counters
//! since boot, made into rates from one reading to the next, timed by
//! TrueNAS's uptime. The TrueNAS card's only source: TrueNAS's API cost it
//! 25 ms of CPU a request, the agent 0.41 ms (nas/README.md).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::{lock, NAS_AGENT_TIMEOUT};
use crate::config::Config;
use crate::net;
use crate::state::{RateTracker, Source};

/// A pool's capacity is read every 30 s; figures older than this mean the
/// agent's last reads of it failed.
const OLD_POOL_SECONDS: f64 = 120.0;

pub struct NasAgent {
    config: Arc<Config>,
    http: ureq::Agent,
    /// Counters by name ("cpu.busy", "nic.ens18.rx", "disk.sdb.read").
    /// Disks and NICs come and go too rarely to need pruning.
    rates: RateTracker,
}

impl NasAgent {
    pub fn new(config: Arc<Config>) -> Self {
        // Plain HTTP on the LAN; the TLS settings go unused.
        NasAgent { config, http: net::http(net::home_ca_tls(), NAS_AGENT_TIMEOUT), rates: RateTracker::default() }
    }

    pub fn poll(&mut self, source: &Mutex<Source>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.config.nas_agent_url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })?;
        let data = read(&mut self.rates, &payload);
        let mut s = lock(source);
        if let Some(interfaces) = data.get("interfaces").and_then(Value::as_object) {
            for (name, nic) in interfaces {
                s.push(&format!("if.{name}.rx"), nic.get("rx_bps").cloned());
                s.push(&format!("if.{name}.tx"), nic.get("tx_bps").cloned());
            }
        }
        s.succeed(data);
        Ok(())
    }
}

/// One reading, with rates against the last one, in the shape the page's
/// NAS card reads. The pool TrueNAS runs from, and its disk, are left off.
fn read(rates: &mut RateTracker, payload: &Value) -> Map<String, Value> {
    let uptime = num(payload.get("uptime"));
    // None without TrueNAS's clock, and for a first reading.
    let mut rate = |key: String, value: Option<f64>| uptime.and_then(|now| rates.rate(&key, value, now));

    let cpu = object(payload.get("cpu"));
    let busy = rate("cpu.busy".into(), num(cpu.get("busy")));
    let total = rate("cpu.total".into(), num(cpu.get("total")));
    let cpu_pct = busy.zip(total).filter(|(_, t)| *t > 0.0).map(|(b, t)| (b / t * 100.0).clamp(0.0, 100.0));
    let mem = object(payload.get("mem"));
    // The agent sends 0 when it could not read /proc/meminfo.
    let mem_total = num(mem.get("total")).filter(|t| *t > 0.0);
    let mem_used = mem_total.zip(num(mem.get("available"))).map(|(t, a)| (t - a).max(0.0));

    let mut interfaces = Map::new();
    for n in list(payload.get("nics")) {
        let Some(name) = text(n.get("name")) else { continue };
        let driver = text(n.get("driver")).unwrap_or("");
        // Proxmox's virtual NICs are virtio; any other driver is a real card
        // passed through, and names it.
        let role = match driver {
            "virtio_net" => "virtual",
            "" => "unknown",
            _ => "physical",
        };
        interfaces.insert(
            name.to_string(),
            json!({
                "rx_bps": rate(format!("nic.{name}.rx"), num(n.get("rx"))),
                "tx_bps": rate(format!("nic.{name}.tx"), num(n.get("tx"))),
                "role": role,
                "media": if role == "physical" { driver } else { "" },
            }),
        );
    }

    let boot: Vec<&str> = list(payload.get("pools"))
        .iter()
        .filter(|p| p.get("boot") == Some(&Value::Bool(true)))
        .filter_map(|p| text(p.get("name")))
        .collect();
    let mut disks: Vec<(String, Value)> = list(payload.get("disks"))
        .iter()
        .filter(|d| !text(d.get("pool")).is_some_and(|p| boot.contains(&p)))
        .filter_map(|d| {
            let name = text(d.get("name"))?;
            Some((
                name.to_string(),
                json!({
                    "read_bps": rate(format!("disk.{name}.read"), num(d.get("read"))),
                    "write_bps": rate(format!("disk.{name}.write"), num(d.get("write"))),
                    "size": num(d.get("size")),
                }),
            ))
        })
        .collect();
    // sdb..sdf in the order the drives are labelled.
    disks.sort_by(|a, b| a.0.cmp(&b.0));

    // Capacity as its root dataset counts it (used + available), what
    // TrueNAS's Storage page shows; ZFS's kstat state word as its status.
    let pools: Vec<Value> = list(payload.get("pools"))
        .iter()
        .filter(|p| p.get("boot") != Some(&Value::Bool(true)))
        .map(|p| {
            let state = text(p.get("state"));
            let used = num(p.get("used"));
            json!({
                "name": text(p.get("name")).unwrap_or("?"),
                "status": state,
                "healthy": state == Some("ONLINE"),
                "used_bytes": used,
                "usable": used.zip(num(p.get("available"))).map(|(u, a)| u + a),
                "old": num(p.get("age")).is_some_and(|a| a > OLD_POOL_SECONDS),
            })
        })
        .collect();

    let mut data = Map::new();
    data.insert("interfaces".into(), Value::Object(interfaces));
    data.insert("disks".into(), Value::Object(disks.into_iter().collect()));
    data.insert("pools".into(), Value::Array(pools));
    data.insert(
        "inside".into(),
        json!({"cpu_pct": cpu_pct, "mem_used": mem_used, "mem_total": mem_total, "uptime": uptime,
               "ip": text(payload.get("ip"))}),
    );
    data
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
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

    /// The agent's answer, as nas/agent writes it.
    fn reply(uptime: f64, busy: u64, total: u64, rx: u64, read: u64) -> Value {
        json!({"uptime": uptime, "cpus": 4, "cpu": {"busy": busy, "total": total},
               "mem": {"total": 1000, "available": 250, "arc": 600}, "ip": "10.10.10.2",
               "nics": [{"name": "ens18", "driver": "virtio_net", "rx": rx, "tx": 0},
                        {"name": "enp1s0", "driver": "ixgbe", "rx": 0, "tx": 0}],
               "disks": [{"name": "sdc", "model": "M", "size": 16, "pool": null, "read": 0, "write": 0},
                         {"name": "sdb", "model": "M", "size": 16, "pool": "tank", "read": read, "write": 0},
                         {"name": "sda", "model": "B", "size": 8, "pool": "boot-pool", "read": 0, "write": 0}],
               "pools": [{"name": "tank", "state": "ONLINE", "used": 30, "available": 70, "age": 2, "boot": false},
                         {"name": "boot-pool", "state": "ONLINE", "used": 1, "available": 7, "age": 2, "boot": true},
                         {"name": "old", "state": "DEGRADED", "used": 5, "available": 5, "age": 300, "boot": false}]})
    }

    #[test]
    fn rates_from_one_reading_to_the_next() {
        let mut rates = RateTracker::default();
        let first = read(&mut rates, &reply(100.0, 10, 100, 0, 0));
        assert_eq!(first["inside"], json!({"cpu_pct": null, "mem_used": 750.0, "mem_total": 1000.0,
                                            "uptime": 100.0, "ip": "10.10.10.2"}));
        assert_eq!(first["interfaces"]["ens18"]["rx_bps"], Value::Null);
        // Two seconds on by TrueNAS's clock: 50 of 200 jiffies busy is 25%.
        let second = read(&mut rates, &reply(102.0, 60, 300, 4000, 2048));
        assert_eq!(second["inside"]["cpu_pct"], json!(25.0));
        assert_eq!(second["interfaces"]["ens18"]["rx_bps"], json!(2000.0));
        assert_eq!(second["disks"]["sdb"]["read_bps"], json!(1024.0));
        // TrueNAS started again: its clock went back, so no rates.
        let third = read(&mut rates, &reply(5.0, 1, 10, 10, 10));
        assert_eq!((third["inside"]["cpu_pct"].clone(), third["interfaces"]["ens18"]["rx_bps"].clone()), (Value::Null, Value::Null));
    }

    #[test]
    fn the_boot_pool_and_its_disk_left_off() {
        let data = read(&mut RateTracker::default(), &reply(100.0, 10, 100, 0, 0));
        let disks: Vec<&String> = data["disks"].as_object().unwrap().keys().collect();
        assert_eq!(disks, ["sdb", "sdc"]);
        assert_eq!(data["pools"], json!([
            {"name": "tank", "status": "ONLINE", "healthy": true, "used_bytes": 30.0, "usable": 100.0, "old": false},
            {"name": "old", "status": "DEGRADED", "healthy": false, "used_bytes": 5.0, "usable": 10.0, "old": true},
        ]));
        assert_eq!(data["interfaces"]["enp1s0"], json!({"rx_bps": null, "tx_bps": null, "role": "physical", "media": "ixgbe"}));
        assert_eq!(data["interfaces"]["ens18"]["role"], json!("virtual"));
    }
}

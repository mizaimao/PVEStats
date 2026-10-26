//! TrueNAS's own agent (nas/ in this repo), on TrueNAS's LAN address: its
//! counters since boot, made into rates here from one reading to the next.
//! While it is configured the TrueNAS card reads nothing else: not TrueNAS's
//! API, which costs TrueNAS 25 ms of CPU a request against the agent's
//! 0.41 ms (nas/README.md), and not PVE.

use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::{lock, Disk, NasData, NasInside, Nic, Pool, NAS_AGENT_TIMEOUT};
use crate::json::{list, num, text, truthy};
use crate::net;
use crate::state::{RateTracker, Source};

pub struct NasAgent {
    url: String,
    http: ureq::Agent,
    /// Counters by name ("cpu.busy", "nic.ens18.rx", "disk.sdb.read"),
    /// timed by TrueNAS's uptime, which comes in the same reading, so a slow
    /// reply does not skew the rates. Disks and NICs come and go too rarely
    /// to need pruning.
    rates: RateTracker,
}

impl NasAgent {
    pub fn new(url: String) -> Self {
        // Plain HTTP on the LAN; the TLS settings go unused.
        NasAgent { url, http: net::http(net::home_ca_tls(), NAS_AGENT_TIMEOUT), rates: RateTracker::default() }
    }

    pub fn poll(&mut self, source: &Mutex<Source<NasData>>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })?;
        let data = self.read(&payload);
        let mut source = lock(source);
        for (name, nic) in &data.interfaces {
            source.push(&format!("if.{name}.rx"), nic.rx_bps);
            source.push(&format!("if.{name}.tx"), nic.tx_bps);
        }
        source.succeed(data);
        Ok(())
    }

    /// One reading, with rates against the last one. The pool TrueNAS runs
    /// from, and its disk, are left off, as TrueNAS's API leaves them off.
    fn read(&mut self, payload: &Value) -> NasData {
        let uptime = num(payload, "uptime");
        let rates = &mut self.rates;
        // None without TrueNAS's clock, and for a first reading.
        let mut rate = |key: String, value: Option<f64>| uptime.and_then(|now| rates.rate(&key, value, now));

        let cpu = payload.get("cpu").cloned().unwrap_or_default();
        let busy = rate("cpu.busy".into(), num(&cpu, "busy"));
        let total = rate("cpu.total".into(), num(&cpu, "total"));
        let cpu_pct = busy.zip(total).filter(|(_, t)| *t > 0.0).map(|(b, t)| (b / t * 100.0).clamp(0.0, 100.0));
        let mem = payload.get("mem").cloned().unwrap_or_default();
        // The agent sends 0 when it could not read /proc/meminfo.
        let mem_total = num(&mem, "total").filter(|t| *t > 0.0);
        let mem_used = mem_total.zip(num(&mem, "available")).map(|(t, a)| (t - a).max(0.0));

        let interfaces = list(payload, "nics")
            .iter()
            .filter_map(|n| {
                let name = text(n, "name")?.to_string();
                let driver = text(n, "driver").unwrap_or("");
                // Proxmox's virtual NICs are virtio; any other driver is a
                // real card passed through, and names it.
                let role = match driver {
                    "virtio_net" => "virtual",
                    "" => "unknown",
                    _ => "physical",
                };
                let nic = Nic {
                    rx_bps: rate(format!("nic.{name}.rx"), num(n, "rx")),
                    tx_bps: rate(format!("nic.{name}.tx"), num(n, "tx")),
                    role: Some(role.into()),
                    media: if role == "physical" { driver.to_string() } else { String::new() },
                };
                Some((name, nic))
            })
            .collect();

        let boot: Vec<&str> =
            list(payload, "pools").iter().filter(|p| truthy(p.get("boot"))).filter_map(|p| text(p, "name")).collect();
        let mut disks: Vec<(String, Disk)> = list(payload, "disks")
            .iter()
            .filter(|d| !text(d, "pool").is_some_and(|p| boot.contains(&p)))
            .filter_map(|d| {
                let name = text(d, "name")?.to_string();
                let disk = Disk {
                    read_bps: rate(format!("disk.{name}.read"), num(d, "read")),
                    write_bps: rate(format!("disk.{name}.write"), num(d, "write")),
                    size: num(d, "size"),
                };
                Some((name, disk))
            })
            .collect();
        disks.sort_by(|a, b| a.0.cmp(&b.0));

        // Capacity as its root dataset counts it (used + available), what
        // TrueNAS's Storage page shows; ZFS's kstat state word as its status.
        let pools = list(payload, "pools")
            .iter()
            .filter(|p| !truthy(p.get("boot")))
            .map(|p| {
                let state = text(p, "state").map(str::to_string);
                let used = num(p, "used");
                Pool {
                    name: text(p, "name").unwrap_or("?").to_string(),
                    healthy: state.as_deref() == Some("ONLINE"),
                    status: state,
                    size: None,
                    allocated: None,
                    used_bytes: used,
                    usable: used.zip(num(p, "available")).map(|(u, a)| u + a),
                }
            })
            .collect();

        NasData {
            interfaces,
            disks,
            pools,
            inside: Some(NasInside { cpu_pct, mem_used, mem_total, uptime, ip: text(payload, "ip").map(str::to_string) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent() -> NasAgent {
        NasAgent { url: String::new(), http: ureq::AgentBuilder::new().build(), rates: RateTracker::default() }
    }

    /// The agent's answer, as nas/agent writes it.
    fn reply(uptime: f64, busy: u64, total: u64, rx: u64, read: u64) -> Value {
        json!({"uptime": uptime, "cpus": 4, "cpu": {"busy": busy, "total": total},
               "mem": {"total": 1000, "available": 250, "arc": 600}, "ip": "10.10.10.2",
               "nics": [{"name": "ens18", "driver": "virtio_net", "rx": rx, "tx": 0},
                        {"name": "enp1s0", "driver": "ixgbe", "rx": 0, "tx": 0}],
               "disks": [{"name": "sdb", "model": "M", "size": 16, "pool": "tank", "read": read, "write": 0},
                         {"name": "sda", "model": "B", "size": 8, "pool": "boot-pool", "read": 0, "write": 0},
                         {"name": "sdc", "model": "M", "size": 16, "pool": null, "read": 0, "write": 0}],
               "pools": [{"name": "tank", "state": "ONLINE", "used": 30, "available": 70, "age": 2, "boot": false},
                         {"name": "boot-pool", "state": "ONLINE", "used": 1, "available": 7, "age": 2, "boot": true},
                         {"name": "old", "state": "DEGRADED", "used": 5, "available": 5, "age": 2, "boot": false}]})
    }

    #[test]
    fn rates_from_one_reading_to_the_next() {
        let mut a = agent();
        let first = a.read(&reply(100.0, 10, 100, 0, 0));
        let inside = first.inside.unwrap();
        assert_eq!((inside.cpu_pct, inside.mem_used, inside.mem_total), (None, Some(750.0), Some(1000.0)));
        assert_eq!((inside.uptime, inside.ip.as_deref()), (Some(100.0), Some("10.10.10.2")));
        assert_eq!((first.interfaces[0].1.rx_bps, first.disks[0].1.read_bps), (None, None));
        // Two seconds on by TrueNAS's clock: 50 of 200 jiffies busy is 25%.
        let second = a.read(&reply(102.0, 60, 300, 4000, 2048));
        assert_eq!(second.inside.unwrap().cpu_pct, Some(25.0));
        assert_eq!((second.interfaces[0].1.rx_bps, second.disks[0].1.read_bps), (Some(2000.0), Some(1024.0)));
        // TrueNAS started again: its clock went back, so no rates.
        let third = a.read(&reply(5.0, 1, 10, 10, 10));
        assert_eq!((third.inside.unwrap().cpu_pct, third.interfaces[0].1.rx_bps), (None, None));
    }

    #[test]
    fn the_boot_pool_and_its_disk_left_off() {
        let data = agent().read(&reply(100.0, 10, 100, 0, 0));
        let disks: Vec<&str> = data.disks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(disks, ["sdb", "sdc"]);
        let pools: Vec<(&str, bool, Option<f64>, Option<f64>)> =
            data.pools.iter().map(|p| (p.name.as_str(), p.healthy, p.used_bytes, p.usable)).collect();
        assert_eq!(pools, [("tank", true, Some(30.0), Some(100.0)), ("old", false, Some(5.0), Some(10.0))]);
        assert_eq!(data.pools[1].status.as_deref(), Some("DEGRADED"));
        let nics: Vec<(&str, Option<&str>, &str)> =
            data.interfaces.iter().map(|(n, nic)| (n.as_str(), nic.role.as_deref(), nic.media.as_str())).collect();
        assert_eq!(nics, [("ens18", Some("virtual"), ""), ("enp1s0", Some("physical"), "ixgbe")]);
    }
}

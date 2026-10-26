//! The Proxmox host's own agent (pve/agent): the host's and each running
//! VM's counters, read from the kernel on the host, made into rates here
//! from one reading to the next. While it answers, its figures stand in for
//! PVE's live ones (ui::overlay), every 2 s instead of every 10, and PVE is
//! asked only for what the agent cannot tell.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::{lock, HostData, HostGuest, PveData, HOST_TIMEOUT};
use crate::json::{int, list, num};
use crate::net;
use crate::state::Source;

pub struct Host {
    url: String,
    http: ureq::Agent,
    last: Option<Reading>,
}

/// One reading's counters: the host's uptime (the clock its rates are
/// timed by) and CPU jiffies, and each VM's CPU microseconds and bytes in
/// and out.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reading {
    uptime: Option<f64>,
    cpu: Option<(f64, f64)>,
    vms: HashMap<u64, Vm>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Vm {
    cpu_usec: Option<f64>,
    netin: f64,
    netout: f64,
}

impl Host {
    pub fn new(url: String) -> Self {
        // Plain HTTP on the LAN; the TLS settings go unused.
        Host { url, http: net::http(net::home_ca_tls(), HOST_TIMEOUT), last: None }
    }

    pub fn poll(&mut self, source: &Mutex<Source<HostData>>, pve: &Mutex<Source<PveData>>) -> Result<(), String> {
        let started = Instant::now();
        let payload: Value = self
            .http
            .get(&self.url)
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })?;
        // Each VM's vCPUs, from PVE: the host sees only CPU time.
        let vcpus: HashMap<u64, f64> = lock(pve)
            .data
            .as_ref()
            .map(|d| d.guests.iter().filter_map(|(v, g)| Some((*v, g.maxcpu?))).collect())
            .unwrap_or_default();
        let (data, samples) = self.read(&payload, &vcpus);
        let mut source = lock(source);
        for (key, value) in samples {
            source.push(&key, value);
        }
        source.succeed(data);
        Ok(())
    }

    /// One reading, with rates against the last one, and the samples for the
    /// charts: the host's CPU and memory, and each VM's CPU.
    fn read(&mut self, payload: &Value, vcpus: &HashMap<u64, f64>) -> (HostData, Vec<(String, Option<f64>)>) {
        let cpu = payload.get("cpu").cloned().unwrap_or_default();
        let mem = payload.get("mem").cloned().unwrap_or_default();
        let now = Reading {
            uptime: num(payload, "uptime"),
            cpu: num(&cpu, "busy").zip(num(&cpu, "total")),
            vms: list(payload, "guests")
                .iter()
                .filter_map(|g| {
                    let vm = Vm { cpu_usec: num(g, "cpu_usec"), netin: num(g, "netin")?, netout: num(g, "netout")? };
                    Some((int(g, "vmid")?, vm))
                })
                .collect(),
        };
        let last = self.last.replace(now.clone());
        // The time between the readings by the host's own clock, so a slow
        // reply does not skew the rates.
        let seconds = last.as_ref().and_then(|l| Some(now.uptime? - l.uptime?)).filter(|s| *s > 0.0);
        let cpu_pct = match (last.as_ref().and_then(|l| l.cpu), now.cpu) {
            (Some((b0, t0)), Some((b1, t1))) if t1 > t0 && b1 >= b0 => Some(((b1 - b0) / (t1 - t0) * 100.0).clamp(0.0, 100.0)),
            _ => None,
        };
        let total = num(&mem, "total");
        let used = total.zip(num(&mem, "available")).map(|(t, a)| (t - a).max(0.0));

        let mut samples = vec![
            ("node.cpu".to_string(), cpu_pct),
            ("node.mem_pct".to_string(), used.zip(total).filter(|(_, t)| *t > 0.0).map(|(u, t)| u / t * 100.0)),
        ];
        let mut guests = BTreeMap::new();
        for (vmid, vm) in &now.vms {
            let before = last.as_ref().and_then(|l| l.vms.get(vmid));
            // A counter that went backwards is a VM started again (a new
            // QEMU, new tap devices): no rate rather than a spike.
            let rate = |a: f64, b: f64| seconds.filter(|_| b >= a).map(|s| (b - a) / s);
            // CPU time over the VM's vCPUs, as PVE counts it.
            let cpu_pct = before.and_then(|b| {
                let (u0, u1, s) = (b.cpu_usec?, vm.cpu_usec?, seconds?);
                let cores = vcpus.get(vmid).copied().filter(|c| *c > 0.0)?;
                (u1 >= u0).then(|| ((u1 - u0) / 1e6 / s / cores * 100.0).clamp(0.0, 100.0))
            });
            samples.push((format!("cpu.{vmid}"), cpu_pct));
            let guest = list(payload, "guests").iter().find(|g| int(g, "vmid") == Some(*vmid));
            guests.insert(
                *vmid,
                HostGuest {
                    cpu_pct,
                    net_in_bps: before.and_then(|b| rate(b.netin, vm.netin)),
                    net_out_bps: before.and_then(|b| rate(b.netout, vm.netout)),
                    uptime: guest.and_then(|g| num(g, "uptime")),
                },
            );
        }
        let data = HostData {
            cpu_pct,
            mem: used,
            maxmem: total,
            loadavg: payload.get("load").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_f64).collect()).unwrap_or_default(),
            uptime: now.uptime,
            cpus: int(payload, "cpus").filter(|c| *c > 0),
            guests,
        };
        (data, samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host() -> Host {
        Host { url: String::new(), http: ureq::AgentBuilder::new().build(), last: None }
    }

    /// The agent's answer, as pve/agent writes it.
    fn reply(uptime: f64, busy: u64, total: u64, usec: u64, netin: u64) -> Value {
        json!({"uptime": uptime, "cpus": 24, "cpu": {"busy": busy, "total": total},
               "mem": {"total": 1000, "available": 250}, "load": [0.52, 0.58, 0.59],
               "guests": [{"vmid": 100, "cpu_usec": usec, "mem": 5, "netin": netin, "netout": 7, "uptime": 99.0}]})
    }

    #[test]
    fn rates_from_one_reading_to_the_next() {
        let mut h = host();
        let vcpus = HashMap::from([(100, 4.0)]);
        let (first, samples) = h.read(&reply(500.0, 100, 1000, 1_000_000, 0), &vcpus);
        assert_eq!((first.cpu_pct, first.guests[&100].cpu_pct, first.guests[&100].net_in_bps), (None, None, None));
        assert_eq!((first.mem, first.maxmem, first.cpus), (Some(750.0), Some(1000.0), Some(24)));
        assert_eq!(first.loadavg, [0.52, 0.58, 0.59]);
        assert!(samples.contains(&("node.mem_pct".to_string(), Some(75.0))));
        // Two seconds on by the host's clock: 4 s of CPU over 4 vCPUs is 50%.
        let (second, samples) = h.read(&reply(502.0, 150, 1100, 5_000_000, 6000), &vcpus);
        assert_eq!(second.cpu_pct, Some(50.0));
        assert_eq!(second.guests[&100].cpu_pct, Some(50.0));
        assert_eq!(second.guests[&100].net_in_bps, Some(3000.0));
        assert_eq!(second.guests[&100].uptime, Some(99.0));
        assert!(samples.contains(&("cpu.100".to_string(), Some(50.0))));
        // Without the VM's vCPUs from PVE, no CPU figure for it.
        let (third, _) = h.read(&reply(504.0, 200, 1200, 9_000_000, 9000), &HashMap::new());
        assert_eq!(third.guests[&100].cpu_pct, None);
        // The VM started again: its counters went back, so no rate.
        let (fourth, _) = h.read(&reply(506.0, 250, 1300, 10, 5), &vcpus);
        assert_eq!((fourth.guests[&100].cpu_pct, fourth.guests[&100].net_in_bps), (None, None));
    }
}

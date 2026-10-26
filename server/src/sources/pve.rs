//! Proxmox VE: guest inventory, host status, and guest-agent addresses and
//! filesystems, as pvestats/sources/pve.py.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::{lock, PVE_TIMEOUT};
use crate::config::Config;
use crate::net;
use crate::state::{RateTracker, Source};

/// Guest addresses barely change and each lookup is a call per guest: asked
/// once, then when a guest is new or rebooted, with this backstop for a
/// DHCP lease change on a guest that never restarts.
const IP_BACKSTOP_EVERY: i64 = 90; // x PVE_INTERVAL = every 15 minutes
/// Filesystem usage moves slowly: all guests on this cycle.
const FS_REFRESH_EVERY: i64 = 30; // x PVE_INTERVAL = every 5 minutes
/// A guest whose agent has not answered is asked again at this pace, not on
/// every poll: each call is a line in pveproxy's log.
const MISSING_RETRY_EVERY: u64 = 6; // x PVE_INTERVAL = every minute
/// Pseudo-filesystems that would crowd out the real ones.
const SKIP_FS: [&str; 6] = ["tmpfs", "devtmpfs", "overlay", "squashfs", "efivarfs", "ramfs"];
/// Passed-through storage reports a handful of bytes since boot while doing
/// real work: under this after an hour, the host cannot see its disk.
const DISK_IO_FLOOR: f64 = 16.0 * 1024.0 * 1024.0;

/// Virtual and container interfaces the guest agent reports beside the real
/// one, by name: sturdier than guessing at address ranges.
fn virtual_nic(name: &str) -> bool {
    ["lo", "docker", "br-", "veth", "virbr", "cni", "kube", "flannel", "tap"].iter().any(|p| name.starts_with(p))
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn data(reply: Value) -> Value {
    reply.get("data").cloned().unwrap_or(Value::Null)
}

fn disk_io_visible(guest: &Value) -> bool {
    let uptime = num(guest, "uptime").unwrap_or(0.0);
    let total = num(guest, "diskread").unwrap_or(0.0) + num(guest, "diskwrite").unwrap_or(0.0);
    uptime < 3600.0 || total >= DISK_IO_FLOOR
}

pub struct Pve {
    config: Arc<Config>,
    http: ureq::Agent,
    rates: RateTracker,
    node: Option<String>,
    ips: HashMap<u64, Vec<String>>,
    seen_uptime: HashMap<u64, f64>,
    backstop: i64,
    filesystems: HashMap<u64, Value>,
    fs_countdown: i64,
    polls: u64,
    ip_retry: HashMap<u64, u64>,
    fs_retry: HashMap<u64, u64>,
}

impl Pve {
    pub fn new(config: Arc<Config>) -> Self {
        // The PVE certificate is self-signed. Not a thing to fix (spec §3.1).
        let http = net::http(net::self_signed_tls(), PVE_TIMEOUT);
        let node = config.pve_node.clone();
        Pve {
            config,
            http,
            rates: RateTracker::default(),
            node,
            ips: HashMap::new(),
            seen_uptime: HashMap::new(),
            backstop: 0,
            filesystems: HashMap::new(),
            fs_countdown: 0,
            polls: 0,
            ip_retry: HashMap::new(),
            fs_retry: HashMap::new(),
        }
    }

    fn get(&self, path: &str, started: Instant) -> Result<Value, String> {
        self.http
            .get(&format!("{}{path}", self.config.pve_base()))
            .set("Authorization", &self.config.pve_auth_header())
            .call()
            .map_err(|e| net::describe(e, started))?
            .into_json()
            .map_err(|e| if net::is_timeout(&e) { net::timed_out(started) } else { format!("unreadable reply: {e}") })
    }

    /// One guest-agent call for each guest, all at once; best effort, so a
    /// guest without an agent just has no answer.
    fn ask_agents(&self, vmids: &[u64], command: &str) -> Vec<(u64, Value)> {
        let node = self.node.clone().unwrap_or_default();
        thread::scope(|scope| {
            let calls: Vec<_> = vmids
                .iter()
                .map(|&vmid| {
                    let path = format!("/nodes/{node}/qemu/{vmid}/agent/{command}");
                    scope.spawn(move || (vmid, self.get(&path, Instant::now()).ok().map(data)))
                })
                .collect();
            calls.into_iter().filter_map(|c| c.join().ok()).filter_map(|(v, d)| d.map(|d| (v, d))).collect()
        })
    }

    fn refresh_ips(&mut self, guests: &[&Value]) {
        let vmids = qemu_vmids(guests);
        for (vmid, payload) in self.ask_agents(&vmids, "network-get-interfaces") {
            let mut found = Vec::new();
            for nic in payload.get("result").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
                if virtual_nic(nic.get("name").and_then(Value::as_str).unwrap_or("")) {
                    continue;
                }
                for entry in nic.get("ip-addresses").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
                    let address = entry.get("ip-address").and_then(Value::as_str).unwrap_or("");
                    if entry.get("ip-address-type").and_then(Value::as_str) == Some("ipv4")
                        && !address.is_empty()
                        && !address.starts_with("127.")
                    {
                        found.push(address.to_string());
                    }
                }
            }
            if !found.is_empty() {
                self.ips.insert(vmid, found);
            }
        }
    }

    /// In-guest filesystem usage: /cluster/resources reports 0 for every
    /// VM's disk. The largest real filesystem stands in for the guest's.
    fn refresh_filesystems(&mut self, guests: &[&Value]) {
        let vmids = qemu_vmids(guests);
        let answers = self.ask_agents(&vmids, "get-fsinfo");
        for (vmid, payload) in answers {
            let mut best: Option<(f64, Value)> = None;
            for fs in payload.get("result").and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
                let total = num(fs, "total-bytes").filter(|t| *t != 0.0);
                let kind = fs.get("type").and_then(Value::as_str).unwrap_or("");
                let Some(total) = total else { continue };
                if SKIP_FS.contains(&kind) {
                    continue;
                }
                if best.as_ref().map_or(true, |(t, _)| total > *t) {
                    let entry = json!({"used": fs.get("used-bytes"), "total": fs.get("total-bytes"), "mount": fs.get("mountpoint")});
                    best = Some((total, entry));
                }
            }
            match best {
                Some((_, entry)) => self.filesystems.insert(vmid, entry),
                None => self.filesystems.remove(&vmid),
            };
        }
    }

    pub fn poll(&mut self, source: &Mutex<Source>) -> Result<(), String> {
        let started = Instant::now();
        if self.node.is_none() {
            let nodes = data(self.get("/nodes", started)?);
            let first = nodes.as_array().and_then(|n| n.first()).and_then(|n| n.get("node")).and_then(Value::as_str);
            self.node = Some(first.ok_or("no nodes returned")?.to_string());
        }
        let node = self.node.clone().unwrap_or_default();
        let resources = data(self.get("/cluster/resources?type=vm", started)?);
        let resources = resources.as_array().cloned().unwrap_or_default();
        let node_status = data(self.get(&format!("/nodes/{node}/status"), started)?);

        // Running guests only: stopped ones report nothing worth a card.
        let running: Vec<&Value> = resources
            .iter()
            .filter(|g| g.get("status").and_then(Value::as_str) == Some("running") && !truthy(g.get("template")))
            .collect();
        self.polls += 1;
        let polls = self.polls;
        let live: HashSet<u64> = running.iter().filter_map(|g| g.get("vmid").and_then(Value::as_u64)).collect();

        // Forget guests that are no longer running, so a later start re-reads.
        self.ips.retain(|v, _| live.contains(v));
        self.filesystems.retain(|v, _| live.contains(v));
        self.seen_uptime.retain(|v, _| live.contains(v));
        self.ip_retry.retain(|v, _| live.contains(v));
        self.fs_retry.retain(|v, _| live.contains(v));

        // Only ask about guests that need an answer.
        self.backstop -= 1;
        let due = self.backstop <= 0;
        if due {
            self.backstop = IP_BACKSTOP_EVERY;
        }
        let mut ask_ips = Vec::new();
        for &guest in &running {
            let Some(vmid) = guest.get("vmid").and_then(Value::as_u64) else { continue };
            let uptime = num(guest, "uptime");
            if let (Some(was), Some(now)) = (self.seen_uptime.get(&vmid), uptime) {
                if now < *was {
                    // Rebooted in place: the old answers may not hold.
                    self.ips.remove(&vmid);
                    self.filesystems.remove(&vmid);
                    self.ip_retry.remove(&vmid);
                    self.fs_retry.remove(&vmid);
                }
            }
            if let Some(u) = uptime {
                self.seen_uptime.insert(vmid, u);
            }
            let missing = !self.ips.contains_key(&vmid) && self.ip_retry.get(&vmid).copied().unwrap_or(0) <= polls;
            if due || missing {
                ask_ips.push(guest);
            }
        }
        if !ask_ips.is_empty() {
            self.refresh_ips(&ask_ips);
            for g in &ask_ips {
                if let Some(vmid) = g.get("vmid").and_then(Value::as_u64) {
                    if !self.ips.contains_key(&vmid) {
                        self.ip_retry.insert(vmid, polls + MISSING_RETRY_EVERY);
                    }
                }
            }
        }

        // All on the slow cycle; in between, only guests with no usage yet.
        self.fs_countdown -= 1;
        let ask_fs: Vec<&Value> = if self.fs_countdown <= 0 {
            self.fs_countdown = FS_REFRESH_EVERY;
            running.clone()
        } else {
            running
                .iter()
                .copied()
                .filter(|g| {
                    let vmid = g.get("vmid").and_then(Value::as_u64);
                    kind(g) == "qemu"
                        && vmid.is_some_and(|v| !self.filesystems.contains_key(&v))
                        && vmid.map_or(0, |v| self.fs_retry.get(&v).copied().unwrap_or(0)) <= polls
                })
                .collect()
        };
        if !ask_fs.is_empty() {
            self.refresh_filesystems(&ask_fs);
            for g in &ask_fs {
                if let Some(vmid) = g.get("vmid").and_then(Value::as_u64) {
                    if !self.filesystems.contains_key(&vmid) {
                        self.fs_retry.insert(vmid, polls + MISSING_RETRY_EVERY);
                    }
                }
            }
        }

        let mut guests = Map::new();
        let mut samples = Vec::new();
        for &guest in &running {
            let Some(vmid) = guest.get("vmid").and_then(Value::as_u64) else { continue };
            // The counters are pvestatd's last snapshot, republished every
            // ~10 s; the guest's uptime comes from that same snapshot, so it
            // times them exactly, and an unchanged uptime is a repeat.
            let snapshot = num(guest, "uptime");
            let mut rate = |name: &str| match snapshot {
                Some(t) => self.rates.rate(&format!("{vmid}.{name}"), num(guest, name), t),
                None => None,
            };
            let rates = [rate("netin"), rate("netout"), rate("diskread"), rate("diskwrite")];
            // A container's disk is the real usage of its root filesystem.
            let mut filesystem = self.filesystems.get(&vmid).cloned().unwrap_or(Value::Null);
            if guest.get("type").and_then(Value::as_str) == Some("lxc") && truthy(guest.get("maxdisk")) {
                filesystem = json!({"used": guest.get("disk"), "total": guest.get("maxdisk"), "mount": "/"});
            }
            let cpu_pct = num(guest, "cpu").map(|c| c * 100.0);
            let pass = |k: &str| guest.get(k).cloned().unwrap_or(Value::Null);
            let entry = json!({
                "vmid": vmid,
                "name": pass("name"),
                "type": pass("type"),
                "status": pass("status"),
                "uptime": pass("uptime"),
                "cpu_pct": cpu_pct,
                "maxcpu": pass("maxcpu"),
                "mem": pass("mem"),
                "maxmem": pass("maxmem"),
                "memhost": pass("memhost"),
                "view": self.config.view_for(vmid),
                "addresses": self.ips.get(&vmid).cloned().unwrap_or_default(),
                "filesystem": filesystem,
                "disk_io_visible": disk_io_visible(guest),
                "net_in_bps": rates[0],
                "net_out_bps": rates[1],
                "disk_read_bps": rates[2],
                "disk_write_bps": rates[3],
            });
            samples.push((format!("cpu.{vmid}"), cpu_pct));
            guests.insert(vmid.to_string(), entry);
        }
        // Stop tracking counters for guests that stopped, so a restart does
        // not diff against a months-old sample.
        self.rates.keep_only(&live.iter().map(u64::to_string).collect());

        let memory = node_status.get("memory").cloned().unwrap_or(Value::Null);
        let cpuinfo = node_status.get("cpuinfo").cloned().unwrap_or(Value::Null);
        let node_cpu = num(&node_status, "cpu").map(|c| c * 100.0);
        let loadavg: Vec<f64> = node_status
            .get("loadavg")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => s.trim().parse().ok(),
                other => other.as_f64(),
            })
            .collect();
        let node_entry = json!({
            "name": node,
            "cpu_pct": node_cpu,
            "mem": memory.get("used"),
            "maxmem": memory.get("total"),
            "loadavg": loadavg,
            "uptime": node_status.get("uptime"),
            "kversion": node_status.get("kversion"),
            "pveversion": node_status.get("pveversion"),
            "cores": cpuinfo.get("cores"),
            "cpus": cpuinfo.get("cpus"),
            "guests_running": running.len(),
            "guests_total": resources.iter().filter(|g| !truthy(g.get("template"))).count(),
        });

        let mut s = lock(source);
        for (key, value) in samples {
            s.push(&key, value.map(|v| json!(v)));
        }
        s.push("node.cpu", node_cpu.map(|v| json!(v)));
        let mut result = Map::new();
        result.insert("node".into(), node_entry);
        result.insert("guests".into(), Value::Object(guests));
        s.succeed(result);
        Ok(())
    }

    /// For the history route: where a guest's rrddata lives.
    pub fn rrd_path(node: &str, kind: &str, vmid: u64, timeframe: &str) -> String {
        format!("/nodes/{node}/{kind}/{vmid}/rrddata?timeframe={timeframe}&cf=AVERAGE")
    }
}

fn kind(g: &Value) -> &str {
    g.get("type").and_then(Value::as_str).unwrap_or("qemu")
}

fn qemu_vmids(guests: &[&Value]) -> Vec<u64> {
    guests.iter().filter(|g| kind(g) == "qemu").filter_map(|g| g.get("vmid").and_then(Value::as_u64)).collect()
}

/// Python truthiness for the flags PVE sends as 0/1.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_nics_and_disk_visibility() {
        assert!(virtual_nic("docker0") && virtual_nic("veth12") && !virtual_nic("ens18"));
        assert!(disk_io_visible(&json!({"uptime": 100})), "too early to judge");
        assert!(!disk_io_visible(&json!({"uptime": 7200, "diskread": 10, "diskwrite": 10})));
        assert!(disk_io_visible(&json!({"uptime": 7200, "diskread": 20_000_000})));
        assert!(truthy(Some(&json!(1))) && !truthy(Some(&json!(0))) && !truthy(None));
    }
}

//! Proxmox VE: guest inventory, host status, and guest-agent addresses.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use serde_json::Value;

use super::{lock, Fs, Guest, Node, PveData, PVE_TIMEOUT};
use crate::config::Config;
use crate::json::{int, list, num, text, truthy};
use crate::net;
use crate::state::{RateTracker, Source};

/// Guest addresses barely change, and each lookup is a separate API call per
/// guest. So they are fetched once at startup and then only when a guest
/// needs it: newly seen, or rebooted (uptime went backwards). The backstop
/// catches a DHCP lease change on a guest that never restarts.
const IP_BACKSTOP_EVERY: i64 = 90; // x PVE_INTERVAL = every 15 minutes

/// Filesystem usage also comes from the guest agent, one call per guest. It
/// moves slowly, so it refreshes on its own slow cycle.
const FS_REFRESH_EVERY: i64 = 30; // x PVE_INTERVAL = every 5 minutes

/// A guest whose agent has not answered yet (still booting, or none at all)
/// is asked again at this pace rather than on every poll.
const MISSING_RETRY_EVERY: u64 = 6; // x PVE_INTERVAL = every minute

/// Pseudo-filesystems that would otherwise crowd out the real ones.
const SKIP_FS: [&str; 6] = ["tmpfs", "devtmpfs", "overlay", "squashfs", "efivarfs", "ramfs"];

/// PVE proxies disk IO only for storage it manages. A guest with passed-
/// through storage reports a handful of bytes since boot while doing real
/// work. Anything under this after an hour of uptime is treated as "not
/// visible to the host".
const DISK_IO_FLOOR: f64 = 16.0 * 1024.0 * 1024.0;

/// Virtual and container interfaces the guest agent reports alongside the
/// real one. Filtering by name is sturdier than guessing at address ranges.
fn virtual_nic(name: &str) -> bool {
    ["lo", "docker", "br-", "veth", "virbr", "cni", "kube", "flannel", "tap"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// False when PVE's disk counters are not measuring this guest.
fn disk_io_visible(guest: &Value) -> bool {
    let uptime = num(guest, "uptime").unwrap_or(0.0);
    let total = num(guest, "diskread").unwrap_or(0.0) + num(guest, "diskwrite").unwrap_or(0.0);
    uptime < 3600.0 || total >= DISK_IO_FLOOR
}

pub struct Pve {
    config: Arc<Config>,
    http: ureq::Agent,
    node: Option<String>,
    rates: RateTracker,
    ips: HashMap<u64, Vec<String>>,
    /// vmid -> last uptime, for reboot detection.
    seen_uptime: HashMap<u64, Option<f64>>,
    backstop: i64,
    filesystems: HashMap<u64, Fs>,
    fs_countdown: i64,
    polls: u64,
    /// vmid -> poll number before which a guest with no answer yet is not
    /// asked again (MISSING_RETRY_EVERY).
    ip_retry: HashMap<u64, u64>,
    fs_retry: HashMap<u64, u64>,
}

impl Pve {
    pub fn new(config: Arc<Config>) -> Self {
        // Verification off: the PVE certificate is self-signed (§3.1).
        let http = net::http(net::self_signed_tls(), PVE_TIMEOUT);
        Pve {
            node: config.pve_node.clone(),
            config,
            http,
            rates: RateTracker::default(),
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
        let url = format!("{}{path}", self.config.pve_base());
        let response = self
            .http
            .get(&url)
            .set("Authorization", &self.config.pve_auth_header())
            .call()
            .map_err(|e| net::describe(e, started))?;
        let mut body: Value = response.into_json().map_err(|e| {
            if net::is_timeout(&e) {
                net::timed_out(started)
            } else {
                format!("unreadable reply: {e}")
            }
        })?;
        Ok(body.get_mut("data").map(Value::take).unwrap_or(Value::Null))
    }

    /// One guest-agent call per guest, all at once. Best effort: a guest
    /// without the agent, or with it stopped, simply has no answer.
    fn ask_agents(&self, vmids: &[u64], command: &str) -> Vec<(u64, Value)> {
        let node = self.node.as_deref().unwrap_or_default();
        thread::scope(|scope| {
            let calls: Vec<_> = vmids
                .iter()
                .map(|&vmid| {
                    let path = format!("/nodes/{node}/qemu/{vmid}/agent/{command}");
                    scope.spawn(move || (vmid, self.get(&path, Instant::now()).ok()))
                })
                .collect();
            calls
                .into_iter()
                .filter_map(|call| match call.join() {
                    Ok((vmid, Some(data))) => Some((vmid, data)),
                    _ => None,
                })
                .collect()
        })
    }

    fn refresh_ips(&mut self, vmids: &[u64]) {
        for (vmid, data) in self.ask_agents(vmids, "network-get-interfaces") {
            let mut found = Vec::new();
            for nic in list(&data, "result") {
                if virtual_nic(text(nic, "name").unwrap_or("")) {
                    continue;
                }
                for entry in list(nic, "ip-addresses") {
                    let address = text(entry, "ip-address").unwrap_or("");
                    if text(entry, "ip-address-type") == Some("ipv4")
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

    /// In-guest filesystem usage from the agent. `/cluster/resources`
    /// reports `disk` as 0 for every VM, so for a VM the agent is the only
    /// source of real usage. The largest real filesystem stands in for "the
    /// guest's disk"; on these guests that is always root.
    fn refresh_filesystems(&mut self, vmids: &[u64]) {
        for (vmid, data) in self.ask_agents(vmids, "get-fsinfo") {
            let mut best: Option<Fs> = None;
            for fs in list(&data, "result") {
                let Some(total) = num(fs, "total-bytes").filter(|t| *t > 0.0) else { continue };
                if text(fs, "type").is_some_and(|t| SKIP_FS.contains(&t)) {
                    continue;
                }
                if best.as_ref().map_or(true, |b| total > b.total) {
                    best = Some(Fs { used: num(fs, "used-bytes"), total });
                }
            }
            match best {
                Some(fs) => self.filesystems.insert(vmid, fs),
                None => self.filesystems.remove(&vmid),
            };
        }
    }

    pub fn poll(&mut self, source: &Mutex<Source<PveData>>) -> Result<(), String> {
        let started = Instant::now();
        if self.node.is_none() {
            let nodes = self.get("/nodes", started)?;
            let first = nodes
                .as_array()
                .and_then(|n| n.first())
                .and_then(|n| text(n, "node"))
                .ok_or("no nodes returned")?;
            self.node = Some(first.to_string());
        }
        let node = self.node.clone().unwrap_or_default();
        let resources = self.get("/cluster/resources?type=vm", started)?;
        let status = self.get(&format!("/nodes/{node}/status"), started)?;
        let resources = resources.as_array().map_or(&[][..], Vec::as_slice);

        // Running guests only. Stopped ones report nothing worth a card, and
        // their stale counters would pollute the rate tracker.
        let running: Vec<&Value> = resources
            .iter()
            .filter(|g| text(g, "status") == Some("running") && !truthy(g.get("template")))
            .collect();
        self.polls += 1;
        let polls = self.polls;
        let live: HashSet<u64> = running.iter().filter_map(|g| int(g, "vmid")).collect();

        // Forget guests that are no longer running, so a later start re-reads.
        self.ips.retain(|v, _| live.contains(v));
        self.filesystems.retain(|v, _| live.contains(v));
        self.seen_uptime.retain(|v, _| live.contains(v));
        self.ip_retry.retain(|v, _| live.contains(v));
        self.fs_retry.retain(|v, _| live.contains(v));

        let is_qemu = |g: &Value| text(g, "type").unwrap_or("qemu") == "qemu";

        // Only ask about guests that actually need an answer.
        self.backstop -= 1;
        let due = self.backstop <= 0;
        if due {
            self.backstop = IP_BACKSTOP_EVERY;
        }
        let mut ask_ips = Vec::new();
        for guest in &running {
            let Some(vmid) = int(guest, "vmid") else { continue };
            let uptime = num(guest, "uptime");
            let was = self.seen_uptime.get(&vmid).copied().flatten();
            if let (Some(was), Some(now)) = (was, uptime) {
                if now < was {
                    // Rebooted in place. The old address and usage may not
                    // hold, and its agent is still starting: ask until it
                    // answers.
                    self.ips.remove(&vmid);
                    self.filesystems.remove(&vmid);
                    self.ip_retry.remove(&vmid);
                    self.fs_retry.remove(&vmid);
                }
            }
            self.seen_uptime.insert(vmid, uptime);
            let missing = !self.ips.contains_key(&vmid)
                && self.ip_retry.get(&vmid).copied().unwrap_or(0) <= polls;
            if due || missing {
                ask_ips.push((vmid, is_qemu(guest)));
            }
        }
        if !ask_ips.is_empty() {
            let qemu: Vec<u64> = ask_ips.iter().filter(|(_, q)| *q).map(|(v, _)| *v).collect();
            self.refresh_ips(&qemu);
            for (vmid, _) in ask_ips {
                if !self.ips.contains_key(&vmid) {
                    self.ip_retry.insert(vmid, polls + MISSING_RETRY_EVERY);
                }
            }
        }

        // All of them on the slow cycle; in between, only guests with no
        // usage yet (newly started or rebooted), at the retry pace.
        self.fs_countdown -= 1;
        let ask_fs: Vec<(u64, bool)> = if self.fs_countdown <= 0 {
            self.fs_countdown = FS_REFRESH_EVERY;
            running.iter().filter_map(|g| Some((int(g, "vmid")?, is_qemu(g)))).collect()
        } else {
            running
                .iter()
                .filter(|g| is_qemu(g))
                .filter_map(|g| int(g, "vmid"))
                .filter(|v| {
                    !self.filesystems.contains_key(v)
                        && self.fs_retry.get(v).copied().unwrap_or(0) <= polls
                })
                .map(|v| (v, true))
                .collect()
        };
        if !ask_fs.is_empty() {
            let qemu: Vec<u64> = ask_fs.iter().filter(|(_, q)| *q).map(|(v, _)| *v).collect();
            self.refresh_filesystems(&qemu);
            for (vmid, _) in ask_fs {
                if !self.filesystems.contains_key(&vmid) {
                    self.fs_retry.insert(vmid, polls + MISSING_RETRY_EVERY);
                }
            }
        }

        let mut guests = BTreeMap::new();
        let mut samples = Vec::new();
        for guest in &running {
            let Some(vmid) = int(guest, "vmid") else { continue };
            // The counters are pvestatd's last snapshot, republished every
            // ~10s, not a reading taken now. The guest's uptime comes from
            // that same snapshot, so it times them exactly; an unchanged
            // uptime is a repeat.
            let mut rate = |name: &str| {
                let snapshot = num(guest, "uptime")?;
                self.rates.rate(&format!("{vmid}.{name}"), num(guest, name), snapshot)
            };
            let (net_in, net_out) = (rate("netin"), rate("netout"));
            let (disk_read, disk_write) = (rate("diskread"), rate("diskwrite"));
            // A container's disk is the real usage of its root filesystem,
            // with no agent involved.
            let mut filesystem = self.filesystems.get(&vmid).cloned();
            if text(guest, "type") == Some("lxc") {
                if let Some(total) = num(guest, "maxdisk").filter(|t| *t > 0.0) {
                    filesystem = Some(Fs { used: num(guest, "disk"), total });
                }
            }
            let cpu_pct = num(guest, "cpu").map(|f| f * 100.0); // 0.0-1.0, NOT a percentage
            samples.push((format!("cpu.{vmid}"), cpu_pct));
            guests.insert(
                vmid,
                Guest {
                    name: text(guest, "name").map(str::to_string),
                    uptime: num(guest, "uptime"),
                    cpu_pct,
                    mem: num(guest, "mem"),
                    maxmem: num(guest, "maxmem"),
                    addresses: self.ips.get(&vmid).cloned().unwrap_or_default(),
                    filesystem,
                    disk_io_visible: disk_io_visible(guest),
                    net_in_bps: net_in,
                    net_out_bps: net_out,
                    disk_read_bps: disk_read,
                    disk_write_bps: disk_write,
                },
            );
        }

        // Stop tracking counters for guests that stopped, so a later restart
        // does not diff against a months-old sample.
        self.rates.keep_only(&live.iter().map(u64::to_string).collect());

        let memory = status.get("memory").cloned().unwrap_or_default();
        let cpuinfo = status.get("cpuinfo").cloned().unwrap_or_default();
        let node = Node {
            name: node,
            cpu_pct: num(&status, "cpu").map(|f| f * 100.0),
            mem: num(&memory, "used"),
            maxmem: num(&memory, "total"),
            loadavg: status
                .get("loadavg")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(|v| match v {
                            Value::String(s) => s.parse().ok(),
                            other => other.as_f64(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            uptime: num(&status, "uptime"),
            pveversion: text(&status, "pveversion").map(str::to_string),
            cpus: int(&cpuinfo, "cpus"),
            guests_running: running.len(),
            guests_total: resources.iter().filter(|g| !truthy(g.get("template"))).count(),
        };

        let mut source = lock(source);
        for (key, value) in samples {
            source.push(&key, value);
        }
        source.push("node.cpu", node.cpu_pct);
        let mem_pct = match (node.mem, node.maxmem) {
            (Some(used), Some(total)) if total > 0.0 => Some(used / total * 100.0),
            _ => None,
        };
        source.push("node.mem_pct", mem_pct);
        source.succeed(PveData { node, guests });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nic_filter() {
        assert!(virtual_nic("docker0"));
        assert!(virtual_nic("br-1a2b"));
        assert!(!virtual_nic("ens18"));
        assert!(!virtual_nic("eth0"));
    }

    #[test]
    fn disk_io_visibility() {
        assert!(disk_io_visible(&json!({"uptime": 100, "diskread": 0})));
        assert!(!disk_io_visible(&json!({"uptime": 7200, "diskread": 1000, "diskwrite": 0})));
        assert!(disk_io_visible(&json!({"uptime": 7200, "diskread": 1e9})));
    }
}

//! The three sources and the snapshot served as JSON, as
//! pvestats/collector.py.

use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Map, Value};

use crate::config::Config;
use crate::sources::{self, agent::Agent, lock, pve::Pve, run_poller, truenas::TrueNas};
use crate::state::{clock, Source};

pub struct Collector {
    pub config: Arc<Config>,
    pub pve: Mutex<Source>,
    pub agent: Mutex<Source>,
    pub truenas: Mutex<Source>,
}

impl Collector {
    pub fn new(config: Arc<Config>) -> Self {
        let window = config.ui().get("window").and_then(Value::as_f64).unwrap_or(180.0);
        let collector = Collector {
            config,
            pve: Mutex::new(Source::new(sources::PVE_INTERVAL, 600.0)),
            agent: Mutex::new(Source::new(sources::AGENT_INTERVAL, 600.0)),
            truenas: Mutex::new(Source::new(sources::TRUENAS_INTERVAL, 600.0)),
        };
        collector.set_history_window(window);
        collector
    }

    /// One thread per configured source; the others say why they are not.
    pub fn start(self: &Arc<Self>) {
        let missing = || "not configured — see .env".to_string();
        if self.config.pve_configured() {
            let (c, mut pve) = (Arc::clone(self), Pve::new(Arc::clone(&self.config)));
            thread::spawn(move || run_poller(&c.pve, sources::PVE_INTERVAL, || pve.poll(&c.pve)));
        } else {
            lock(&self.pve).fail(missing());
        }
        if self.config.agent_configured() {
            let (c, mut agent) = (Arc::clone(self), Agent::new(Arc::clone(&self.config)));
            thread::spawn(move || run_poller(&c.agent, sources::AGENT_INTERVAL, || agent.poll(&c.agent)));
        } else {
            lock(&self.agent).fail(missing());
        }
        if self.config.truenas_configured() {
            let (c, mut nas) = (Arc::clone(self), TrueNas::new(Arc::clone(&self.config)));
            thread::spawn(move || run_poller(&c.truenas, sources::TRUENAS_INTERVAL, || nas.poll(&c.truenas)));
        } else {
            lock(&self.truenas).fail(missing());
        }
    }

    /// Re-lengths every ring buffer; charts take their span from it.
    pub fn set_history_window(&self, seconds: f64) {
        let seconds = seconds.clamp(30.0, 3600.0).trunc();
        for source in [&self.pve, &self.agent, &self.truenas] {
            lock(source).resize(seconds);
        }
    }

    /// The whole state, as GET /api/state serves it. No credentials: only
    /// observed values (spec §6).
    pub fn snapshot(&self) -> Value {
        let pve = lock(&self.pve);
        let agent = lock(&self.agent);
        let nas = lock(&self.truenas);

        let mut guests: Vec<(u64, Map<String, Value>)> = pve
            .data
            .get("guests")
            .and_then(Value::as_object)
            .map(|g| {
                g.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_object()?.clone())))
                    .collect()
            })
            .unwrap_or_default();

        // ml60's card merges two sources at two rates: PVE for CPU and RAM,
        // the agent for the GPU. The fast panels must not wait on the slow
        // poll (spec §4.2), so they are attached whatever PVE did.
        for (vmid, host) in &self.config.hosts {
            let view = host.get("view").and_then(Value::as_str);
            let position = guests.iter().position(|(v, _)| v == vmid);
            let guest = match position {
                Some(i) => {
                    // A name in config.yaml wins over the VM's name in
                    // Proxmox, which can be years out of date.
                    if let Some(name) = host.get("name").filter(|n| truthy(n)) {
                        guests[i].1.insert("name".into(), name.clone());
                    }
                    &mut guests[i].1
                }
                None => {
                    // Before PVE first answers there is no list of running
                    // guests, but the gpu and nas cards have sources of their
                    // own: they get a placeholder whose PVE fields read "–".
                    // Once PVE has answered, a guest missing from it is not
                    // running, and its card goes.
                    if pve.updated_at.is_some() || !matches!(view, Some("gpu" | "nas")) {
                        continue;
                    }
                    let name = host.get("name").filter(|n| truthy(n)).cloned().unwrap_or_else(|| json!(vmid.to_string()));
                    let placeholder = json!({"vmid": vmid, "name": name, "view": host.get("view"), "addresses": []});
                    guests.push((*vmid, placeholder.as_object().cloned().unwrap_or_default()));
                    &mut guests.last_mut().expect("pushed just above").1
                }
            };
            // The status always travels, even with no data yet, so a card can
            // say why it is empty (spec §7).
            match view {
                Some("gpu") => {
                    guest.insert("gpu_status".into(), agent.status());
                    if !agent.data.is_empty() {
                        let mut gpu = agent.data.clone();
                        let (ram, ram_at) = (gpu.shift_remove("ram"), gpu.shift_remove("ram_at"));
                        guest.insert("gpu".into(), Value::Object(gpu));
                        // ml60's memory from inside it, in place of PVE's
                        // (which counts the disk cache as used) while under
                        // three polls old; after that the card goes back to
                        // PVE's.
                        let fresh = ram_at.and_then(|t| t.as_f64()).is_some_and(|t| clock() - t <= sources::AGENT_INTERVAL * 3.0);
                        if let Some(ram) = ram.filter(|r| fresh && !r.is_null()) {
                            guest.insert("ram".into(), ram);
                        }
                    }
                }
                Some("nas") => {
                    guest.insert("nas_status".into(), nas.status());
                    if !nas.data.is_empty() {
                        let mut data = nas.data.clone();
                        let labels = host.get("nic_labels").and_then(Value::as_object).filter(|l| !l.is_empty());
                        if let (Some(labels), Some(interfaces)) = (labels, data.get_mut("interfaces").and_then(Value::as_object_mut)) {
                            for (name, nic) in interfaces.iter_mut() {
                                if let (Some(label), Some(nic)) = (labels.get(name), nic.as_object_mut()) {
                                    nic.insert("label".into(), label.clone());
                                }
                            }
                        }
                        guest.insert("nas".into(), Value::Object(data));
                    }
                }
                _ => {}
            }
        }
        guests.sort_by_key(|(vmid, _)| *vmid);

        let mut sources = Map::new();
        sources.insert("pve".into(), pve.status());
        sources.insert("agent".into(), agent.status());
        sources.insert("truenas".into(), nas.status());
        let mut history = Map::new();
        history.insert("pve".into(), pve.history());
        history.insert("agent".into(), agent.history());
        history.insert("truenas".into(), nas.history());
        json!({
            "sources": sources,
            "node": pve.data.get("node").cloned().unwrap_or(Value::Null),
            "guests": guests.into_iter().map(|(_, g)| Value::Object(g)).collect::<Vec<_>>(),
            "history": history,
        })
    }
}

/// Python truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

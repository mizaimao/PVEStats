//! The web page's JSON (GET /api/state): the sources the terminal draws, in
//! the shape static/js reads. No credentials: only observed values (spec §6).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::config::{Config, View};
use crate::sources::{lock, AgentData, Guest, NasData, Node, RouterData, Shared};
use crate::state::{clock, Source};
use crate::ui::overlay;

/// A source as the page's status dots and charts read it: charts place
/// samples by `interval` and span `capacity` of them.
pub fn status<T>(s: &Source<T>) -> Value {
    let st = s.status();
    json!({
        "ok": st.ok,
        "error": st.error,
        "age": st.age.map(|a| (a * 10.0).round() / 10.0),
        "interval": s.interval,
        "capacity": s.capacity(),
        "stale": st.stale,
    })
}

/// Every series of a source, to four decimals; gaps are null.
pub fn history<T>(s: &Source<T>) -> Value {
    s.all_series()
        .map(|(key, buf)| (key.clone(), buf.iter().map(|v| json!(v.map(|x| (x * 1e4).round() / 1e4))).collect()))
        .collect::<Map<String, Value>>()
        .into()
}

fn view(v: View) -> &'static str {
    match v {
        View::Basic => "basic",
        View::Gpu => "gpu",
        View::Nas => "nas",
    }
}

pub fn snapshot(config: &Config, shared: &Shared) -> Value {
    let pve = lock(&shared.pve);
    let host = lock(&shared.host);
    let agent = lock(&shared.agent);
    let nas = lock(&shared.nas);
    let router = lock(&shared.router);

    // As the terminal draws it: the host agent's live figures over PVE's
    // while it answers, with its 2 s history for the host's and guests'
    // charts; PVE's otherwise.
    let hs = host.status();
    let live = config.host_configured() && hs.ok && !hs.stale;
    let data = pve.data.as_ref().map(|d| if live { overlay(d, host.data.as_ref()) } else { d.clone() });
    let (mut pve_status, pve_history) = if live { (status(&*host), history(&*host)) } else { (status(&*pve), history(&*pve)) };
    // Live figures with the API failing: names and limits are going old.
    if let (true, Some(error)) = (live, pve.status().error) {
        pve_status["ok"] = json!(false);
        pve_status["error"] = json!(format!("API: {error}"));
    }

    // Guests in VMID order. A name in config.yaml wins over the VM's name in
    // Proxmox, which can be years out of date.
    let mut guests: BTreeMap<u64, Map<String, Value>> = BTreeMap::new();
    for (&vmid, g) in data.iter().flat_map(|d| &d.guests) {
        guests.insert(vmid, guest(vmid, g, config));
    }
    // Before PVE first answers there is no list of running guests, but the
    // GPU and NAS cards have sources of their own: they get a placeholder
    // whose PVE fields read "–". Once PVE has answered, a guest missing from
    // it is not running, and its card goes.
    if pve.updated_at.is_none() {
        for (&vmid, h) in &config.hosts {
            if matches!(h.view, View::Gpu | View::Nas) && !guests.contains_key(&vmid) {
                let name = h.name.clone().unwrap_or_else(|| vmid.to_string());
                let placeholder = json!({"vmid": vmid, "name": name, "view": view(h.view), "addresses": []});
                guests.insert(vmid, placeholder.as_object().cloned().unwrap_or_default());
            }
        }
    }
    // The status always travels, even with no data yet, so a card can say
    // why it is empty (spec §7).
    for (vmid, g) in guests.iter_mut() {
        match config.view_for(*vmid) {
            View::Gpu => {
                g.insert("gpu_status".into(), status(&*agent));
                if let Some(a) = &agent.data {
                    g.insert("gpu".into(), gpu(a));
                    // ml60's memory from inside it, in place of PVE's (which
                    // counts the disk cache as used) while under three polls
                    // old.
                    if let Some(r) = a.ram.as_ref().filter(|r| clock() - r.at <= agent.interval * 3.0) {
                        g.insert("ram".into(), json!({"total": r.total, "available": r.available, "cache": r.cache}));
                    }
                }
            }
            View::Nas => {
                g.insert("nas_status".into(), status(&*nas));
                if let Some(n) = &nas.data {
                    g.insert("nas".into(), truenas(n, config.hosts.get(vmid).map(|h| &h.nic_labels)));
                }
            }
            View::Basic => {}
        }
    }

    let mut sources = Map::new();
    let mut charts = Map::new();
    sources.insert("pve".into(), pve_status);
    charts.insert("pve".into(), pve_history);
    sources.insert("agent".into(), status(&*agent));
    charts.insert("agent".into(), history(&*agent));
    sources.insert("truenas".into(), status(&*nas));
    charts.insert("truenas".into(), history(&*nas));
    let router_data = if config.router_shown() {
        sources.insert("router".into(), status(&*router));
        charts.insert("router".into(), history(&*router));
        // Last known good, like every other card's: a failing router keeps
        // its numbers, greyed, rather than blanking.
        router.data.as_ref().map_or(Value::Null, gateway)
    } else {
        Value::Null
    };
    json!({
        "sources": sources,
        "node": data.as_ref().map_or(Value::Null, |d| node(&d.node)),
        "guests": guests.into_values().map(Value::Object).collect::<Vec<_>>(),
        "router": router_data,
        "history": charts,
    })
}

fn guest(vmid: u64, g: &Guest, config: &Config) -> Map<String, Value> {
    let name = config.hosts.get(&vmid).and_then(|h| h.name.clone()).or_else(|| g.name.clone());
    let value = json!({
        "vmid": vmid,
        "name": name,
        "uptime": g.uptime,
        "cpu_pct": g.cpu_pct,
        "maxcpu": g.maxcpu,
        "mem": g.mem,
        "maxmem": g.maxmem,
        "view": view(config.view_for(vmid)),
        "addresses": g.addresses,
        "filesystem": g.filesystem.as_ref().map(|f| json!({"used": f.used, "total": f.total})),
        "disk_io_visible": g.disk_io_visible,
        "net_in_bps": g.net_in_bps,
        "net_out_bps": g.net_out_bps,
        "disk_read_bps": g.disk_read_bps,
        "disk_write_bps": g.disk_write_bps,
    });
    value.as_object().cloned().unwrap_or_default()
}

fn node(n: &Node) -> Value {
    json!({
        "name": n.name,
        "cpu_pct": n.cpu_pct,
        "mem": n.mem,
        "maxmem": n.maxmem,
        "loadavg": n.loadavg,
        "uptime": n.uptime,
        "pveversion": n.pveversion,
        "cores": n.cores,
        "cpus": n.cpus,
        "guests_running": n.guests_running,
        "guests_total": n.guests_total,
    })
}

/// While NVML fails on ml60 the GPU's readings are absent but the models
/// still come through.
fn gpu(a: &AgentData) -> Value {
    let mut m = Map::new();
    if let Some(g) = &a.gpu {
        let fields = json!({
            "util": g.util,
            "vram_used": g.vram_used,
            "vram_total": g.vram_total,
            "temp": g.temp,
            "power": g.power,
            "power_limit": g.power_limit,
            "clock_sm": g.clock_sm,
            "fan": g.fan,
            "pcie": {
                "gen": g.pcie.gen,
                "width": g.pcie.width,
                "max_gen": g.pcie.max_gen,
                "tx_kbps": g.pcie.tx_kbps,
                "rx_kbps": g.pcie.rx_kbps,
            },
            "procs": g.procs.iter().map(|p| json!({"pid": p.pid, "name": p.name, "vram": p.vram})).collect::<Vec<_>>(),
        });
        m.extend(fields.as_object().cloned().unwrap_or_default());
    }
    let models = a.models.iter().map(|m| {
        json!({
            "name": m.name,
            "size_vram": m.size_vram,
            "processor": m.processor,
            "offloaded": m.offloaded,
            "unload_in": m.unload_in,
        })
    });
    m.insert("models".into(), models.collect());
    m.insert("can_unload".into(), json!(a.can_unload));
    Value::Object(m)
}

fn truenas(n: &NasData, labels: Option<&BTreeMap<String, String>>) -> Value {
    let interfaces: Map<String, Value> = n
        .interfaces
        .iter()
        .map(|(name, nic)| {
            let mut v = json!({"rx_bps": nic.rx_bps, "tx_bps": nic.tx_bps, "role": nic.role, "media": nic.media});
            if let Some(label) = labels.and_then(|l| l.get(name)) {
                v["label"] = json!(label);
            }
            (name.clone(), v)
        })
        .collect();
    let disks: Map<String, Value> = n
        .disks
        .iter()
        .map(|(name, d)| (name.clone(), json!({"read_bps": d.read_bps, "write_bps": d.write_bps, "size": d.size})))
        .collect();
    let pools: Vec<Value> = n
        .pools
        .iter()
        .map(|p| {
            json!({"name": p.name, "status": p.status, "healthy": p.healthy, "used_bytes": p.used_bytes,
                   "usable": p.usable, "old": p.old})
        })
        .collect();
    let inside = n.inside.as_ref().map(|i| {
        json!({"cpu_pct": i.cpu_pct, "mem_used": i.mem_used, "mem_total": i.mem_total, "uptime": i.uptime, "ip": i.ip})
    });
    json!({"interfaces": interfaces, "disks": disks, "pools": pools, "inside": inside})
}

fn gateway(r: &RouterData) -> Value {
    json!({
        "model": r.model,
        "uptime": r.uptime,
        "cpu_pct": r.cpu_pct,
        "mem_used": r.mem_used,
        "mem_total": r.mem_total,
        "temp": r.temp,
        "wan_up": r.wan_up,
        "wan_ip": r.wan_ip,
        "wan_down_bps": r.wan_down_bps,
        "wan_up_bps": r.wan_up_bps,
        "ports": r.ports.iter().map(|p| json!({"up": p.up, "wan": p.wan, "down_bps": p.down_bps, "up_bps": p.up_bps})).collect::<Vec<_>>(),
        "wired_devices": r.wired_devices,
        "bands": r.bands.iter().map(|b| json!({"name": b.name, "on": b.on, "devices": b.devices, "down_bps": b.down_bps, "up_bps": b.up_bps})).collect::<Vec<_>>(),
    })
}

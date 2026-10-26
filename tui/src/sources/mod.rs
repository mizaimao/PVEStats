//! One independent poller per data source, each on its own thread and
//! interval. A source that fails sets its own error; the rest of the screen
//! keeps working (spec §2). Every outbound call has a timeout (spec §7).
//!
//! Field handling follows the app's collector (pvestats/sources), which was
//! written against captured responses.

pub mod agent;
pub mod host;
pub mod nas_agent;
pub mod pve;
pub mod router;
pub mod weather;

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::state::Source;

/// Intervals are properties of the sources, not preferences. PVE's 10s is
/// pvestatd's own recompute cycle (§3.1); the agent's NVML reads take
/// microseconds, so 1s is genuinely real time.
pub const PVE_INTERVAL: f64 = 10.0;
/// With the host agent answering, PVE is asked only for what the agent
/// cannot tell (names, memory limits, vCPUs, balloon memory), every 30 s.
/// Measured on the host: the agent every 2 s and the API every 30 s cost
/// 0.041% of a core, the API alone every 10 s 0.044% (pve/README.md).
pub const PVE_INTERVAL_WITH_AGENT: f64 = 30.0;
/// The host agent (pve/agent) reads the kernel's counters as it is asked:
/// the default seconds between polls, `tui: pve_agent_every` in config.yaml
/// to change it (0.67 ms of the host's CPU a poll, measured).
pub const HOST_INTERVAL: f64 = 2.0;
pub const AGENT_INTERVAL: f64 = 1.0;
/// TrueNAS's own agent (nas/agent) reads the kernel as it is asked: the
/// default seconds between polls, `tui: nas_agent_every` in config.yaml to
/// change it (0.41 ms of TrueNAS's CPU a poll, measured; the API's 25 ms).
pub const NAS_AGENT_INTERVAL: f64 = 2.0;
/// The router's agent answers from kernel counters: measured on the router,
/// polling it every second costs 0.05% of its CPU.
pub const ROUTER_INTERVAL: f64 = 1.0;
/// Open-Meteo refreshes its forecast hourly; every 15 minutes is plenty.
pub const WEATHER_INTERVAL: f64 = 900.0;

pub const PVE_TIMEOUT: Duration = Duration::from_secs(5);
pub const HOST_TIMEOUT: Duration = Duration::from_secs(2);
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(2);
pub const NAS_AGENT_TIMEOUT: Duration = Duration::from_secs(2);
pub const ROUTER_TIMEOUT: Duration = Duration::from_secs(2);
pub const WEATHER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Shared {
    pub pve: Mutex<Source<PveData>>,
    /// The host agent's live figures, over PVE's while fresh.
    pub host: Mutex<Source<HostData>>,
    pub agent: Mutex<Source<AgentData>>,
    pub nas: Mutex<Source<NasData>>,
    pub router: Mutex<Source<RouterData>>,
    pub weather: Mutex<Source<WeatherData>>,
}

impl Shared {
    pub fn new(window: f64, pve_interval: f64, host_interval: f64, nas_interval: f64) -> Self {
        Shared {
            // Ten minutes at least: PVE's chart gives each 10s sample a cell,
            // so a wide one reaches back further than the window.
            pve: Mutex::new(Source::new(pve_interval, window.max(600.0))),
            host: Mutex::new(Source::new(host_interval, window.max(600.0))),
            agent: Mutex::new(Source::new(AGENT_INTERVAL, window)),
            nas: Mutex::new(Source::new(nas_interval, window)),
            router: Mutex::new(Source::new(ROUTER_INTERVAL, window)),
            weather: Mutex::new(Source::new(WEATHER_INTERVAL, window)),
        }
    }
}

/// A poisoned lock still holds good data: a panic in one poll must not take
/// the screen down with it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Call `poll` forever on a fixed interval, absorbing every failure.
pub fn run_poller<T>(
    source: &Mutex<Source<T>>,
    interval: f64,
    mut poll: impl FnMut() -> Result<(), String>,
) -> ! {
    loop {
        let started = Instant::now();
        // Before new samples land, account for any that were missed: a stall
        // should read as a gap, not as compressed time.
        lock(source).note_poll();
        if let Err(error) = poll() {
            lock(source).fail(error);
        }
        // One slot per series per poll, so a failed poll is drawn as a gap.
        lock(source).end_poll();
        // Drift-free: subtract however long the poll actually took.
        let spent = started.elapsed().as_secs_f64();
        thread::sleep(Duration::from_secs_f64((interval - spent).max(0.0)));
    }
}

// ---------------------------------------------------------------- PVE data

#[derive(Clone, Debug, Default)]
pub struct PveData {
    pub node: Node,
    pub guests: BTreeMap<u64, Guest>,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub cpu_pct: Option<f64>,
    pub mem: Option<f64>,
    pub maxmem: Option<f64>,
    pub loadavg: Vec<f64>,
    pub uptime: Option<f64>,
    pub pveversion: Option<String>,
    pub cpus: Option<u64>,
    pub guests_running: usize,
    pub guests_total: usize,
    /// clock() when PVE's node status (CPU, memory, load, uptime) was read:
    /// while the host agent answers it is read only every 10 minutes, so a
    /// screen falling back to PVE's figures can tell they are old.
    pub status_at: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Fs {
    pub used: Option<f64>,
    pub total: f64,
}

/// The host agent's figures (pve/agent), rates made here: the host's, and
/// each running VM's by vmid.
#[derive(Clone, Debug, Default)]
pub struct HostData {
    pub cpu_pct: Option<f64>,
    pub mem: Option<f64>,
    pub maxmem: Option<f64>,
    pub loadavg: Vec<f64>,
    pub uptime: Option<f64>,
    pub cpus: Option<u64>,
    pub guests: BTreeMap<u64, HostGuest>,
}

#[derive(Clone, Debug, Default)]
pub struct HostGuest {
    /// Of the VM's vCPUs, as PVE counts it; None until there is a rate, or
    /// while PVE has not given the VM's vCPUs.
    pub cpu_pct: Option<f64>,
    pub net_in_bps: Option<f64>,
    pub net_out_bps: Option<f64>,
    pub uptime: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Guest {
    pub name: Option<String>,
    pub uptime: Option<f64>,
    pub cpu_pct: Option<f64>,
    /// Its vCPUs, which the host agent's CPU time is divided by.
    pub maxcpu: Option<f64>,
    pub mem: Option<f64>,
    pub maxmem: Option<f64>,
    pub addresses: Vec<String>,
    /// None when the guest agent gave no usage.
    pub filesystem: Option<Fs>,
    /// Whether the host can actually see this guest's disk traffic.
    pub disk_io_visible: bool,
    /// Cumulative counters as bytes/sec; None until the second snapshot.
    pub net_in_bps: Option<f64>,
    pub net_out_bps: Option<f64>,
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
}

// -------------------------------------------------------------- agent data

#[derive(Clone, Debug, Default)]
pub struct AgentData {
    /// None while NVML is failing on ml60; the models still come through.
    pub gpu: Option<Gpu>,
    pub models: Vec<Model>,
    /// ml60's memory as ml60 itself sees it; None from an agent that does
    /// not report it.
    pub ram: Option<Ram>,
    /// ml60's CPU, network, disk, uptime and address as ml60 itself sees
    /// them; None from an agent that does not report them.
    pub inside: Option<Inside>,
}

/// A guest's CPU, network, disk, uptime and address from inside it, the
/// rates made from one reading to the next.
#[derive(Clone, Debug, Default)]
pub struct Inside {
    /// Of all its cores, 0-100; None for the first reading.
    pub cpu_pct: Option<f64>,
    /// Bytes a second into the guest and out of it.
    pub net_in_bps: Option<f64>,
    pub net_out_bps: Option<f64>,
    /// Its largest local filesystem, in bytes.
    pub disk_used: Option<f64>,
    pub disk_total: Option<f64>,
    pub uptime: Option<f64>,
    pub ip: Option<String>,
    /// clock() when it was read.
    pub at: f64,
}

/// Memory from inside a guest, in bytes. PVE's figure for a VM counts the
/// guest's disk cache as used; this tells the two apart.
#[derive(Clone, Debug, Default)]
pub struct Ram {
    pub total: f64,
    /// What programs can have without swapping: free memory plus cache the
    /// kernel can drop.
    pub available: f64,
    /// Buffers, page cache and reclaimable slab, as `free` counts it.
    pub cache: f64,
    /// clock() when it was read, since the agent still sends it while its
    /// GPU reading fails.
    pub at: f64,
}

impl Ram {
    /// What programs use, in % of the total: all but what is available.
    pub fn used_pct(&self) -> f64 {
        (self.total - self.available).max(0.0) / self.total * 100.0
    }

    /// Programs and the cache together, in % of the total, at most 100:
    /// about what PVE counts as used for the VM.
    pub fn cached_pct(&self) -> f64 {
        ((self.total - self.available).max(0.0) + self.cache).min(self.total) / self.total * 100.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct Gpu {
    pub util: Option<f64>,
    pub vram_used: Option<f64>,
    pub vram_total: Option<f64>,
    pub temp: Option<f64>,
    pub power: Option<f64>,
    pub power_limit: Option<f64>,
    pub clock_sm: Option<f64>,
    pub fan: Option<f64>,
    pub pcie: Pcie,
    pub procs: Vec<Proc>,
}

#[derive(Clone, Debug, Default)]
pub struct Pcie {
    pub gen: Option<u64>,
    pub width: Option<u64>,
    pub max_gen: Option<u64>,
    pub tx_kbps: Option<f64>,
    pub rx_kbps: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: Option<u64>,
    pub name: String,
    pub vram: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub size_vram: Option<f64>,
    /// Where it runs, as `ollama ps` says it: "100% GPU", "36%/64% CPU/GPU".
    pub processor: Option<String>,
    pub offloaded: bool,
    /// Seconds until Ollama unloads it, measured when it was polled.
    pub unload_in: Option<i64>,
}

// ------------------------------------------------------------ TrueNAS data

#[derive(Clone, Debug, Default)]
pub struct NasData {
    /// In the order the agent lists them.
    pub interfaces: Vec<(String, Nic)>,
    /// By device name, so sdb..sdf read in the order they are labelled.
    pub disks: Vec<(String, Disk)>,
    pub pools: Vec<Pool>,
    /// From TrueNAS's own agent: what the card otherwise takes from PVE.
    pub inside: Option<NasInside>,
}

/// TrueNAS's CPU, memory, address and uptime as its agent reads them.
#[derive(Clone, Debug, Default)]
pub struct NasInside {
    /// None until there are two readings.
    pub cpu_pct: Option<f64>,
    /// In use, ZFS's cache (the ARC) included, as Proxmox counts it.
    pub mem_used: Option<f64>,
    pub mem_total: Option<f64>,
    pub uptime: Option<f64>,
    pub ip: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Nic {
    /// None until there are two readings.
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    /// "virtual", "physical" or "unknown"; None before discovery saw it.
    pub role: Option<String>,
    pub media: String,
}

#[derive(Clone, Debug)]
pub struct Disk {
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    pub size: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Pool {
    pub name: String,
    pub status: Option<String>,
    pub healthy: bool,
    /// From the root dataset: what the pool holds and can hold, as
    /// TrueNAS's Storage page shows it. None until the agent has read it.
    pub used_bytes: Option<f64>,
    pub usable: Option<f64>,
    /// The figures are over two minutes old: TrueNAS's agent reads them
    /// every 30 s, and its last reads failed.
    pub old: bool,
}

// ------------------------------------------------------------- router data

#[derive(Clone, Debug, Default)]
pub struct RouterData {
    /// "GT-BE98_PRO", as the firmware names it.
    pub model: String,
    pub uptime: Option<f64>,
    /// Over all cores, from the last two readings.
    pub cpu_pct: Option<f64>,
    pub mem_used: Option<f64>,
    pub mem_total: Option<f64>,
    /// The SoC's, °C.
    pub temp: Option<f64>,
    pub wan_up: bool,
    pub wan_ip: String,
    /// Bytes per second from the internet and to it; None until two
    /// readings, or across a router reboot.
    pub wan_down_bps: Option<f64>,
    pub wan_up_bps: Option<f64>,
    pub ports: Vec<Port>,
    /// Devices on the wired side (VMs behind one port each count); None
    /// until the router has counted them.
    pub wired_devices: Option<u64>,
    pub bands: Vec<Band>,
}

/// A wired LAN port.
#[derive(Clone, Debug, Default)]
pub struct Port {
    pub up: bool,
    /// The port the internet comes in on (the router reports it): every
    /// download crosses it, so it is no LAN traffic of its own.
    pub wan: bool,
    /// Bytes per second to what is plugged in, and from it.
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
}

/// A Wi-Fi radio with the networks it carries.
#[derive(Clone, Debug, Default)]
pub struct Band {
    /// "2.4 GHz", "5 GHz", "6 GHz-1".
    pub name: String,
    pub on: bool,
    /// None until the agent has counted them.
    pub devices: Option<u64>,
    /// Bytes per second to its devices, and from them.
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
}

// ------------------------------------------------------------ weather data

#[derive(Clone, Debug, Default)]
pub struct WeatherData {
    /// The place's name, as looked up or as given.
    pub place: String,
    /// °F, mph and inches rather than °C, km/h and mm.
    pub imperial: bool,
    /// Who made the forecast: "NWS" or "Open-Meteo".
    pub source: &'static str,
    pub now: Now,
    /// From the hour now is in, hourly, a few days ahead.
    pub hours: Vec<Hour>,
    /// Today first.
    pub days: Vec<Day>,
    /// In force now, worst first; none where the source has no alerts.
    pub alerts: Vec<Alert>,
}

#[derive(Clone, Debug, Default)]
pub struct Now {
    /// Local, "2026-10-25T21:30".
    pub time: String,
    pub temp: Option<f64>,
    pub feels: Option<f64>,
    pub humidity: Option<f64>,
    pub wind: Option<f64>,
    pub gusts: Option<f64>,
    /// Where the wind blows from, as a compass point: "SW".
    pub wind_from: String,
    /// WMO weather code, or the nearest one to the forecaster's words.
    pub code: Option<u64>,
    /// The forecaster's words, where the source has them.
    pub text: Option<String>,
    pub day: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Hour {
    /// Local, "2026-10-25T22:00".
    pub time: String,
    pub temp: Option<f64>,
    pub rain_chance: Option<f64>,
    pub code: Option<u64>,
    pub text: Option<String>,
    pub humidity: Option<f64>,
    pub wind: Option<f64>,
    pub wind_from: String,
    pub day: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Day {
    /// "2026-10-25".
    pub date: String,
    pub code: Option<u64>,
    pub text: Option<String>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub rain_chance: Option<f64>,
    /// Rain and snow, mm or inches; None where the source does not say.
    pub rain: Option<f64>,
    /// Local, "2026-10-25T07:21".
    pub sunrise: String,
    pub sunset: String,
    /// From Open-Meteo's longer outlook, past the days the forecaster
    /// covers: less sure, and shown dimmer.
    pub outlook: bool,
}

/// A weather alert in force for the place: an advisory, watch or warning.
#[derive(Clone, Debug, Default)]
pub struct Alert {
    /// "Frost Advisory".
    pub event: String,
    /// "Extreme", "Severe", "Moderate", "Minor" or "Unknown".
    pub severity: String,
    pub headline: Option<String>,
    /// Local, "2026-10-26T09:00", when it ends or, failing that, expires.
    pub until: String,
}

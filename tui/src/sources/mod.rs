//! One independent poller per data source, each on its own thread and
//! interval. A source that fails sets its own error; the rest of the screen
//! keeps working (spec §2). Every outbound call has a timeout (spec §7).
//!
//! Field handling follows the app's collector (pvestats/sources), which was
//! written against captured responses.

pub mod agent;
pub mod pve;
pub mod router;
pub mod truenas;
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
pub const AGENT_INTERVAL: f64 = 1.0;
pub const TRUENAS_INTERVAL: f64 = 5.0;
/// The router's agent answers from kernel counters: measured on the router,
/// polling it every second costs 0.05% of its CPU.
pub const ROUTER_INTERVAL: f64 = 1.0;
/// Open-Meteo refreshes its forecast hourly; every 15 minutes is plenty.
pub const WEATHER_INTERVAL: f64 = 900.0;

pub const PVE_TIMEOUT: Duration = Duration::from_secs(5);
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(2);
pub const TRUENAS_TIMEOUT: Duration = Duration::from_secs(5);
pub const ROUTER_TIMEOUT: Duration = Duration::from_secs(2);
pub const WEATHER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Shared {
    pub pve: Mutex<Source<PveData>>,
    pub agent: Mutex<Source<AgentData>>,
    pub nas: Mutex<Source<NasData>>,
    pub router: Mutex<Source<RouterData>>,
    pub weather: Mutex<Source<WeatherData>>,
}

impl Shared {
    pub fn new(window: f64) -> Self {
        Shared {
            // Ten minutes at least: PVE's chart gives each 10s sample a cell,
            // so a wide one reaches back further than the window.
            pve: Mutex::new(Source::new(PVE_INTERVAL, window.max(600.0))),
            agent: Mutex::new(Source::new(AGENT_INTERVAL, window)),
            nas: Mutex::new(Source::new(TRUENAS_INTERVAL, window)),
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
}

#[derive(Clone, Debug)]
pub struct Fs {
    pub used: Option<f64>,
    pub total: f64,
}

#[derive(Clone, Debug)]
pub struct Guest {
    pub name: Option<String>,
    pub uptime: Option<f64>,
    pub cpu_pct: Option<f64>,
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
    /// In the order netdata lists them.
    pub interfaces: Vec<(String, Nic)>,
    /// By device name, so sdb..sdf read in the order they are labelled.
    pub disks: Vec<(String, Disk)>,
    pub pools: Vec<Pool>,
}

#[derive(Clone, Debug, Default)]
pub struct Nic {
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// "virtual", "physical" or "unknown"; None before discovery saw it.
    pub role: Option<String>,
    pub media: String,
}

#[derive(Clone, Debug)]
pub struct Disk {
    pub read_bps: f64,
    pub write_bps: f64,
    pub size: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Pool {
    pub name: String,
    pub status: Option<String>,
    pub healthy: bool,
    pub size: Option<f64>,
    pub allocated: Option<f64>,
    /// From the root dataset: what the pool holds and can hold. None when
    /// TrueNAS did not say, and the card falls back to raw zpool figures.
    pub used_bytes: Option<f64>,
    pub usable: Option<f64>,
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

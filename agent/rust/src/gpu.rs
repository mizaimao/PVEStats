//! The GPU, read through NVML by one thread only, about once a second and
//! only while someone is asking. Requests are served the latest reading and
//! never call NVML themselves, so no client sets how often the GPU is read,
//! and the PCIe counters' ~20 ms sampling windows stay off the request path.

use std::ffi::OsStr;
use std::time::{Duration, Instant};

use nvml_wrapper::enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor};
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::Nvml;
use serde_json::{json, Value};

/// After NVML fails, it is shut down and tried again after a second, then
/// after twice as long each time it fails again, up to this.
const RETRY_MOST: Duration = Duration::from_secs(30);

/// NVML, initialised once and kept: initialising it costs far more than
/// keeping it. Shut down (dropped) only after a device-level failure.
#[derive(Default)]
pub struct Gpu {
    nvml: Option<Nvml>,
    retry_at: Option<Instant>,
    /// The wait before the next try, after the next failure.
    backoff: Duration,
    error: String,
}

/// The driver installs the versioned name; the bare one comes only with
/// development packages. Only a library that will not load is tried under
/// the other name: an NVML that loads but will not start says why itself.
fn init() -> Result<Nvml, NvmlError> {
    match Nvml::builder().lib_path(OsStr::new("libnvidia-ml.so.1")).init() {
        Err(NvmlError::LibloadingError(_)) => Nvml::init(),
        other => other,
    }
}

impl Gpu {
    /// Initialises NVML now.
    pub fn start(&mut self) -> Result<(), String> {
        match init() {
            Ok(nvml) => {
                self.nvml = Some(nvml);
                Ok(())
            }
            Err(e) => {
                self.fail(format!("NVML: {e}"));
                Err(self.error.clone())
            }
        }
    }

    fn fail(&mut self, error: String) {
        // Dropping it shuts NVML down; the next reading starts afresh.
        self.nvml = None;
        self.backoff = (self.backoff * 2).clamp(Duration::from_secs(1), RETRY_MOST);
        self.retry_at = Some(Instant::now() + self.backoff);
        self.error = error;
    }

    /// The `gpu` object of the reply, or why there is none.
    pub fn read(&mut self, index: u32) -> Result<Value, String> {
        if self.nvml.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return Err(self.error.clone());
            }
            self.start()?;
        }
        let nvml = self.nvml.as_ref().expect("set just above");
        match sample(nvml, index) {
            Ok(value) => {
                self.backoff = Duration::ZERO;
                Ok(value)
            }
            Err(e) => {
                self.fail(format!("NVML: {e}"));
                Err(self.error.clone())
            }
        }
    }
}

/// One metric the board may not offer (fan speed on a fanless board, say):
/// None for that, so one unsupported metric does not cost the whole reading.
/// Any other error (the GPU lost from the bus) fails the reading.
fn metric<T>(r: Result<T, NvmlError>) -> Result<Option<T>, NvmlError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(
            NvmlError::NotSupported
            | NvmlError::NotFound
            | NvmlError::NoPermission
            | NvmlError::FunctionNotFound
            | NvmlError::FailedToLoadSymbol(_),
        ) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A process's name: NVML's, else the kernel's, else "?". The last part of
/// a path only: the dashboard shows no more, and the reply reaches the LAN.
fn process_name(nvml: &Nvml, pid: u32) -> String {
    let name = nvml
        .sys_process_name(pid, 256)
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()))
        .filter(|n| !n.is_empty());
    match name {
        Some(n) => n.rsplit('/').next().unwrap_or(&n).to_string(),
        None => "?".to_string(),
    }
}

fn watts(milliwatts: u32) -> f64 {
    (milliwatts as f64 / 100.0).round() / 10.0
}

/// The device, utilisation and memory say whether the GPU can be read at
/// all: their errors (beyond `metric`'s) fail the reading. Any error from
/// the rest is that one figure missing this time, so a passing glitch in a
/// fan or PCIe reading does not blank the card.
fn sample(nvml: &Nvml, index: u32) -> Result<Value, NvmlError> {
    let d = nvml.device_by_index(index)?;
    let util = metric(d.utilization_rates())?;
    let mem = metric(d.memory_info())?;
    let power = d.power_usage().ok();
    let limit = d.enforced_power_limit().or_else(|_| d.power_management_limit()).ok();
    // Needs to run as Ollama's user (or root) to see its processes;
    // otherwise NVML quietly lists none.
    let procs: Vec<Value> = d
        .running_compute_processes()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let vram = match p.used_gpu_memory {
                UsedGpuMemory::Used(b) if b < 1 << 63 => Some(b),
                _ => None,
            };
            json!({"pid": p.pid, "name": process_name(nvml, p.pid), "vram": vram})
        })
        .collect();
    // KB/s over NVML's last ~20 ms window, not cumulative: no rate maths.
    let pcie = json!({
        "gen": d.current_pcie_link_gen().ok(),
        "width": d.current_pcie_link_width().ok(),
        "max_gen": d.max_pcie_link_gen().ok(),
        "max_width": d.max_pcie_link_width().ok(),
        "tx_kbps": d.pcie_throughput(PcieUtilCounter::Send).ok(),
        "rx_kbps": d.pcie_throughput(PcieUtilCounter::Receive).ok(),
    });
    Ok(json!({
        "util": util.as_ref().map(|u| u.gpu),
        "mem_util": util.as_ref().map(|u| u.memory),
        "pcie": pcie,
        // Used as NVML's first memory call counts it, reserved memory
        // included: total less free, as the agent has always reported.
        "vram_used": mem.as_ref().map(|m| m.total.saturating_sub(m.free)),
        "vram_total": mem.as_ref().map(|m| m.total),
        "temp": d.temperature(TemperatureSensor::Gpu).ok(),
        "power": power.map(watts),
        "power_limit": limit.map(watts),
        "clock_sm": d.clock_info(Clock::SM).ok(),
        "fan": d.fan_speed(0).ok(),
        "procs": procs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watts_to_one_decimal() {
        assert_eq!(watts(9000), 9.0);
        assert_eq!(watts(80_449), 80.4);
        assert_eq!(watts(450_000), 450.0);
    }

    #[test]
    fn unsupported_metrics_are_null_and_the_rest_fail() {
        assert_eq!(metric::<u32>(Err(NvmlError::NotSupported)).unwrap(), None);
        assert_eq!(metric::<u32>(Err(NvmlError::FunctionNotFound)).unwrap(), None);
        assert_eq!(metric(Ok(3u32)).unwrap(), Some(3));
        assert!(metric::<u32>(Err(NvmlError::GpuLost)).is_err());
    }

    #[test]
    fn without_a_driver_the_reading_fails_and_waits_to_retry() {
        // The build machine has no NVIDIA driver.
        let mut gpu = Gpu::default();
        let first = gpu.read(0).unwrap_err();
        assert!(first.starts_with("NVML: "), "{first}");
        let started = Instant::now();
        assert_eq!(gpu.read(0).unwrap_err(), first);
        assert!(started.elapsed() < Duration::from_millis(50), "no second init within the retry wait");
        // The wait doubles with each failure, up to 30 s.
        assert_eq!(gpu.backoff, Duration::from_secs(1));
        for _ in 0..8 {
            gpu.retry_at = None;
            let _ = gpu.read(0);
        }
        assert_eq!(gpu.backoff, RETRY_MOST);
    }
}

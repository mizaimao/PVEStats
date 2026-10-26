//! One independent poller per data source, each on its own thread and
//! interval, as pvestats/sources. A source that fails sets its own error;
//! the rest keep working (spec §2). Every outbound call has a timeout
//! (spec §7).

pub mod agent;
pub mod pve;
pub mod truenas;

use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::state::Source;

/// Properties of the sources, not preferences: PVE's 10 s is pvestatd's own
/// cycle (spec §3.1); the agent's 1 s is real time.
pub const PVE_INTERVAL: f64 = 10.0;
pub const AGENT_INTERVAL: f64 = 1.0;
pub const TRUENAS_INTERVAL: f64 = 5.0;

pub const PVE_TIMEOUT: Duration = Duration::from_secs(5);
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(2);
pub const TRUENAS_TIMEOUT: Duration = Duration::from_secs(5);

/// A poisoned lock still holds good data: a panic in one poll must not take
/// the rest with it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Calls `poll` forever on a fixed interval, absorbing every failure.
pub fn run_poller(source: &Mutex<Source>, interval: f64, mut poll: impl FnMut() -> Result<(), String>) -> ! {
    loop {
        let started = Instant::now();
        // Before new samples land, account for any that were missed: a
        // stall should read as a gap, not as compressed time.
        lock(source).note_poll(interval);
        if let Err(error) = poll() {
            lock(source).fail(error);
        }
        // One slot per series per poll, so a failed poll is drawn as a gap.
        lock(source).end_poll();
        // Drift-free: less however long the poll took.
        thread::sleep(Duration::from_secs_f64((interval - started.elapsed().as_secs_f64()).max(0.0)));
    }
}

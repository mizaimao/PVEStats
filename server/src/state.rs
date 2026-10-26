//! In-memory state, as pvestats/state.py. Nothing here is ever written to
//! disk (constraint 3).
//!
//! A source that fails keeps its last known values and is marked stale with
//! an age, rather than being zeroed: a blank card and a stale reading have to
//! look different to a reader (spec §7).

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{json, Map, Number, Value};

/// Monotonic seconds that keep counting while the computer sleeps:
/// CLOCK_MONOTONIC on macOS, CLOCK_BOOTTIME on Linux. A closed lid must read
/// as time passing, or rates spike on wake and the sleep leaves no gap.
pub fn clock() -> f64 {
    #[cfg(target_os = "linux")]
    let id = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let id = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime only writes the timespec it is given.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// A sample for a ring buffer: integers as they came, floats to four places.
fn sample(v: &Value) -> Value {
    match v.as_f64() {
        Some(f) if v.is_f64() => Number::from_f64((f * 1e4).round() / 1e4).map_or(Value::Null, Value::Number),
        _ => v.clone(),
    }
}

/// Python's round(x, 1) for an age.
fn one_decimal(v: f64) -> Value {
    Number::from_f64((v * 10.0).round() / 10.0).map_or(Value::Null, Value::Number)
}

/// Latest values plus bounded history for one data source.
pub struct Source {
    pub interval: f64,
    pub ok: bool,
    pub error: Option<String>,
    /// Last known good; survives failures. A JSON object.
    pub data: Map<String, Value>,
    /// clock() of the last success.
    pub updated_at: Option<f64>,
    /// Consecutive failed polls since the last success.
    pub failures: u32,
    maxlen: usize,
    /// In the order they were first pushed.
    series: Vec<(String, VecDeque<Value>)>,
    filled: HashSet<String>,
    last_poll: Option<f64>,
}

impl Source {
    pub fn new(interval: f64, history_seconds: f64) -> Self {
        Source {
            interval,
            ok: false,
            error: None,
            data: Map::new(),
            updated_at: None,
            failures: 0,
            maxlen: Self::length(history_seconds, interval),
            series: Vec::new(),
            filled: HashSet::new(),
            last_poll: None,
        }
    }

    fn length(history_seconds: f64, interval: f64) -> usize {
        ((history_seconds / interval).round() as usize).max(2)
    }

    /// Change how much history is kept, keeping the newest samples.
    pub fn resize(&mut self, history_seconds: f64) {
        let maxlen = Self::length(history_seconds, self.interval);
        if maxlen == self.maxlen {
            return;
        }
        self.maxlen = maxlen;
        for (_, buf) in &mut self.series {
            while buf.len() > maxlen {
                buf.pop_front();
            }
        }
    }

    /// Appends one sample to a named ring buffer. None marks a gap in a
    /// series that already exists, so series drawn on one chart stay
    /// aligned; it never creates a series.
    pub fn push(&mut self, key: &str, value: Option<Value>) {
        let value = value.filter(|v| !v.is_null());
        let maxlen = self.maxlen;
        let buf = match self.series.iter_mut().position(|(k, _)| k == key) {
            Some(i) => &mut self.series[i].1,
            None => {
                if value.is_none() {
                    return;
                }
                self.series.push((key.to_string(), VecDeque::new()));
                &mut self.series.last_mut().expect("pushed just above").1
            }
        };
        buf.push_back(value.as_ref().map_or(Value::Null, sample));
        while buf.len() > maxlen {
            buf.pop_front();
        }
        self.filled.insert(key.to_string());
    }

    /// Gives every series one slot for the poll that just ended: a gap for
    /// those that got no value, so an outage is drawn as one.
    pub fn end_poll(&mut self) {
        let maxlen = self.maxlen;
        for (key, buf) in &mut self.series {
            if !self.filled.contains(key) {
                buf.push_back(Value::Null);
                while buf.len() > maxlen {
                    buf.pop_front();
                }
            }
        }
        self.filled.clear();
    }

    /// Inserts gaps for polls that never happened (a stall, a closed lid),
    /// so the x-axis stays honest.
    pub fn note_poll(&mut self, interval: f64) {
        let now = clock();
        let previous = self.last_poll.replace(now);
        let Some(previous) = previous else { return };
        let missed = ((now - previous) / interval).round() as i64 - 1;
        if missed < 1 {
            return;
        }
        let maxlen = self.maxlen;
        for (_, buf) in &mut self.series {
            for _ in 0..(missed as usize).min(maxlen) {
                buf.push_back(Value::Null);
            }
            while buf.len() > maxlen {
                buf.pop_front();
            }
        }
    }

    pub fn succeed(&mut self, data: Map<String, Value>) {
        self.data = data;
        self.ok = true;
        self.error = None;
        self.failures = 0;
        self.updated_at = Some(clock());
    }

    /// The data is deliberately left alone: last known values stay visible.
    pub fn fail(&mut self, error: String) {
        self.ok = false;
        self.error = Some(error);
        self.failures += 1;
    }

    /// While a source that has never answered is on its first retry: its
    /// first error is held back until the retry fails too. A source that
    /// never polls (not configured) is never waiting.
    fn waiting(&self) -> bool {
        self.updated_at.is_none() && self.failures == 1 && self.last_poll.is_some()
    }

    pub fn status(&self) -> Value {
        let age = self.updated_at.map(|t| clock() - t);
        json!({
            "ok": self.ok,
            "error": if self.waiting() { None } else { self.error.clone() },
            "age": age.map_or(Value::Null, one_decimal),
            "interval": self.interval,
            "capacity": self.maxlen,
            "stale": age.is_some_and(|a| a > self.interval * 2.0),
        })
    }

    pub fn history(&self) -> Value {
        Value::Object(self.series.iter().map(|(k, buf)| (k.clone(), Value::Array(buf.iter().cloned().collect()))).collect())
    }
}

/// Turns cumulative byte counters into per-second rates. They count from a
/// guest's boot, so a negative delta means it rebooted and clamps to zero.
#[derive(Default)]
pub struct RateTracker {
    previous: HashMap<String, (f64, f64, Option<f64>)>,
}

impl RateTracker {
    /// Per second since the previous sample of `key`, timed by `now`: PVE's
    /// guests use their own uptime, which comes with the counters. A sample
    /// at the same time is the same snapshot again and keeps its rate; time
    /// going backwards starts over.
    pub fn rate(&mut self, key: &str, value: Option<f64>, now: f64) -> Option<f64> {
        let value = value?;
        let Some(&(then, before, last)) = self.previous.get(key) else {
            self.previous.insert(key.to_string(), (now, value, None));
            return None;
        };
        let elapsed = now - then;
        if elapsed == 0.0 {
            return last;
        }
        if elapsed < 0.0 {
            self.previous.insert(key.to_string(), (now, value, None));
            return None;
        }
        let rate = (value - before).max(0.0) / elapsed;
        self.previous.insert(key.to_string(), (now, value, Some(rate)));
        Some(rate)
    }

    /// Drops the counters of guests no longer running.
    pub fn keep_only(&mut self, live: &HashSet<String>) {
        self.previous.retain(|k, _| k.split('.').next().is_some_and(|guest| live.contains(guest)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_like_python() {
        let mut s = Source::new(1.0, 5.0);
        s.push("a", Some(json!(3)));
        s.push("a", Some(json!(1.234567)));
        s.push("b", None);
        assert_eq!(s.history(), json!({"a": [3, 1.2346]}));
        s.end_poll();
        s.push("a", Some(json!(2)));
        s.end_poll();
        assert_eq!(s.history()["a"], json!([3, 1.2346, 2]));
        s.push("c", Some(json!(1)));
        s.end_poll();
        assert_eq!(s.history()["a"], json!([3, 1.2346, 2, null]));
        assert_eq!(s.status()["capacity"], 5);
        s.resize(2.0);
        assert_eq!(s.history()["a"], json!([2, null]));
    }

    #[test]
    fn rates_from_snapshots() {
        let mut r = RateTracker::default();
        assert_eq!(r.rate("1.netin", Some(100.0), 10.0), None);
        assert_eq!(r.rate("1.netin", Some(300.0), 20.0), Some(20.0));
        assert_eq!(r.rate("1.netin", Some(300.0), 20.0), Some(20.0), "the same snapshot again");
        assert_eq!(r.rate("1.netin", Some(50.0), 30.0), Some(0.0), "a reboot clamps");
        assert_eq!(r.rate("1.netin", Some(60.0), 5.0), None, "time going back starts over");
        r.keep_only(&HashSet::new());
        assert_eq!(r.rate("1.netin", Some(70.0), 6.0), None);
    }
}

//! In-memory state. Nothing here is ever written to disk (constraint 3).
//!
//! A source that fails keeps its last known values and is marked stale with
//! an age, rather than being zeroed: a blank card and a stale reading have to
//! look different to a reader (spec §7).

use std::collections::{HashMap, HashSet, VecDeque};

/// Monotonic seconds that keep counting while the computer sleeps:
/// CLOCK_BOOTTIME on Linux, and CLOCK_MONOTONIC elsewhere, which on macOS
/// counts through sleep. std's Instant stops in both cases, so a suspend
/// would leave no gap in the charts.
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

pub type Series = VecDeque<Option<f64>>;

/// What a card needs to know about the source behind it.
#[derive(Clone, Debug)]
pub struct Status {
    pub ok: bool,
    /// None while waiting, so the screen says "waiting", not a fault.
    pub error: Option<String>,
    pub age: Option<f64>,
    /// True once a source has gone missing for more than two polls.
    pub stale: bool,
}

/// What a chart reads from a source, whatever its data: a series by name,
/// how many samples a series holds, and the seconds between them. The host
/// card and the guest cards draw from PVE's history or from its host
/// agent's, whichever is live.
pub trait History {
    fn series(&self, key: &str) -> Option<&Series>;
    fn capacity(&self) -> usize;
    fn interval(&self) -> f64;
}

impl<T> History for Source<T> {
    fn series(&self, key: &str) -> Option<&Series> {
        Source::series(self, key)
    }

    fn capacity(&self) -> usize {
        Source::capacity(self)
    }

    fn interval(&self) -> f64 {
        self.interval
    }
}

/// Latest values plus bounded history for one data source.
pub struct Source<T> {
    pub interval: f64,
    pub ok: bool,
    pub error: Option<String>,
    /// Last known good; survives failures.
    pub data: Option<T>,
    /// clock() of the last success.
    pub updated_at: Option<f64>,
    /// Consecutive failed polls since the last success.
    pub failures: u32,
    maxlen: usize,
    series: HashMap<String, Series>,
    filled: HashSet<String>,
    last_poll: Option<f64>,
}

impl<T> Source<T> {
    pub fn new(interval: f64, history_seconds: f64) -> Self {
        Source {
            interval,
            ok: false,
            error: None,
            data: None,
            updated_at: None,
            failures: 0,
            maxlen: ((history_seconds / interval).round() as usize).max(2),
            series: HashMap::new(),
            filled: HashSet::new(),
            last_poll: None,
        }
    }

    /// Ring-buffer length: charts hold their x-axis to it.
    pub fn capacity(&self) -> usize {
        self.maxlen
    }

    pub fn series(&self, key: &str) -> Option<&Series> {
        self.series.get(key)
    }

    /// Append one sample to a named ring buffer.
    ///
    /// None marks a gap in a series that already exists, so series drawn on
    /// one chart stay aligned when one of them misses a value. It never
    /// creates a series.
    pub fn push(&mut self, key: &str, value: Option<f64>) {
        let maxlen = self.maxlen;
        let buf = match self.series.get_mut(key) {
            Some(buf) => buf,
            None if value.is_none() => return,
            None => self.series.entry(key.to_string()).or_default(),
        };
        append(buf, value, maxlen);
        self.filled.insert(key.to_string());
    }

    /// Give every series exactly one slot for the poll that just ended.
    ///
    /// Charts place samples one interval apart, so a failed poll that added
    /// nothing would draw an outage as a continuous line and misdate every
    /// older sample. Each series that got no value this poll gets a gap.
    pub fn end_poll(&mut self) {
        let maxlen = self.maxlen;
        for (key, buf) in &mut self.series {
            if !self.filled.contains(key) {
                append(buf, None, maxlen);
            }
        }
        self.filled.clear();
    }

    /// Insert gaps for polls that never happened: a stall (a suspend, a
    /// stopped process) is drawn as a break, not as compressed time.
    pub fn note_poll(&mut self) {
        let now = clock();
        let previous = self.last_poll.replace(now);
        let Some(previous) = previous else { return };
        let missed = ((now - previous) / self.interval).round() as i64 - 1;
        if missed < 1 {
            return;
        }
        let maxlen = self.maxlen;
        for buf in self.series.values_mut() {
            for _ in 0..(missed as usize).min(maxlen) {
                append(buf, None, maxlen);
            }
        }
    }

    pub fn succeed(&mut self, data: T) {
        self.data = Some(data);
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

    /// True while a source that has never answered is on its first retry.
    ///
    /// A first failure right after launch is usually transient, and PVE
    /// retries only every 10s, so the error is held back until the retry
    /// fails too. A source that never polls (not configured) is never
    /// waiting: its error is final.
    pub fn waiting(&self) -> bool {
        self.updated_at.is_none() && self.failures == 1 && self.last_poll.is_some()
    }

    /// Seconds since the last successful poll, or None if never.
    pub fn age(&self) -> Option<f64> {
        self.updated_at.map(|t| clock() - t)
    }

    pub fn status(&self) -> Status {
        let age = self.age();
        Status {
            ok: self.ok,
            error: if self.waiting() { None } else { self.error.clone() },
            age,
            stale: age.is_some_and(|a| a > self.interval * 2.0),
        }
    }
}

fn append(buf: &mut Series, value: Option<f64>, maxlen: usize) {
    while buf.len() >= maxlen {
        buf.pop_front();
    }
    buf.push_back(value);
}

/// Turns cumulative byte counters into per-second rates.
///
/// `netin`/`netout` and `diskread`/`diskwrite` count from guest boot and
/// reset to zero on restart, so a negative delta means the guest rebooted
/// rather than that throughput went backwards. Those clamp to zero (§3.1).
#[derive(Default)]
pub struct RateTracker {
    previous: HashMap<String, (f64, f64, Option<f64>)>,
}

impl RateTracker {
    /// Bytes/sec since the previous sample of `key`.
    ///
    /// `now` may be any monotonic time base; PVE's guests use their own
    /// uptime, which comes with the counters in one snapshot. A sample at the
    /// same time as the last is that snapshot again and keeps its rate; time
    /// going backwards (a guest's uptime after a reboot) starts over.
    pub fn rate(&mut self, key: &str, value: Option<f64>, now: f64) -> Option<f64> {
        let value = value?;
        let Some(&(prev_time, prev_value, prev_rate)) = self.previous.get(key) else {
            self.previous.insert(key.to_string(), (now, value, None));
            return None; // the first sample has nothing to diff against
        };
        let elapsed = now - prev_time;
        if elapsed == 0.0 {
            return prev_rate;
        }
        if elapsed < 0.0 {
            self.previous.insert(key.to_string(), (now, value, None));
            return None;
        }
        let rate = (value - prev_value).max(0.0) / elapsed;
        self.previous.insert(key.to_string(), (now, value, Some(rate)));
        Some(rate)
    }

    /// Drop counters for guests that have gone away, so a later restart does
    /// not diff against a months-old sample. Keys are "<vmid>.<counter>".
    pub fn keep_only(&mut self, live: &HashSet<String>) {
        self.previous
            .retain(|key, _| live.contains(key.split('.').next().unwrap_or("")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_keep_series_aligned() {
        let mut s: Source<()> = Source::new(1.0, 4.0);
        s.push("a", None); // never creates a series
        assert!(s.series("a").is_none());
        s.push("a", Some(1.0));
        s.push("b", Some(2.0));
        s.end_poll();
        s.push("a", Some(3.0));
        s.end_poll(); // b missed this poll
        assert_eq!(s.series("b").unwrap(), &[Some(2.0), None]);
        for _ in 0..5 {
            s.end_poll();
        }
        assert_eq!(s.series("a").unwrap().len(), 4);
        assert!(s.series("a").unwrap().iter().all(Option::is_none));
    }

    #[test]
    fn waiting_then_failed() {
        let mut s: Source<()> = Source::new(10.0, 60.0);
        s.fail("not configured".into());
        assert!(!s.waiting()); // never polled: final
        let mut s: Source<()> = Source::new(10.0, 60.0);
        s.note_poll();
        s.fail("refused".into());
        assert!(s.waiting());
        assert!(s.status().error.is_none());
        s.fail("refused".into());
        assert_eq!(s.status().error.as_deref(), Some("refused"));
        s.succeed(());
        assert!(s.status().ok && !s.status().stale);
    }

    #[test]
    fn rates() {
        let mut r = RateTracker::default();
        assert_eq!(r.rate("1.netin", Some(100.0), 10.0), None);
        assert_eq!(r.rate("1.netin", Some(300.0), 20.0), Some(20.0));
        // The same snapshot again keeps its rate.
        assert_eq!(r.rate("1.netin", Some(300.0), 20.0), Some(20.0));
        // A counter reset clamps to zero.
        assert_eq!(r.rate("1.netin", Some(50.0), 30.0), Some(0.0));
        // Uptime going backwards starts over.
        assert_eq!(r.rate("1.netin", Some(10.0), 5.0), None);
        r.keep_only(&HashSet::from(["2".to_string()]));
        assert_eq!(r.rate("1.netin", Some(20.0), 6.0), None);
    }
}

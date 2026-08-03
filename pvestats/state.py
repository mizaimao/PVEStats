"""In-memory state. Nothing here is ever written to disk (constraint 3).

A source that fails keeps its last known values and is marked stale with an
age, rather than being zeroed — a blank card and a stale reading have to look
different to a reader (spec §7).
"""

import time
from collections import deque


class Source:
    """Latest values plus bounded history for one data source."""

    def __init__(self, name, interval, history_seconds=600):
        self.name = name
        self.interval = interval
        self.ok = False
        self.error = None
        self.data = {}          # last known good; survives failures
        self.updated_at = None  # time.monotonic() of last success
        # Roughly `history_seconds` of samples at this source's poll rate.
        self.maxlen = max(2, round(history_seconds / interval))
        self.series = {}
        self._last_poll = None

    def resize(self, history_seconds):
        """Change how much history is kept, preserving the newest samples."""
        maxlen = max(2, round(history_seconds / self.interval))
        if maxlen == self.maxlen:
            return
        self.maxlen = maxlen
        for key, buf in self.series.items():
            self.series[key] = deque(buf, maxlen=maxlen)

    def push(self, key, value):
        """Append one sample to a named ring buffer."""
        if value is None:
            return
        buf = self.series.get(key)
        if buf is None:
            buf = self.series[key] = deque(maxlen=self.maxlen)
        buf.append(round(value, 4) if isinstance(value, float) else value)

    def note_poll(self, interval):
        """Insert nulls for polls that never happened.

        Charts place samples one interval apart, so a stall — App Nap, a
        closed lid, a dead source — would otherwise be drawn as a continuous
        line across time that was never sampled. Padding keeps the x-axis
        honest and renders the stall as a visible break.
        """
        now = time.monotonic()
        previous, self._last_poll = self._last_poll, now
        if previous is None:
            return
        missed = int(round((now - previous) / interval)) - 1
        if missed < 1:
            return
        for buf in self.series.values():
            for _ in range(min(missed, buf.maxlen)):
                buf.append(None)

    def succeed(self, data):
        self.data = data
        self.ok = True
        self.error = None
        self.updated_at = time.monotonic()

    def fail(self, error):
        # data is deliberately left alone — last known values stay visible.
        self.ok = False
        self.error = str(error)

    @property
    def age(self):
        """Seconds since the last successful poll, or None if never."""
        if self.updated_at is None:
            return None
        return round(time.monotonic() - self.updated_at, 1)

    def status(self):
        return {
            "ok": self.ok,
            "error": self.error,
            "age": self.age,
            "interval": self.interval,
            # Ring-buffer length, so the page can hold the x-axis fixed instead
            # of rescaling as samples accumulate.
            "capacity": self.maxlen,
            # True once a source has gone missing for more than two polls.
            "stale": self.updated_at is not None
            and (time.monotonic() - self.updated_at) > self.interval * 2,
        }

    def history(self):
        return {key: list(buf) for key, buf in self.series.items()}


class RateTracker:
    """Turns cumulative byte counters into per-second rates.

    `netin`/`netout` and `diskread`/`diskwrite` count from guest boot and reset
    to zero on restart, so a negative delta means the guest rebooted rather
    than that throughput went backwards. Those clamp to zero (spec §3.1).
    """

    def __init__(self):
        self._previous = {}

    def rate(self, key, value, now=None):
        if value is None:
            return None
        now = now if now is not None else time.monotonic()
        previous = self._previous.get(key)
        self._previous[key] = (now, value)
        if previous is None:
            return None  # first sample has nothing to diff against
        prev_time, prev_value = previous
        elapsed = now - prev_time
        if elapsed <= 0:
            return None
        delta = value - prev_value
        if delta < 0:
            delta = 0
        return delta / elapsed

    def forget(self, prefix):
        """Drop counters for a guest that has gone away."""
        for key in [k for k in self._previous if k.startswith(prefix)]:
            del self._previous[key]

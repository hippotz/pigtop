use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::ntstat::SourceSample;

/// How long a burst stays visible as "peak" after the process goes back to idle.
const PEAK_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct PrevCounts {
    rx_bytes: u64,
    tx_bytes: u64,
}

#[derive(Debug, Clone)]
struct PeakInfo {
    name: String,
    peak_rate: f64,
    at: Instant,
}

#[derive(Debug, Clone)]
pub struct ProcessBandwidth {
    pub pid: u32,
    pub name: String,
    pub rx_rate: f64,
    pub tx_rate: f64,
    /// Highest total rate seen for this pid within the trailing PEAK_WINDOW, so a short burst
    /// (e.g. a 10s download) stays visible for a while after it finishes rather than vanishing
    /// the instant the flow that caused it closes.
    pub peak_rate: f64,
    pub peak_age: Duration,
}

impl ProcessBandwidth {
    pub fn total_rate(&self) -> f64 {
        self.rx_rate + self.tx_rate
    }
}

/// Turns raw ntstat snapshots into per-process bandwidth rates, in two decoupled phases so the
/// poll cadence (which needs to be fast to catch short-lived flows, e.g. in "boost" mode) can
/// run independently of the UI refresh cadence (which should stay steady so the displayed number
/// doesn't flicker): `record()` folds one poll's byte deltas into a running per-pid total, cheap
/// enough to call every poll; `snapshot()` converts whatever's accumulated since the *last*
/// snapshot into a rate averaged over the actual wall time elapsed, at whatever cadence the
/// caller wants the UI to update.
///
/// Keeps prior cumulative counters keyed by srcref (a process can own several concurrent flows),
/// so a closed-then-reopened socket that happens to reuse a srcref never sees a stale baseline —
/// any srcref absent from the latest poll is dropped from the baseline before the next one.
pub struct RateTracker {
    prev: HashMap<u64, PrevCounts>,
    accum: HashMap<u32, (String, f64, f64)>,
    peaks: HashMap<u32, PeakInfo>,
    last_snapshot: Instant,
}

impl RateTracker {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            accum: HashMap::new(),
            peaks: HashMap::new(),
            last_snapshot: Instant::now(),
        }
    }

    /// Folds one poll's raw samples into the running per-pid byte totals. Cheap and safe to call
    /// far more often than snapshot() — it never computes a rate or touches the UI-facing state.
    pub fn record(&mut self, samples: &[SourceSample]) {
        let mut seen = HashSet::new();

        for s in samples {
            seen.insert(s.srcref);
            let pid = if s.pid != 0 { s.pid } else { s.epid };

            if let Some(prev) = self.prev.get(&s.srcref) {
                // Counters are cumulative and monotonic; a negative delta means the source was
                // torn down and a new one reused the srcref — treat as 0 rather than letting a
                // bogus negative delta cancel out real traffic accumulated elsewhere this pid.
                let rx_delta = s.rx_bytes.saturating_sub(prev.rx_bytes) as f64;
                let tx_delta = s.tx_bytes.saturating_sub(prev.tx_bytes) as f64;
                let entry = self.accum.entry(pid).or_insert_with(|| (s.pname.clone(), 0.0, 0.0));
                entry.1 += rx_delta;
                entry.2 += tx_delta;
            } else {
                self.accum.entry(pid).or_insert_with(|| (s.pname.clone(), 0.0, 0.0));
            }

            self.prev.insert(s.srcref, PrevCounts { rx_bytes: s.rx_bytes, tx_bytes: s.tx_bytes });
        }

        self.prev.retain(|srcref, _| seen.contains(srcref));
    }

    /// Converts everything accumulated since the last snapshot into a rate (bytes over the
    /// actual elapsed wall time, not the poll interval), resets the accumulator, and returns the
    /// ranked list for display. Call this at whatever cadence you want the UI to refresh.
    pub fn snapshot(&mut self) -> Vec<ProcessBandwidth> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_snapshot).as_secs_f64().max(f64::EPSILON);
        self.last_snapshot = now;

        let per_pid: HashMap<u32, (String, f64, f64)> = std::mem::take(&mut self.accum)
            .into_iter()
            .map(|(pid, (name, rx_bytes, tx_bytes))| {
                (pid, (name, rx_bytes / elapsed, tx_bytes / elapsed))
            })
            .collect();

        // A new high for a pid refreshes its peak (and the peak's age); a pid currently below
        // its own recent peak just leaves the existing peak alone to age out naturally.
        for (&pid, (name, rx_rate, tx_rate)) in &per_pid {
            let total = rx_rate + tx_rate;
            let is_new_high = self.peaks.get(&pid).is_none_or(|p| total >= p.peak_rate);
            if is_new_high {
                self.peaks.insert(pid, PeakInfo { name: name.clone(), peak_rate: total, at: now });
            }
        }
        self.peaks.retain(|_, p| now.duration_since(p.at) <= PEAK_WINDOW);

        let mut pids: HashSet<u32> = per_pid.keys().copied().collect();
        pids.extend(self.peaks.keys().copied());

        let mut result: Vec<ProcessBandwidth> = pids
            .into_iter()
            .map(|pid| {
                let (name, rx_rate, tx_rate) = per_pid
                    .get(&pid)
                    .cloned()
                    .unwrap_or_else(|| (self.peaks[&pid].name.clone(), 0.0, 0.0));
                let (peak_rate, peak_age) = match self.peaks.get(&pid) {
                    Some(p) => (p.peak_rate, now.duration_since(p.at)),
                    None => (0.0, Duration::ZERO),
                };
                ProcessBandwidth { pid, name, rx_rate, tx_rate, peak_rate, peak_age }
            })
            .collect();

        result.sort_by(|a, b| {
            let a_key = a.total_rate().max(a.peak_rate);
            let b_key = b.total_rate().max(b.peak_rate);
            b_key.partial_cmp(&a_key).unwrap()
        });
        result
    }
}

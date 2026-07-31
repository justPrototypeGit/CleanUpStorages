//! Stopping a running scan, and estimating how long one has left.
//!
//! Both concerns are about a scan's *control*, not its work, so they live outside `scanner.rs`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Set by the platform signal handler. A process runs at most one CLI scan, so a global is
/// sufficient; the web path never uses this and drives its per-job `StopFlag` directly.
static CLI_STOP: AtomicBool = AtomicBool::new(false);

/// Cooperative stop request, checked once per file. Cloning shares one flag.
///
/// Cooperative rather than pre-emptive: a scan must never be interrupted mid-file, or it could
/// leave a partially-written row.
#[derive(Clone, Default)]
pub struct StopFlag {
    flag: Arc<AtomicBool>,
    /// Also honour the process-global CLI stop. Only the CLI sets this.
    watch_global: bool,
}

impl StopFlag {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            watch_global: false,
        }
    }

    /// A flag that is also set by the CLI signal handler.
    ///
    /// Consulting the global here rather than mirroring it: a signal handler cannot touch an `Arc`,
    /// so mirroring would need a polling thread that outlives every scan and never exits. One
    /// extra atomic load on a path already doing file I/O costs nothing.
    ///
    /// Unused until the CLI installs the signal handler; remove this attribute then, after which
    /// dead code here is a real defect.
    #[allow(dead_code)]
    fn watching_global() -> Self {
        Self {
            watch_global: true,
            ..Self::new()
        }
    }

    pub fn request(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst) || (self.watch_global && CLI_STOP.load(Ordering::SeqCst))
    }
}

/// How far back the samples used for the rate estimate reach.
const WINDOW_SECS: f64 = 30.0;
/// Below this the window is too short to divide by without producing nonsense.
const MIN_SPAN_SECS: f64 = 1.0;

/// Rolling-rate ETA.
///
/// A lifetime average is the wrong estimator here: throughput swings between roughly 28 MB/s on
/// small files and 125 MB/s on large ones, so an average lags badly in both directions for long
/// stretches. A short window self-corrects as the scan moves between regions.
pub struct EtaTracker {
    started: Instant,
    /// (seconds since start, bytes in that sample)
    samples: VecDeque<(f64, u64)>,
}

impl Default for EtaTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl EtaTracker {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            samples: VecDeque::new(),
        }
    }

    /// Record bytes processed now.
    pub fn record(&mut self, bytes: u64) {
        let at = self.started.elapsed().as_secs_f64();
        self.record_at(bytes, at);
    }

    /// Record bytes at an explicit timestamp (seconds since start). Exposed so tests do not have to
    /// depend on wall-clock timing.
    pub fn record_at(&mut self, bytes: u64, at_secs: f64) {
        self.samples.push_back((at_secs, bytes));
        while let Some(&(t, _)) = self.samples.front() {
            if at_secs - t > WINDOW_SECS {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Bytes per second over the window, or None while the window is too short to be meaningful.
    pub fn rate_bytes_per_sec(&self) -> Option<f64> {
        let now = self.samples.back()?.0;
        self.rate_at(now)
    }

    fn rate_at(&self, now_secs: f64) -> Option<f64> {
        if self.samples.len() < 2 {
            return None;
        }
        let first = self.samples.front()?.0;
        let span = now_secs - first;
        if span < MIN_SPAN_SECS {
            return None;
        }
        // Skip the first sample's bytes: they accumulated before the window opened.
        let bytes: u64 = self.samples.iter().skip(1).map(|&(_, b)| b).sum();
        Some(bytes as f64 / span)
    }

    /// Seconds remaining for `remaining_bytes`, or None without a usable rate.
    pub fn eta_seconds(&self, remaining_bytes: u64) -> Option<u64> {
        let now = self.samples.back()?.0;
        self.eta_seconds_at(remaining_bytes, now)
    }

    /// As `eta_seconds`, at an explicit timestamp. Exposed for tests.
    pub fn eta_seconds_at(&self, remaining_bytes: u64, now_secs: f64) -> Option<u64> {
        let rate = self.rate_at(now_secs)?;
        if rate <= 0.0 {
            return None;
        }
        Some((remaining_bytes as f64 / rate).round() as u64)
    }
}

/// "1h 42m" / "3m 20s" / "45s"
pub fn fmt_duration(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_flag_is_shared_between_clones() {
        let a = StopFlag::new();
        let b = a.clone();
        assert!(!a.is_requested());
        b.request();
        assert!(a.is_requested(), "clones must observe the same flag");
    }

    #[test]
    fn a_plain_flag_ignores_the_cli_global() {
        // Only the CLI's flag watches the global; a web job must not be stopped by it.
        let f = StopFlag::new();
        CLI_STOP.store(true, Ordering::SeqCst);
        let observed = f.is_requested();
        CLI_STOP.store(false, Ordering::SeqCst); // restore for other tests
        assert!(!observed, "a per-job flag must not honour the CLI global");
    }

    #[test]
    fn eta_is_absent_until_there_are_enough_samples() {
        let mut t = EtaTracker::new();
        assert_eq!(t.eta_seconds(1000), None, "no samples yet");
        t.record(100);
        // A single sample over a near-zero window would imply an absurd rate.
        assert_eq!(t.eta_seconds(1000), None, "one sample is not an estimate");
    }

    #[test]
    fn eta_uses_the_observed_rate() {
        let mut t = EtaTracker::new();
        // 10 MB over 2 seconds => 5 MB/s; 20 MB remaining => ~4 s.
        t.record_at(5_000_000, 0.0);
        t.record_at(5_000_000, 2.0);
        let rate = t.rate_bytes_per_sec().expect("a rate after two samples");
        assert!(
            (rate - 2_500_000.0).abs() < 250_000.0,
            "expected ~2.5 MB/s (the window skips the opening sample), got {rate}"
        );
        let eta = t.eta_seconds_at(20_000_000, 2.0).expect("an eta");
        assert!((6..=10).contains(&eta), "expected ~8s, got {eta}");
    }

    #[test]
    fn the_rate_follows_recent_throughput_not_the_lifetime_average() {
        // Slow start then fast: a lifetime average would badly overestimate the time remaining.
        let mut t = EtaTracker::new();
        t.record_at(1_000_000, 0.0);
        t.record_at(1_000_000, 60.0); // ~17 kB/s so far
        t.record_at(50_000_000, 61.0);
        t.record_at(50_000_000, 62.0); // now ~50 MB/s
        let rate = t.rate_bytes_per_sec().unwrap();
        assert!(
            rate > 10_000_000.0,
            "rolling rate should reflect the recent burst, got {rate}"
        );
    }

    #[test]
    fn durations_render_readably() {
        assert_eq!(fmt_duration(45), "45s");
        assert_eq!(fmt_duration(200), "3m 20s");
        assert_eq!(fmt_duration(6120), "1h 42m");
    }
}

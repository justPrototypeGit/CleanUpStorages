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

/// Install a Ctrl+C handler that requests a graceful stop; a second press terminates.
///
/// The returned flag mirrors the global, so callers poll it like any other `StopFlag`.
///
/// **The handler stores to an atomic and does nothing else.** Signal handlers may only call
/// async-signal-safe functions: an atomic store qualifies, and POSIX lists `signal()` as safe.
/// Allocating, locking, or logging from a handler can deadlock the process.
pub fn install_signal_handler() -> StopFlag {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe extern "system" fn handler(_event: u32) -> i32 {
            // Returning FALSE on the second press lets the default handler terminate us.
            if CLI_STOP.swap(true, Ordering::SeqCst) {
                0
            } else {
                1
            }
        }
        SetConsoleCtrlHandler(Some(handler), 1);
    }
    #[cfg(not(windows))]
    unsafe {
        unsafe extern "C" fn handler(_sig: libc::c_int) {
            CLI_STOP.store(true, Ordering::SeqCst);
            // Restore the default disposition so a second Ctrl+C terminates immediately.
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }
    StopFlag::watching_global()
}

use std::sync::Mutex;

/// Live progress for the CLI. Rewrites one line on a terminal; prints periodic lines when piped.
pub struct CliProgress {
    inner: Mutex<CliProgressState>,
    is_tty: bool,
}

struct CliProgressState {
    files: u64,
    bytes: u64,
    total_files: Option<u64>,
    total_bytes: Option<u64>,
    eta: EtaTracker,
    last_paint: Instant,
}

impl CliProgress {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CliProgressState {
                files: 0,
                bytes: 0,
                total_files: None,
                total_bytes: None,
                eta: EtaTracker::new(),
                // Backdated so the first file paints immediately rather than after the interval.
                // `checked_sub` because `Instant - Duration` panics when it would land before the
                // platform's earliest instant — reachable only seconds after boot, but a panic in a
                // constructor is not worth the shorter line.
                last_paint: Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or_else(Instant::now),
            }),
            // Progress goes to stderr so redirecting stdout stays clean; carriage returns are only
            // meaningful on a terminal.
            is_tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
        }
    }

    /// The state, recovered rather than propagated if a previous holder panicked.
    ///
    /// `unwrap()` here would let a panic while painting poison the lock and abort the *next* file,
    /// killing a multi-day scan over a cosmetic counter. Progress is display data: it must never be
    /// able to take down the work it is describing.
    fn state(&self) -> std::sync::MutexGuard<'_, CliProgressState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn paint(&self, st: &mut CliProgressState) {
        use std::io::Write;
        if st.last_paint.elapsed() < std::time::Duration::from_secs(2) {
            return;
        }
        st.last_paint = Instant::now();
        let gb = |b: u64| b as f64 / 1_073_741_824.0;
        let rate = st
            .eta
            .rate_bytes_per_sec()
            .map(|r| format!("{:.1} MB/s", r / 1_048_576.0))
            .unwrap_or_else(|| "—".into());
        let line = match (st.total_files, st.total_bytes) {
            (Some(tf), Some(tb)) if tb > 0 => {
                let pct = (st.bytes as f64 / tb as f64 * 100.0).min(100.0);
                let eta = st
                    .eta
                    .eta_seconds(tb.saturating_sub(st.bytes))
                    .map(fmt_duration)
                    .unwrap_or_else(|| "—".into());
                format!(
                    "Scanning {pct:>3.0}% · {}/{} files · {:.1}/{:.1} GB · {rate} · ETA {eta}",
                    st.files,
                    tf,
                    gb(st.bytes),
                    gb(tb)
                )
            }
            _ => format!(
                "Scanning · {} files · {:.1} GB · {rate}",
                st.files,
                gb(st.bytes)
            ),
        };
        let mut err = std::io::stderr();
        if self.is_tty {
            let _ = write!(err, "\r\x1b[K{line}");
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }

    /// Clear the progress line so following output starts clean.
    pub fn finish(&self) {
        use std::io::Write;
        if self.is_tty {
            let _ = write!(std::io::stderr(), "\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }
}

impl Default for CliProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::scanner::Progress for CliProgress {
    fn on_hashed(&self) {
        let mut st = self.state();
        st.files += 1;
        self.paint(&mut st);
    }
    fn on_skipped(&self) {
        let mut st = self.state();
        st.files += 1;
        self.paint(&mut st);
    }
    fn on_error(&self) {}
    fn on_archive_entry(&self) {}
    fn on_total(&self, files: u64, bytes: u64) {
        let mut st = self.state();
        st.total_files = Some(files);
        st.total_bytes = Some(bytes);
    }
    fn on_bytes(&self, bytes: u64) {
        let mut st = self.state();
        st.bytes += bytes;
        st.eta.record(bytes);
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

    /// `CLI_STOP` is process-global, so the tests that write it must not run concurrently.
    static GLOBAL_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn a_plain_flag_ignores_the_cli_global() {
        // Only the CLI's flag watches the global; a web job must not be stopped by it.
        let _g = GLOBAL_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let f = StopFlag::new();
        CLI_STOP.store(true, Ordering::SeqCst);
        let observed = f.is_requested();
        CLI_STOP.store(false, Ordering::SeqCst); // restore for other tests
        assert!(!observed, "a per-job flag must not honour the CLI global");
    }

    #[test]
    fn the_cli_flag_honours_the_global_the_signal_handler_sets() {
        // The signal handler can only reach an atomic, so this load is the entire path from Ctrl+C
        // to the scan. Without this test, `watch_global: false` would pass every other test while
        // making Ctrl+C silently do nothing for the length of a multi-day scan.
        let _g = GLOBAL_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let f = StopFlag::watching_global();
        assert!(!f.is_requested(), "clean before the handler fires");
        CLI_STOP.store(true, Ordering::SeqCst);
        let observed = f.is_requested();
        CLI_STOP.store(false, Ordering::SeqCst); // restore for other tests
        assert!(observed, "the CLI flag must observe the handler's store");
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

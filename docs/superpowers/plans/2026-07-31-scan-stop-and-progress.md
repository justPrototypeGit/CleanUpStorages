# Stoppable Scans with Progress and ETA — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a multi-day scan be stopped cleanly from the CLI or the web UI, and show a live percentage and ETA while it runs.

**Architecture:** A cooperative `StopFlag` (an `Arc<AtomicBool>`) is threaded into the scan and checked once per file. A metadata-only counting pass runs first to produce a total, so a percentage and a rolling-rate ETA can be shown. A stopped scan commits what it has, **skips the missing-sweep**, and is recorded as `cancelled`.

**Tech Stack:** Rust, rusqlite, axum, walkdir; `windows-sys` (existing dependency, one new feature) and `libc` (already in the lockfile) for signal handling.

**Spec:** [docs/superpowers/specs/2026-07-31-scan-stop-and-progress-design.md](../specs/2026-07-31-scan-stop-and-progress-design.md)

## Global Constraints

- **A scan that did not finish never sweeps.** The `mark_missing_scanned` call is guarded by `if !stopped`. This is the one rule that can lose user-visible state; it gets its own regression test.
- **The stop is cooperative** — checked between files, never interrupting a file mid-hash. No half-written rows.
- **Signal handlers store to an atomic and do nothing else.** No allocation, no locking, no logging: only async-signal-safe operations. An atomic store qualifies; POSIX lists `signal()` as safe.
- **No new crates.** `windows-sys` gains the `Win32_System_Console` feature; `libc` becomes a direct non-Windows dependency (already in the lockfile via `sysinfo`/`rfd`).
- **Resume is not built.** Re-running fast-forwards via the incremental skip. No walk-position checkpoint.
- Progress output goes to **stderr**, with carriage-return updates only when stderr is a terminal.
- Existing scanner tests must pass unmodified.
- Conventional Commits; both trailers:
  `Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Every task ends green: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Work on branch `feat/scan-stop-and-progress`. Do not merge or tag.

## File structure

| File | Responsibility |
| --- | --- |
| `src/scan_control.rs` **(new)** | `StopFlag`, the process-global CLI signal handler, and the rolling-rate `EtaTracker`. One cohesive "control and estimation" unit; keeps `scanner.rs` from growing. |
| `src/lib.rs` | Declares the new module. |
| `src/scanner.rs` | `count_tree()`; `stop` threaded through `scan_volume_with_progress` / `run_scan`; the `if !stopped` sweep guard; `ScanSummary.stopped`. |
| `src/commands.rs` | `cmd_scan` installs the handler, runs the counting pass, prints live progress. |
| `src/main.rs` | `--no-count` flag. |
| `src/scan_queue.rs` | Per-job `StopFlag`; `RunningDto` progress fields; `request_stop()`. |
| `src/web.rs` | `POST /api/scan/stop`. |
| `src/web_ui.rs` | Percentage, ETA and a Stop button on the Scan page. |
| `Cargo.toml` | `Win32_System_Console` feature; `libc` for non-Windows. |

---

### Task 1: `StopFlag` and the ETA tracker

**Files:**
- Create: `src/scan_control.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `StopFlag::new()`, `StopFlag::request()`, `StopFlag::is_requested()`, `StopFlag::clone()`; `EtaTracker::new()`, `EtaTracker::record(bytes: u64)`, `EtaTracker::eta_seconds(remaining_bytes: u64) -> Option<u64>`, `EtaTracker::rate_bytes_per_sec() -> Option<f64>`. Tasks 2–6 use these.

- [ ] **Step 1: Write the failing tests**

Create `src/scan_control.rs` with only this test module for now:

```rust
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
        // Simulate 10 MB over 2 seconds => 5 MB/s; 20 MB remaining => ~4 s.
        t.record_at(5_000_000, 0.0);
        t.record_at(5_000_000, 2.0);
        let rate = t.rate_bytes_per_sec().expect("a rate after two samples");
        assert!(
            (rate - 5_000_000.0).abs() < 500_000.0,
            "expected ~5 MB/s, got {rate}"
        );
        let eta = t.eta_seconds_at(20_000_000, 2.0).expect("an eta");
        assert!((3..=5).contains(&eta), "expected ~4s, got {eta}");
    }

    #[test]
    fn the_rate_follows_recent_throughput_not_the_lifetime_average() {
        // Slow start, then fast: a lifetime average would badly overestimate the remaining time.
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
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib scan_control`
Expected: FAIL — module not declared / `StopFlag` not found.

- [ ] **Step 3: Implement**

Put this above the test module in `src/scan_control.rs`:

```rust
//! Stopping a running scan, and estimating how long one has left.
//!
//! Both concerns are about a scan's *control*, not its work, so they live outside `scanner.rs`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Cooperative stop request, checked once per file. Cloning shares one flag.
///
/// Cooperative rather than pre-emptive: a scan must never be interrupted mid-file, or it could
/// leave a partially-written row.
#[derive(Clone, Default)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How long the samples used for the rate estimate reach back.
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

    /// Record bytes at an explicit timestamp (seconds since start). Exposed for tests, which must
    /// not depend on wall-clock timing.
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

    /// Bytes per second over the window, or None when the window is too short to be meaningful.
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
```

- [ ] **Step 4: Declare the module**

In `src/lib.rs`, alongside the other `pub mod` lines:

```rust
pub mod scan_control;
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib scan_control`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/scan_control.rs src/lib.rs
git commit -m "feat(scanner): stop flag and a rolling-rate ETA tracker

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Counting pass and the stop-aware scan

**Files:**
- Modify: `src/scanner.rs`

**Interfaces:**
- Consumes: `StopFlag` (Task 1).
- Produces: `pub struct TreeTotals { pub files: u64, pub bytes: u64 }`; `pub fn count_tree(root: &Path, stop: &StopFlag) -> TreeTotals`; `ScanSummary.stopped: bool`; `scan_volume_with_progress(..., stop: &StopFlag)` and `run_scan(..., stop: &StopFlag)` gain a trailing `stop` parameter.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/scanner.rs`:

```rust
#[test]
fn count_tree_totals_files_and_bytes_and_skips_the_marker() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("drive");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.bin"), vec![b'x'; 100]).unwrap();
    std::fs::write(root.join("sub/b.bin"), vec![b'y'; 250]).unwrap();
    // The identity marker is skipped by the scan, so it must not be counted either.
    std::fs::write(root.join(crate::volume::MARKER), b"vol-1").unwrap();

    let totals = count_tree(&root, &crate::scan_control::StopFlag::new());
    assert_eq!(totals.files, 2);
    assert_eq!(totals.bytes, 350);
}

#[test]
fn count_tree_returns_promptly_when_stopped() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..200 {
        std::fs::write(root.join(format!("f{i}.bin")), b"x").unwrap();
    }
    let stop = crate::scan_control::StopFlag::new();
    stop.request(); // already requested before we start
    let totals = count_tree(&root, &stop);
    assert!(totals.files < 200, "counting should stop early, got {}", totals.files);
}

#[test]
fn a_stopped_scan_sweeps_nothing_and_reports_stopped() {
    // THE rule: a scan that did not finish must never mark files missing. Without the guard, every
    // file the walk had not reached yet would be flagged as gone.
    let (tmp, cat) = setup();
    let root = tmp.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"one").unwrap();
    std::fs::write(root.join("b.txt"), b"two").unwrap();

    // First pass catalogues both files.
    let m = crate::scan_metrics::ScanMetrics::new();
    let stop = crate::scan_control::StopFlag::new();
    scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();
    assert_eq!(cat.search("", None, None, Some("active")).unwrap().len(), 2);

    // Second pass is stopped before it starts: nothing is re-seen, so an unguarded sweep would
    // mark BOTH files missing.
    let stop2 = crate::scan_control::StopFlag::new();
    stop2.request();
    let m2 = crate::scan_metrics::ScanMetrics::new();
    let s = scan_volume_with_progress(&cat, &root, &ident(), false, 300, None, &m2, &stop2).unwrap();

    assert!(s.stopped, "the summary must report that it was stopped");
    assert_eq!(s.marked_missing, 0, "a stopped scan must not sweep");
    assert_eq!(
        cat.search("", None, None, Some("active")).unwrap().len(),
        2,
        "both files are still on disk and must stay active"
    );
}

#[test]
fn an_unstopped_scan_still_sweeps() {
    // The guard must not disable the feature: a genuinely deleted file still becomes missing.
    let (tmp, cat) = setup();
    let root = tmp.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("gone.txt"), b"bye").unwrap();

    let m = crate::scan_metrics::ScanMetrics::new();
    let stop = crate::scan_control::StopFlag::new();
    scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();

    std::fs::remove_file(root.join("gone.txt")).unwrap();
    let m2 = crate::scan_metrics::ScanMetrics::new();
    let s = scan_volume_with_progress(&cat, &root, &ident(), false, 300, None, &m2, &stop).unwrap();
    assert!(!s.stopped);
    assert_eq!(s.marked_missing, 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib scanner::tests::a_stopped_scan_sweeps_nothing`
Expected: FAIL — `count_tree` not found and `scan_volume_with_progress` has the wrong arity.

- [ ] **Step 3: Add `stopped` to `ScanSummary`**

In `src/scanner.rs`, in the `ScanSummary` struct:

```rust
    /// True when the scan ended on a stop request rather than reaching the end of the tree.
    /// A stopped scan must not run the missing-sweep.
    pub stopped: bool,
```

- [ ] **Step 4: Implement `count_tree`**

Add to `src/scanner.rs`, above `scan_volume_with_progress`:

```rust
/// Files and bytes a scan of `root` would process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeTotals {
    pub files: u64,
    pub bytes: u64,
}

/// Count the tree without reading any file contents — `readdir` + `stat` only.
///
/// This is what makes a real percentage possible for a folder that has never been scanned, which is
/// most of a first pass. It costs a metadata walk, so it scales with file count rather than with
/// terabytes. Errors are ignored: this is an estimate, and a directory the scan cannot read is
/// reported by the scan itself.
pub fn count_tree(root: &Path, stop: &crate::scan_control::StopFlag) -> TreeTotals {
    let mut totals = TreeTotals::default();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if stop.is_requested() {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if should_skip(entry.path(), entry.file_name()) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            totals.files += 1;
            totals.bytes += meta.len();
        }
    }
    totals
}
```

- [ ] **Step 5: Thread `stop` through the scan**

In `scan_volume_with_progress`, add `stop: &crate::scan_control::StopFlag` as the final parameter, and at the top of the walk loop (immediately after the `let Some(entry) = next else { break };` line) add:

```rust
        if stop.is_requested() {
            summary.stopped = true;
            break;
        }
```

Then guard the sweep. Replace the existing sweep block:

```rust
    {
        let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
        cat.conn.execute_batch("COMMIT")?;
        // THE rule: a scan that did not finish never sweeps. Every file the walk had not reached
        // yet looks untouched, so sweeping here would mark present files as missing.
        if !summary.stopped {
            summary.marked_missing =
                cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now, &unreadable_dirs)?;
        }
    }
```

- [ ] **Step 6: Thread `stop` through `run_scan` and record `cancelled`**

Add `stop: &crate::scan_control::StopFlag` as the final parameter of `run_scan`, pass it to `scan_volume_with_progress`, and change the status recorded on success so a stopped run is not called `completed`:

```rust
            Ok(summary) => {
                let status = if summary.stopped { "cancelled" } else { "completed" };
                cat.finish_scan_run(id, finished_at, status, None, summary)
            }
```

- [ ] **Step 7: Update every other call site**

`scan_volume` passes `&crate::scan_control::StopFlag::new()`. Test call sites and `src/scan_queue.rs` pass a flag as well; the compiler lists them:

```bash
cargo test --no-run 2>&1 | grep -E "^error\[E0061\]" -A4
```

- [ ] **Step 8: Run the scanner suite**

Run: `cargo test --lib scanner`
Expected: PASS — the four new tests plus every pre-existing scanner test, unmodified.

- [ ] **Step 9: Commit**

```bash
git add src/scanner.rs src/scan_queue.rs
git commit -m "feat(scanner): stoppable scan and a metadata-only counting pass

A stopped scan commits what it hashed, skips the missing-sweep, and reports
stopped so run_scan records it as cancelled. Skipping the sweep is the whole
point: without it, stopping would mark every unreached file missing.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Progress trait extensions

**Files:**
- Modify: `src/scanner.rs` (the `Progress` trait)

**Interfaces:**
- Produces: `Progress::on_total(&self, files: u64, bytes: u64)` and `Progress::on_bytes(&self, bytes: u64)`, both with default no-op bodies so existing implementors keep compiling.

- [ ] **Step 1: Extend the trait**

In `src/scanner.rs`:

```rust
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
    /// Totals from the counting pass. Never called when counting is skipped, so a percentage is
    /// absent rather than wrong.
    fn on_total(&self, _files: u64, _bytes: u64) {}
    /// Bytes of the file just finished, hashed or skipped. Drives the rate and the ETA.
    fn on_bytes(&self, _bytes: u64) {}
}
```

- [ ] **Step 2: Emit `on_bytes` from the scan**

In `scan_volume_with_progress`, in the incremental-skip branch, immediately after `metrics.add_bytes_skipped(size);`:

```rust
                    if let Some(p) = progress {
                        p.on_bytes(size as u64);
                    }
```

and after the hashed file's `metrics.add_bytes_hashed(size);`:

```rust
        if let Some(p) = progress {
            p.on_bytes(size as u64);
        }
```

- [ ] **Step 3: Verify nothing broke**

Run: `cargo test`
Expected: PASS. The default bodies mean `Counters` and the test `CountingProgress` need no change.

- [ ] **Step 4: Commit**

```bash
git add src/scanner.rs
git commit -m "feat(scanner): report totals and per-file bytes to Progress

Default no-op bodies, so existing implementors are untouched.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: CLI — signal handler, counting pass, live progress

**Files:**
- Modify: `Cargo.toml`, `src/scan_control.rs`, `src/commands.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `StopFlag`, `EtaTracker` (Task 1); `count_tree`, `TreeTotals` (Task 2); `Progress::on_total`/`on_bytes` (Task 3).
- Produces: `scan_control::install_signal_handler() -> StopFlag`; `cmd_scan(path, force, fallback, no_count)`.

- [ ] **Step 1: Add the dependencies**

In `Cargo.toml`, add `"Win32_System_Console"` to the existing `windows-sys` feature list, and add a non-Windows dependency section:

```toml
[target.'cfg(not(windows))'.dependencies]
libc = "0.2"
```

- [ ] **Step 2: Implement the handler**

Append to `src/scan_control.rs` (above the test module):

```rust
/// Set by the platform signal handler. A process runs at most one CLI scan, so a global is
/// sufficient; the web path never uses this and drives its per-job `StopFlag` directly.
static CLI_STOP: AtomicBool = AtomicBool::new(false);

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
```

`StopFlag` gains a variant that also consults the global, so the handler needs no thread and nothing
has to be mirrored. Replace the struct and its impl from Task 1 with:

```rust
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
        self.flag.load(Ordering::SeqCst)
            || (self.watch_global && CLI_STOP.load(Ordering::SeqCst))
    }
}
```

> **Why not mirror the global into the Arc with a watcher thread:** a signal handler cannot touch an
> `Arc`, so mirroring needs a polling thread that outlives every scan and never exits. Consulting the
> global inside `is_requested` costs one relaxed load on a path already doing file I/O, and adds no
> thread and no lifetime question.

- [ ] **Step 3: CLI progress printer**

Append to `src/scan_control.rs` (above the test module):

```rust
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
                last_paint: Instant::now() - std::time::Duration::from_secs(10),
            }),
            // Progress goes to stderr so redirecting stdout stays clean; carriage returns are only
            // meaningful on a terminal.
            is_tty: std::io::IsTerminal::is_terminal(&std::io::stderr()),
        }
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

/// "1h 42m" / "3m 20s" / "45s"
pub fn fmt_duration(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

impl crate::scanner::Progress for CliProgress {
    fn on_hashed(&self) {
        let mut st = self.inner.lock().unwrap();
        st.files += 1;
        self.paint(&mut st);
    }
    fn on_skipped(&self) {
        let mut st = self.inner.lock().unwrap();
        st.files += 1;
        self.paint(&mut st);
    }
    fn on_error(&self) {}
    fn on_archive_entry(&self) {}
    fn on_total(&self, files: u64, bytes: u64) {
        let mut st = self.inner.lock().unwrap();
        st.total_files = Some(files);
        st.total_bytes = Some(bytes);
    }
    fn on_bytes(&self, bytes: u64) {
        let mut st = self.inner.lock().unwrap();
        st.bytes += bytes;
        st.eta.record(bytes);
    }
}
```

Add a test for the formatter to the test module:

```rust
    #[test]
    fn durations_render_readably() {
        assert_eq!(fmt_duration(45), "45s");
        assert_eq!(fmt_duration(200), "3m 20s");
        assert_eq!(fmt_duration(6120), "1h 42m");
    }
```

- [ ] **Step 4: Wire `cmd_scan`**

In `src/commands.rs`, change the signature and body:

```rust
pub fn cmd_scan(
    path: &Path,
    force: bool,
    fallback: ReadonlyFallback,
    no_count: bool,
) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog_checked()?;
    let now = now_secs();
    let stop = crate::scan_control::install_signal_handler();
    let progress = crate::scan_control::CliProgress::new();

    if !no_count {
        eprintln!("Counting files…");
        let totals = scanner::count_tree(path, &stop);
        eprintln!(
            "Counting… {} files ({:.1} GB)",
            totals.files,
            totals.bytes as f64 / 1_073_741_824.0
        );
        crate::scanner::Progress::on_total(&progress, totals.files, totals.bytes);
    }

    let outcome = scanner::run_scan(&cat, path, force, fallback.into(), now, Some(&progress), &stop);
    progress.finish();
    match outcome? {
        None => {
            println!("Skipped read-only drive at {}", path.display());
            return Ok(());
        }
        Some((identity, s)) => {
            println!(
                "Scanned {} (volume {}, id by {})",
                path.display(),
                identity.label,
                identity.identified_by
            );
            if s.stopped {
                println!("STOPPED before the end of the tree — nothing was marked missing.");
                println!("Re-run the same command to continue; catalogued files are skipped fast.");
            }
            println!(
                "Done: {} hashed, {} unchanged, {} errors, {} newly missing, {} archive entries.",
                s.hashed, s.skipped, s.errors, s.marked_missing, s.archive_entries
            );
            print!("{}", s.metrics.report());
        }
    }
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}
```

- [ ] **Step 5: Add `--no-count` to the CLI**

In `src/main.rs`, in the `Scan` subcommand:

```rust
        /// Skip the pre-scan counting pass. Faster to start; no percentage or ETA.
        #[arg(long)]
        no_count: bool,
```

and in the dispatch: `Command::Scan { path, force, readonly_fallback, no_count } => commands::cmd_scan(&path, force, readonly_fallback, no_count),`

- [ ] **Step 6: Full gates**

Run: `cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 7: Manual check**

Run a scan of a small folder and confirm: a progress line appears, Ctrl+C stops it within a file, and the summary says STOPPED with `0 newly missing`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/scan_control.rs src/commands.rs src/main.rs
git commit -m "feat(cli): Ctrl+C stops a scan, with live progress and an ETA

Signal handling is hand-rolled on the existing windows-sys and libc, so no
new crate is added. The handler stores to an atomic and nothing else --
signal handlers may only do async-signal-safe work.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Web — stop endpoint and progress payload

**Files:**
- Modify: `src/scan_queue.rs`, `src/web.rs`, `src/web_ui.rs`

**Interfaces:**
- Consumes: `StopFlag` (Task 1); `count_tree` (Task 2); `Progress::on_total`/`on_bytes` (Task 3).
- Produces: `ScanQueue::request_stop()`; `POST /api/scan/stop`; `RunningDto` progress fields.

- [ ] **Step 1: Write the failing web test**

Append to the test module in `src/web.rs`:

```rust
    #[tokio::test]
    async fn scan_stop_requires_csrf_and_answers_when_idle() {
        let (_t, _db, state) = seed_dupes();
        // Without a token the write endpoint must refuse, like every other write endpoint.
        let app = super::router(state.clone());
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/scan/stop")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);

        // With a token, stopping while nothing runs is a no-op success, not an error.
        let (code, v) = post_json(
            state,
            "/api/scan/stop",
            Some(super::TEST_TOKEN),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(code, axum::http::StatusCode::OK);
        assert_eq!(v["stopping"], false, "nothing was running");
    }
```

> The helper is `post_json(state, uri, token: Option<&str>, body: serde_json::Value) ->
> (StatusCode, serde_json::Value)`, already in this test module. Use the same token constant the
> other write-endpoint tests pass — check how `quarantine_requires_csrf_token` obtains it and match
> that exactly rather than inventing a name.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib web::tests::scan_stop_requires_csrf`
Expected: FAIL — route not found (404, not 403).

- [ ] **Step 3: Per-job stop flag in the queue**

In `src/scan_queue.rs`: store a `StopFlag` on the running job, create a fresh one per job, pass it to `run_scan`, and add:

```rust
    /// Ask the running scan to stop. Returns false when nothing is running.
    pub fn request_stop(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        match inner.running.as_ref() {
            Some(r) => {
                r.stop.request();
                true
            }
            None => false,
        }
    }
```

Run the counting pass before `run_scan` in the worker and feed `on_total`, so the web UI gets a percentage too:

```rust
            let totals = crate::scanner::count_tree(&path, &stop);
            crate::scanner::Progress::on_total(progress, totals.files, totals.bytes);
```

- [ ] **Step 4: Progress fields on `RunningDto`**

```rust
pub struct RunningDto {
    pub path: String,
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub archive_entries: usize,
    pub done_bytes: u64,
    pub total_files: Option<u64>,
    pub total_bytes: Option<u64>,
    pub eta_seconds: Option<u64>,
}
```

`Counters` gains `done_bytes: AtomicU64`, `total_files`/`total_bytes` (as `AtomicU64` with 0 meaning
absent), and an `EtaTracker` behind a `Mutex`, updated in `on_bytes`.

- [ ] **Step 5: The route**

In `src/web.rs`, register `.route("/api/scan/stop", post(api_scan_stop))` and add:

```rust
/// Ask the running scan to stop. Idempotent, and harmless when nothing is running.
async fn api_scan_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    check_csrf(&headers, &state)?;
    let stopping = state.scans.request_stop();
    Ok(Json(serde_json::json!({ "stopping": stopping })))
}
```

- [ ] **Step 6: Scan page UI**

In `src/web_ui.rs`, in the live-status block, render a percentage and ETA when present and add a Stop button:

```javascript
      const pct = (s.running.total_bytes && s.running.total_bytes > 0)
        ? Math.min(100, Math.round(s.running.done_bytes * 100 / s.running.total_bytes)) : null;
      const eta = s.running.eta_seconds != null ? fmtEta(s.running.eta_seconds) : null;
      $("#status-sub").textContent =
        (pct != null ? pct + "% · " : "") +
        `${r.hashed} hashed · ${r.skipped} unchanged` +
        (eta != null ? ` · ETA ${eta}` : "");
      $("#stopscan").hidden = false;
```

with the button and handler:

```html
<button class="btn" id="stopscan" hidden>Stop scan</button>
```

```javascript
$("#stopscan").addEventListener("click", async ()=>{
  $("#stopscan").disabled = true;
  try {
    await apiPost("/api/scan/stop", {});
    $("#msg").textContent = "Stopping after the current file… nothing will be marked missing.";
  } catch(e) { $("#msg").textContent = "Could not stop: " + e; }
  $("#stopscan").disabled = false;
});
function fmtEta(s){ return s>=3600 ? `${Math.floor(s/3600)}h ${String(Math.floor(s%3600/60)).padStart(2,"0")}m`
  : s>=60 ? `${Math.floor(s/60)}m ${String(s%60).padStart(2,"0")}s` : `${s}s`; }
```

- [ ] **Step 7: Gates and manual check**

Run: `cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Then build release, scan a folder from the web UI, and confirm the percentage, the ETA and that Stop
ends the scan.

- [ ] **Step 8: Commit**

```bash
git add src/scan_queue.rs src/web.rs src/web_ui.rs
git commit -m "feat(web): stop a running scan, with percentage and ETA

POST /api/scan/stop sets the running job's stop flag. worker.abort() could
not do this -- aborting a tokio task does not interrupt blocking work.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Measure the resume cost and document it

**Files:**
- Modify: `docs/benchmarking-scans.md`, `README.md`

- [ ] **Step 1: Measure**

On a real folder already catalogued, run `cleanupstorages scan <folder>` (no `--force`) and record the
wall time against the original full scan of the same folder. This is the fast-forward cost that
justifies not building checkpointed resume.

- [ ] **Step 2: Document**

Add to `docs/benchmarking-scans.md` a "Stopping and resuming a scan" section with the measured
numbers, and to the README's scan section:

```markdown
A scan can be stopped at any time — Ctrl+C, or the Stop button in the web UI. It finishes the
current file, keeps everything it has hashed, and **never marks anything missing**. Re-run the same
command to continue: already-catalogued files are skipped without re-reading them.
```

- [ ] **Step 3: Commit**

```bash
git add docs README.md
git commit -m "docs: how to stop and resume a scan, with the measured fast-forward cost

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

# Scan Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure where a scan's time actually goes — split five ways, persisted per run, reported in the CLI and on the Scan page — so every later child of epic #21 can be proven rather than asserted.

**Architecture:** A new `src/scan_metrics.rs` accumulates phase durations and counters in `AtomicU64`s, driven by RAII timer guards. The scanner owns one instance per scan and returns a `MetricsSnapshot` on `ScanSummary`. `run_scan` — the single shared entry point for both the CLI and the web worker — writes a `scan_runs` row at start and updates it at the end, outside the scan's long transaction.

**Tech Stack:** Rust, rusqlite (bundled SQLite), axum, plain HTML/CSS/JS.

**Spec:** [docs/superpowers/specs/2026-07-22-scan-instrumentation-design.md](../specs/2026-07-22-scan-instrumentation-design.md)

**Two deliberate deviations from the spec's architecture sketch** (behaviour and stored data unchanged):

1. **The scanner owns `ScanMetrics`; it is not passed in as a parameter.** The spec sketched `&ScanMetrics` alongside `Option<&dyn Progress>`. Passing it would change the signature of `scan_volume_with_progress` and every one of its ~12 call sites (mostly tests) for no gain, since live per-phase display is an explicit non-goal. `ScanMetrics` is still `Send + Sync`, so #23 wraps it in an `Arc` locally and hands clones to workers — the property that mattered is preserved.
2. **Counters have exactly one owner each.** `ScanSummary` keeps `hashed`/`skipped`/`errors`/`archive_entries` (it already owns them, and tests assert on them); `ScanMetrics` owns only what is new — phase timings, `files_seen`, `bytes_hashed`, `bytes_skipped`, and the histogram. Duplicating the existing counters into `ScanMetrics` would create two sources of truth that can drift. `finish_scan_run` takes both structs.

**One spec test deliberately not built.** The spec's risk table proposes asserting that an
instrumented scan "stays within tolerance of the same scan uninstrumented". There is no honest way
to write that — once the code is instrumented there is no uninstrumented scan to compare against,
and a wall-clock tolerance on a shared CI runner measures the runner, not the overhead. What
replaces it: the per-file cost is two `Instant::now()` calls and one relaxed `fetch_add` (tens of
nanoseconds against millisecond-scale disk I/O), and `overlap_ratio()` makes any pathological
timing overhead visible as an `accounted` figure that approaches wall-clock on a scan doing real
I/O. If overhead is ever suspected, the measurement is a one-off `git stash` A/B, not a CI test.

## Global Constraints

- **Instrumentation must not change scan behaviour.** Existing scan tests pass unmodified. Hashes and catalogue contents are unaffected.
- **A metrics failure must never fail a scan.** Every `scan_runs` write is logged-and-swallowed. Losing a measurement is acceptable; losing a scan is not.
- **Phases are disjoint by construction.** No timer guard may be alive inside another guard's scope. `archive` is timed as a whole and is *not* decomposed.
- **No lower bound is asserted** on sum-of-phases ÷ wall-clock. Tests assert `sum <= wall` and that each phase is non-zero on a tree that exercises it. Untimed glue legitimately sits outside every phase.
- Histogram buckets are **half-open on the upper bound**: `4KiB–64KiB` means `4096 <= size < 65536`.
- Histogram is over **files seen** during the loose-file walk (including skipped files). Archive entries are counted by `archive_entries` only.
- `status` values: `running` | `completed` | `failed` | `cancelled`. **`cancelled` is unreachable today** — reserved for #5.
- This issue only measures. **No optimisation** — no buffer sizes, no `prepare_cached`, no pragma changes. Those are #23/#24/#26.
- Conventional Commits; both trailers:
  `Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Every task ends green: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Branch `feat/scan-instrumentation`. Do not merge, tag or push.

## File structure

| File | Responsibility in this change |
| --- | --- |
| `src/scan_metrics.rs` **(new)** | Phase enum, atomic accumulator, RAII timer, snapshot, histogram bucketing, human-readable report formatting. One cohesive responsibility; keeps `scanner.rs` (607 lines) from growing further. |
| `src/lib.rs` | Declares the new module. |
| `src/scanner.rs` | Owns a `ScanMetrics` per scan, wraps each phase in a timer, returns the snapshot on `ScanSummary`; `run_scan` writes the `scan_runs` row. |
| `src/catalog/schema.rs` | Adds the `scan_runs` table + index. |
| `src/catalog/scan_runs.rs` **(new)** | `start_scan_run` / `finish_scan_run` / `recent_scan_runs` + the `ScanRun` row type. |
| `src/catalog/mod.rs` | Declares the new catalog submodule. |
| `src/commands.rs` | `cmd_scan` prints the breakdown. |
| `src/web.rs` | `GET /api/scan-runs`. |
| `src/web_ui.rs` | Scan page "Recent scans" panel. |
| `docs/benchmarking-scans.md` **(new)** | The measurement protocol and its three traps. |

---

### Task 1: `scan_metrics` module

**Files:**
- Create: `src/scan_metrics.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `Phase`, `ScanMetrics::new()`, `ScanMetrics::timer(Phase) -> PhaseTimer`, `ScanMetrics::record_file_seen(i64)`, `ScanMetrics::add_bytes_hashed(i64)`, `ScanMetrics::add_bytes_skipped(i64)`, `ScanMetrics::snapshot() -> MetricsSnapshot`, `MetricsSnapshot` (public fields below), `bucket_for(i64) -> usize`, `BUCKET_LABELS`. Tasks 2–6 rely on all of these.

- [ ] **Step 1: Write the failing tests**

Create `src/scan_metrics.rs` containing ONLY the test module for now (the code follows in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_half_open_on_the_upper_bound() {
        assert_eq!(bucket_for(0), 0);
        assert_eq!(bucket_for(1), 1);
        assert_eq!(bucket_for(4095), 1);
        assert_eq!(bucket_for(4096), 2, "4 KiB starts the 4KiB-64KiB bucket");
        assert_eq!(bucket_for(65535), 2);
        assert_eq!(bucket_for(65536), 3, "64 KiB starts the 64KiB-1MiB bucket");
        assert_eq!(bucket_for(1_048_575), 3);
        assert_eq!(bucket_for(1_048_576), 4);
        assert_eq!(bucket_for(16_777_215), 4);
        assert_eq!(bucket_for(16_777_216), 5);
        assert_eq!(bucket_for(268_435_455), 5);
        assert_eq!(bucket_for(268_435_456), 6);
        assert_eq!(bucket_for(i64::MAX), 6);
    }

    #[test]
    fn labels_cover_every_bucket() {
        assert_eq!(BUCKET_LABELS.len(), BUCKET_COUNT);
        assert_eq!(bucket_for(i64::MAX), BUCKET_COUNT - 1);
    }

    #[test]
    fn recording_files_fills_counters_and_histogram() {
        let m = ScanMetrics::new();
        m.record_file_seen(0);
        m.record_file_seen(5_000);
        m.record_file_seen(5_000);
        m.add_bytes_hashed(10_000);
        m.add_bytes_skipped(7);
        let s = m.snapshot();
        assert_eq!(s.files_seen, 3);
        assert_eq!(s.histogram[0], 1, "the empty file");
        assert_eq!(s.histogram[2], 2, "two 5 KB files");
        assert_eq!(s.bytes_hashed, 10_000);
        assert_eq!(s.bytes_skipped, 7);
    }

    #[test]
    fn timer_accumulates_into_its_own_phase_only() {
        let m = ScanMetrics::new();
        {
            let _t = m.timer(Phase::Hash);
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        let s = m.snapshot();
        assert!(s.hash_ms >= 10, "expected >=10ms, got {}", s.hash_ms);
        assert_eq!(s.walk_ms, 0);
        assert_eq!(s.skip_check_ms, 0);
        assert_eq!(s.db_write_ms, 0);
        assert_eq!(s.archive_ms, 0);
    }

    #[test]
    fn phases_never_exceed_wall_clock() {
        let m = ScanMetrics::new();
        {
            let _t = m.timer(Phase::Walk);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let _t = m.timer(Phase::DbWrite);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let s = m.snapshot();
        assert!(
            s.total_phase_ms() <= s.wall_ms,
            "phases {} exceeded wall {}",
            s.total_phase_ms(),
            s.wall_ms
        );
    }

    #[test]
    fn metrics_are_shareable_across_threads() {
        // Guards the property #23 depends on: this must compile and stay true.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScanMetrics>();
    }

    #[test]
    fn report_names_every_phase_and_is_divide_by_zero_safe() {
        let s = MetricsSnapshot::default();
        let out = s.report();
        for name in ["walk", "skip_check", "hash", "db_write", "archive"] {
            assert!(out.contains(name), "report missing {name}: {out}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib scan_metrics`
Expected: FAIL — the module isn't declared yet, so this reports `error[E0583]: file not found for module` or unresolved names once declared. Both count as red.

- [ ] **Step 3: Write the implementation**

Put this ABOVE the `#[cfg(test)] mod tests` block in `src/scan_metrics.rs`:

```rust
//! Where a scan's time goes.
//!
//! Phase timings and counters accumulate into atomics, so the same accumulator works unchanged
//! when #23 hashes on worker threads. Cost per file is two `Instant::now()` calls and a relaxed
//! `fetch_add` — nanoseconds against millisecond-scale disk I/O.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// A disjoint slice of scan work. No timer for one phase may be alive inside another's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Walk,
    SkipCheck,
    Hash,
    DbWrite,
    Archive,
}

impl Phase {
    const COUNT: usize = 5;
    fn index(self) -> usize {
        match self {
            Phase::Walk => 0,
            Phase::SkipCheck => 1,
            Phase::Hash => 2,
            Phase::DbWrite => 3,
            Phase::Archive => 4,
        }
    }
}

pub const BUCKET_COUNT: usize = 7;

/// Human labels for the size histogram, in bucket order.
pub const BUCKET_LABELS: [&str; BUCKET_COUNT] = [
    "0",
    "1B-4KiB",
    "4KiB-64KiB",
    "64KiB-1MiB",
    "1MiB-16MiB",
    "16MiB-256MiB",
    ">256MiB",
];

/// Bucket index for a file size. Upper bounds are exclusive: 4096 lands in `4KiB-64KiB`.
/// The 64 KiB boundary is deliberate — it is both the current read-buffer size and the line the
/// seek-bound hypothesis turns on.
pub fn bucket_for(size_bytes: i64) -> usize {
    match size_bytes {
        s if s <= 0 => 0,
        s if s < 4_096 => 1,
        s if s < 65_536 => 2,
        s if s < 1_048_576 => 3,
        s if s < 16_777_216 => 4,
        s if s < 268_435_456 => 5,
        _ => 6,
    }
}

/// Accumulator for one scan. `Send + Sync` so #23 can share it across workers via `Arc`.
pub struct ScanMetrics {
    started: Instant,
    phase_ns: [AtomicU64; Phase::COUNT],
    files_seen: AtomicU64,
    bytes_hashed: AtomicU64,
    bytes_skipped: AtomicU64,
    histogram: [AtomicU64; BUCKET_COUNT],
}

impl Default for ScanMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanMetrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            phase_ns: Default::default(),
            files_seen: AtomicU64::new(0),
            bytes_hashed: AtomicU64::new(0),
            bytes_skipped: AtomicU64::new(0),
            histogram: Default::default(),
        }
    }

    /// Start timing `phase`. The elapsed time is added when the returned guard drops.
    pub fn timer(&self, phase: Phase) -> PhaseTimer<'_> {
        PhaseTimer {
            metrics: self,
            phase,
            started: Instant::now(),
        }
    }

    /// Count a file the walk considered — including one about to be skipped, because a skipped
    /// file still costs a seek and a stat, which is the cost under investigation.
    pub fn record_file_seen(&self, size_bytes: i64) {
        self.files_seen.fetch_add(1, Relaxed);
        self.histogram[bucket_for(size_bytes)].fetch_add(1, Relaxed);
    }

    pub fn add_bytes_hashed(&self, n: i64) {
        self.bytes_hashed.fetch_add(n.max(0) as u64, Relaxed);
    }

    pub fn add_bytes_skipped(&self, n: i64) {
        self.bytes_skipped.fetch_add(n.max(0) as u64, Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let ms = |i: usize| self.phase_ns[i].load(Relaxed) / 1_000_000;
        let mut histogram = [0u64; BUCKET_COUNT];
        for (i, slot) in self.histogram.iter().enumerate() {
            histogram[i] = slot.load(Relaxed);
        }
        MetricsSnapshot {
            wall_ms: self.started.elapsed().as_millis() as u64,
            walk_ms: ms(0),
            skip_check_ms: ms(1),
            hash_ms: ms(2),
            db_write_ms: ms(3),
            archive_ms: ms(4),
            files_seen: self.files_seen.load(Relaxed),
            bytes_hashed: self.bytes_hashed.load(Relaxed),
            bytes_skipped: self.bytes_skipped.load(Relaxed),
            histogram,
        }
    }
}

/// Adds its lifetime to one phase when dropped.
pub struct PhaseTimer<'a> {
    metrics: &'a ScanMetrics,
    phase: Phase,
    started: Instant,
}

impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        let ns = self.started.elapsed().as_nanos() as u64;
        self.metrics.phase_ns[self.phase.index()].fetch_add(ns, Relaxed);
    }
}

/// A point-in-time copy of the accumulator. Milliseconds throughout — nanosecond precision on a
/// multi-hour scan is noise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    pub wall_ms: u64,
    pub walk_ms: u64,
    pub skip_check_ms: u64,
    pub hash_ms: u64,
    pub db_write_ms: u64,
    pub archive_ms: u64,
    pub files_seen: u64,
    pub bytes_hashed: u64,
    pub bytes_skipped: u64,
    pub histogram: [u64; BUCKET_COUNT],
}

impl MetricsSnapshot {
    pub fn total_phase_ms(&self) -> u64 {
        self.walk_ms + self.skip_check_ms + self.hash_ms + self.db_write_ms + self.archive_ms
    }

    /// Sum-of-phases over wall-clock. ~1.0 while the pipeline is sequential; after #23 this is the
    /// overlap actually achieved — the single number that says whether parallelising worked.
    pub fn overlap_ratio(&self) -> f64 {
        if self.wall_ms == 0 {
            return 0.0;
        }
        self.total_phase_ms() as f64 / self.wall_ms as f64
    }

    fn mb_per_s(bytes: u64, ms: u64) -> f64 {
        if ms == 0 {
            return 0.0;
        }
        (bytes as f64 / 1_048_576.0) / (ms as f64 / 1000.0)
    }

    /// Overall throughput including every phase.
    pub fn overall_mb_per_s(&self) -> f64 {
        Self::mb_per_s(self.bytes_hashed + self.bytes_skipped, self.wall_ms)
    }

    /// Throughput while actually hashing — the number #24 would move.
    pub fn hashing_mb_per_s(&self) -> f64 {
        Self::mb_per_s(self.bytes_hashed, self.hash_ms)
    }

    pub fn files_per_s(&self) -> f64 {
        if self.wall_ms == 0 {
            return 0.0;
        }
        self.files_seen as f64 / (self.wall_ms as f64 / 1000.0)
    }

    /// Multi-line human report. Every divisor is guarded, so a zero-length scan prints zeroes
    /// rather than `NaN`.
    pub fn report(&self) -> String {
        let pct = |ms: u64| {
            if self.wall_ms == 0 {
                0.0
            } else {
                ms as f64 * 100.0 / self.wall_ms as f64
            }
        };
        let mut out = format!(
            "Where the time went ({} ms wall):\n\
               walk        {:>8} ms  {:>5.1}%\n\
               skip_check  {:>8} ms  {:>5.1}%\n\
               hash        {:>8} ms  {:>5.1}%\n\
               db_write    {:>8} ms  {:>5.1}%\n\
               archive     {:>8} ms  {:>5.1}%\n\
               accounted   {:>8} ms  {:>5.1}%\n",
            self.wall_ms,
            self.walk_ms,
            pct(self.walk_ms),
            self.skip_check_ms,
            pct(self.skip_check_ms),
            self.hash_ms,
            pct(self.hash_ms),
            self.db_write_ms,
            pct(self.db_write_ms),
            self.archive_ms,
            pct(self.archive_ms),
            self.total_phase_ms(),
            pct(self.total_phase_ms()),
        );
        out.push_str(&format!(
            "Rates: {:.1} files/s · {:.1} MB/s overall · {:.1} MB/s while hashing\n",
            self.files_per_s(),
            self.overall_mb_per_s(),
            self.hashing_mb_per_s(),
        ));
        out.push_str("File sizes seen:");
        for (i, label) in BUCKET_LABELS.iter().enumerate() {
            if self.histogram[i] > 0 {
                out.push_str(&format!(" {}={}", label, self.histogram[i]));
            }
        }
        out.push('\n');
        out
    }
}
```

- [ ] **Step 4: Declare the module**

In `src/lib.rs`, add alongside the existing `pub mod` declarations:

```rust
pub mod scan_metrics;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib scan_metrics`
Expected: PASS — 7 tests.

- [ ] **Step 6: Commit**

```bash
git add src/scan_metrics.rs src/lib.rs
git commit -m "feat(scanner): add a phase-timing accumulator for scans

Atomics plus RAII guards, so the same accumulator works unchanged when #23
hashes on worker threads. The 64 KiB histogram boundary is the line the
seek-bound hypothesis turns on.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Wire the timers into the scan loop

**Files:**
- Modify: `src/scanner.rs` (`ScanSummary` at :24, `scan_volume_with_progress` at :70-212)

**Interfaces:**
- Consumes: everything from Task 1.
- Produces: `ScanSummary.metrics: MetricsSnapshot`. Tasks 4 and 5 read it.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/scanner.rs`:

```rust
#[test]
fn scan_records_phase_timings_and_the_size_histogram() {
    let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("big.bin", 5000)]);
    let s = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
    let m = &s.metrics;

    assert_eq!(m.files_seen, 2);
    assert_eq!(m.histogram[1], 1, "the 10-byte file");
    assert_eq!(m.histogram[2], 1, "the 5000-byte file");
    assert_eq!(m.bytes_hashed, 5010);
    assert_eq!(m.bytes_skipped, 0);
    assert!(m.hash_ms > 0 || m.walk_ms > 0, "some phase must have been timed");
    assert!(
        m.total_phase_ms() <= m.wall_ms,
        "phases {} exceeded wall {}",
        m.total_phase_ms(),
        m.wall_ms
    );
}

#[test]
fn rescan_attributes_bytes_to_skipped_not_hashed() {
    let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("b.txt", 20)]);
    scan_volume(&cat, &root, &ident(), false, 100).unwrap();
    let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();

    assert_eq!(s.skipped, 2, "second pass takes the incremental-skip path");
    assert_eq!(s.metrics.bytes_hashed, 0);
    assert_eq!(s.metrics.bytes_skipped, 30);
    assert_eq!(s.metrics.files_seen, 2, "skipped files are still 'seen'");
    assert_eq!(s.metrics.histogram[1], 2);
}
```

Add this helper to the same test module (place it just above the two tests):

```rust
/// A temp dir containing `files` (name, byte length), plus an open catalog.
fn fixture_with_files(files: &[(&str, usize)]) -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    for (name, len) in files {
        std::fs::write(root.join(name), vec![b'x'; *len]).unwrap();
    }
    let cat = Catalog::open(&t.path().join("c.db")).unwrap();
    (t, cat, root)
}
```

> **Note for the implementer:** the existing test module already has helpers named `ident()` and
> uses `Catalog::open`. If a fixture helper with this exact behaviour already exists, use it instead
> of adding a duplicate — do not create a second one.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib scanner::tests::scan_records_phase_timings_and_the_size_histogram`
Expected: FAIL — `no field 'metrics' on type 'ScanSummary'`.

- [ ] **Step 3: Add the field to `ScanSummary`**

In `src/scanner.rs`, change the struct at line 24:

```rust
pub struct ScanSummary {
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub marked_missing: usize,
    pub archive_entries: usize,
    /// Where this scan's time went. Measured always; see `scan_metrics`.
    pub metrics: crate::scan_metrics::MetricsSnapshot,
}
```

- [ ] **Step 4: Instrument the scan loop**

In `scan_volume_with_progress`, add the accumulator immediately after `let mut summary = ScanSummary::default();`:

```rust
    let metrics = crate::scan_metrics::ScanMetrics::new();
```

Then make these five edits inside the loop. Each one keeps phases disjoint — read the placement
carefully, because an overlapping guard would silently corrupt the percentages.

**(a) Walk + metadata.** The `for entry in WalkDir::new(root)` loop currently starts at line 84.
Wrap the walk step and the metadata call. Replace the `let meta = match entry.metadata() {` line
with a timed block, and time the iterator itself by restructuring the loop head:

```rust
    let mut walker = WalkDir::new(root).into_iter();
    loop {
        let next = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Walk);
            walker.next()
        };
        let Some(entry) = next else { break };
        // ... the existing `let entry = match entry { Ok(e) => e, Err(err) => { ... } };` body
        //     continues unchanged from here ...
```

and for the metadata call:

```rust
        let meta = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Walk);
            match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    // ... existing error arm unchanged ...
                }
            }
        };
```

**(b) Count the file as seen**, immediately after `let size = meta.len() as i64;`:

```rust
        metrics.record_file_seen(size);
```

**(c) Skip-check.** Wrap the incremental-skip block's DB work:

```rust
        if !force {
            let _t = metrics.timer(crate::scan_metrics::Phase::SkipCheck);
            if let Some((old_size, old_mtime)) = cat.get_file_meta(&identity.volume_id, &rel)? {
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    if archive::is_archive_name(&rel) {
                        cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                    }
                    summary.skipped += 1;
                    metrics.add_bytes_skipped(size);
                    if let Some(p) = progress {
                        p.on_skipped();
                    }
                    in_batch += 1;
                    rotate_batch(cat, &mut in_batch)?;
                    continue;
                }
            }
        }
```

**(d) Hash.** Wrap the `hash_file` call only — not the `NewFile` construction:

```rust
        let hash = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Hash);
            match hashing::hash_file(path) {
                Ok(h) => h,
                Err(e) => {
                    // ... existing error arm unchanged ...
                }
            }
        };
        metrics.add_bytes_hashed(size);
```

**(e) DB write and archive.** Replace the `cat.upsert_file(&nf, now)?;` … `descend_archive(...)`
tail of the loop body with:

```rust
        {
            let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
            cat.upsert_file(&nf, now)?;
            in_batch += 1;
            rotate_batch(cat, &mut in_batch)?;
        }
        summary.hashed += 1;
        if let Some(p) = progress {
            p.on_hashed();
        }

        if archive::is_archive_name(&rel) {
            let _t = metrics.timer(crate::scan_metrics::Phase::Archive);
            descend_archive(
                cat, path, &rel, identity, &limits, now, &mut summary, &mut in_batch, progress,
            )?;
        }
```

`descend_archive` needs **no** parameter change: the whole call is timed as one phase, exactly as
the spec requires, and its internal read/hash/write must not be counted again under the loose-file
phases.

- [ ] **Step 5: Attach the snapshot before returning**

Replace the tail of `scan_volume_with_progress` (currently lines 209-211):

```rust
    // The final COMMIT and the missing-sweep are both real scan cost and both hit SQLite, so they
    // belong to db_write. Leaving them untimed would inflate the unaccounted gap and understate
    // exactly the fsync cost #26 targets.
    {
        let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
        cat.conn.execute_batch("COMMIT")?;
        summary.marked_missing =
            cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    }
    summary.metrics = metrics.snapshot();
    Ok(summary)
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib scanner`
Expected: PASS — the two new tests plus every pre-existing scanner test, unmodified.

- [ ] **Step 7: Run the whole suite — behaviour must be unchanged**

Run: `cargo test`
Expected: PASS, all binaries. Any failure here means the instrumentation changed scan behaviour, which the Global Constraints forbid.

- [ ] **Step 8: Commit**

```bash
git add src/scanner.rs
git commit -m "feat(scanner): time each scan phase and record the size histogram

Phases are disjoint by construction; the archive descent is timed as a whole
so its internal read/hash/write is not double-counted against the loose-file
phases. Skipped files still count as 'seen' -- a skipped file costs a seek
and a stat, which is the cost under investigation.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `scan_runs` table and catalog methods

**Files:**
- Modify: `src/catalog/schema.rs`
- Create: `src/catalog/scan_runs.rs`
- Modify: `src/catalog/mod.rs`

**Interfaces:**
- Consumes: `MetricsSnapshot` from Task 1; `ScanSummary` from Task 2.
- Produces: `Catalog::start_scan_run(...) -> Result<i64>`, `Catalog::finish_scan_run(...)`, `Catalog::recent_scan_runs(usize) -> Result<Vec<ScanRun>>`, `ScanRun`. Tasks 4 and 6 use these.

- [ ] **Step 1: Add the table**

In `src/catalog/schema.rs`, inside the existing `conn.execute_batch(r#" ... "#)` block, after the
`actions_log` table:

```sql
        CREATE TABLE IF NOT EXISTS scan_runs (
            id              INTEGER PRIMARY KEY,
            volume_id       TEXT,
            root_path       TEXT NOT NULL,
            started_at      INTEGER NOT NULL,
            finished_at     INTEGER,
            wall_ms         INTEGER,
            forced          INTEGER NOT NULL,
            status          TEXT NOT NULL,
            error_message   TEXT,
            files_seen      INTEGER NOT NULL DEFAULT 0,
            hashed          INTEGER NOT NULL DEFAULT 0,
            skipped         INTEGER NOT NULL DEFAULT 0,
            errors          INTEGER NOT NULL DEFAULT 0,
            archive_entries INTEGER NOT NULL DEFAULT 0,
            bytes_hashed    INTEGER NOT NULL DEFAULT 0,
            bytes_skipped   INTEGER NOT NULL DEFAULT 0,
            walk_ms         INTEGER NOT NULL DEFAULT 0,
            skip_check_ms   INTEGER NOT NULL DEFAULT 0,
            hash_ms         INTEGER NOT NULL DEFAULT 0,
            db_write_ms     INTEGER NOT NULL DEFAULT 0,
            archive_ms      INTEGER NOT NULL DEFAULT 0,
            size_histogram  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_scan_runs_started ON scan_runs(started_at DESC);
```

- [ ] **Step 2: Write the failing tests**

Create `src/catalog/scan_runs.rs` with ONLY this test module for now:

```rust
#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;
    use crate::scan_metrics::MetricsSnapshot;
    use crate::scanner::ScanSummary;

    fn open() -> (tempfile::TempDir, Catalog) {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        (t, cat)
    }

    #[test]
    fn a_started_run_is_visible_as_running_before_it_finishes() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(Some("v1"), "D:/drive", 100, false).unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].finished_at, None);
        assert_eq!(runs[0].root_path, "D:/drive");
    }

    #[test]
    fn finishing_a_run_stores_counters_phases_and_histogram() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(Some("v1"), "D:/drive", 100, true).unwrap();
        let summary = ScanSummary {
            hashed: 7,
            skipped: 3,
            errors: 1,
            marked_missing: 0,
            archive_entries: 2,
            metrics: MetricsSnapshot {
                wall_ms: 1234,
                walk_ms: 100,
                skip_check_ms: 50,
                hash_ms: 900,
                db_write_ms: 80,
                archive_ms: 40,
                files_seen: 10,
                bytes_hashed: 5000,
                bytes_skipped: 300,
                histogram: [1, 2, 3, 0, 0, 0, 0],
            },
        };
        cat.finish_scan_run(id, 200, "completed", None, &summary).unwrap();

        let r = &cat.recent_scan_runs(10).unwrap()[0];
        assert_eq!(r.status, "completed");
        assert_eq!(r.finished_at, Some(200));
        assert_eq!(r.forced, true);
        assert_eq!(r.hashed, 7);
        assert_eq!(r.errors, 1);
        assert_eq!(r.archive_entries, 2);
        assert_eq!(r.metrics.hash_ms, 900);
        assert_eq!(r.metrics.files_seen, 10);
        assert_eq!(r.metrics.histogram, [1, 2, 3, 0, 0, 0, 0]);
        assert_eq!(r.error_message, None);
    }

    #[test]
    fn a_failed_run_keeps_its_error_and_its_partial_numbers() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(None, "D:/x", 100, false).unwrap();
        let mut summary = ScanSummary::default();
        summary.hashed = 4;
        summary.metrics.hash_ms = 25;
        cat.finish_scan_run(id, 150, "failed", Some("disk fell out"), &summary)
            .unwrap();

        let r = &cat.recent_scan_runs(10).unwrap()[0];
        assert_eq!(r.status, "failed");
        assert_eq!(r.error_message.as_deref(), Some("disk fell out"));
        assert_eq!(r.hashed, 4, "partial work is still recorded");
        assert_eq!(r.metrics.hash_ms, 25);
        assert_eq!(r.volume_id, None);
    }

    #[test]
    fn recent_runs_are_newest_first_and_bounded() {
        let (_t, cat) = open();
        for i in 0..5 {
            cat.start_scan_run(Some("v"), "D:/d", 100 + i, false).unwrap();
        }
        let runs = cat.recent_scan_runs(3).unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].started_at, 104, "newest first");
        assert!(runs[0].started_at > runs[1].started_at);
    }

    #[test]
    fn a_corrupt_histogram_column_reads_back_as_zeroes_not_an_error() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(Some("v"), "D:/d", 100, false).unwrap();
        cat.conn
            .execute("UPDATE scan_runs SET size_histogram='not json' WHERE id=?1", [id])
            .unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs[0].metrics.histogram, [0; 7], "display data must never fail a read");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib scan_runs`
Expected: FAIL — module not declared / `no method named start_scan_run`.

- [ ] **Step 4: Write the implementation**

Put this ABOVE the test module in `src/catalog/scan_runs.rs`:

```rust
//! One row per scan: what it cost and how it ended.
//!
//! Written outside the scan's long transaction, so an interrupted multi-day scan still leaves a
//! record. Every write here is best-effort at the call site: losing a measurement is acceptable,
//! losing a scan is not.

use crate::catalog::Catalog;
use crate::scan_metrics::{MetricsSnapshot, BUCKET_COUNT};
use crate::scanner::ScanSummary;
use rusqlite::params;

/// A persisted scan run, as shown by the CLI and the Scan page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanRun {
    pub id: i64,
    pub volume_id: Option<String>,
    pub root_path: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub forced: bool,
    pub status: String,
    pub error_message: Option<String>,
    pub hashed: i64,
    pub skipped: i64,
    pub errors: i64,
    pub archive_entries: i64,
    pub metrics: MetricsSnapshot,
}

impl Catalog {
    /// Record a scan as started. Returns the row id to pass to `finish_scan_run`.
    ///
    /// Called before the scan opens its transaction, so the row is committed immediately and a
    /// killed scan leaves a visible `running` row rather than silence.
    pub fn start_scan_run(
        &self,
        volume_id: Option<&str>,
        root_path: &str,
        started_at: i64,
        forced: bool,
    ) -> anyhow::Result<i64> {
        self.conn.execute(
            "INSERT INTO scan_runs(volume_id, root_path, started_at, forced, status)
             VALUES (?1, ?2, ?3, ?4, 'running')",
            params![volume_id, root_path, started_at, forced as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close out a run with its final counters and timings. `status` is one of
    /// `completed` | `failed` | `cancelled` (the last is reserved for #5 and unreachable today).
    pub fn finish_scan_run(
        &self,
        id: i64,
        finished_at: i64,
        status: &str,
        error_message: Option<&str>,
        summary: &ScanSummary,
    ) -> anyhow::Result<()> {
        let m = &summary.metrics;
        let histogram = serde_json::to_string(&m.histogram)?;
        self.conn.execute(
            "UPDATE scan_runs SET finished_at=?2, status=?3, error_message=?4, wall_ms=?5,
                 files_seen=?6, hashed=?7, skipped=?8, errors=?9, archive_entries=?10,
                 bytes_hashed=?11, bytes_skipped=?12, walk_ms=?13, skip_check_ms=?14,
                 hash_ms=?15, db_write_ms=?16, archive_ms=?17, size_histogram=?18
             WHERE id=?1",
            params![
                id,
                finished_at,
                status,
                error_message,
                m.wall_ms as i64,
                m.files_seen as i64,
                summary.hashed as i64,
                summary.skipped as i64,
                summary.errors as i64,
                summary.archive_entries as i64,
                m.bytes_hashed as i64,
                m.bytes_skipped as i64,
                m.walk_ms as i64,
                m.skip_check_ms as i64,
                m.hash_ms as i64,
                m.db_write_ms as i64,
                m.archive_ms as i64,
                histogram,
            ],
        )?;
        Ok(())
    }

    /// Most recent runs, newest first.
    pub fn recent_scan_runs(&self, limit: usize) -> anyhow::Result<Vec<ScanRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, root_path, started_at, finished_at, forced, status,
                    error_message, files_seen, hashed, skipped, errors, archive_entries,
                    bytes_hashed, bytes_skipped, wall_ms, walk_ms, skip_check_ms, hash_ms,
                    db_write_ms, archive_ms, size_histogram
             FROM scan_runs ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            // Display data must never fail a read: a corrupt histogram degrades to zeroes.
            let histogram = r
                .get::<_, Option<String>>(21)?
                .and_then(|s| serde_json::from_str::<[u64; BUCKET_COUNT]>(&s).ok())
                .unwrap_or([0; BUCKET_COUNT]);
            Ok(ScanRun {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                root_path: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                forced: r.get::<_, i64>(5)? != 0,
                status: r.get(6)?,
                error_message: r.get(7)?,
                hashed: r.get(9)?,
                skipped: r.get(10)?,
                errors: r.get(11)?,
                archive_entries: r.get(12)?,
                metrics: MetricsSnapshot {
                    files_seen: r.get::<_, i64>(8)? as u64,
                    bytes_hashed: r.get::<_, i64>(13)? as u64,
                    bytes_skipped: r.get::<_, i64>(14)? as u64,
                    wall_ms: r.get::<_, Option<i64>>(15)?.unwrap_or(0) as u64,
                    walk_ms: r.get::<_, i64>(16)? as u64,
                    skip_check_ms: r.get::<_, i64>(17)? as u64,
                    hash_ms: r.get::<_, i64>(18)? as u64,
                    db_write_ms: r.get::<_, i64>(19)? as u64,
                    archive_ms: r.get::<_, i64>(20)? as u64,
                    histogram,
                },
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
```

- [ ] **Step 5: Declare the module**

In `src/catalog/mod.rs`, add alongside the existing declarations:

```rust
pub mod scan_runs;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib scan_runs`
Expected: PASS — 5 tests.

- [ ] **Step 7: Commit**

```bash
git add src/catalog/schema.rs src/catalog/scan_runs.rs src/catalog/mod.rs
git commit -m "feat(catalog): persist one row per scan run

Inserted at start as 'running' and updated at the end, so an interrupted
multi-day scan leaves a visible record instead of silence. A corrupt
histogram column degrades to zeroes rather than failing the read -- it is
display data.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `run_scan` records the run

**Files:**
- Modify: `src/scanner.rs` (`run_scan` at :229-266)

**Interfaces:**
- Consumes: `start_scan_run` / `finish_scan_run` from Task 3.
- Produces: no new API. Every caller of `run_scan` (CLI `cmd_scan`, the web worker) now gets persistence for free.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/scanner.rs`:

```rust
#[test]
fn run_scan_records_a_completed_run() {
    let (_t, cat, root) = fixture_with_files(&[("a.txt", 10)]);
    let out = run_scan(
        &cat,
        &root,
        false,
        crate::volume::ReadonlyMode::Fingerprint,
        100,
        None,
    )
    .unwrap();
    assert!(out.is_some());

    let runs = cat.recent_scan_runs(10).unwrap();
    assert_eq!(runs.len(), 1, "exactly one row per scan, not one per file");
    assert_eq!(runs[0].status, "completed");
    assert!(runs[0].finished_at.is_some());
    assert_eq!(runs[0].hashed, 1);
    assert_eq!(runs[0].metrics.files_seen, 1);
    assert!(!runs[0].root_path.is_empty());
}

#[test]
fn a_metrics_write_failure_never_fails_the_scan() {
    let (_t, cat, root) = fixture_with_files(&[("a.txt", 10)]);
    // Drop the table out from under the run: recording must degrade, not propagate.
    cat.conn.execute_batch("DROP TABLE scan_runs").unwrap();
    let out = run_scan(
        &cat,
        &root,
        false,
        crate::volume::ReadonlyMode::Fingerprint,
        100,
        None,
    );
    assert!(out.is_ok(), "a bookkeeping failure must not fail a scan: {out:?}");
    assert_eq!(out.unwrap().unwrap().1.hashed, 1, "the scan still did its work");
}
```

> **Note for the implementer:** check the exact name of the read-only fallback variant in
> `src/volume.rs` (`ReadonlyMode`) and use the one meaning "identify by fingerprint". If the
> existing tests already call `run_scan` with a particular variant, match them.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib scanner::tests::run_scan_records_a_completed_run`
Expected: FAIL — `recent_scan_runs` returns an empty vec (`assert_eq!(runs.len(), 1)` fails).

- [ ] **Step 3: Record the run in `run_scan`**

In `src/scanner.rs`, replace the body of `run_scan` from the `set_volume_path` line through the
`Ok(Some(...))` with:

```rust
    let _ = cat.set_volume_path(&identity.volume_id, &mount_root.display().to_string(), now);

    // Best-effort throughout: a bookkeeping failure must never fail a scan. Started before the
    // scan opens its transaction, so the 'running' row is committed immediately and an
    // interrupted multi-day scan leaves a record.
    let run_id = cat
        .start_scan_run(
            Some(&identity.volume_id),
            &mount_root.display().to_string(),
            now,
            force,
        )
        .map_err(|e| tracing::warn!("could not record scan start: {e}"))
        .ok();

    let result = scan_volume_with_progress(cat, mount_root, &identity, force, now, progress);

    if let Some(id) = run_id {
        let finished_at = crate::commands::now_secs();
        let outcome = match &result {
            Ok(summary) => cat.finish_scan_run(id, finished_at, "completed", None, summary),
            Err(e) => {
                let msg = e.to_string();
                cat.finish_scan_run(id, finished_at, "failed", Some(&msg), &ScanSummary::default())
            }
        };
        if let Err(e) = outcome {
            tracing::warn!("could not record scan result: {e}");
        }
    }

    let summary = result?;
    // Audit trail: one row per completed scan so the Overview "recent activity" feed can show it.
    let _ = cat.log_action(
        "scan",
        &serde_json::json!({
            "volume_id": identity.volume_id, "label": identity.label,
            "hashed": summary.hashed, "skipped": summary.skipped, "errors": summary.errors,
            "marked_missing": summary.marked_missing, "archive_entries": summary.archive_entries,
        })
        .to_string(),
        now,
    );
    Ok(Some((identity, summary)))
```

> **Implementer note on `now_secs`:** if `crate::commands::now_secs` is private, either make it
> `pub(crate)` or inline the equivalent
> `std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64`.
> Do not add a second timestamp helper — there must be one definition.

> **Known limitation to leave as-is:** on the error path the summary is unavailable (it was
> consumed by the failure), so a failed run records zeroed counters with its error message. The
> `running` → `failed` transition and the message are what matter; recovering partial counters from
> a failed scan would require restructuring `scan_volume_with_progress` to return them alongside the
> error, which is out of scope here.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib scanner`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test`
Expected: PASS, all binaries.

- [ ] **Step 6: Commit**

```bash
git add src/scanner.rs
git commit -m "feat(scanner): record every run through run_scan

run_scan is the one shared entry point for the CLI and the web worker, so
both get persistence without a second code path. Recording is best-effort
at every step: a bookkeeping failure must never fail a scan.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: CLI reporting

**Files:**
- Modify: `src/commands.rs` (`cmd_scan` at :67-90)

**Interfaces:**
- Consumes: `ScanSummary.metrics` (Task 2), `MetricsSnapshot::report()` (Task 1).
- Produces: no new API.

- [ ] **Step 1: Write the failing test**

Append to `tests/scan_and_search.rs`. That file defines only `bin()`; it sets
`CLEANUPSTORAGES_DATA_DIR` inline per command, and this test follows the same shape:

```rust
#[test]
fn scan_prints_where_the_time_went() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("a.txt"), b"hello").unwrap();
    let data = tmp.path().join("appdata");

    let out = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("scan")
        .arg(&drive)
        .arg("--readonly-fallback")
        .arg("fingerprint")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Where the time went"), "missing breakdown: {text}");
    for phase in ["walk", "skip_check", "hash", "db_write", "archive"] {
        assert!(text.contains(phase), "missing phase {phase}: {text}");
    }
    assert!(text.contains("files/s"), "missing rates: {text}");
    assert!(text.contains("File sizes seen"), "missing histogram: {text}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test scan_and_search scan_prints_where_the_time_went`
Expected: FAIL — "missing breakdown".

- [ ] **Step 3: Print the breakdown**

In `src/commands.rs`, in `cmd_scan`, after the existing `println!("Done: ...")`:

```rust
            print!("{}", s.metrics.report());
```

`report()` already ends with a newline, so `print!` (not `println!`) is correct here.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test scan_and_search`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs tests/scan_and_search.rs
git commit -m "feat(cli): print the scan time breakdown after a scan

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: `/api/scan-runs` and the Scan page panel

**Files:**
- Modify: `src/web.rs` (route table at :52-71)
- Modify: `src/web_ui.rs` (scan page)

**Interfaces:**
- Consumes: `recent_scan_runs` (Task 3).
- Produces: `GET /api/scan-runs?limit=` → `[ScanRun]`.

- [ ] **Step 1: Write the failing test**

Append to the test module in `src/web.rs`:

```rust
#[tokio::test]
async fn api_scan_runs_lists_recent_runs_newest_first() {
    let (_t, db, _state) = seed_dupes();
    {
        let cat = Catalog::open(&db).unwrap();
        let id = cat.start_scan_run(Some("vol-1"), "D:/one", 100, false).unwrap();
        let mut s = crate::scanner::ScanSummary::default();
        s.hashed = 3;
        s.metrics.hash_ms = 42;
        s.metrics.histogram = [0, 1, 2, 0, 0, 0, 0];
        cat.finish_scan_run(id, 150, "completed", None, &s).unwrap();
        cat.start_scan_run(Some("vol-1"), "D:/two", 200, true).unwrap();
    }

    let v = get_json(&db, "/api/scan-runs?limit=10").await;
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["root_path"], "D:/two", "newest first");
    assert_eq!(arr[0]["status"], "running");
    assert_eq!(arr[1]["status"], "completed");
    assert_eq!(arr[1]["hashed"], 3);
    assert_eq!(arr[1]["metrics"]["hash_ms"], 42);
    assert_eq!(arr[1]["metrics"]["histogram"][2], 2);
}

#[tokio::test]
async fn api_scan_runs_clamps_its_limit() {
    let (_t, db, _state) = seed_dupes();
    let v = get_json(&db, "/api/scan-runs?limit=100000").await;
    assert!(v.is_array(), "an absurd limit must not error: {v}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib web::tests::api_scan_runs_lists_recent_runs_newest_first`
Expected: FAIL — the route 404s, so the JSON body does not parse as an array.

- [ ] **Step 3: Add the route and handler**

In `src/web.rs`, add to the router alongside the other `get` routes:

```rust
        .route("/api/scan-runs", get(api_scan_runs))
```

and the handler (place it next to `api_scan_status`):

```rust
#[derive(Deserialize, Default)]
struct ScanRunsParams {
    limit: Option<usize>,
}

/// Recent scan runs with their phase breakdown. Read-only — no CSRF surface.
async fn api_scan_runs(
    State(state): State<AppState>,
    Query(p): Query<ScanRunsParams>,
) -> Result<Json<Vec<crate::catalog::scan_runs::ScanRun>>, (StatusCode, String)> {
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let limit = p.limit.unwrap_or(20).clamp(1, 200);
    Ok(Json(cat.recent_scan_runs(limit).map_err(err500)?))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib web::tests::api_scan_runs`
Expected: PASS — 2 tests.

- [ ] **Step 5: Add the Scan page panel**

In `src/web_ui.rs`, in the scan page's `main` HTML, append a panel after the existing content:

```html
<div class="card" style="margin-top:20px">
  <h2 style="margin:0 0 12px;font-size:15px">Recent scans</h2>
  <div id="runs"><span class="mut">Loading…</span></div>
</div>
```

and in the scan page's script:

```javascript
function runRow(r){
  const m=r.metrics, tot=m.walk_ms+m.skip_check_ms+m.hash_ms+m.db_write_ms+m.archive_ms;
  const pct=v=>tot?Math.round(v*100/tot):0;
  const when=r.started_at?fmtDate(r.started_at):"";
  const status=r.status==="running"?'<span class="mut">running…</span>'
    :r.status==="failed"?`<span style="color:var(--danger)">failed</span>`:r.status;
  // Phase split as a single bar: the shape is the point, not the exact numbers.
  const bar=[["walk",m.walk_ms],["skip",m.skip_check_ms],["hash",m.hash_ms],
             ["db",m.db_write_ms],["arch",m.archive_ms]]
    .filter(([,v])=>v>0)
    .map(([k,v])=>`<span title="${k} ${v} ms">${k} ${pct(v)}%</span>`).join(" · ");
  return `<div class="dl"><span class="k">${esc(when)} — ${esc(r.root_path)}</span>
    <span class="v">${status}</span></div>
    <div class="mut" style="font-size:12px;padding:0 0 10px">
      ${r.hashed} hashed · ${r.skipped} unchanged · ${r.errors} errors ·
      ${fmtSize(m.bytes_hashed)} hashed in ${m.wall_ms} ms${bar?" · "+bar:""}
      ${r.error_message?"<br>"+esc(r.error_message):""}
    </div>`;
}
async function loadRuns(){
  try{
    const rs=await apiGet("/api/scan-runs?limit=10");
    $("#runs").innerHTML = rs.length ? rs.map(runRow).join("")
      : '<span class="mut">No scans recorded yet.</span>';
  }catch(e){ $("#runs").innerHTML='<span class="mut">Could not load scan history.</span>'; }
}
loadRuns();
```

> **Implementer note:** `esc`, `fmtSize`, `fmtDate`, `apiGet` and `$` are existing shared helpers in
> this file — do not redefine them. Check the exact CSS variable used for error red (`--danger` or
> similar) and match what the rest of the file uses.

- [ ] **Step 6: Verify the page renders**

Run: `cargo build --release`, then `.\target\release\cleanupstorages.exe browse --no-open`, open the
Scan page, and confirm the panel lists runs (or says none are recorded). Stop the server afterwards.

- [ ] **Step 7: Run the whole suite and the gates**

Run: `cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add src/web.rs src/web_ui.rs
git commit -m "feat(web): show recent scan runs and their phase split

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: Benchmark protocol doc

**Files:**
- Create: `docs/benchmarking-scans.md`
- Modify: `CLAUDE.md` (documentation map)

**Interfaces:** none — documentation only.

- [ ] **Step 1: Write the doc**

Create `docs/benchmarking-scans.md`:

```markdown
# Benchmarking scans

Scan timings are easy to measure and easy to measure *wrongly*. Every number below should state
which of the three conditions it was taken under, or it cannot be compared to anything.

## Reading the breakdown

`cleanupstorages scan <path>` prints a phase split after the summary, and the Scan page keeps the
last runs. The two numbers that decide epic #21's ordering:

- **`hash` vs `walk` + `skip_check`.** If hashing is a small slice and the walk dominates, the scan
  is seek-bound: faster hashing (#24) will buy almost nothing, and concurrency (#23) must be tuned
  carefully because more parallel readers on a spinning disk means more seeking, not less.
- **MB/s while hashing vs MB/s overall.** A large gap means time is going somewhere other than
  reading bytes.

`accounted` is the sum of the phases. While the pipeline is sequential it should be close to wall
clock; untimed glue (loop overhead, path and category string work) makes up the rest. After #23
parallelises the pipeline, `accounted` will *exceed* wall clock, and the ratio is the overlap
achieved — that is the point of the number.

## The three traps

### 1. Windows Defender

Defender scans every file we open. On a corpus that is 88.3% files under 64 KB, that per-open tax
can rival seek time — and from inside our process it is indistinguishable from slow I/O.

Run the A/B once:

1. Scan a representative subtree, note files/s and MB/s.
2. Add that subtree to Defender's exclusions
   (Windows Security → Virus & threat protection → Manage settings → Exclusions).
3. Scan the same subtree again the same way, and compare.
4. **Remove the exclusion afterwards** if it is not somewhere you want permanently excluded.

If this alone moves throughput materially, the fix is a documentation note, not code.

### 2. Cold vs warm OS file cache

The second scan of the same subtree reads from the OS cache and will look faster for reasons that
have nothing to do with our code. Either reboot between runs, use a subtree far larger than RAM, or
label the number "warm" and only compare it to other warm numbers.

### 3. First pass vs rescan

The incremental skip means a second scan of already-catalogued files exercises `skip_check`, not
`hash`. These measure different code paths and must never be compared to each other. Use
`--force` to make a rescan take the hashing path, or compare first-pass to first-pass.

## Recording a result

Runs are persisted in the `scan_runs` table and survive restarts, so a multi-day scan's numbers are
not lost. Note in the issue which condition each figure was taken under.
```

- [ ] **Step 2: Link it from the documentation map**

In `CLAUDE.md`, under "Documentation map", add:

```markdown
- How to benchmark a scan (and the three traps): `docs/benchmarking-scans.md`
```

- [ ] **Step 3: Commit**

```bash
git add docs/benchmarking-scans.md CLAUDE.md
git commit -m "docs: how to benchmark a scan without measuring the wrong thing

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## After the plan: the measurement that justifies it

This plan builds the instrument. Pointing it at the real 4 TB drive is what closes #22:

1. Scan a representative subtree, record the breakdown.
2. Run the Defender A/B from `docs/benchmarking-scans.md`.
3. Post both results on #22, stating the conditions.
4. **Then** decide the order of #23 / #24 / #26 from the evidence — specifically whether `hash` or
   `walk` + `skip_check` dominates.

Do not start #23 or #24 before step 4. The whole point of #22 is that the two plausible diagnoses
demand opposite fixes.

# Stoppable scans with progress and ETA — design

**Status:** approved
**Date:** 2026-07-31
**Closes:** #5 (stop/cancel a running scan), #25 (resumable scans with progress and ETA)
**Epic:** #2 (scan control & visibility)

## Why

~16 TB remain to be catalogued at ~35 MB/s — **more than five days of scanning**, in multi-hour and
multi-day runs, on external drives that get unplugged and a PC that gets rebooted. Three concrete
gaps make that unpleasant today:

1. **No way to stop a scan.** The CLI offers only Ctrl+C. The web layer calls `worker.abort()`, which
   cancels a *tokio task* — it cannot interrupt the blocking scan running inside `spawn_blocking`, so
   the scan continues regardless.
2. **An interrupted run is recorded as `running` forever.** `start_scan_run` writes `running` and only
   `finish_scan_run` clears it, so a killed scan leaves a row that claims to still be going.
3. **A multi-day scan is opaque.** No percentage, no ETA, no live rate. There is no way to tell 20%
   from 80%, or to decide whether to let it run overnight.

## What is already true (and therefore not being built)

Investigated before designing, because it removes most of the assumed scope:

- **Interrupted scans keep their work.** `rotate_batch` commits every 200 files, so completed batches
  survive a kill.
- **Interrupted scans cannot corrupt state.** `mark_missing_scanned` runs *only* after the final
  commit, so a scan that dies never mis-marks the files it had not reached. The hazard this design
  set out to solve is already avoided by placement — the job here is to *keep* it true when a stop
  becomes graceful rather than fatal.
- **Resume already works.** Re-running fast-forwards through catalogued files via the incremental
  skip (stat + one indexed lookup, no re-hash).

**So "resumable scans" is not built.** No checkpoint of walk position: directory iteration order is
not guaranteed stable between runs, so a persisted position can silently mean something different on
resume — real risk on the scan path for a modest saving over a fast-forward that already works. It is
documented and measured instead.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Stop mechanism | **Cooperative `Arc<AtomicBool>`, checked once per file** | Never interrupts a file mid-hash, so no half-written row. Works identically for CLI and web. |
| Stop semantics | Finish current file → commit → **skip the sweep** → record `cancelled` | Skipping the sweep is load-bearing: without it, stopping would mark every unreached file `missing`. |
| Resume | **Re-run; no new state** | The incremental skip already fast-forwards. A walk-position checkpoint is fragile (unstable iteration order) for a small gain. |
| Progress total | **Metadata-only counting pass before hashing** | The only source of a true percentage for a folder never scanned before — which is most of the remaining 16 TB. Cost scales with *file count*, not bytes. |
| ETA basis | **Bytes, on a rolling recent rate** | Throughput swings 28–125 MB/s with file size; a lifetime average would lag badly. A rolling rate self-corrects and is labelled an estimate. |
| Live counters | **Always on, from the first second** | Independent of the counting pass, always correct, zero cost. Percentage and ETA appear once a total exists. |
| Ctrl+C handling | **Add the `ctrlc` crate** (one small cross-platform dependency) | The CLI scan is synchronous, so `tokio::signal` does not apply; the alternative is per-platform `signal`/`SetConsoleCtrlHandler` code. A new dependency is a real cost and is called out rather than slipped in. |

## Architecture

### Stop signal

```rust
/// Cooperative stop. Checked once per file by the counting pass and the scan.
#[derive(Clone, Default)]
pub struct StopFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl StopFlag {
    pub fn request(&self);        // set
    pub fn is_requested(&self) -> bool;
}
```

Threaded into `scan_volume_with_progress` and `run_scan` as `stop: &StopFlag`. Checked at the top of
each loop iteration, in both the counting pass and the hashing pass.

- **CLI**: a `ctrlc` handler sets the flag; a second Ctrl+C aborts the process as usual. This adds the
  `ctrlc` crate — small, widely used, no transitive weight. Worth naming explicitly: the project
  compiles to a single self-contained binary and every dependency is a deliberate choice.
- **Web**: `POST /api/scan/stop` (CSRF-guarded, like every other write endpoint) sets the flag on the
  currently running job. Replaces the ineffective `worker.abort()`.

### Scan outcome

`ScanSummary` gains `stopped: bool`. `run_scan` records the run as **`cancelled`** when it is set —
the status already exists in `scan_runs` and was reserved for exactly this.

**The single reliability rule:** *a scan that did not finish never sweeps.*

```rust
if !stopped {
    summary.marked_missing =
        cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now, &unreadable_dirs)?;
}
```

One condition, one place, one test.

### Counting pass

A metadata-only walk (`readdir` + `stat`, no file contents, no DB) returning `(files, bytes)`, run
before hashing. It respects `should_skip` so its total matches what the scan will actually process,
and it honours the stop flag.

`scan --no-count` skips it: live counters still work, percentage and ETA are simply absent.

Cost scales with file count rather than volume size — a few minutes for a 225k-file tree, which is
why it is affordable against a multi-day scan.

### Progress reporting

The existing `Progress` trait already receives per-file events. It gains the totals and a byte
counter so a percentage can be derived:

```rust
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
    /// Totals from the counting pass. Not called when --no-count.
    fn on_total(&self, files: u64, bytes: u64) {}
    /// Bytes processed by the file just finished (hashed or skipped).
    fn on_bytes(&self, bytes: u64) {}
}
```

Default no-op bodies, so existing implementors keep compiling.

**CLI**: a line rewritten every ~2 s to **stderr** (so redirecting stdout stays clean), and only when
stderr is a terminal — piped output gets periodic plain lines instead of carriage returns.

```
Counting… 148,746 files (121 GB)
Scanning  38% · 56,412/148,746 files · 46.1/121 GB · 34.8 MB/s · ETA 1h 42m
```

Without a count: `Scanning · 56,412 files · 46.1 GB · 34.8 MB/s · 1h 07m elapsed`.

**Web**: `RunningDto` (returned by `/api/scan/status`, which the Scan page already polls) gains four
fields, all optional so the payload stays valid before the counting pass finishes and under
`--no-count`:

```rust
pub struct RunningDto {
    pub path: String,
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub archive_entries: usize,
    pub done_bytes: u64,             // always present
    pub total_files: Option<u64>,    // from the counting pass
    pub total_bytes: Option<u64>,    // from the counting pass
    pub eta_seconds: Option<u64>,    // needs a total and enough samples
}
```

The page renders a percentage and ETA when they are present, live counters when they are not, and
gains a **Stop** button wired to `POST /api/scan/stop`.

### ETA

```
rate      = bytes in the last ~30 s / that window        (rolling, self-correcting)
remaining = total_bytes - done_bytes
eta       = remaining / rate
```

Shown only with a total, suppressed until ~10 s of samples exist, and always rendered as an estimate.
A rolling window is required rather than a lifetime average: this corpus moves between 125 MB/s
(large files) and 28 MB/s (small files), so a lifetime average would be wrong in both directions for
long stretches.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **A stopped scan sweeps unreached files to `missing`** — the serious one | One `if !stopped` guard around the sweep, plus a test that stops mid-tree and asserts zero rows changed to `missing` |
| A stop is ignored because the check sits in the wrong loop | The flag is checked in both passes; a test asserts a stop during counting and during hashing both return promptly |
| The counting pass doubles the disk cost | Metadata-only, no file reads; measured and reported, and `--no-count` opts out |
| ETA is wildly wrong and erodes trust | Rolling window, suppressed until enough samples, labelled an estimate |
| Progress output corrupts piped/redirected output | Written to stderr, carriage-return updates only when stderr is a terminal |

## Non-goals

- **No walk-position checkpointing.** Re-running fast-forwards via the incremental skip.
- No pause/resume of a live process — stop then re-run.
- No change to hashing, quarantine, purge or repack.
- No change to scan concurrency: scans stay single-threaded, per the measured result in
  `2026-07-24-parallel-scan-design.md`.
- No stop for `purge`/`repack` — those are short and transactional.

## Success criteria

1. `POST /api/scan/stop` and Ctrl+C both end a running scan within one file.
2. A stopped scan keeps everything it hashed, marks **zero** extra files `missing`, and is recorded as
   `cancelled` with its partial metrics.
3. Re-running a stopped scan completes the remaining work, and the fast-forward cost over
   already-catalogued files is measured and documented.
4. A scan prints a live percentage and ETA, and both are absent (not wrong) under `--no-count`.
5. Existing scanner tests pass unmodified; the stop guard has its own regression test.

# Scan instrumentation — design

**Status:** approved
**Date:** 2026-07-22
**Closes:** #22 (instrument the scanner)
**Epic:** #21 (scan performance — 20 TB must be practical)

## Why

A 4 TB drive scanned at ~3.5 MB/s: ~500 GB in ~2 days, against hardware that sustains 100–150 MB/s.
We are an order of magnitude off, and we do not know why.

Two diagnoses fit the same symptom and demand **opposite** fixes:

- **Seek-bound.** 88.3% of catalogued files are under 64 KB. At ~10 ms/seek that is ~100 files/s, and
  at the ~35 KB mean that is ~3.5 MB/s — which matches the observed rate almost exactly. If this is
  the cause, faster hashing (#24) buys nearly nothing, and concurrency (#23) can make it *worse* by
  increasing seek thrash on spinning media.
- **Compute- or commit-bound.** Single-threaded BLAKE3 with a 64 KiB buffer, 15 hot-path
  `conn.execute` sites re-parsing SQL per file, and `synchronous` unset (an fsync per 200-file
  commit). If this is the cause, #24 and #26 are the wins.

The arithmetic above is suggestive, not evidence: it was derived from a file-size distribution, not
from a timed scan. This issue produces the evidence, and every later child of #21 is measured
against it.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Lifetime | **Permanent feature**, CLI + Scan page | Each optimisation in #23/#24/#26 must be provable; re-adding throwaway instrumentation each time is how "it feels faster" ships. |
| Persistence | **One `scan_runs` row per scan** | A 2-day run whose numbers vanish on restart was not measured. Also enables diffing runs months apart. |
| Structure | **Atomics + RAII timer guards** behind `Arc` | #23 (parallelise) is the very next issue. Instrumentation whose purpose is proving #23 helped must not need rewriting *by* #23. |
| Defender | **Document the A/B, do not detect** | Detection is a Windows-only PowerShell/WMI call that can fail or need elevation, to answer a question a one-off manual control run answers better. |
| Live per-phase UI | **Out of scope** | The existing live counters already answer "is it moving". |
| Archive timing | **One phase, not split** | Its inner read/hash/write would double-count against the loose-file phases, and it is a different workload (one long sequential read, not seeks). |

## What is measured

### Phases

Five phases, each timed by an RAII guard that accumulates nanoseconds into an atomic on drop:

| Phase | Covers |
| --- | --- |
| `walk` | `WalkDir::next()` + `entry.metadata()` |
| `skip_check` | `get_file_meta` + `touch_seen` on the incremental-skip path |
| `hash` | `hashing::hash_file` for loose files |
| `db_write` | `upsert_file` + the batch commit inside `rotate_batch` |
| `archive` | the whole of `descend_archive` |

Phases are disjoint by construction: no guard is live inside another guard's scope. `archive` is the
one exception and is therefore *not* decomposed — the read/hash/write it performs internally is
counted only under `archive`.

### Counters

`files_seen`, `hashed`, `skipped`, `errors`, `archive_entries`; `bytes_hashed`, `bytes_skipped`.

`files_seen` counts every regular file the walk considered, including skipped ones — a skipped file
still costs a seek and a stat, which is exactly the cost under investigation.

### Size histogram

Over **files seen**, not files hashed:

`0` · `1B–4KiB` · `4KiB–64KiB` · `64KiB–1MiB` · `1MiB–16MiB` · `16MiB–256MiB` · `>256MiB`

Buckets are half-open on the upper bound (`4KiB–64KiB` means `4096 <= size < 65536`). The 64 KiB
boundary is deliberate: it is both the current read-buffer size and the line the seek-bound
hypothesis turns on.

### Derived (computed for display, not stored)

- files/s and MB/s over wall-clock
- MB/s **during `hash` only** — the number #24 would move
- mean ms/file per phase
- **sum-of-phases ÷ wall-clock** — ~1.0 while the pipeline is sequential; after #23 this is the
  overlap actually achieved, and is the single number that says whether parallelising worked

## Architecture

### `src/scan_metrics.rs` (new)

```rust
pub enum Phase { Walk, SkipCheck, Hash, DbWrite, Archive }

/// Accumulates phase timings and counters. Cheap enough to call per file: two `Instant::now()`
/// calls and a relaxed `fetch_add`, against millisecond-scale I/O.
pub struct ScanMetrics { /* AtomicU64 per phase + per counter + per histogram bucket */ }

impl ScanMetrics {
    pub fn timer(&self, phase: Phase) -> PhaseTimer<'_>;  // stops on drop
    pub fn record_file_seen(&self, size_bytes: i64);      // counter + histogram bucket
    pub fn snapshot(&self) -> MetricsSnapshot;
}
```

All fields are `AtomicU64` with `Ordering::Relaxed`. Relaxed is correct here: these are independent
counters, no other memory is published through them, and an exact ordering between two counters
carries no meaning.

`ScanMetrics` is passed as `&ScanMetrics` alongside the existing `Option<&dyn Progress>`, and is
`Send + Sync` so #23 can wrap it in an `Arc` and hand clones to workers without touching this module.

### Storage

```sql
CREATE TABLE IF NOT EXISTS scan_runs (
    id             INTEGER PRIMARY KEY,
    volume_id      TEXT,
    root_path      TEXT NOT NULL,
    started_at     INTEGER NOT NULL,
    finished_at    INTEGER,
    wall_ms        INTEGER,
    forced         INTEGER NOT NULL,
    status         TEXT NOT NULL,          -- running | completed | failed | cancelled
    error_message  TEXT,
    files_seen     INTEGER NOT NULL DEFAULT 0,
    hashed         INTEGER NOT NULL DEFAULT 0,
    skipped        INTEGER NOT NULL DEFAULT 0,
    errors         INTEGER NOT NULL DEFAULT 0,
    archive_entries INTEGER NOT NULL DEFAULT 0,
    bytes_hashed   INTEGER NOT NULL DEFAULT 0,
    bytes_skipped  INTEGER NOT NULL DEFAULT 0,
    walk_ms        INTEGER NOT NULL DEFAULT 0,
    skip_check_ms  INTEGER NOT NULL DEFAULT 0,
    hash_ms        INTEGER NOT NULL DEFAULT 0,
    db_write_ms    INTEGER NOT NULL DEFAULT 0,
    archive_ms     INTEGER NOT NULL DEFAULT 0,
    size_histogram TEXT                    -- JSON array of 7 bucket counts
);
CREATE INDEX IF NOT EXISTS idx_scan_runs_started ON scan_runs(started_at DESC);
```

`size_histogram` is JSON rather than seven columns because it is display data, not something we
query or aggregate over; keeping it as one column also lets the bucket list change without a
migration.

**The row is inserted at scan start with `status='running'` and updated at the end.** This matters:
the scan body runs inside one long transaction (`BEGIN`, then `COMMIT; BEGIN` every 200 files), so a
row written only at the end is lost to a hard kill — and a multi-day run is exactly the one likely to
be killed. A `running` row left behind is itself the signal that a scan was interrupted.

The insert and the final update use **their own transaction**, committed independently of the scan's
batch transaction, so a rolled-back scan still records what it did.

**A metrics failure must never fail a scan.** Every `scan_runs` write is logged-and-swallowed on
error. Losing a measurement is acceptable; losing a scan is not.

### Reporting

**CLI** — `scan` prints a breakdown after the existing summary line: phase table with ms and % of
wall, files/s, MB/s overall and during hashing, sum-of-phases ÷ wall, and the histogram.

**Scan page** — a "Recent scans" panel listing the last runs per drive with their phase split, fed by
a new `GET /api/scan-runs?limit=`. Read-only; no new write endpoint, no CSRF surface.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Timing overhead distorts what it measures | Two `Instant::now()` + one relaxed `fetch_add` per phase per file, against ms-scale I/O. A test asserts a scan of a synthetic tree stays within tolerance of the same scan uninstrumented. |
| Phases silently overlap, so percentages mislead | Phases are disjoint by construction; a test asserts sum-of-phases ≤ wall-clock and ≥ 90% of it on a sequential synthetic scan. |
| `scan_runs` growth | One row per scan, not per file. Even daily scans for years is trivial. No retention policy needed; revisit if that ever changes. |
| A benchmark measures the wrong thing (warm cache, rescan path, Defender) | `docs/benchmarking-scans.md` documents the protocol and the three traps. |

## Benchmark protocol (`docs/benchmarking-scans.md`)

1. **Defender A/B** — identical scans of the same subtree, with and without a Defender exclusion on
   it. Defender scans every file we open; on a corpus that is 88.3% sub-64 KB that tax can rival seek
   time, and from inside the process it is indistinguishable from slow I/O.
2. **Cold vs warm OS cache** — a repeated scan of the same subtree gets faster for reasons unrelated
   to our code. State which one a number is.
3. **First pass vs rescan** — the incremental skip means a second scan exercises `skip_check`, not
   `hash`. These are different measurements and must never be compared to each other.

## Non-goals

- No optimisation. This issue only measures; #23/#24/#26 act on it.
- No change to what is scanned, hashed, or stored — hashes and catalogue contents are unaffected.
- No antivirus detection or configuration advice.
- No live per-phase display during a running scan.
- No retention/pruning of `scan_runs`.

## Success criteria

1. A scan reports where its time went, split five ways, in the CLI and on the Scan page.
2. A run of the real 4 TB drive answers the question this epic opens with: **is it seek-bound?** —
   by comparing `hash` time against `walk` + `skip_check`, and MB/s-during-hash against overall MB/s.
3. The Defender A/B has been run once and its result recorded on #22.
4. A killed scan leaves a `running` row; a failed scan leaves a `failed` row with its error.
5. Instrumentation changes no scan behaviour: existing scan tests pass unmodified.
6. `ScanMetrics` is `Send + Sync` and needs no change to be used from #23's worker threads.

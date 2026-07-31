# Parallel scan pipeline — design

**Status:** NOT MERGED — implemented, measured, abandoned. The hypothesis was disproved, and the
pipeline is slower than the serial scan it replaced at *every* worker count. Kept as the record of
why. Tag: `experiment/parallel-scan`.
**Date:** 2026-07-24
**Closes:** #23 (parallelise the scan pipeline)
**Epic:** #21 (scan performance — 20 TB must be practical)

## Result (measured 2026-07-29/30, after implementation)

**Concurrency made the scan slower on the target hardware, in both regimes.** Measured on the real
external drive with `--force`:

| corpus | `--jobs 1` | `--jobs 4` |
| --- | --- | --- |
| 172 large files (~32 GB, all >64 KB, 128 over 16 MB) | 4.3 min · 125.0 MB/s | 8.7 min · 61.5 MB/s (**2.03x slower**) |
| 225,285 files (91% under 64 KB) + archives | 1.25 h · 28.3 MB/s | 2.29 h · 15.4 MB/s (**1.83x slower**) |

**Then the decisive one — this pipeline against the serial scan it replaced**, same folder, same
225,285 files:

| | wall | overall | while hashing | walk phase |
| --- | --- | --- | --- | --- |
| **`main` (serial loop)** | **1.01 h** | **35.1 MB/s** | 42.7 MB/s | 247 s |
| this branch, `--jobs 1` | 1.25 h | 28.3 MB/s | 31.1 MB/s | 483 s |
| this branch, `--jobs 4` | 2.29 h | 15.4 MB/s | 4.2 MB/s | — |

**The pipeline is 24% slower than the serial scan even at one worker.** The `walk` phase alone nearly
doubled (247 s → 483 s) on identical work, because at `--jobs 1` there are still *two* disk
consumers: the walker doing `readdir`/`stat`, and the worker streaming file data. `accounted` states
it exactly — `main` 99.9% (no overlap), this branch 112.8%. That 12.8% of overlap **costs 24% of wall
time** in seek contention. The overlap is real, and counterproductive.

On the 20 TB target: roughly 6.6 days (serial) versus 8.2 days (this pipeline at `--jobs 1`).

The mechanism is unambiguous and visible *within* each run: per-stream throughput collapsed ~7x
(31.1 -> 4.2 MB/s while hashing) with only 4 workers, so four readers delivered ~16.8 MB/s aggregate
where one delivered 31.1. The archive phase suffered worst (7.6x). A single disk head is one physical
resource; asking it for concurrent streams turns sequential reads into seek storms.

The design's reasoning — "the scan is I/O-bound, therefore overlap I/O with hashing" — silently
assumed the disk could serve concurrent requests productively. On a spinning USB drive it cannot.
The seek-bound small-file corpus, predicted here to be the case that benefits, lost just as badly.

**Consequence: the branch is not merged.** Shipping it with `--jobs 1` as the default was considered
and then rejected, once the third measurement landed: `--jobs 1` is not "the old behaviour", it is
this pipeline with one worker, and it costs 24%. `main` already holds the faster implementation, so
abandoning this required writing nothing and stops paying that 24% permanently.

Overlap does work on NVMe (385% accounted, measured), so the idea is not universally wrong — it is
wrong for this project's target hardware, which is external spinning drives.

This is the outcome #22 existed to make visible. Without it, `--jobs 4` would have shipped as the
default and roughly doubled a 20 TB scan.

## Why

The #22 instrumentation, run against the real drive (148,746 files, ~121 GB, `--force`), settled the
diagnosis: the scan is **I/O-bound on small files**, not compute-bound.

- 91% of files are under 64 KB (78,591 under 4 KB).
- Hashing ran at 36–64 MB/s while BLAKE3 does gigabytes/s — the hasher is nearly idle, waiting on
  the disk to feed it one small file at a time.
- The pipeline is fully sequential: walk → read → hash → write → next. While one file is being read,
  no other work happens; the disk gets no queue depth to order seeks.

The fix the measurement points to is **overlap**: keep several reads in flight so the disk stays
busy and the CPU is not idle between them. (The same measurement deprioritised #24 — the hasher
already idles — and #26 — db_write is 2% of wall on the real drive.)

A separate finding from the same run: **Windows Defender was a ~30% tax**, fixable by a folder
exclusion with zero code. That is documentation, tracked on #21; it is not part of this change.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Scope | **Loose files *and* archives**, parallelised at **top-level-entry granularity** | A worker handles one whole loose file, or one whole `archive::scan_archive`. Different archives run concurrently; the nested descent inside a single archive is never threaded, so the #18 buffer budget stays a local `&mut`. |
| Topology | **Three-stage pipeline**: walker → workers → writer | Each role is small and independently testable; the writer stays the sole owner of the transaction; workers are completely DB-free. |
| Worker DB access | **None** | Workers only read bytes and hash. There is no concurrent-writer question — the single writer thread owns all SQLite writes. |
| Concurrency | ~~**Fixed `--jobs`, default 4**~~ → **default 1** | The reasoning below ("2–4 concurrent reads give the OS queue depth to order seeks") was the hypothesis, and measurement disproved it — see Result at the top. Concurrent reads made a spinning drive 1.8–2.0x slower in both regimes. |
| Staying out of the way | **Below-normal worker thread priority** | Lets the OS scheduler yield to foreground apps automatically. Serves "don't hog the PC" without a load monitor. Adaptive load-based scaling is explicitly deferred (own issue + own measurement). |
| Number of implementations | **One.** The pipeline is the only scan; `--jobs=1` runs a single worker | No separate serial path to drift. `--jobs=1` is the correctness anchor: it must produce a catalogue bit-identical to the pre-parallel scan. |

## Architecture

```
 WalkDir + stat + skip-check          read+hash / scan_archive           upsert + batch-commit
    [Walker thread]  ──jobs chan──▶   [Worker × N, no DB]  ──results──▶   [Writer thread]
    (read-only conn)   (bounded)       (below-normal prio)   (bounded)     (the one write conn)
```

### Jobs (walker → workers)

```rust
enum Job {
    Touch(String),                       // rel — unchanged file, skip decided by the walker
    HashLoose { path: PathBuf, rel: String, size: i64, mtime: Option<i64>,
                created: Option<i64>, accessed: Option<i64> },
    ScanArchive { path: PathBuf, rel: String, size: i64, mtime: Option<i64>,
                  created: Option<i64>, accessed: Option<i64> },
}
```

The walker does the incremental **skip-check** (`get_file_meta`, a cheap indexed read on its
read-only connection). Unchanged files (same size + second-granularity mtime) become `Touch`; new or
changed files become `HashLoose`, or `ScanArchive` when the name is an archive.

**A `Touch` job passes through a worker unchanged** (the worker does no I/O for it, just re-emits
`ScanResult::Touch`). This keeps the topology strictly one-in/one-out — the walker has a single
output channel and the writer a single input channel — rather than giving the walker a second path
straight to the writer. Touch jobs are cheap to forward, so the uniformity is worth more than the
saved hop.

### Results (workers → writer)

```rust
enum ScanResult {
    Touch(String),                                   // rel
    Upsert(NewFile),                                 // a hashed loose file
    Archive { rel: String, mtime: Option<i64>, scan: archive::ArchiveScanResult },
    Error { rel: String, reason: String },           // read/stat/hash failure — logged, not fatal
}
```

`archive::scan_archive` is already pure (no DB, no mutation of shared state), so a worker runs the
whole recursive descent and ships the `ArchiveScanResult` for the writer to persist with
`upsert_archive_entry` (inheriting the archive's mtime, per #10).

### Channels and backpressure

- **Jobs channel: bounded** (e.g. capacity `jobs * 4`). If workers are saturated the walker blocks,
  which bounds how far ahead of the workers the walk can run — no unbounded queue of pending paths.
- **Results channel: bounded** (same order of magnitude). The writer keeps up easily (db_write is a
  few percent of wall), but bounding it caps memory if a burst of archive results lands at once.
- Channel **disconnect** is the shutdown signal: the walker drops the jobs sender when the walk ends;
  each worker exits when the jobs channel is empty and closed; the writer finishes when the results
  channel is empty and closed, then runs the missing-sweep and commits.

### Connection ownership

`run_scan` opens the write connection, records the `scan_runs` "running" row, then **moves** the
owned `Catalog` into the writer thread. The walker opens its **own read-only** connection from the
same path (WAL permits readers alongside the single writer). Workers open nothing. The writer thread
returns **`(Catalog, ScanSummary)`** on join, handing the owned connection back so `run_scan` records
the "finished" row on it — the #22 bookkeeping is unchanged, it just spans a thread boundary.

`Connection` is `Send` but not `Sync`; moving one owned connection into the writer thread and opening
a separate read-only one in the walker is exactly within those bounds. No connection is shared.

## Reliability (must not regress)

- **Byte-identical hashes.** Same BLAKE3 over the same bytes; only *when* it runs changes.
- **Correctness anchor.** A scan of a fixed tree produces a **bit-identical catalogue** at `--jobs=1`
  and `--jobs=8` (same rows, hashes, statuses, archive entries). This is a test, and it is what makes
  "one implementation" safe.
- **One bad file never sinks the scan.** A read/stat/hash failure becomes `ScanResult::Error`, logged
  via `log_scan_error` exactly as today; the scan continues.
- **A worker panic is contained.** Worker threads are joined; a panic surfaces as a scan error and a
  clean abort, never a silently truncated catalogue. (The walk/hash of a file must not, on panic,
  leave a partial row — workers only *send* completed records; the writer writes whole records.)
- **Writer DB error** aborts the scan, `ROLLBACK`, propagates — as today. The `scan_runs` row records
  `failed` with its message (#22 machinery, unchanged).
- **Missing-sweep unchanged and order-independent.** It runs after all results are applied; every
  file seen this pass has `last_seen_at = now`, so out-of-order arrival cannot mis-flag a file.
- **No new on-disk writes to the drive.** The scan still only writes the identity marker; everything
  else is catalogue writes on the computer. Quarantine/purge/repack are untouched.

## Metrics, priority, CLI

- `ScanMetrics` (already `Send + Sync`) is shared as an `Arc`. Phases now fire on different threads:
  `walk` on the walker, `hash` and `archive` on workers, `db_write` on the writer. Sum-of-phases will
  **exceed** wall-clock; `overlap_ratio()` (built in #22 for this) becomes the number that says
  whether parallelism worked — ~1.0 means no overlap, ~N means N-way overlap.
- Workers set **below-normal thread priority** at start: `SetThreadPriority(THREAD_PRIORITY_BELOW_NORMAL)`
  on Windows, `libc::setpriority`/`nice(+10)` on Unix. Best-effort — a failure to lower priority is
  logged and ignored, never fatal.
- **CLI:** `scan <path> --jobs <N>` (default 4). `run_scan` gains a `jobs` parameter; the web scan
  passes the default. The chosen `jobs` is stored on the `scan_runs` row so a later comparison knows
  the concurrency each run used.

## Testing

- **Anchor:** a fixed synthetic tree (loose files of varied sizes + a nested archive) yields an
  identical catalogue at `--jobs=1`, `--jobs=4`, `--jobs=8` — same file rows, hashes, statuses, and
  archive entries.
- A file that fails to read is logged as a scan error and does **not** abort the scan; other files in
  the same run still land.
- A rescan still skips unchanged files (the walker's skip-check still fires) and re-hashes a changed
  file.
- Archives still catalogue every entry, dated by the archive's mtime (#10 preserved).
- Every pre-existing scanner/quarantine/web test passes unmodified.
- A stress tree of many small files at `--jobs=8` matches `--jobs=1` and completes without deadlock.

## Non-goals

- No adaptive/load-based worker scaling — deferred to its own issue and its own measurement.
- No change to #24 (faster hashing) or #26 (SQLite tuning) — both deprioritised by the measurement.
- No parallelism *within* a single archive's nested descent — archives parallelise against each other
  and against loose files, not internally.
- No cancellation of a running scan (#5) — the channel/thread structure makes it natural to add later,
  but it is out of scope here.
- No Defender detection — the exclusion is a documented manual step on #21.

## Success criteria

1. `scan --jobs N` runs a walker + N workers + a writer; workers do no SQLite I/O.
2. On the real drive (or a faithful large synthetic tree), wall-clock at `--jobs=4` is materially
   below `--jobs=1`, and `overlap_ratio()` rises above 1.0 — measured, not assumed.
3. The catalogue produced is bit-identical across `--jobs` values on a fixed tree.
4. A read error in one file is logged and non-fatal; a writer error aborts with `ROLLBACK` and a
   `failed` scan_runs row.
5. There is exactly one scan implementation; `--jobs=1` is a single worker, not a separate path.
6. Workers run below foreground priority.

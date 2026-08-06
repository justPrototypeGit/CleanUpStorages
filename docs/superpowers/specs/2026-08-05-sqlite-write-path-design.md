# SQLite write-path wins — design

**Status:** approved
**Date:** 2026-08-05
**Closes:** #26 (cheap SQLite wins: prepare_cached, synchronous=NORMAL, bigger batches)
**Epic:** #21 (scan performance — 20 TB must be practical)

## Why

The catalogue is about to be wiped and rebuilt by a scan of ~20 TB taking five days or more. Three
changes to the write path are cheap, independently measurable, and reversible. They are worth doing
before that run because they compound across every one of tens of millions of rows.

Measured on the resume benchmark (`docs/benchmarking-scans.md`): `db_write` was **30–44%** of a
fast-forward pass.

**That figure must not be over-read, and this spec says so up front.** A fast-forward pass does no
hashing, so `db_write` dominates it by default. On a first scan — which is what the 20 TB run is —
hashing dominates instead, and the realistic share of total wall time is single-digit percent. The
honest expectation is a modest saving, and the point of measuring each change separately is to find
out whether even that is real.

## What this is not

No schema change, no index change, no `content_hash` representation change. That is #32, and it is
**deliberately deferred**: its benefit is currently unknown to within a factor of 2.2 (the issue
measured 963 bytes/file; the live catalogue currently shows 2,135 bytes/row, and has never been
`VACUUM`ed so neither is a trustworthy baseline), while its risk is a rewrite of the core `files`
table, both identity indexes, the FTS table and every dedup query — immediately before the one scan
that matters. It should be revisited when the real corpus exists and the footprint can be measured
rather than projected.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Statement caching | **`prepare_cached` on the per-file write sites only** | Those run tens of millions of times; cold sites would be churn for nothing |
| Durability | **`PRAGMA synchronous = NORMAL`** | Standard for WAL. Cannot corrupt; worst case loses the most recent commits, which a rescan rebuilds |
| Commit trigger | **N files *or* M bytes, whichever comes first** | A pure count cannot bound the re-work a stopped scan costs: 200 videos and 200 text files are wildly different amounts of re-hashing |
| Evidence | **Each change benchmarked separately; anything that does not measurably help is reverted** | This project's history is that measurement overturns expectations |

## Architecture

### 1. `prepare_cached` on the per-file path

`conn.execute(sql, params)` prepares, executes and discards a statement every call. On the scan's hot
path that re-parses the same SQL once per file — on the order of 50 million times for a 20 TB corpus.
`prepare_cached` keeps a per-connection cache keyed on the SQL text.

Scope is deliberately narrow: **only statements executed once or more per file.** From the current
code that is `upsert_file`, `touch_seen`, `touch_archive_entries`, `get_file_meta`, and the archive
entry insert. Cold statements (`forget_volume`, snapshots, the settings and pending-format handlers)
keep `execute` — caching them buys nothing and widens the diff.

The cache lives on the `Connection`, so this changes no behaviour and no SQL. It is the lowest-risk
item here.

### 2. `PRAGMA synchronous = NORMAL`

Set beside the existing `journal_mode = WAL` and `busy_timeout` in `Catalog::open`.

**Why this is safe, written down because "reduce durability" needs justifying in this project.** In
WAL mode `NORMAL` does not fsync on every commit; it syncs at checkpoints. SQLite's own guarantee is
that WAL + `NORMAL` **cannot corrupt the database** — a power loss or OS crash can lose the most
recent transactions, but never leave a torn or inconsistent file.

What losing recent transactions costs here: at most the last committed batch of files. Those files
are simply not yet in the catalogue, so the next scan re-hashes them via the ordinary incremental
skip. Nothing on disk is touched, nothing is marked missing, and the catalogue is self-healing by
construction. That is a materially different risk from losing user data, and it is the reason this
is acceptable where a durability reduction elsewhere would not be.

The integrity check (`Catalog::integrity_ok`) and the snapshot mechanism are unchanged.

### 3. A commit trigger bounded by bytes as well as files

`BATCH_SIZE` is currently 200 files (`src/scanner.rs:11`), and `rotate_batch` commits when the
in-batch count reaches it.

Raising it reduces fsyncs. But it interacts with the stop/resume feature: a stopped, crashed, or
power-cut scan loses the **current uncommitted batch**, and those files are re-hashed on resume. A
pure file count cannot bound that cost — 200 large video files is minutes of re-hashing, 200 text
files is milliseconds.

So the trigger becomes **whichever comes first**:

```rust
/// Commit when either bound is reached. The byte bound is what makes a larger file count safe:
/// it caps the work a stopped or interrupted scan has to redo, which a count alone cannot do.
const BATCH_MAX_FILES: usize = /* measured */;
const BATCH_MAX_BYTES: u64  = /* measured */;
```

`rotate_batch` gains a byte accumulator, reset with the counter. Both values are chosen by
measurement, not assumption.

The stop/resume guarantees are unchanged: a stopped scan still commits what it has, still skips the
missing-file sweep, and still resumes via the incremental skip.

## Measurement

`docs/benchmarking-scans.md` documents three traps, and all three apply:

1. **Windows Defender** — exclude the benchmark folder, or every number is noise.
2. **Cold vs warm cache** — label each figure, and compare only like with like.
3. **First pass vs rescan** — `--force` to measure the hashing path; a plain rescan measures
   `skip_check`. They are different code paths and must never be compared to each other.

Method: measure a baseline on a fixed folder, then apply the three changes **one at a time**, taking
`Phase::DbWrite` and wall clock from the existing instrumentation at each step. Record the numbers in
`docs/benchmarking-scans.md` next to the existing tables.

**Anything that does not measurably help is reverted, not kept because it seemed reasonable.** This
project has already abandoned parallel scanning on measurement, rewritten the ratio cap on
measurement, and discovered the counting pass was a cache artefact on measurement. A "cheap win" that
does not show up is not a win.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| `synchronous = NORMAL` is read as "less safe with your data" | It cannot corrupt; it can only lose the most recent commits, which a rescan rebuilds. Documented in the code and the README |
| A larger batch makes a stopped scan expensive to resume | The byte bound caps the re-work; both bounds are measured, not guessed |
| A larger batch holds a write transaction longer, blocking the web UI | The UI reads through a separate connection with `busy_timeout(5s)`; measure whether the Scan page stalls during a scan and keep the batch below anything that causes it |
| `prepare_cached` changes behaviour subtly | It changes no SQL and no parameters; existing tests must pass unmodified, and the diff is mechanical |
| Numbers are taken under different conditions and compared | The three traps above, applied explicitly, with each figure labelled |

## Non-goals

- No schema, index or `content_hash` change (#32, deferred and revisited after the real scan).
- No change to hashing, quarantine, purge, repack, or what gets catalogued.
- No change to the stop/resume contract or the missing-file sweep.
- No parallelism — settled by measurement in `2026-07-24-parallel-scan-design.md`.

## Success criteria

1. Each of the three changes has a recorded before/after measurement on the same folder under the
   same conditions, added to `docs/benchmarking-scans.md`.
2. Any change without a measurable improvement is reverted, and that is recorded too.
3. `synchronous = NORMAL` is set, and its safety rationale is stated in the code where it is set.
4. The commit trigger is bounded by both files and bytes, with both values justified by measurement.
5. A stopped scan still commits what it has, marks nothing missing, and resumes correctly — the
   existing regression tests pass unmodified.
6. Existing tests pass unmodified; `prepare_cached` introduces no behavioural change.

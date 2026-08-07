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

**Measured result: excluding the drive is ~30% faster.** Same 148,746 files, `--force`, twice:

| | wall | throughput |
| --- | --- | --- |
| Defender active | 73.1 min | 27.7 MB/s |
| drive excluded | **51.4 min** | **39.4 MB/s** |

Hashing throughput nearly doubled, and the `walk` phase halved — Defender's cost is per *file
opened*, so it lands hardest on the many-small-file phases. The README tells users how to set the
exclusion; the rest of this section is how to reproduce the measurement.

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

## Stopping and resuming a scan (#5, #25)

A scan can be stopped with Ctrl+C or the web UI's Stop button. Resuming is just re-running the same
command — the incremental skip fast-forwards over what is already catalogued, which is why no walk
position is checkpointed.

Measured on the same folder used everywhere else in this document (225,285 files, 124.2 GB):

| run | wall | what it did |
| --- | --- | --- |
| first scan, cold | **1.01 h** | hashed every byte |
| re-run over catalogued files | **25 s** | 0 hashed, 225,285 unchanged |

About **145x cheaper than the scan it replaces**. That gap is the whole argument against checkpointed
resume: persisting a walk position would buy seconds while introducing a file that can disagree with
reality (directory iteration order is not guaranteed stable between runs).

### The counting pass is free warm and expensive cold

The pre-scan counting pass (metadata only — `readdir` + `stat`, no file contents) is what makes the
percentage and ETA possible. Its cost is entirely **trap #2 above**, and the difference is dramatic:

| counting pass over 225,285 files | wall |
| --- | --- |
| warm (directory metadata cached) | **~1 s** |
| cold, drive spun down, under concurrent load | **~8.6 min** (upper bound, see caveat) |

Warm, it is free: an A/B of the same re-scan with and without `--no-count` came out 23.7 s vs 25.0 s
— the run *with* counting was marginally faster, so the difference is noise, not signal.

**Caveat on the cold figure:** it was taken while a separate process was recursing the same drive, so
it is a contaminated upper bound, not a clean benchmark. Treat it as "a cold metadata traversal of a
large tree on an external HDD costs minutes", not as a precise number. Measuring it properly needs a
reboot or a remount between runs.

The practical consequence, and the reason `--no-count` exists:

- **First scan of a folder** — keep the counting pass. Even at the cold figure it is a single-digit
  percentage of a multi-hour hashing run, and it buys a real percentage and ETA.
- **Resuming, or any run that will mostly skip** — consider `--no-count`. The counting pass can cost
  more than the fast-forward it is estimating.

Note also that during a fast-forward the ETA is erratic by nature (observed swinging 1m 26s → 3m 05s
→ 17s): skipped bytes complete in bursts at several GB/s, so the rolling rate has nothing steady to
lock onto. The ETA is meaningful while hashing, which is the case it was built for.

## Write-path tuning (#26)

Epic #26 makes small changes to the scan's per-file SQLite write path (cached statements,
`synchronous = NORMAL`, a byte-bounded commit trigger) before a 20 TB, five-day scan. Every change
is measured against the baseline below and reverted if it does not measurably help — see the
"Global Constraints" in `docs/superpowers/plans/2026-08-05-sqlite-write-path.md`.

### The corpus

`scripts/make-write-path-corpus.ps1` builds a fixed, rebuildable corpus of **20,000 files, 6.44 GB**
(default parameters ask for 50,000; 20,000 was chosen instead — see "A slow first attempt" below).
Sizes are weighted toward the small end (60% under 4 KiB, 25% 4–64 KiB, 12% 64 KiB–1 MiB, 3%
1–16 MiB) to mirror the "88.3% under 64 KB" shape measured elsewhere in this doc, because #26 is
about *per-file* overhead — many small files stress that path far more than a few large ones.
Files are spread across 1,000 leaf directories (40×25) so `walk` does real traversal. The corpus
lives under `Documents\cleanup-write-path-corpus`, never inside the real catalog directory.

`scripts/bench-write-path.ps1 -Label <name> -CacheCondition <cold|warm> -DefenderExcluded <bool>`
runs both code paths against that corpus:
- `scan --force` on a **fresh** `CLEANUPSTORAGES_DATA_DIR` — the hashing path, every file new.
- an immediate plain rescan against the same fresh catalog — the `skip_check` path, every file
  unchanged.

These are never averaged together (trap #3): the script prints both `Phase` breakdowns separately
and labels the run with the cache condition and Defender state so figures can't be silently mixed.

### A slow first attempt, and a repeatability problem — both fixed

Two issues came up building this baseline, worth recording so the next person doesn't re-discover
them:

1. **50,000 files was originally attempted and was far too slow to generate.** The first version of
   `make-write-path-corpus.ps1` filled each file's bytes via PowerShell range-slicing
   (`$buffer[0..($size-1)]`), which builds a new array one element at a time through the pipeline —
   effectively O(size) per element, not O(size). Rewritten to fill an exact-size array per file with
   a single `NextBytes` call and `WriteAllBytes`, which is what let the corpus build in ~2 minutes.
   Even after the fix, 20,000 files (not 50,000) was kept — it fits comfortably in a few minutes and
   is already enough files to make `db_write` legible, per the plan's own "20,000 is perfectly
   adequate" guidance.

2. **The very first hashing-path run after generating the corpus was not representative.** A run
   taken immediately after `make-write-path-corpus.ps1` finished came in anomalously fast (17.1 s)
   compared to every run after it (~26–35 s) — the corpus's pages were still warm in the OS write
   cache from having just been created, which is a sharper version of trap #2. Two throwaway
   "priming" runs (same `scan --force` over the corpus, discarded) were enough to settle the
   machine into a steady state; **all baselines below were taken after that priming**, and any
   future comparison run should prime once first if the corpus was just rebuilt.

Even after priming, hashing-path wall clock still varies **~10% run to run** on this machine, which
is consistent with — not contradictory to — the Defender trap documented above (Defender's
on-access scan cost is not perfectly deterministic even for unmodified files). `db_write_ms`, the
number #26 actually cares about, is far tighter: **within 2%** across the three runs below. The
`skip_check` (plain rescan) path is tighter still on every figure, because it does no hashing at
all and so is least exposed to Defender's per-open variance.

### Baseline (current code, before any #26 change)

Conditions: release build (`cargo build --release`), Windows Defender **not** excluded (the user
was AFK and this session was told not to change Defender settings — see the trap above), OS file
cache warm, corpus primed with two discarded runs first. All figures are wall clock for the whole
CLI invocation (process launch to exit), so they include the counting pass and snapshot as well as
the phases below.

**These are A/B figures for judging #26's changes under one machine's constant conditions — not
absolute throughput numbers.** Compare later runs only to this table, never to numbers taken
elsewhere in this doc (different Defender/cache state) or to a different code path.

`scan --force` (hashing path — every file new):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| baseline-1 | 29.3 s | 208 ms (0.7%) | 924 ms (3.2%) | 22,016 ms (75.7%) | **4,104 ms (14.1%)** | 0 ms | 93.7% |
| baseline-2 | 32.6 s | 192 ms (0.6%) | 917 ms (2.8%) | 25,296 ms (78.2%) | **4,071 ms (12.6%)** | 0 ms | 94.3% |
| baseline-3 | 32.3 s | 187 ms (0.6%) | 910 ms (2.8%) | 25,102 ms (78.3%) | **4,024 ms (12.5%)** | 0 ms | 94.3% |

`scan` (plain rescan, immediately after — `skip_check` path, every file unchanged):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| baseline-1 | 1.8 s | 129 ms (8.5%) | 357 ms (23.5%) | 0 ms | **304 ms (20.0%)** | 0 ms | 51.9% |
| baseline-2 | 1.6 s | 110 ms (8.0%) | 312 ms (22.8%) | 0 ms | **300 ms (21.9%)** | 0 ms | 52.7% |
| baseline-3 | 1.7 s | 119 ms (8.3%) | 323 ms (22.4%) | 0 ms | **332 ms (23.1%)** | 0 ms | 53.8% |

**Repeatable, with one caveat stated plainly:** `db_write_ms` — the figure #26's changes are judged
on — agrees within 2% on the hashing path and within 10% on the rescan path. Total wall clock on
the hashing path varies up to ~10% run to run, which is Defender's per-open scan cost, not this
benchmark's own noise (see point 2 above); it does not change which path dominates or move the
`db_write` share enough to affect any decision this plan will make.

Reproduce with:
```powershell
.\scripts\make-write-path-corpus.ps1              # once, or after any corpus change
.\scripts\bench-write-path.ps1 -Label priming -DefenderExcluded $false   # discard, run twice if just rebuilt
.\scripts\bench-write-path.ps1 -Label <your-label> -DefenderExcluded $false
```

### Task 2: `prepare_cached` on the per-file statements — kept

Converted the five statements that run at least once per file (`get_file_meta`, `upsert_file`,
`touch_seen`, `upsert_archive_entry`, `touch_archive_entries`) in `src/catalog/store.rs` from
`conn.execute`/`conn.query_row` to `conn.prepare_cached(SQL)?.execute(...)` (or the `prepare_cached`
+ `query_row` equivalent). SQL text and parameters are unchanged; only the prepare step is cached
across calls, avoiding a re-parse/re-plan per file. Everything else in the module (`forget_volume`,
snapshots, settings, pending-format handlers) still uses plain `execute` — those run at most once
per scan, so caching them buys nothing.

Conditions: same machine, same corpus, same release build process, Windows Defender **not**
excluded, OS file cache warm, corpus primed with two discarded runs first (same trap as the
baseline — the first hashing-path run after the corpus was last touched came in anomalously slow,
149.5 s wall with hash at 96.2% of the total, and was discarded along with a second priming run
that settled back to the ~29-33 s steady state before any figures were recorded).

`scan --force` (hashing path — every file new):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| prepare_cached-1 | 29.3 s | 214 ms (0.7%) | 132 ms (0.5%) | 24,043 ms (82.8%) | **2,700 ms (9.3%)** | 0 ms | 93.3% |
| prepare_cached-2 | 30.9 s | 217 ms (0.7%) | 133 ms (0.4%) | 25,456 ms (83.2%) | **2,787 ms (9.1%)** | 0 ms | 93.4% |
| prepare_cached-3 | 32.6 s | 212 ms (0.7%) | 131 ms (0.4%) | 27,199 ms (83.9%) | **2,859 ms (8.8%)** | 0 ms | 93.8% |

`scan` (plain rescan, immediately after — `skip_check` path, every file unchanged):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| prepare_cached-1 | 0.8 s | 90 ms (11.1%) | 57 ms (7.0%) | 0 ms | **245 ms (30.2%)** | 0 ms | 48.3% |
| prepare_cached-2 | 0.8 s | 92 ms (11.4%) | 55 ms (6.8%) | 0 ms | **247 ms (30.5%)** | 0 ms | 48.6% |
| prepare_cached-3 | 0.8 s | 87 ms (11.1%) | 55 ms (7.0%) | 0 ms | **236 ms (30.0%)** | 0 ms | 48.0% |

**Comparison against the baseline (`db_write_ms`, the figure this change is judged on):**

- Hashing path: baseline mean 4,066 ms (4,104 / 4,071 / 4,024) → new mean 2,782 ms (2,700 / 2,787 /
  2,859) — **31.6% lower**, far outside the baseline's documented ~2% run-to-run band.
- Rescan path: baseline mean 312 ms (304 / 300 / 332) → new mean 243 ms (245 / 247 / 236) —
  **22.2% lower**, outside the baseline's documented ~10% band.

**Decision: kept.** Both paths improve well beyond run-to-run variance, so this is a real win, not
noise — the opposite of the "cheap win that doesn't show up" failure mode this branch is guarding
against. No SQL, parameters, schema, or the stop/resume contract changed; `cargo test` passed
unmodified.

### Task 3: `PRAGMA synchronous = NORMAL` — kept (asymmetric result)

Set `synchronous = NORMAL` in `Catalog::open` (WAL mode) instead of the default `FULL`. Dropped one
fsync per commit at checkpoint boundaries instead of on every commit. Cannot corrupt the database —
a power loss can only lose the most recent commits, which the ordinary incremental rescan rebuilds.

Conditions: same machine, same corpus, same release build, Windows Defender **not** excluded, OS
file cache warm, two priming runs discarded first. A fourth run was taken beyond the required three
because the first measured run's hashing-path `db_write_ms` was a clear outlier (see below); all
four are reported for transparency rather than silently dropping the inconvenient one.

`scan --force` (hashing path — every file new):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| synchronous_normal-1 | 29.5 s | 269 ms (0.9%) | 151 ms (0.5%) | 22,182 ms (76.0%) | **4,513 ms (15.5%)** | 0 ms | 92.9% |
| synchronous_normal-2 | 31.5 s | 274 ms (0.9%) | 152 ms (0.5%) | 25,779 ms (82.5%) | **2,934 ms (9.4%)** | 0 ms | 93.2% |
| synchronous_normal-3 | 34.1 s | 270 ms (0.8%) | 150 ms (0.4%) | 28,483 ms (84.0%) | **2,891 ms (8.5%)** | 0 ms | 93.8% |
| synchronous_normal-4 | 25.8 s | 227 ms (0.9%) | 121 ms (0.5%) | 21,039 ms (82.5%) | **2,389 ms (9.4%)** | 0 ms | 93.2% |

`scan` (plain rescan, immediately after — `skip_check` path, every file unchanged):

| run | wall | walk | skip_check | hash | db_write | archive | accounted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| synchronous_normal-1 | 1.4 s | 153 ms (14.6%) | 86 ms (8.2%) | 0 ms | **169 ms (16.1%)** | 0 ms | 38.9% |
| synchronous_normal-2 | 1.5 s | 160 ms (14.7%) | 90 ms (8.3%) | 0 ms | **165 ms (15.2%)** | 0 ms | 38.1% |
| synchronous_normal-3 | 1.1 s | 110 ms (14.1%) | 64 ms (8.2%) | 0 ms | **134 ms (17.2%)** | 0 ms | 39.4% |
| synchronous_normal-4 | 1.1 s | 108 ms (14.1%) | 62 ms (8.1%) | 0 ms | **131 ms (17.1%)** | 0 ms | 39.4% |

**Comparison against the Task 2 baseline (`db_write_ms`, the figure this change is judged on):**

- Hashing path: baseline mean 2,782 ms (2,700 / 2,787 / 2,859). Run 1 (4,513 ms) is a clear outlier
  — 63% above the other three, and above every number in this plan's history; treated the same as
  Task 2's own documented 149.5 s priming outlier, i.e. real but not representative of steady state.
  Runs 2-4 mean 2,738 ms (2,934 / 2,891 / 2,389) — 1.6% lower than baseline, i.e. flat, well inside
  noise. This was originally written up as **"no measurable win on the hashing path"**.

  **That conclusion was wrong, and it was wrong because the measurement was too noisy to support
  it.** The four runs above spread 47% (2,389 to 4,513 ms), against ~2% in Task 1 — a
  measurement-quality collapse, not a property of the change. With that much noise neither "no win"
  nor "a real win" is distinguishable, and the honest reading at the time was "this measurement
  cannot answer the question", not "the answer is no".

  A clean re-measurement settled it. Two priming runs discarded, `--no-count` to remove the counting
  pass as a variable, a fresh data dir per run, three runs per side:

  | | `db_write_ms`, hashing path |
  | --- | --- |
  | with `synchronous = NORMAL` | 2,253 / 2,341 / 2,352 → mean **2,315** |
  | without (default `FULL`) | 2,492 / 2,504 / 2,527 → mean **2,508** |

  **−7.7%, with within-group spread of only 1.4–4.4%.** So there *is* a real win on the hashing path
  — smaller than on the rescan path, but outside noise and on the path that governs a first scan.
  The cumulative table in the Task 5 section uses this corrected 2,315 ms figure.

  The lesson is worth more than the number: a null result from a noisy measurement is not a null
  result, it is an absent one. Two priming runs and `--no-count` were what turned a 47% spread into
  a 4% one.
- Rescan path: baseline mean 243 ms (245 / 247 / 236) → new mean 150 ms (169 / 165 / 134 / 131) —
  **38% lower**, consistent across all four runs and far outside the baseline's ~10% band. On this
  path db_write is close to the entire workload (no hashing), so removing a fsync per commit shows
  up directly.

**Decision: kept — and after the re-measurement above, for a stronger reason than originally
recorded.** Both paths improve: **−7.7%** on the hashing path (what a 20 TB first scan spends its
time on) and **−38%** on the rescan/skip_check path (what every scan after the first one is). The
asymmetry is expected — on the rescan path `db_write` is close to the entire workload, so removing
an fsync per commit shows up directly, while hashing dominates the first pass.

The change is also a strict safety-preserving durability trade: WAL + `NORMAL` cannot corrupt the
database, only lose the last uncommitted batch, which a rescan silently rebuilds. `cargo test` passed unmodified, including `integrity_ok` and the snapshot
mechanism, both re-verified manually against a real scan into an isolated temp data dir.

### Task 4: a commit trigger bounded by bytes as well as files — `BATCH_MAX_FILES = 1000`, `BATCH_MAX_BYTES = 64 MiB`

Replaced the fixed `BATCH_SIZE = 200` commit trigger with `rotate_batch(cat, in_batch, batch_bytes)`,
which commits when **either** bound is reached. The byte bound exists because a stopped, crashed, or
power-cut scan loses its current uncommitted batch and re-hashes it on resume — a file count alone
cannot bound that cost (200 video files is minutes of re-work, 200 text files is milliseconds; see
the plan's rationale). No schema change, no change to what gets catalogued; the stop/resume
regression tests pass unmodified.

Conditions: same machine, same corpus (20,000 files, 6.44 GB), same release build, Windows Defender
**not** excluded, OS file cache warm. Measured with the method that gave the tightest spread in this
project's history — a fresh `CLEANUPSTORAGES_DATA_DIR` per run, `--no-count`, judged on `db_write_ms`
only:

```
D=/tmp/cus-$RANDOM; mkdir -p "$D"
CLEANUPSTORAGES_DATA_DIR="$D" ./target/release/cleanupstorages.exe \
  scan "<corpus>" --force --no-count --readonly-fallback fingerprint 2>&1 | grep -E "^db_write"
rm -rf "$D"
```

**File count alone** (byte bound held effectively disabled at 10 GiB, so only the file count could
trigger a commit):

| BATCH_MAX_FILES | run 1 | run 2 | run 3 | mean |
| --- | --- | --- | --- | --- |
| 200 | 2,485 ms | 2,435 ms | — | 2,460 ms |
| 1,000 | 2,319 ms | 2,292 ms | 2,450 ms | 2,354 ms |
| 2,000 | 2,521 ms | 2,527 ms | 2,548 ms | 2,532 ms |
| 5,000 | 2,658 ms | 2,558 ms | — | 2,608 ms |

The curve is U-shaped, not monotonic: 1,000 is the minimum among the values tried — fewer files per
commit (200) pays for more fsyncs, and more files per commit (2,000, 5,000) pays for larger
transactions (more WAL growth/page traffic per commit). 1,000 is where it flattens/bottoms out.

**Byte bound alone** (file count held effectively disabled at 1,000,000, so only the byte total could
trigger a commit):

| BATCH_MAX_BYTES | run 1 | run 2 | run 3 | mean |
| --- | --- | --- | --- | --- |
| 1 MiB | 3,744 ms | 3,674 ms | — | 3,709 ms |
| 4 MiB | 3,199 ms *(outlier, hash phase itself 28.8 s vs ~15 s elsewhere — discarded)* | 2,312 ms | 2,114 ms | 2,213 ms |
| 16 MiB | 2,086 ms | 2,061 ms | 2,000 ms | 2,049 ms |
| 64 MiB | 3,250 ms *(outlier, hash phase 167 s — discarded)* | 1,749 ms | 1,546 ms | 1,506 ms *(3rd run, run 4 used to replace the outlier)* |
| 256 MiB | 2,139 ms | 2,055 ms | 1,489 ms | 1,894 ms |

Two runs were discarded as clear outliers (hash-phase time 2-10x every other run in the same series —
Defender's per-open scan cost, the documented non-deterministic trap, not this change). Past 64 MiB
the mean does not improve and the spread widens (256 MiB), so 64 MiB is where the curve flattens.

**Chosen configuration, measured together** (`BATCH_MAX_FILES = 1000`, `BATCH_MAX_BYTES = 64 MiB`):

| run | db_write |
| --- | --- |
| final-1 | 1,561 ms |
| final-2 | 1,555 ms |
| final-3 | 1,563 ms |

Mean **1,560 ms**, spread <0.5% — the tightest of any series measured for this task. Against the
Task 3 baseline this branch started from (`db_write` ≈ 2,315-2,354 ms with the old fixed
`BATCH_SIZE = 200`), that is roughly **33% lower**.

**Reasoning for the chosen values:**
- `BATCH_MAX_FILES = 1000`: the file-count-only sweep bottoms out here; smaller batches pay more
  fsyncs, larger batches pay more per-commit overhead, for no clear win.
- `BATCH_MAX_BYTES = 64 MiB`: the byte-only sweep flattens here; larger bounds (256 MiB) show no
  further mean improvement while widening variance. It is also the number that keeps the safety
  rationale intact: this corpus's largest files are 1-16 MiB, so a 64 MiB batch caps worst-case
  re-hash-on-interruption at roughly four such files — seconds of rework, not minutes — while a scan
  dominated by small files (the common case, 88% under 64 KB elsewhere in this doc) will usually hit
  the file-count bound first and commit well before the byte bound would matter.
- Together, whichever bound is reached first governs, so the pair is safe for both a many-small-files
  tree (file count triggers) and a few-huge-files tree (byte count triggers) — the situation a fixed
  file count alone could not express.

**Step 6 — proving the byte bound actually discriminates:** with the
`|| *batch_bytes >= BATCH_MAX_BYTES` clause temporarily removed from `rotate_batch`,
`cargo test --lib the_batch_commits_on_bytes_even_when_the_file_count_is_low` failed:
`assertion left == right failed: the byte bound must trigger a commit / left: 3 / right: 0`. Restoring
the clause made it pass again. This confirms the test is not vacuously true — it fails specifically
when the byte bound is disabled, i.e. it does test what it claims to.

**Decision: kept.** `BATCH_MAX_FILES = 1000`, `BATCH_MAX_BYTES = 64 * 1024 * 1024`. `cargo test`,
`cargo clippy --all-targets --locked -- -D warnings`, and `cargo fmt --check` all pass; the
stop/resume regression tests were not modified.

### Task 5: the combined result, measured together

All three kept changes (`prepare_cached`, `synchronous = NORMAL`, the byte-bounded commit trigger)
present at once, measured the same way as Task 4 — fresh `CLEANUPSTORAGES_DATA_DIR` per run,
`--no-count`, two priming runs discarded first, judged on `db_write_ms`. Same corpus (20,000 files,
6.44 GB), same release build, Windows Defender **not** excluded.

`scan --force` (hashing path — every file new):

| run | db_write |
| --- | --- |
| combined-1 | 1,594 ms |
| combined-2 | 5,374 ms *(outlier — discarded from the mean, reported for transparency; hash phase itself was not anomalous, so this is the same per-open Defender variance documented throughout this doc, landing on `db_write` this time instead of `hash`)* |
| combined-3 | 1,671 ms |
| combined-4 | 1,618 ms *(extra run taken to replace the discarded outlier, per the same practice as Task 3)* |

Steady-state mean (runs 1, 3, 4): **1,628 ms**.

`scan` (plain rescan, immediately after — `skip_check` path, every file unchanged):

| run | db_write |
| --- | --- |
| combined-1 | 112 ms |
| combined-2 | 117 ms |
| combined-3 | 115 ms |

Mean **115 ms**, spread <5% — the tightest rescan-path series measured on this branch.

**Against the original baseline (before any #26 change):**

- Hashing path: 4,066 ms → 1,628 ms — **≈60% lower**. This is close to, but not identical to, Task
  4's own final figure (1,560 ms) — a ~4% gap consistent with ordinary run-to-run variance on this
  path, not a sign the changes interact badly with each other.
- Rescan path: 312 ms → 115 ms — **≈63% lower**.

Both paths land within a few percent of the cumulative figures the earlier tasks' own measurements
already implied, so nothing cancelled and nothing needed reverting when combined — the concern this
task exists to check.

**A discrepancy that was found here and has since been corrected above:** Task 3 was originally
written up as "no measurable win on the hashing path", from a first-pass measurement whose runs
spread 47%. A clean A/B (three runs each side, two priming runs, `--no-count`, fresh data dir per
run) found the opposite — a real **−7.7%** from `synchronous = NORMAL` alone, 2,508 ms without it
against 2,315 ms with it, at a within-group spread of 1.4–4.4%.

The Task 3 section above now records both the original figures and the correction, so the document no
longer contradicts itself. This task's combined measurement (1,628 ms after adding Task 4's batching)
is consistent with the corrected trajectory.

The generalisable lesson, and the reason this is written down rather than quietly fixed: **a null
result from a noisy measurement is not a null result, it is an absent one.** The original conclusion
was not merely unlucky — it drew a confident negative from data that could not support any
conclusion. Two priming runs and `--no-count` were the difference between a 47% spread and a 4% one.

**What this means for the 20 TB first scan — the number that matters:**

`db_write` was 12.5–14.1% of the hashing-path pass in the original baseline (call it ~13%). Cutting
that slice by ~60% removes roughly **13% × 60% ≈ 8% of total scan wall time** — not 60% of the scan,
because `db_write` was never more than an eighth of it to begin with. On a five-day (~120 hour) 20 TB
scan, 8% is on the order of **9–10 hours** — real, worth having, but not transformative. That is the
number to plan from, not the 60% figure above, which describes only the slice that changed.

**The 30–44% headline in issue #26 does not transfer here, and this is worth stating plainly for
anyone reading the issue later.** That figure was measured on a **fast-forward pass** (a rescan of an
already-catalogued tree, which does no hashing at all — see `docs/superpowers/specs/2026-08-05-sqlite-write-path-design.md`).
With no hashing to compete against, `db_write` naturally dominates a fast-forward's wall time. A first
scan of 20 TB is not a fast-forward — it is almost entirely hashing (75–84% of the pass throughout
this document) — so the *share* of the pass this branch could ever touch was ~13%, not ~40%, before a
single line of code was written. This document's own rescan-path figures (63% lower `db_write`, 115 ms
mean) are closer in spirit to that fast-forward number, and even they are not directly comparable to
issue #26's original figure since the corpus, Defender state, and code have all since changed.

**Nothing was reverted.** All three changes measured individually — `prepare_cached` (Task 2),
`synchronous = NORMAL` (Task 3, once correctly measured), and the byte-bounded commit trigger
(Task 4) — showed a real, reproducible improvement on at least one of the two paths, and none
regressed the other. The combined measurement above confirms they compose rather than cancel.

**The one number in this whole document that dwarfs everything above:** Windows Defender was **not**
excluded for any figure on this branch, by design (constant conditions for A/B comparison, and the
user was AFK). The separately-measured Defender exclusion earlier in this document is **~30% faster
wall clock** on a real scan — nearly four times the ~8% this entire branch is worth on the hashing
path. If only one lever gets pulled before the 20 TB run, it should be the Defender exclusion, not
anything in #26.

**Two measurement traps cost real time building this section and are worth recording so nobody
re-discovers them:**

1. **The first run after building the binary or touching the corpus is an outlier from OS write-cache
   residue**, not a code effect — this is Task 2's finding restated: two throwaway priming runs
   (discarded) are a prerequisite before any recorded run, every time the corpus or binary changes.
2. **Without `--no-count` and a fresh `CLEANUPSTORAGES_DATA_DIR` per run, spread reached 47%** in an
   early Task 3 measurement (one 4,513 ms outlier against a ~2,738 ms mean) — wide enough that a real
   -7.7% effect read as "flat, inside noise." The fix, used for every measurement from Task 4 onward:
   a fresh data directory per run (so no run inherits another's catalogue) plus `--no-count` (so the
   counting pass, itself variable under cold-cache conditions, is not part of what's being measured).

Reproduce with the same commands as Task 4, run three (or four, if an outlier appears) times per path:

```
D=/tmp/cus-$RANDOM; mkdir -p "$D"
CLEANUPSTORAGES_DATA_DIR="$D" ./target/release/cleanupstorages.exe \
  scan "<corpus>" --force --no-count --readonly-fallback fingerprint 2>&1 | grep -E "^db_write"
rm -rf "$D"
```

Drop `--force` (and scan the same fresh data dir twice, discarding the first scan's output) for the
rescan/`skip_check` path.

## Parallel scanning was tried and abandoned (#23)

A walker → workers → writer pipeline was fully built, reviewed and measured against this drive. It
was **slower than the serial scan at every worker count**, so it was not merged:

| same folder, 225,285 files | wall | overall | walk phase |
| --- | --- | --- | --- |
| serial scan (what ships) | **1.01 h** | **35.1 MB/s** | 247 s |
| pipeline, `--jobs 1` | 1.25 h | 28.3 MB/s | 483 s |
| pipeline, `--jobs 4` | 2.29 h | 15.4 MB/s | — |

On a separate large-file corpus (172 files, ~32 GB) `--jobs 4` was 2.03x slower than `--jobs 1`.

One disk head is a single physical resource: concurrent readers turn sequential reads into seeking,
and per-stream throughput collapsed about sevenfold at 4 workers. Even one worker loses, because the
walker and the hasher are already two competing consumers — the `walk` phase doubled on identical
work.

The code is preserved at git tag `experiment/parallel-scan` with the full reasoning in
`docs/superpowers/specs/2026-07-24-parallel-scan-design.md`. Do not re-attempt this for spinning
drives without new evidence. (It does help on NVMe, where overlap measured 385% accounted.)

## Identical-tree collapse (#38) — measured against the live catalogue

The point of collapsing identical folders is to cut the *number of decisions*, so the number that
matters is how many duplicate groups stop needing an individual answer.

Measured on a read-only copy of the live catalogue: 3 volumes, 808,588 rows, 535,313 of them loose.

| | archives as leaves | **archives descended (shipped)** |
| --- | --- | --- |
| maximal identical-tree groups | 1,773 | **1,458** |
| folders involved | 3,984 | **4,251** |
| duplicate groups explained by them | 109,023 (86.5%) | 84,449 (58.5%) |
| groups left needing per-file review | 16,954 | 59,924 |
| reclaimable by collapsing | 29.7 GB | **53.2 GB** |
| members inside an archive (needs repack) | 0 | **1,911** |

Against 125,977 duplicate groups reviewed one at a time — roughly 350 hours at ten seconds a
decision — 1,458 decisions is about five hours.

**Read the two columns together.** Descending into archives nearly doubles the space collapsing
recovers (29.7 → 53.2 GB), which is why it was chosen. It also triples the residual per-file queue,
because it surfaces duplicates that exist only *inside* archives. Those are not independently
actionable — deleting one entry means repacking its container — so they are excluded from the
per-file duplicate view and reachable only through their archive.

Collapsing recovers 53.2 GB of the 72.1 GB total reclaimable. It is a decision-count win first and a
space win second.

### The Rust implementation was checked against an independent one

`scripts/measure-tree-collapse.py` computes the same thing in Python, and is committed so the
numbers can be re-derived rather than trusted. Run over the same catalogue copy, the two agree
exactly on directory nodes (80,202), maximal groups (1,458), folders involved (4,251) and
reclaimable bytes (53.2 GB).

They disagreed on one figure, and **the Python was the wrong one**: it counted a node as "inside an
archive" when the path equalled the archive as well as when it was beneath it, so all 55 archive
roots were miscounted (1,966 rather than 1,911). Those are precisely the case that CAN be moved by a
single rename. The discrepancy was resolved by finding which implementation was wrong, not by
adjusting either to match the other.

### Caveats, so the ratio is not over-read

- This is the **partial catalogue**, not the full 20 TB. The ratio is directional, not a promise.
- The maximal rule suppresses a folder whenever its parent is duplicated. That is exact in the
  common case but can miss a folder paired with a different partner than its parent's, so 1,458 is a
  slight **under**count. For a UI offering a destructive action, under-reporting is the right
  direction to be wrong in.
- A first draft of the Python measurement had a real bug: an archive is both a file row and a set of
  entry rows, and the script kept whichever it saw first, silently orphaning some entry trees. The
  same class of bug then turned up in the Rust — recognising an archive by its entries' *immediate*
  parent works only when entries sit directly inside it, and real archives nest. Both are fixed and
  both have regression tests. **An archive appearing as both a leaf and a directory is the failure
  mode to check first if these numbers ever move.**

### Memory: this does not yet scale to 20 TB, and that is measured not guessed

`rebuild_directory_trees` materialises every active row for a volume before hashing. Peak working
set on the live catalogue copy (808,588 rows, 3 volumes):

| | peak |
| --- | --- |
| first implementation | 194 MB |
| after removing the per-row volume id and moving hashes instead of cloning them | **160 MB** (-17.5%) |

That works out at roughly **198 bytes per row**. The design spec for the write path puts the 20 TB
corpus at *"on the order of 50 million"* files, which projects to about **9.9 GB** for a single
rebuild — down from ~12 GB, and still far too much for the machine this runs on.

**So the current implementation is fine for the catalogue as it stands and is NOT fine for the full
20 TB scan.** The fix is to stream rather than materialise: read rows ordered by path and fold them
with a stack, so only the current root-to-leaf spine is in memory. That is a redesign of
`build_dir_hashes`, deliberately not bolted onto the end of the feature branch — see the tracking
issue.

Re-measure with `examples/validate_trees.rs` against a catalogue copy; it prints the same aggregate
figures the Python script does, so a memory change that alters behaviour is immediately visible.

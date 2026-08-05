# SQLite Write-Path Wins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the scan's per-file write path cheaper before a 20 TB, five-day scan — and prove each change earns its place, or revert it.

**Architecture:** Three independent changes to the write path: cached statements on the per-file sites, `synchronous = NORMAL` in WAL, and a commit trigger bounded by bytes as well as by file count. Each is measured on its own against the existing `Phase::DbWrite` instrumentation.

**Tech Stack:** Rust, rusqlite/SQLite (WAL).

## Global Constraints

- **Nothing may ever be lost or corrupted.** ~20 TB of irreplaceable data.
- **Measurement decides.** Anything that does not measurably help is reverted, and the reversion is recorded. This project has already abandoned parallel scanning, rewritten the ratio cap, and re-explained the counting pass on measurement — a "cheap win" that does not show up is not a win.
- **The stop/resume contract is unchanged**: a stopped scan commits what it has, marks nothing missing, and resumes via the incremental skip. Existing regression tests must pass unmodified.
- **No schema, index, or `content_hash` change** — that is #32, deliberately deferred.
- **Never touch the user's real catalogue or data directory** (`%APPDATA%\justPrototype\CleanUpStorages\`). Set `CLEANUPSTORAGES_DATA_DIR` to a temp dir on **every** cargo invocation and binary run — known issue #44 means one unprotected run writes into the real backups folder.
- Gates for every task: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Commit trailers on every commit:
  ```
  Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
  Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
  ```

## Known pre-existing issue

`scanner::tests::run_scan_logs_volume_resolution` is flaky under parallel execution (#39). Re-run if it fires; do not fix it here.

## The measurement discipline that governs Tasks 2-4

`docs/benchmarking-scans.md` documents three traps. All three apply, and a number taken without them is worthless:

1. **Windows Defender** — exclude the benchmark folder or every figure is noise.
2. **Cold vs warm cache** — label each figure; compare only like with like.
3. **First pass vs rescan** — `--force` exercises the hashing path; a plain rescan exercises `skip_check`. Never compare one to the other.

Every measurement in this plan uses **the same fixed folder**, built once in Task 1 and reused, with `CLEANUPSTORAGES_DATA_DIR` pointed at a fresh temp dir per run.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/catalog/store.rs` (modify) | `prepare_cached` on the five per-file statements |
| `src/catalog/mod.rs` (modify) | `synchronous = NORMAL`, with its rationale |
| `src/scanner.rs` (modify) | Commit trigger bounded by files **and** bytes |
| `docs/benchmarking-scans.md` (modify) | The recorded before/after figures |

---

### Task 1: A repeatable benchmark, and the baseline

**Files:**
- Create: `scripts/bench-write-path.ps1`
- Test: none (this task produces a measurement, not behaviour)

**Interfaces:**
- Produces: a script that scans a fixed corpus into a throwaway data dir and prints wall time plus the `Phase` breakdown; and a recorded baseline for the current `main`.

**Why this is a task and not a preamble:** every later task is judged against this baseline. If the baseline is not repeatable, none of the later numbers mean anything, and this plan degenerates into three changes kept because they sounded reasonable.

- [ ] **Step 1: Build a fixed benchmark corpus**

Generate it deterministically so it can be rebuilt identically. Aim for a corpus whose *file count*
is large enough that per-file costs dominate — this plan is about per-file write overhead, so many
small files is the right shape, not a few large ones. Around 40,000–60,000 files of mixed small sizes
is enough to make `db_write` legible without taking an hour per run.

Reuse `scripts/make-test-sandbox.ps1`'s style. The corpus must live under a throwaway root, never in
the user's real data.

- [ ] **Step 2: Write the benchmark script**

`scripts/bench-write-path.ps1` must:
- take a label argument, so each run is recorded against what it was testing;
- create a **fresh** temp `CLEANUPSTORAGES_DATA_DIR` per run (never the real one);
- run `scan --force` (the hashing path) and also a plain rescan (the `skip_check` path), reporting both;
- print the `Phase` breakdown the scan already emits, plus wall clock;
- state cache condition (cold/warm) in its output so figures cannot be silently mixed.

- [ ] **Step 3: Take the baseline, three times**

Run it three times on unmodified `main`. Report all three, not an average — if they disagree by more
than a few percent the benchmark is not yet repeatable and must be fixed before proceeding. Note
whether the Defender exclusion is in place; if it is not, say so and apply it.

- [ ] **Step 4: Record the baseline**

Add a "Write-path tuning (#26)" section to `docs/benchmarking-scans.md` with the three baseline runs
and the conditions they were taken under. Later tasks append to this table.

- [ ] **Step 5: Commit**

```bash
git add scripts/bench-write-path.ps1 docs/benchmarking-scans.md
git commit -m "bench: repeatable write-path benchmark and baseline

Every change in #26 is judged against this. An unrepeatable baseline
would turn the whole exercise into three changes kept because they
sounded reasonable.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `prepare_cached` on the per-file statements

**Files:**
- Modify: `src/catalog/store.rs` — `get_file_meta` (:63), `upsert_file` (:128), `touch_seen` (:159), `upsert_archive_entry` (:215), `touch_archive_entries` (:269)
- Test: existing tests must pass unmodified

**Interfaces:**
- Consumes: the benchmark from Task 1.
- Produces: no API change. Same SQL, same parameters, same behaviour.

**Scope is deliberately narrow.** Only statements that run **once or more per file** are converted —
on a 20 TB corpus those execute on the order of 50 million times each. Everything else
(`forget_volume`, snapshots, settings and pending-format handlers) keeps `execute`: caching a
statement that runs once per scan buys nothing and widens the diff for no reason.

- [ ] **Step 1: Convert the five sites**

Replace `self.conn.execute(SQL, params)` with `self.conn.prepare_cached(SQL)?.execute(params)`, and
the `query_row` in `get_file_meta` with the `prepare_cached` equivalent. The SQL text must be
**identical** — the cache is keyed on it, so an accidental whitespace change silently creates a
second cache entry.

Add a short comment at the first converted site explaining why these five and not the others.

- [ ] **Step 2: Run the tests**

Run: `cargo test` (with `CLEANUPSTORAGES_DATA_DIR` set)
Expected: PASS, unmodified. This change alters no SQL and no parameters, so **any** test whose result
changes means something is wrong — stop and report rather than adjusting the test.

- [ ] **Step 3: Measure**

Run the Task 1 benchmark three times with label `prepare_cached`. Report all three runs.

- [ ] **Step 4: Decide, and record the decision**

If `db_write` improves measurably beyond run-to-run variance, keep it and append the figures to
`docs/benchmarking-scans.md`. **If it does not, revert the change** and record that it did not help —
a null result is a real result and belongs in the document.

State explicitly in your report which way it went and by how much.

- [ ] **Step 5: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/store.rs docs/benchmarking-scans.md
git commit -m "perf(catalog): cache the per-file statements

<replace with the measured result, or with the reversion and its number>

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `PRAGMA synchronous = NORMAL`

**Files:**
- Modify: `src/catalog/mod.rs` (`Catalog::open`, beside the existing `journal_mode`/`busy_timeout` at :24-27)
- Test: `src/catalog/mod.rs` or `src/catalog/schema.rs` tests

**Interfaces:**
- Consumes: the benchmark from Task 1.
- Produces: no API change.

**The safety rationale must be in the code, not only in the spec.** "Reduce durability" needs
justifying in a project whose whole premise is never losing data, and the next reader deserves the
argument rather than the bare pragma.

In WAL mode, `synchronous = NORMAL` does not fsync on every commit; it syncs at checkpoints. SQLite
guarantees WAL + `NORMAL` **cannot corrupt the database** — a power loss can lose the most recent
transactions, never leave a torn file. Here that costs at most the last committed batch of files,
which are simply not yet catalogued, and the next scan re-hashes them through the ordinary
incremental skip. Nothing on disk is touched and nothing is marked missing.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_catalog_opens_in_wal_with_synchronous_normal() {
        // WAL + NORMAL is the pairing that makes dropping the per-commit fsync safe: it cannot
        // corrupt the file, only lose the most recent commits -- which a rescan rebuilds.
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        let journal: String = cat
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        // 1 == NORMAL. FULL (2) would keep the fsync we are removing; OFF (0) would be unsafe.
        let sync: i64 = cat
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "expected NORMAL");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib the_catalog_opens_in_wal_with_synchronous_normal`
Expected: FAIL — the default is FULL (2).

- [ ] **Step 3: Implement**

In `Catalog::open`, beside the existing pragmas:

```rust
        // NORMAL, not the default FULL: in WAL this drops one fsync per COMMIT and *cannot corrupt
        // the database* -- a power loss can lose the most recent commits, never leave a torn file.
        // What that costs here is at most the last batch of files, which are simply not yet
        // catalogued; the next scan re-hashes them through the ordinary incremental skip. No file on
        // disk is touched and nothing is marked missing, which is why a durability reduction is
        // acceptable here and would not be elsewhere in this project.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
```

Leave `open_readonly` alone — it never writes.

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS, including the existing integrity and snapshot tests unmodified.

- [ ] **Step 5: Measure**

Run the Task 1 benchmark three times with label `synchronous_normal`. Report all three.

- [ ] **Step 6: Decide and record**

Keep it if it measurably helps; revert and record if not. Append the figures either way.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/mod.rs docs/benchmarking-scans.md
git commit -m "perf(catalog): synchronous=NORMAL in WAL

<replace with the measured result>

Cannot corrupt the database -- worst case is losing the most recent
commits, which a rescan rebuilds through the incremental skip.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: A commit trigger bounded by bytes as well as files

**Files:**
- Modify: `src/scanner.rs` — `BATCH_SIZE` (:11) and `rotate_batch` (:89)
- Test: `src/scanner.rs` `mod tests`

**Interfaces:**
- Produces:
  ```rust
  const BATCH_MAX_FILES: usize = /* measured */;
  const BATCH_MAX_BYTES: u64 = /* measured */;
  fn rotate_batch(cat: &Catalog, in_batch: &mut usize, batch_bytes: &mut u64) -> anyhow::Result<()>;
  ```

**Why bytes and not just a bigger count — this is the point of the task.** Raising the file count
reduces fsyncs, but a stopped, crashed, or power-cut scan loses the **current uncommitted batch**,
and those files are re-hashed on resume. A file count cannot bound that: 200 video files is minutes
of re-hashing, 200 text files is milliseconds. The byte bound is what makes a larger file count safe.

This matters more than it would have a month ago, because this project now has stop/resume and the
user is about to run a five-day scan they may well interrupt.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_batch_commits_on_bytes_even_when_the_file_count_is_low() {
        // The point of the byte bound: a handful of large files must still commit, or a stopped
        // scan would have to re-hash all of them. A count-only trigger cannot express this.
        let (_t, cat) = setup();
        cat.conn.execute_batch("BEGIN").unwrap();
        // Well below BATCH_MAX_FILES, well above BATCH_MAX_BYTES. Declared at their final values:
        // assigning then overwriting would trip `unused_assignments` under `-D warnings`.
        let mut in_batch = 3usize;
        let mut batch_bytes = BATCH_MAX_BYTES + 1;
        rotate_batch(&cat, &mut in_batch, &mut batch_bytes).unwrap();
        assert_eq!(in_batch, 0, "the byte bound must trigger a commit");
        assert_eq!(batch_bytes, 0, "and reset the byte accumulator");
        cat.conn.execute_batch("COMMIT").ok();
    }

    #[test]
    fn the_batch_still_commits_on_the_file_count() {
        let mut in_batch = BATCH_MAX_FILES;
        let mut batch_bytes = 0u64;
        let (_t, cat) = setup();
        cat.conn.execute_batch("BEGIN").unwrap();
        rotate_batch(&cat, &mut in_batch, &mut batch_bytes).unwrap();
        assert_eq!(in_batch, 0);
        cat.conn.execute_batch("COMMIT").ok();
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib the_batch_commits_on_bytes`
Expected: FAIL to compile — `rotate_batch` takes two arguments and the constants do not exist.

- [ ] **Step 3: Implement**

```rust
/// Commit when EITHER bound is reached.
///
/// The byte bound is what makes a larger file count safe. A stopped or interrupted scan loses the
/// current uncommitted batch and re-hashes those files on resume; a file count alone cannot bound
/// that cost, because 200 video files and 200 text files are wildly different amounts of work.
const BATCH_MAX_FILES: usize = /* from Task 1's measurement */;
const BATCH_MAX_BYTES: u64 = /* from Task 1's measurement */;

fn rotate_batch(cat: &Catalog, in_batch: &mut usize, batch_bytes: &mut u64) -> anyhow::Result<()> {
    if *in_batch >= BATCH_MAX_FILES || *batch_bytes >= BATCH_MAX_BYTES {
        cat.conn.execute_batch("COMMIT; BEGIN")?;
        *in_batch = 0;
        *batch_bytes = 0;
    }
    Ok(())
}
```

Thread a `batch_bytes` accumulator alongside the existing `in_batch` through the scan loop, adding
each file's size after it is written. Every `rotate_batch` call site needs the new argument.

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS. The stop/resume regression tests must pass **unmodified** — if any of them changes
behaviour, stop and report.

- [ ] **Step 5: Measure, and choose the constants**

Try at least three file-count values (e.g. 200, 1000, 5000) with the byte bound held constant, then
vary the byte bound. Report the table. Choose values where the curve flattens — past that point
larger batches buy little and cost more re-work on interruption.

State the chosen values and the reasoning in your report, and record the table in
`docs/benchmarking-scans.md`.

- [ ] **Step 6: Prove the byte bound discriminates**

Remove the `|| *batch_bytes >= BATCH_MAX_BYTES` clause, run
`cargo test --lib the_batch_commits_on_bytes_even_when_the_file_count_is_low`, confirm it FAILS,
restore, confirm green. Report both outputs.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/scanner.rs docs/benchmarking-scans.md
git commit -m "perf(scanner): bound the commit batch by bytes as well as files

<replace with the measured table and the chosen values>

A file count alone cannot bound what a stopped scan has to re-hash.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: The combined result, and the honest write-up

**Files:**
- Modify: `docs/benchmarking-scans.md`

**Why:** three individually-small wins can add up, or can interact and cancel. Only a combined
measurement answers that, and it is the number that actually predicts the 20 TB run.

- [ ] **Step 1: Measure the combination**

With whichever changes survived their own tasks, run the benchmark three times on **both** paths
(`--force` and plain rescan). Report all runs.

- [ ] **Step 2: Write it up honestly**

Add the combined figures to `docs/benchmarking-scans.md`, and state plainly:

- what the total saving is on the `--force` (hashing) path — the one that predicts the 20 TB run;
- what it is on the rescan path, where `db_write` dominates and the number will look better;
- **the caveat that the headline 30–44% came from a fast-forward pass** and does not transfer to a
  first scan, so readers do not over-read it;
- any change that was reverted, and its null result.

If the combined saving on the hashing path is small, say so. A modest honest number is worth more
than a flattering one, and this document is what the next person will plan from.

- [ ] **Step 3: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add docs/benchmarking-scans.md
git commit -m "docs: measured results of the write-path tuning

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final review

Check against the spec's six success criteria, and pay particular attention to:

1. **Did every change earn its place by measurement, and was anything that did not get reverted?**
   The failure mode for this branch is keeping a change because it is theoretically sound. Verify the
   recorded numbers actually support what was kept.
2. **Is the stop/resume contract genuinely unchanged?** Task 4 alters when commits happen, which is
   exactly the mechanism a stopped scan depends on. Confirm the existing regression tests pass
   unmodified and that a stopped scan still commits what it has, marks nothing missing, and resumes.
3. **Does `synchronous = NORMAL` interact with the integrity check or the snapshot mechanism?** Both
   are the project's rollback story; neither should change.
4. **Was the benchmark repeatable?** If the three baseline runs disagreed materially, every
   conclusion downstream is suspect.

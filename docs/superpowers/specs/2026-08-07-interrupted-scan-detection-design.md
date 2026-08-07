# Detecting an interrupted scan — design

**Status:** approved
**Date:** 2026-08-07
**Closes:** #36 (a hard-killed scan is recorded as `running` forever)
**Epic:** #2 (scan control & visibility)

## Why

`start_scan_run` writes `status='running'` and only `finish_scan_run` ever changes it. A process that
never returns — power loss, the drive yanked, `kill -9`, Task Manager — leaves the row `running`
permanently. A *graceful* stop already records `cancelled`, so this is only the hard-kill case.

The cost is display, not data: the missing-file sweep runs after the final commit and is guarded by
`if !summary.stopped`, so an interrupted scan never marks anything missing. But the Scan page and
`status` show a scan that appears to run forever, and the live catalogue already has such a row. On a
five-day scan of drives that get unplugged, this will happen again.

## The constraint that shapes the design

The obvious fix — reconcile stale `running` rows on `Catalog::open` — is wrong: more than one
process can hold the catalogue open. `browse` is long-lived and a CLI `scan` can run against the
same catalogue concurrently, so restarting `browse` would declare a live scan dead.

**And the liveness signal cannot live in SQLite.** The scanner opens a write transaction and holds
it, committing only at batch rotation (`scanner.rs`, `BEGIN` … `COMMIT; BEGIN`). Two consequences:

- a heartbeat written from a *second connection* blocks on the write lock, and during a single large
  file there is no commit to release it — a 100 GB file at 35 MB/s is ~48 minutes of silence;
- a heartbeat written on the *scan's own connection* is inside the open transaction, so no other
  process can see it until that transaction commits.

Either way an alive scan would be reported dead. The signal therefore has to sit outside the
database.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Liveness signal | **A heartbeat file whose mtime is refreshed by a ticker thread** | Outside SQLite, so the scan's write lock is irrelevant. No platform-specific process APIs |
| Where the truth lives | **Read-time derivation only** | No process ever rewrites another's row. A stale `running` row is *displayed* as interrupted; the stored value is left alone |
| New status | **`interrupted`**, distinct from `cancelled` | An unclean end is not a user decision, and conflating them would hide the difference that matters when diagnosing a five-day scan |
| Tick / staleness | **15 s tick, 120 s stale** | Eight missed ticks. Fast enough to be useful, loose enough to survive a stalled machine |

## Architecture

```rust
/// Refreshes `<catalog dir>/scan-heartbeats/<run_id>` every 15s until dropped, then removes it.
pub struct Heartbeat { /* thread handle + stop flag */ }
impl Heartbeat {
    pub fn start(catalog_path: &Path, run_id: i64) -> Heartbeat;
}
impl Drop for Heartbeat { /* signal stop, join, delete the file */ }

/// True when a run of this id looks alive: its heartbeat file exists and was touched recently.
pub fn is_alive(catalog_path: &Path, run_id: i64, now: i64) -> bool;
```

The scan holds a `Heartbeat` for its whole life. `Drop` covers the normal, error and panic-unwind
paths; only a hard kill leaves the file behind, and a left-behind file stops being refreshed, which
is exactly the signal.

Reading: a row with `status == "running"` whose heartbeat is not fresh is reported as `interrupted`.
Everything else is reported as stored. The derivation lives in one place so the Scan page and the CLI
cannot disagree.

**No schema change.** The heartbeat file's mtime is the heartbeat; adding a column would only
duplicate it into the medium that cannot carry it.

### Why a missing file means interrupted, not "never started"

`start_scan_run` commits the `running` row before the scan opens its transaction, and the heartbeat
is created immediately after. A row can therefore exist for a moment with no heartbeat file. The
staleness test uses the run's `started_at` as a floor: a run is alive if its heartbeat is fresh **or**
it started within the staleness window. Without that, a run could be declared interrupted in the
first instants of its life.

### Clock movement

`now - mtime` can go negative if the clock moves backwards (see #45 for the same hazard in the
sweep). A negative age is treated as fresh, never as stale: the failure mode of guessing "alive" is a
row that stays `running` a little longer, and the failure mode of guessing "dead" is telling the user
a running scan has died. The first is the one to prefer.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **A live scan is reported interrupted** — the worst outcome, since it invites the user to start a second scan over the same drive | The signal is outside SQLite so the write lock cannot silence it; the thread ticks on wall time, independent of what the scan is doing; and clock skew resolves to "alive" |
| The heartbeat directory accumulates files | `Drop` removes them on every non-hard-kill exit, and a stale file is overwritten when its run id is reused. They are empty files |
| A reader on another machine sees no heartbeat | Out of scope: the catalogue is documented as living on the computer, not on the scanned drives |
| Derivation drifts between the Scan page and the CLI | One shared helper, used by both |

## Non-goals

- No reconciliation writes. Nothing rewrites a stored `running` value, so no process can mislabel
  another's work.
- No change to the sweep, to stop/resume, or to what a scan catalogues.
- No process-liveness APIs (`OpenProcess`, `kill(pid, 0)`) and no pid-reuse handling.

## Success criteria

1. A run whose heartbeat has gone stale is reported `interrupted`, not `running`.
2. A run with a fresh heartbeat is still reported `running`, even while the scan holds its write
   transaction — the case a SQLite-based heartbeat gets wrong.
3. A run that has just started is not reported interrupted before its first tick.
4. A completed, cancelled or failed run is reported exactly as stored.
5. A clock that moves backwards never turns a live run into an interrupted one.
6. The Scan page and the CLI agree, because they share one derivation.

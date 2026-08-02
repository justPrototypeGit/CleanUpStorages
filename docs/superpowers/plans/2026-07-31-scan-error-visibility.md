# Scan-Error Visibility (Completeness Audit) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `scan_errors` from an invisible append-only log into a per-volume completeness audit that answers "is this catalogue complete, and what is missing?" — on the Drives page and the CLI.

**Architecture:** Errors gain a `phase` and a locale-independent `kind` recorded at scan time, and become one row per `(volume_id, path)` instead of one per failure per scan. They self-heal: file errors clear when that path is successfully re-catalogued (keyed on `last_seen_at`, the same mechanism that makes the missing-file sweep safe under stop/resume), and directory/archive-entry errors clear at the end of a completed scan that did not re-record them. A three-bucket query (absent / unverified / unreadable directories) drives a Drives-page panel, a read-only endpoint, and CLI summary lines.

**Tech Stack:** Rust, rusqlite/SQLite (WAL), axum 0.7, plain HTML/CSS/JS (no build step, no CDN).

## Global Constraints

- **Nothing may ever be lost or corrupted.** This tool operates on ~20 TB of irreplaceable data. A change that could lose or mis-mark data is never acceptable.
- **A scan that did not finish never sweeps, and never over-clears.** The stopped-scan rule established in `docs/superpowers/specs/2026-07-31-scan-stop-and-progress-design.md` extends here: a stopped scan clears errors only for paths it actually re-reached.
- **Classification never reads message text.** `kind` derives from `std::io::ErrorKind` and the raw OS error code. Windows `io::Error` messages are localized by the OS (the dev machine is Italian), so text matching would misclassify on the machines this feature serves.
- **An unreadable directory is never counted as one missing file.** The number of files beneath it is unknown; it gets its own bucket labelled "contents unknown".
- **No new crates.** No CDN, no fonts fetched at runtime, no frontend build step.
- **The web server binds 127.0.0.1 only.** Read-only `GET` endpoints carry no CSRF token, matching every other read endpoint; do not add auth or CORS.
- Gates for every task: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Commit trailers on every commit:
  ```
  Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
  Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
  ```

## File Structure

| File | Responsibility |
| --- | --- |
| `src/catalog/schema.rs` (modify) | Add `phase`/`kind` columns, dedupe existing rows, create the unique + lookup indexes |
| `src/catalog/scan_errors.rs` (**create**) | Everything about recording, classifying, clearing and querying scan errors. Mirrors how `scan_runs.rs` owns the run table. Moves `log_scan_error` out of the already-large `store.rs`. |
| `src/scanner.rs` (modify) | Pass `phase` + classified `kind` at the five error sites; call self-heal at end of scan |
| `src/web.rs` (modify) | `GET /api/volumes/:id/errors`; completeness counts on `DriveDto` |
| `src/web_ui.rs` (modify) | Drives-page completeness panel |
| `src/commands.rs` (modify) | Completeness line in `status` and at the end of `scan` |

`log_scan_error` currently lives in `store.rs` (line 201). Task 2 moves it into the new module and updates callers — `store.rs` is already large and this is a cohesive unit, matching the existing `scan_runs.rs` precedent.

---

### Task 1: Schema migration — columns, dedupe, indexes

**Files:**
- Modify: `src/catalog/schema.rs:118-121` (the `ensure_column` block), and the index section
- Test: `src/catalog/schema.rs` (its existing `mod tests`)

**Interfaces:**
- Produces: `scan_errors` has nullable `phase TEXT` and `kind TEXT`; a `UNIQUE(volume_id, path)` index named `idx_scan_errors_identity`; a `volume_id` index named `idx_scan_errors_volume`.

**Background the implementer needs:** `ensure_column` (`schema.rs:196`) is an existing idempotent helper — it checks `pragma_table_info` and only then runs `ALTER TABLE`. Four columns already use it. **The unique index cannot simply be created:** real catalogues contain many duplicate `(volume_id, path)` rows, because until now every scan appended a fresh row for the same failing path. Creating the index without deduping first fails, and a failing migration means the catalogue will not open at all.

SQLite treats `NULL`s as distinct in a unique index, so rows with `volume_id IS NULL` are neither deduped nor constrained. That is acceptable — the scanner always passes a volume id — and is called out so nobody is surprised later.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/catalog/schema.rs`:

```rust
#[test]
fn migration_dedupes_scan_errors_then_enforces_one_row_per_path() {
    // A pre-existing catalogue has one row per failure per scan. The unique index cannot be
    // created over that, so the migration must collapse them first -- keeping the newest.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("catalog.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE scan_errors (
                id INTEGER PRIMARY KEY, volume_id TEXT, path TEXT NOT NULL,
                reason TEXT NOT NULL, occurred_at INTEGER NOT NULL
            );
            INSERT INTO scan_errors(volume_id, path, reason, occurred_at)
                 VALUES ('v','a/x.pst','read: old',100),
                        ('v','a/x.pst','read: newer',200),
                        ('v','a/x.pst','read: newest',300),
                        ('v','b/y.jpg','read: other',150);
            "#,
        )
        .unwrap();
    }

    let cat = Catalog::open(&db).unwrap();

    let n: i64 = cat
        .conn
        .query_row("SELECT count(*) FROM scan_errors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2, "three rows for one path collapse to one");

    let kept: String = cat
        .conn
        .query_row(
            "SELECT reason FROM scan_errors WHERE path='a/x.pst'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "read: newest", "the newest row survives");

    // The columns exist and the constraint is live.
    cat.conn
        .execute(
            "INSERT INTO scan_errors(volume_id,path,reason,occurred_at,phase,kind)
             VALUES ('v','c/z.bin','read: x',400,'read','io')",
            [],
        )
        .unwrap();
    let dup = cat.conn.execute(
        "INSERT INTO scan_errors(volume_id,path,reason,occurred_at,phase,kind)
         VALUES ('v','c/z.bin','read: again',500,'read','io')",
        [],
    );
    assert!(dup.is_err(), "a second row for the same path is rejected");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib migration_dedupes_scan_errors -- --nocapture`
Expected: FAIL — `phase` column does not exist, and no unique constraint, so the "dup" insert succeeds.

- [ ] **Step 3: Implement the migration**

In `src/catalog/schema.rs`, next to the existing `ensure_column` calls (after line 121):

```rust
    ensure_column(conn, "scan_errors", "phase", "TEXT")?;
    ensure_column(conn, "scan_errors", "kind", "TEXT")?;

    // Until now every scan appended a fresh row for the same failing path, so an existing
    // catalogue holds duplicates and CREATE UNIQUE INDEX would fail -- taking the whole catalogue
    // offline, since this runs on open. Collapse to the newest row per path first.
    conn.execute_batch(
        r#"
        DELETE FROM scan_errors WHERE id NOT IN (
            SELECT MAX(id) FROM scan_errors GROUP BY volume_id, path
        );
        -- NULL volume_id rows are neither deduped nor constrained (SQLite treats NULLs as
        -- distinct). The scanner always supplies a volume id, so this affects nothing real.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_scan_errors_identity
            ON scan_errors(volume_id, path);
        CREATE INDEX IF NOT EXISTS idx_scan_errors_volume ON scan_errors(volume_id);
        "#,
    )?;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib migration_dedupes_scan_errors`
Expected: PASS

- [ ] **Step 5: Run the full gates**

Run: `cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: all green. Existing schema tests must pass unmodified.

- [ ] **Step 6: Commit**

```bash
git add src/catalog/schema.rs
git commit -m "feat(catalog): scan_errors gains phase/kind and one row per path

Existing catalogues hold duplicate (volume_id, path) rows because every
scan appended a fresh one, so the migration collapses them to the newest
before creating the unique index -- this runs on open, and a failing
migration would take the catalogue offline.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Classification and recording

**Files:**
- Create: `src/catalog/scan_errors.rs`
- Modify: `src/catalog/mod.rs` (add `pub mod scan_errors;`), `src/catalog/store.rs:201-219` (remove `log_scan_error`)
- Test: in the new file's `mod tests`

**Interfaces:**
- Consumes: the `phase`/`kind` columns and unique index from Task 1.
- Produces:
  - `pub fn classify_io(e: &std::io::Error) -> &'static str`
  - `impl Catalog { pub fn log_scan_error(&self, volume_id: Option<&str>, path: &str, reason: &str, phase: &str, kind: &str, now: i64) -> anyhow::Result<()> }`
  - Valid `phase` values: `"walk"`, `"metadata"`, `"read"`, `"archive_open"`, `"archive_entry"`.
  - Valid `kind` values: `"permission"`, `"locked"`, `"not_found"`, `"io"`, `"other"`.

**Background:** `log_scan_error` exists today at `src/catalog/store.rs:201` with the signature `(volume_id, path, reason, now)`. Move it — do not leave a copy behind. Its five callers in `src/scanner.rs` are updated in Task 3; to keep the tree compiling between tasks, this task updates those call sites mechanically by passing `"read"`/`"other"` placeholders **only where needed to compile**, and Task 3 replaces them with correct values. Prefer instead to do Task 2 and Task 3 back to back.

On Windows a file locked by another process surfaces as OS error 32 (`ERROR_SHARING_VIOLATION`) or 33 (`ERROR_LOCK_VIOLATION`), and `ErrorKind` for it is not stable across Rust versions — so the raw code is checked. `zip` crate errors are not `io::Error`, so archive-entry failures record `"other"`.

- [ ] **Step 1: Write the failing test**

Create `src/catalog/scan_errors.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use std::io::{Error, ErrorKind};

    #[test]
    fn classification_never_reads_the_message_text() {
        // The dev machine's Windows is Italian, so io::Error messages arrive translated.
        // Classification must come from ErrorKind and the raw OS code, never the string --
        // these errors carry deliberately misleading text to prove it.
        assert_eq!(
            classify_io(&Error::new(ErrorKind::PermissionDenied, "totally unrelated words")),
            "permission"
        );
        assert_eq!(
            classify_io(&Error::new(ErrorKind::NotFound, "permission denied")),
            "not_found"
        );
        assert_eq!(classify_io(&Error::from_raw_os_error(32)), "locked");
        assert_eq!(classify_io(&Error::from_raw_os_error(33)), "locked");
        assert_eq!(
            classify_io(&Error::new(ErrorKind::Other, "file is locked")),
            "io",
            "text saying 'locked' must not make it 'locked'"
        );
    }

    #[test]
    fn recording_the_same_path_twice_updates_rather_than_duplicates() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();

        cat.log_scan_error(Some("v"), "a/x.pst", "read: locked", "read", "locked", 100)
            .unwrap();
        cat.log_scan_error(Some("v"), "a/x.pst", "read: i/o error", "read", "io", 200)
            .unwrap();

        let (n, reason, kind, at): (i64, String, String, i64) = cat
            .conn
            .query_row(
                "SELECT count(*), max(reason), max(kind), max(occurred_at) FROM scan_errors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "one row per path, however many scans hit it");
        assert_eq!(reason, "read: i/o error", "the latest failure wins");
        assert_eq!(kind, "io");
        assert_eq!(at, 200);
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --lib scan_errors::`
Expected: FAIL to compile — `classify_io` and the module do not exist.

- [ ] **Step 3: Implement the module**

Put this above the test module in `src/catalog/scan_errors.rs`:

```rust
//! Recording scan errors, and classifying them so they can be grouped.
//!
//! Separate from `store.rs` for the same reason `scan_runs.rs` is: one table, one cohesive set of
//! operations, in a file small enough to hold in your head.

use crate::catalog::Catalog;
use rusqlite::params;

/// Classify an I/O failure for grouping.
///
/// From `ErrorKind` and the raw OS code, **never** the message: Windows renders `io::Error`
/// messages in the OS language (this project's dev machine is Italian), so text matching would
/// misclassify on exactly the machines this feature exists to serve.
pub fn classify_io(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => "permission",
        ErrorKind::NotFound => "not_found",
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION: another process holds the file. Checked
        // by raw code because the ErrorKind mapping for these is not stable across Rust versions.
        _ => match e.raw_os_error() {
            Some(32) | Some(33) => "locked",
            _ => "io",
        },
    }
}

impl Catalog {
    /// Record (or refresh) the failure for one path. One row per `(volume_id, path)`: a path that
    /// fails on twenty scans is one problem, not twenty.
    pub fn log_scan_error(
        &self,
        volume_id: Option<&str>,
        path: &str,
        reason: &str,
        phase: &str,
        kind: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_errors(volume_id, path, reason, occurred_at, phase, kind)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(volume_id, path) DO UPDATE SET
                 reason=excluded.reason, occurred_at=excluded.occurred_at,
                 phase=excluded.phase, kind=excluded.kind",
            params![volume_id, path, reason, now, phase, kind],
        )?;
        Ok(())
    }
}
```

Add `pub mod scan_errors;` to `src/catalog/mod.rs` beside the other module declarations, and delete `log_scan_error` from `src/catalog/store.rs` (lines 201-219).

- [ ] **Step 4: Update the five call sites so the tree compiles**

In `src/scanner.rs`, add the two new arguments at each site. Task 3 sets the correct values; for now use the phase that matches the site and `"other"` for the kind:

- line ~112 (`walk:`): `..., "walk", "other", now)?`
- line ~149 (`metadata:`): `..., "metadata", "other", now)?`
- line ~211 (`read:`): `..., "read", "other", now)?`
- line ~401 (`archive open:`): `..., "archive_open", "other", now)?`
- line ~430 (archive entry): `..., "archive_entry", "other", now)?`

Also update the existing test at `src/catalog/store.rs:786` which calls `log_scan_error` with the old four-argument signature — pass `"read", "io"` before `now`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS, including the two new tests.

- [ ] **Step 6: Run the full gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/scan_errors.rs src/catalog/mod.rs src/catalog/store.rs src/scanner.rs
git commit -m "feat(catalog): classify scan errors from ErrorKind, one row per path

Classification reads ErrorKind and the raw OS code, never the message:
Windows localizes io::Error text, so string matching would misclassify on
an Italian-locale machine -- which is the machine this is built for.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: The scanner records real kinds

**Files:**
- Modify: `src/scanner.rs` (the five `log_scan_error` sites)
- Test: `src/scanner.rs` `mod tests`

**Interfaces:**
- Consumes: `classify_io` and the six-argument `log_scan_error` from Task 2.
- Produces: every recorded error carries an accurate `phase` and, where an `io::Error` is available, an accurate `kind`.

**Background:** four of the five sites have a real `std::io::Error` in scope and must pass `classify_io(&e)`. The archive-entry site (`scanner.rs:~428`) receives a `reason: &str` produced from a `zip` crate error, not an `io::Error`, so it records `"other"` — do not invent a classification by parsing that string.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/scanner.rs`:

```rust
#[test]
fn a_permission_error_is_recorded_with_its_phase_and_kind() {
    // Rather than manufacture a real permission failure (which differs per OS and per CI
    // runner), drive the catalogue call the scanner makes and assert the shape it stores.
    let (tmp, cat) = setup();
    let _ = tmp;
    let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    cat.log_scan_error(
        Some("vol-1"),
        "locked/dir",
        &format!("walk: {e}"),
        "walk",
        crate::catalog::scan_errors::classify_io(&e),
        100,
    )
    .unwrap();

    let (phase, kind): (String, String) = cat
        .conn
        .query_row(
            "SELECT phase, kind FROM scan_errors WHERE path='locked/dir'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(phase, "walk");
    assert_eq!(kind, "permission");
}

#[test]
fn an_unreadable_file_records_the_read_phase() {
    // A directory that exists where a file is expected makes read() fail on every platform.
    let (tmp, cat) = setup();
    let root = tmp.path().join("drive");
    std::fs::create_dir_all(root.join("notafile.bin")).unwrap();
    let m = crate::scan_metrics::ScanMetrics::new();
    let stop = crate::scan_control::StopFlag::new();
    scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();

    // Directories are walked, not read, so nothing should be recorded as a file error here;
    // this asserts the phase vocabulary is actually used rather than defaulted.
    let phases: Vec<String> = cat
        .conn
        .prepare("SELECT DISTINCT IFNULL(phase,'<null>') FROM scan_errors")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        !phases.iter().any(|p| p == "<null>"),
        "every recorded error must carry a phase, got {phases:?}"
    );
}
```

- [ ] **Step 2: Run and verify the first test fails**

Run: `cargo test --lib a_permission_error_is_recorded`
Expected: FAIL — `kind` is the `"other"` placeholder from Task 2, not `"permission"`.

- [ ] **Step 3: Replace the placeholders with real classification**

At `src/scanner.rs` ~line 112 (walk):

```rust
                cat.log_scan_error(
                    Some(&identity.volume_id),
                    &p,
                    &format!("walk: {err}"),
                    "walk",
                    err.io_error().map(crate::catalog::scan_errors::classify_io).unwrap_or("other"),
                    now,
                )?;
```

`walkdir::Error::io_error()` returns `Option<&std::io::Error>`; a walk error without one (a loop in symlinks) is genuinely not an I/O failure, hence `"other"`.

At ~line 149 (metadata), ~line 211 (read) and ~line 401 (archive open), the local error binding is already an `std::io::Error` — pass `crate::catalog::scan_errors::classify_io(&e)` as the `kind`, keeping the existing `phase` string.

At ~line 430 (archive entry), keep `"other"` and add the comment:

```rust
        // `reason` comes from the zip crate, not an io::Error, so there is no ErrorKind to read.
        // Parsing the string is exactly what classification exists to avoid.
        cat.log_scan_error(Some(&identity.volume_id), &where_, reason, "archive_entry", "other", now)?;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/scanner.rs
git commit -m "feat(scanner): record the phase and classified kind of every scan error

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Self-heal — the task with the reliability rule

**Files:**
- Modify: `src/catalog/scan_errors.rs`, `src/scanner.rs` (end-of-scan block at ~line 325)
- Test: `src/catalog/scan_errors.rs` and `src/scanner.rs`

**Interfaces:**
- Produces: `impl Catalog { pub fn clear_resolved_scan_errors(&self, volume_id: &str, scan_started_at: i64, completed: bool) -> anyhow::Result<usize> }` — returns rows removed.

**Background — read this carefully.** Two rules, because two kinds of path:

1. `metadata`, `read`, `archive_open` record a plain file path, so they join `files.relative_path` and clear when that path was re-seen this scan (`last_seen_at >= scan_started_at`). This is the *same* predicate that makes the missing-file sweep safe: a path the walk never reached never had `last_seen_at` bumped, so **a stopped scan cannot over-clear**.
2. `walk` records a directory and `archive_entry` records a composite `archive.zip › inner/path` — neither exists in `files`. They clear only when `completed` is true and they were not re-recorded this scan. Because the upsert refreshes `occurred_at` to the scan's `now`, "not re-recorded" is `occurred_at < scan_started_at`. `now` and `scan_started_at` are the same value in `scan_volume_with_progress`.

The end-of-scan block already exists at `src/scanner.rs:~325`, guarded by `if !summary.stopped` for the sweep. Self-heal runs for stopped scans too — rule 1 is safe by construction — so place the call outside that guard and pass `completed: !summary.stopped`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/scanner.rs`:

```rust
#[test]
fn a_resolved_file_error_clears_but_an_unreached_one_survives() {
    // THE rule, restated for errors: a stopped scan must clear only what it re-reached.
    // Without the last_seen_at predicate, stopping would wipe findings for the part of the
    // tree the run never visited -- silently reporting a catalogue as complete when it is not.
    let (tmp, cat) = setup();
    let root = tmp.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"one").unwrap();

    // Two errors on record: one for a file that will be scanned, one for a path that will not.
    cat.log_scan_error(Some("vol-1"), "a.txt", "read: was locked", "read", "locked", 50)
        .unwrap();
    cat.log_scan_error(Some("vol-1"), "never/reached.bin", "read: i/o", "read", "io", 50)
        .unwrap();

    let m = crate::scan_metrics::ScanMetrics::new();
    let stop = crate::scan_control::StopFlag::new();
    scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();

    let remaining: Vec<String> = cat
        .conn
        .prepare("SELECT path FROM scan_errors ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        remaining,
        vec!["never/reached.bin".to_string()],
        "a.txt was re-catalogued so its error clears; the unreached path keeps its error"
    );
}

#[test]
fn a_stopped_scan_does_not_clear_walk_errors() {
    // Only a completed scan proves a directory is readable again. A stopped scan may simply
    // never have got there.
    let (tmp, cat) = setup();
    let root = tmp.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"one").unwrap();
    cat.log_scan_error(Some("vol-1"), "locked/dir", "walk: denied", "walk", "permission", 50)
        .unwrap();

    let m = crate::scan_metrics::ScanMetrics::new();
    let stop = crate::scan_control::StopFlag::new();
    stop.request(); // stopped before it starts
    let s = scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();
    assert!(s.stopped);

    let n: i64 = cat
        .conn
        .query_row(
            "SELECT count(*) FROM scan_errors WHERE path='locked/dir'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "a stopped scan never clears a walk error");
}
```

- [ ] **Step 2: Run and verify they fail**

Run: `cargo test --lib a_resolved_file_error_clears`
Expected: FAIL — nothing clears anything yet, so `a.txt`'s error is still present.

- [ ] **Step 3: Implement the clear**

Add to `src/catalog/scan_errors.rs`:

```rust
impl Catalog {
    /// Drop errors this scan resolved. Returns how many rows went.
    ///
    /// Two rules, because two kinds of path:
    ///
    /// * `metadata`/`read`/`archive_open` store a real file path, so an error clears when that
    ///   path was re-seen this scan. Keyed on `last_seen_at` -- the same predicate that makes the
    ///   missing-file sweep safe -- so **a stopped scan cannot over-clear**: a path the walk never
    ///   reached never had its stamp bumped.
    /// * `walk` stores a directory and `archive_entry` stores a composite `archive › inner` path;
    ///   neither exists in `files`. Only a *completed* scan proves the location was visited and is
    ///   readable again, so those clear only when `completed` and not re-recorded this run.
    pub fn clear_resolved_scan_errors(
        &self,
        volume_id: &str,
        scan_started_at: i64,
        completed: bool,
    ) -> anyhow::Result<usize> {
        let mut removed = self.conn.execute(
            "DELETE FROM scan_errors
              WHERE volume_id=?1
                AND IFNULL(phase,'') IN ('metadata','read','archive_open')
                AND path IN (SELECT relative_path FROM files
                              WHERE volume_id=?1 AND last_seen_at >= ?2)",
            params![volume_id, scan_started_at],
        )?;

        if completed {
            // The upsert refreshes `occurred_at` to this scan's stamp, so anything still older
            // was not re-recorded -- the directory opened cleanly this time.
            removed += self.conn.execute(
                "DELETE FROM scan_errors
                  WHERE volume_id=?1 AND IFNULL(phase,'') IN ('walk','archive_entry')
                    AND occurred_at < ?2",
                params![volume_id, scan_started_at],
            )?;
        }
        Ok(removed)
    }
}
```

In `src/scanner.rs`, inside the existing timed block at ~line 325, **after** the `if !summary.stopped { ... sweep ... }`:

```rust
        // Outside the sweep guard on purpose: the file rule is keyed on `last_seen_at`, so a
        // stopped scan clears only paths it actually re-reached. The directory rule is the part
        // that needs a completed scan, and `completed` carries that.
        cat.clear_resolved_scan_errors(&identity.volume_id, scan_started_at, !summary.stopped)?;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Prove the regression test discriminates**

Temporarily delete `AND path IN (SELECT ...)` from the first `DELETE` so it clears everything for the volume, then run `cargo test --lib a_resolved_file_error_clears`.
Expected: FAIL, reporting that `never/reached.bin` was wrongly cleared. **Restore the line** and re-run to confirm green.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/scan_errors.rs src/scanner.rs
git commit -m "feat(scanner): scan errors self-heal when their path is re-catalogued

Keyed on last_seen_at, the same predicate that makes the missing-file
sweep safe under stop/resume: a stopped scan clears only the paths it
actually re-reached. Directory and archive-entry errors need a completed
scan, since only that proves the location was visited.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: The completeness query

**Files:**
- Modify: `src/catalog/scan_errors.rs`
- Test: same file

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, Default, serde::Serialize)]
  pub struct Completeness { pub absent: i64, pub unverified: i64, pub unreadable_dirs: i64 }
  impl Completeness { pub fn is_complete(&self) -> bool }

  #[derive(Debug, Clone, serde::Serialize)]
  pub struct ScanErrorRow {
      pub path: String, pub reason: String,
      pub kind: Option<String>, pub phase: Option<String>,
      pub occurred_at: i64, pub bucket: String,
  }

  impl Catalog {
      pub fn volume_completeness(&self, volume_id: &str) -> anyhow::Result<Completeness>;
      pub fn volume_scan_errors(&self, volume_id: &str, bucket: Option<&str>,
                                kind: Option<&str>, limit: usize, offset: usize)
          -> anyhow::Result<Vec<ScanErrorRow>>;
  }
  ```
- `bucket` is one of `"absent"`, `"unverified"`, `"unreadable_dir"`.

**Background:** legacy rows have `phase IS NULL`. In SQL, `phase <> 'walk'` is `NULL` — neither true nor false — so such rows would vanish from *both* buckets and the audit would under-report. Every predicate therefore uses `IFNULL(phase,'')`. A `walk` row is always `unreadable_dir` regardless of whether a `files` row happens to share its path.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn completeness_splits_absent_unverified_and_unreadable_directories() {
    let t = tempfile::tempdir().unwrap();
    let cat = Catalog::open(&t.path().join("c.db")).unwrap();
    cat.upsert_volume(&crate::catalog::models::Volume {
        volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
        first_seen_at: 1, last_seen_at: 1,
    })
    .unwrap();
    // A file that IS catalogued but could not be re-read -> unverified (its hash may be stale).
    cat.upsert_file(
        &crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: "have.pst".into(),
            filename: "have.pst".into(),
            extension: "pst".into(),
            size_bytes: 10,
            content_hash: "H".into(),
            created_time: Some(1),
            modified_time: Some(1),
            accessed_time: Some(1),
            // NOTE: Category lives in `crate::category`, not in `catalog::models`.
            category: crate::category::Category::Other,
            container_chain: None,
        },
        1,
    )
    .unwrap();

    cat.log_scan_error(Some("v"), "have.pst", "read: locked", "read", "locked", 10).unwrap();
    cat.log_scan_error(Some("v"), "gone.jpg", "read: i/o", "read", "io", 10).unwrap();
    cat.log_scan_error(Some("v"), "sysvol", "walk: denied", "walk", "permission", 10).unwrap();
    // A legacy row from before classification existed: phase IS NULL. It must still be counted.
    cat.conn
        .execute(
            "INSERT INTO scan_errors(volume_id,path,reason,occurred_at) VALUES ('v','old.bin','read: ?',5)",
            [],
        )
        .unwrap();

    let c = cat.volume_completeness("v").unwrap();
    assert_eq!(c.unverified, 1, "have.pst is catalogued but unverified");
    assert_eq!(c.absent, 2, "gone.jpg and the legacy old.bin are absent");
    assert_eq!(c.unreadable_dirs, 1, "a walk error is never counted as a missing file");
    assert!(!c.is_complete());

    let rows = cat.volume_scan_errors("v", Some("absent"), None, 50, 0).unwrap();
    let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["gone.jpg", "old.bin"], "ordered by path");
}

#[test]
fn a_volume_with_no_errors_is_complete() {
    let t = tempfile::tempdir().unwrap();
    let cat = Catalog::open(&t.path().join("c.db")).unwrap();
    let c = cat.volume_completeness("v").unwrap();
    assert_eq!((c.absent, c.unverified, c.unreadable_dirs), (0, 0, 0));
    assert!(c.is_complete(), "no errors means complete, not unknown");
}
```

- [ ] **Step 2: Run and verify it fails**

Run: `cargo test --lib completeness_splits`
Expected: FAIL to compile — `volume_completeness` does not exist.

- [ ] **Step 3: Implement**

```rust
/// Per-volume answer to "is this catalogue complete, and what is missing?"
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Completeness {
    /// Files with no catalogue row: invisible to search and dedup. The real hole.
    pub absent: i64,
    /// Files with a row from an earlier scan that this scan could not re-read, so the stored hash
    /// may be stale -- which can pair the wrong files during duplicate review.
    pub unverified: i64,
    /// Directories the walk could not open. Never counted as one missing file: the number of files
    /// beneath an unopenable directory is unknown, and printing 1 would make a denied folder of
    /// 40,000 photos look like a denied `System Volume Information`.
    pub unreadable_dirs: i64,
}

impl Completeness {
    pub fn is_complete(&self) -> bool {
        self.absent == 0 && self.unverified == 0 && self.unreadable_dirs == 0
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanErrorRow {
    pub path: String,
    pub reason: String,
    pub kind: Option<String>,
    pub phase: Option<String>,
    pub occurred_at: i64,
    pub bucket: String,
}

/// `IFNULL(phase,'')` throughout: rows written before classification have a NULL phase, and
/// `phase <> 'walk'` is NULL for those -- which would drop them from *both* buckets and
/// under-report the very thing this feature measures.
const BUCKET_SQL: &str = "CASE WHEN IFNULL(e.phase,'')='walk' THEN 'unreadable_dir' \
                               WHEN f.id IS NULL THEN 'absent' ELSE 'unverified' END";

impl Catalog {
    pub fn volume_completeness(&self, volume_id: &str) -> anyhow::Result<Completeness> {
        let sql = format!(
            "SELECT {BUCKET_SQL} AS bucket, count(*) FROM scan_errors e
               LEFT JOIN files f
                 ON f.volume_id=e.volume_id AND f.relative_path=e.path AND f.status='active'
              WHERE e.volume_id=?1 GROUP BY bucket"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut out = Completeness::default();
        let rows = stmt.query_map(params![volume_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (bucket, n) = row?;
            match bucket.as_str() {
                "absent" => out.absent = n,
                "unverified" => out.unverified = n,
                "unreadable_dir" => out.unreadable_dirs = n,
                _ => {}
            }
        }
        Ok(out)
    }

    pub fn volume_scan_errors(
        &self,
        volume_id: &str,
        bucket: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<ScanErrorRow>> {
        let sql = format!(
            "SELECT e.path, e.reason, e.kind, e.phase, e.occurred_at, {BUCKET_SQL} AS bucket
               FROM scan_errors e
               LEFT JOIN files f
                 ON f.volume_id=e.volume_id AND f.relative_path=e.path AND f.status='active'
              WHERE e.volume_id=?1
                AND (?2 IS NULL OR bucket = ?2)
                AND (?3 IS NULL OR IFNULL(e.kind,'') = ?3)
              ORDER BY e.path LIMIT ?4 OFFSET ?5"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![volume_id, bucket, kind, limit as i64, offset as i64],
            |r| {
                Ok(ScanErrorRow {
                    path: r.get(0)?,
                    reason: r.get(1)?,
                    kind: r.get(2)?,
                    phase: r.get(3)?,
                    occurred_at: r.get(4)?,
                    bucket: r.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib scan_errors::`
Expected: PASS

- [ ] **Step 5: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/scan_errors.rs
git commit -m "feat(catalog): three-bucket completeness query for a volume

An unreadable directory gets its own bucket rather than counting as one
missing file -- the number of files beneath it is unknown, and printing 1
would understate an arbitrarily large hole.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Web endpoint and drive counts

**Files:**
- Modify: `src/web.rs` (route table ~line 56, `DriveDto` at line 216, `api_drives` at ~line 440)
- Test: `src/web.rs` `mod tests`

**Interfaces:**
- Consumes: `volume_completeness`, `volume_scan_errors`, `Completeness`, `ScanErrorRow` from Task 5.
- Produces: `GET /api/volumes/:id/errors?bucket&kind&limit&offset`; `DriveDto` fields `absent`, `unverified`, `unreadable_dirs`.

**Background:** this is a **read-only** endpoint, so it takes no CSRF token — that matches every other `get(...)` route (`/api/copies`, `/api/search`). Do not add one; a CSRF check on a GET would be inconsistent and pointless. Follow the `api_copies` shape (`web.rs:537`): open with `Catalog::open_readonly`, map errors with `err500`.

`DriveDto.has_errors` currently comes from `volume_has_scan_errors`. Derive it from the counts instead so one query serves both, and so the pill stops latching once self-heal removes the rows.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn volume_errors_endpoint_reports_buckets_and_filters() {
    let (_t, db, _state) = seed_dupes();
    {
        let cat = Catalog::open(&db).unwrap();
        cat.log_scan_error(Some("vol-1"), "gone.jpg", "read: i/o", "read", "io", 10).unwrap();
        cat.log_scan_error(Some("vol-1"), "sysvol", "walk: denied", "walk", "permission", 10).unwrap();
    }
    let v = get_json(&db, "/api/volumes/vol-1/errors").await;
    assert_eq!(v["totals"]["absent"], 1);
    assert_eq!(v["totals"]["unreadable_dirs"], 1);
    assert_eq!(v["rows"].as_array().unwrap().len(), 2);

    let only = get_json(&db, "/api/volumes/vol-1/errors?bucket=unreadable_dir").await;
    let rows = only["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "sysvol");
    assert_eq!(rows[0]["kind"], "permission");
}

#[tokio::test]
async fn a_clean_volume_reports_complete() {
    let (_t, db, _state) = seed_dupes();
    let v = get_json(&db, "/api/volumes/vol-1/errors").await;
    assert_eq!(v["totals"]["absent"], 0);
    assert_eq!(v["totals"]["unverified"], 0);
    assert_eq!(v["totals"]["unreadable_dirs"], 0);
    assert!(v["rows"].as_array().unwrap().is_empty());
}
```

- [ ] **Step 2: Run and verify it fails**

Run: `cargo test --lib volume_errors_endpoint`
Expected: FAIL — 404, the route does not exist.

- [ ] **Step 3: Implement**

Add near the other DTOs in `src/web.rs`:

```rust
#[derive(serde::Deserialize)]
struct VolumeErrorParams {
    bucket: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(serde::Serialize)]
struct VolumeErrorsDto {
    totals: crate::catalog::scan_errors::Completeness,
    rows: Vec<crate::catalog::scan_errors::ScanErrorRow>,
}

// NOTE: `axum::extract::Path` is imported in this file as `AxPath` (web.rs:6), because
// `std::path::PathBuf` is also in scope. Use `AxPath` -- writing `Path` here will not compile.
async fn api_volume_errors(
    State(state): State<AppState>,
    AxPath(volume_id): AxPath<String>,
    Query(p): Query<VolumeErrorParams>,
) -> Result<Json<VolumeErrorsDto>, (StatusCode, String)> {
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    // Bounded by default: a badly failing drive can hold a lot of rows, and the totals are the
    // headline anyway.
    let limit = p.limit.unwrap_or(200).min(1000);
    Ok(Json(VolumeErrorsDto {
        totals: cat.volume_completeness(&volume_id).map_err(err500)?,
        rows: cat
            .volume_scan_errors(
                &volume_id,
                p.bucket.as_deref(),
                p.kind.as_deref(),
                limit,
                p.offset.unwrap_or(0),
            )
            .map_err(err500)?,
    }))
}
```

Register beside the other GET routes (after line 56):

```rust
        .route("/api/volumes/:id/errors", get(api_volume_errors))
```

Add to `DriveDto` (line 216) and populate in `api_drives`, replacing the `has_errors` line:

```rust
    absent: i64,
    unverified: i64,
    unreadable_dirs: i64,
```

```rust
        let completeness = cat.volume_completeness(&volume_id).map_err(err500)?;
        // Derived, so the pill stops latching: self-heal removes the rows, the count drops to
        // zero, and the drive stops claiming an error it no longer has.
        let has_errors = !completeness.is_complete();
```

then set `absent: completeness.absent, unverified: completeness.unverified, unreadable_dirs: completeness.unreadable_dirs, has_errors,` in the struct literal.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web.rs
git commit -m "feat(web): completeness endpoint and per-drive counts

has_errors is now derived from the counts, so the pill stops latching on
forever once the errors behind it have healed.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: The Drives-page panel

**Files:**
- Modify: `src/web_ui.rs` (drive card at ~lines 917-940)
- Test: `src/web.rs` `mod tests` (the existing self-contained-page assertions)

**Interfaces:**
- Consumes: `/api/volumes/:id/errors` and the `DriveDto` counts from Task 6.

**Background:** the page is plain HTML/CSS/JS inside a Rust string — no build step, no CDN, no fonts fetched at runtime; a test asserts the page contains no `http://` or `https://`. Match the surrounding style (`esc()` for interpolation, `$()` helper, `material-symbols-outlined` icons already vendored). Line 922 currently renders the latching pill text; replace it with real counts and add an expandable panel that fetches the endpoint on demand — do not fetch every drive's errors on page load.

- [ ] **Step 1: Write the failing test**

**There is no Drives-page test today** — only the Scan page has one. Create this in `src/web.rs` `mod tests`, modelled on the existing `scan_page_is_self_contained_and_wired`:

```rust
#[tokio::test]
async fn drives_page_is_self_contained_and_wired() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let (_t, _db, state) = seed_dupes();
    let app = build_router_with(state);
    let res = app
        .oneshot(Request::builder().uri("/drives").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    // The panel is the only way to see WHICH files are missing; a page without it leaves the
    // completeness answer unreachable from the browser.
    assert!(body.contains("/api/volumes/"));
    assert!(body.contains("completeness"));
    // Self-contained: no CDN, no runtime font fetch. Asserted for every page in this project.
    assert!(!body.contains("http://") && !body.contains("https://"));
}
```

- [ ] **Step 2: Run and verify it fails**

Run: `cargo test --lib drives_page_is_self_contained`
Expected: FAIL — the markup is not there yet.

- [ ] **Step 3: Implement**

Replace the status-line branch at `src/web_ui.rs:922`:

```js
  if(d.has_errors){
    const bits=[];
    if(d.absent) bits.push(`${d.absent} not catalogued`);
    if(d.unverified) bits.push(`${d.unverified} unverified`);
    if(d.unreadable_dirs) bits.push(`${d.unreadable_dirs} unreadable folder${d.unreadable_dirs>1?'s':''}`);
    return `<span class="sdot" style="background:var(--red)"></span><span style="color:var(--red)">${bits.join(' · ')}</span>`;
  }
```

Add a details panel inside the drive card markup (after the buttons row, ~line 940):

```js
      <details class="completeness" data-vid="${esc(d.volume_id)}">
        <summary>Completeness</summary>
        <div class="cbody mut">loading…</div>
      </details>
```

And the on-demand loader, beside the other listeners:

```js
// Fetched only when opened: a drive with thousands of failures should not cost anything on
// page load, and the counts on the card already answer the common question.
document.addEventListener('toggle', async e=>{
  const el=e.target;
  if(!el.matches('details.completeness') || !el.open || el.dataset.loaded) return;
  el.dataset.loaded='1';
  const body=el.querySelector('.cbody');
  try{
    const r=await fetch(`/api/volumes/${encodeURIComponent(el.dataset.vid)}/errors`);
    const d=await r.json();
    if(!d.rows.length){ body.textContent='Complete — every file was catalogued.'; return; }
    body.innerHTML=d.rows.map(x=>`<div class="erow">
        <span class="tag">${esc(x.bucket==='unreadable_dir'?'folder':x.bucket)}</span>
        <span class="epath">${esc(x.path)}</span>
        <span class="mut">${esc(x.kind||'recorded before classification')}</span>
      </div>`).join('')
      + (d.rows.length>=200?'<div class="mut">showing the first 200</div>':'');
  }catch(err){ body.textContent='Could not load the error list.'; }
}, true);
```

Add matching CSS near the other card rules (`.erow{display:flex;gap:8px;align-items:baseline;padding:2px 0}` `.epath{font-family:var(--mono);word-break:break-all}`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Check it by eye**

Run: `cargo run -- browse`, open the Drives page, expand **Completeness** on a drive. Confirm counts render, the panel loads on expand (not before), and a clean drive says "Complete".

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web_ui.rs src/web.rs
git commit -m "feat(review): completeness panel on the Drives page

Loads on expand rather than on page load: a drive with thousands of
failures should not cost anything until asked.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: CLI completeness lines

**Files:**
- Modify: `src/commands.rs` (`cmd_status` volume loop ~line 147, `cmd_scan` after the "Done:" line)
- Test: `src/catalog/scan_errors.rs` (formatting helper)

**Interfaces:**
- Consumes: `Completeness` and `volume_completeness` from Task 5.
- Produces: `impl Completeness { pub fn summary_line(&self) -> String }`

**Background:** a long scan is run from the terminal, so the answer must appear there without opening a browser. When everything is clean the line must still print — `Completeness: complete.` — because an absent warning is only trustworthy if the check is known to have run.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/catalog/scan_errors.rs`:

```rust
#[test]
fn the_summary_line_states_completeness_positively_when_clean() {
    // Silence is not reassurance: the clean case must say so, or a user cannot tell "checked and
    // fine" from "not checked".
    let c = Completeness::default();
    assert_eq!(c.summary_line(), "Completeness: complete.");
}

#[test]
fn the_summary_line_names_each_bucket_it_has() {
    let c = Completeness { absent: 12, unverified: 35, unreadable_dirs: 2 };
    assert_eq!(
        c.summary_line(),
        "Completeness: 12 files NOT catalogued, 35 unverified, 2 unreadable directories (contents unknown)."
    );
    let only_absent = Completeness { absent: 1, unverified: 0, unreadable_dirs: 0 };
    assert_eq!(only_absent.summary_line(), "Completeness: 1 file NOT catalogued.");
}
```

- [ ] **Step 2: Run and verify it fails**

Run: `cargo test --lib the_summary_line`
Expected: FAIL — `summary_line` does not exist.

- [ ] **Step 3: Implement**

Add to `impl Completeness` in `src/catalog/scan_errors.rs`:

```rust
    /// One line for the CLI. Prints even when clean: an absent warning only means something if
    /// the user knows the check ran.
    pub fn summary_line(&self) -> String {
        if self.is_complete() {
            return "Completeness: complete.".to_string();
        }
        let mut parts = Vec::new();
        if self.absent > 0 {
            parts.push(format!(
                "{} file{} NOT catalogued",
                self.absent,
                if self.absent == 1 { "" } else { "s" }
            ));
        }
        if self.unverified > 0 {
            parts.push(format!("{} unverified", self.unverified));
        }
        if self.unreadable_dirs > 0 {
            parts.push(format!(
                "{} unreadable director{} (contents unknown)",
                self.unreadable_dirs,
                if self.unreadable_dirs == 1 { "y" } else { "ies" }
            ));
        }
        format!("Completeness: {}.", parts.join(", "))
    }
```

In `src/commands.rs`, in `cmd_scan` after the existing `Done: ...` line:

```rust
            println!("{}", cat.volume_completeness(&identity.volume_id)?.summary_line());
```

In `cmd_status`, inside the per-volume loop (after the existing `println!` for the volume):

```rust
        let c = cat.volume_completeness(&id)?;
        if !c.is_complete() {
            println!("     ⚠ {}", c.summary_line().trim_start_matches("Completeness: "));
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Check by eye**

Run `cargo run -- status` and confirm the warning appears only for volumes with errors, then `cargo run -- scan <a small folder>` and confirm the completeness line prints.

- [ ] **Step 6: Update the docs**

In `README.md`, under the scan section, add:

```markdown
After a scan the CLI reports whether the catalogue is complete:

```
Completeness: 12 files NOT catalogued, 35 unverified, 2 unreadable directories (contents unknown).
```

**Not catalogued** means the file is absent from the catalogue entirely — invisible to search and
deduplication. **Unverified** means it is catalogued but this scan could not re-read it, so its hash
may be stale. **Unreadable directories** are counted separately because the number of files inside
one is unknown. The Drives page lists the paths and reasons. Fixing the cause and re-scanning clears
them automatically.
```

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/scan_errors.rs src/commands.rs README.md
git commit -m "feat(cli): report catalogue completeness after a scan and in status

Prints even when clean: an absent warning only reassures if the user
knows the check ran.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final review

After Task 8, review the whole branch against the spec's six success criteria:

1. CLI and Drives page report absent / unverified / unreadable per volume, and say so positively when clean.
2. Fixing a cause and re-scanning clears the errors with no manual step.
3. A **stopped** scan clears errors only for paths it actually re-reached — the regression test in Task 4 proves it.
4. Grouping is correct on a non-English Windows locale (Task 2's classification test).
5. A path failing on many consecutive scans holds exactly one row (Task 1 + Task 2).
6. Existing scanner and catalogue tests pass unmodified.

Pay particular attention to the migration in Task 1 running against a **real** catalogue: back up `%APPDATA%\justPrototype\CleanUpStorages\data\catalog.db` and open it with the built binary before merging, since a failing migration would take the catalogue offline on open.

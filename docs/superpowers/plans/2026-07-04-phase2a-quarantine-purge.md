# Phase 2a — Quarantine / Purge Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The safe, CLI-driven core of the deduplication workflow: move a confirmed-duplicate loose file to a same-drive `_ToDelete` quarantine folder (a rename — instant, zero extra space, fully reversible until purged), record every action in an audit log, report reclaimable space, and let the user `purge` a drive's quarantine to reclaim space when they are ready. This is the load-bearing, reliability-critical foundation the Phase 2b review GUI will call; it is useful on its own via `duplicates` / `quarantine` / `purge` CLI commands.

**Architecture:** A new `src/quarantine.rs` module performs the on-disk move and the catalog transition; a new `src/purge.rs` performs the one user-initiated hard delete. Both go through new `catalog::store` methods that enforce the safety invariants (never remove the last copy; only loose active files; append-only `actions_log`). Quarantine repoints a row's `relative_path` to its `_ToDelete/…` location and stores its origin in a new `original_path` column, so a later scan can never resurrect a quarantined row. The CLI gains `duplicates` (list groups), `quarantine <mount> <id…>`, and `purge <mount>`, and `status` gains a per-volume recoverable-space line.

**Tech Stack:** Rust 1.88, existing deps only (`rusqlite`, `serde_json` for `actions_log` details). No new crates.

## Global Constraints

- **Reliability is paramount — nothing may ever be lost. This phase performs the project's first destructive operations, so every rule below is a hard gate:**
  - **Quarantine is a same-drive rename**, never a copy. If `std::fs::rename` fails with a cross-device error, the move is **aborted and logged** — never fall back to copy+delete (that risks a partial copy filling a near-full drive). The original file stays exactly where it is.
  - **Never remove the last copy.** A file may be quarantined only if at least one *other* row with the same `content_hash` and `status='active'` (not itself, not another member being quarantined in the same batch) survives. Otherwise the file is skipped and the refusal is logged.
  - **Only loose, active files can be quarantined** (`container_chain IS NULL AND status='active'`). Archive-internal removal is Phase 2c (Case 4), not here.
  - **`purge` is the only hard delete, and only on explicit user command** for one named drive. It deletes that drive's `_ToDelete` tree and marks the rows `purged`. Nothing else ever calls the irreversible delete.
  - **Rows are never deleted from the catalog**; `status` transitions `active → quarantined → purged`, and `actions_log` records every transition (append-only).
- **The catalog is never on a drive.** Quarantine/purge require the target drive mounted and its `.cleanupstorages_id` marker present and matching the files' `volume_id`; a missing/mismatched marker aborts the operation (we refuse to touch a drive we can't positively identify).
- **`_ToDelete` layout:** a file at `<mount>/<origin>` moves to `<mount>/_ToDelete/<origin>`; parent dirs are created; a name collision at the destination gets a ` (n)` suffix before the extension. The scanner already skips any path containing a `_ToDelete` component, so quarantined files are never re-catalogued.
- **`actions_log.details` is JSON** (built with `serde_json`) so the audit trail is machine-readable: e.g. `{"file_id":42,"volume_id":"…","from":"Photos/a.jpg","to":"_ToDelete/Photos/a.jpg","hash":"…","survivor_id":7}`.
- **Timestamps** are `i64` unix seconds (existing `now_secs()` convention).
- **Git:** work on branch `feat/phase2a-quarantine` off `main`. Conventional Commits, scope `catalog`/`quarantine`/`purge`/`cli`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (already merged):** `Catalog` (+ `open`, store methods, `conn`), `models::{FileRecord, FileStatus}`, `volume` (marker), `config::Config`, scanner's `_ToDelete` skip. **Out of scope (later sub-plans):** the review GUI + web decision endpoints (2b), photo previews/metadata-diff (2b), Case 3 advisory reporting (2b), Case 4 archive repack + scratch/pre-flight space checks (2c), near-duplicate detection.

---

## File Structure

- `src/catalog/schema.rs` — idempotent migration adding the `original_path` column to `files`.
- `src/catalog/models.rs` — `FileRecord` gains `original_path: Option<String>`.
- `src/catalog/store.rs` — quarantine/purge/listing/audit methods; `map_file_record` reads the new column.
- `src/volume.rs` — expose `read_volume_id(root)` (read the marker without creating one).
- `src/quarantine.rs` — **new**: the move-to-`_ToDelete` engine. Registered in `lib.rs`.
- `src/purge.rs` — **new**: the hard-delete-and-mark engine. Registered in `lib.rs`.
- `src/commands.rs` — `cmd_duplicates`, `cmd_quarantine`, `cmd_purge`; extend `cmd_status`.
- `src/main.rs` — `Duplicates`, `Quarantine`, `Purge` subcommands.
- `src/lib.rs` — `pub mod quarantine; pub mod purge;`.
- `tests/quarantine_flow.rs` — **new** end-to-end: scan → duplicates → quarantine → purge.

---

### Task 1: Schema migration — `original_path` column

**Files:**
- Modify: `src/catalog/schema.rs`
- Modify: `src/catalog/models.rs`
- Modify: `src/catalog/store.rs` (`map_file_record`)
- Test: inline `#[cfg(test)]` in `src/catalog/schema.rs`

**Interfaces:**
- Produces: `files.original_path TEXT` (NULL normally; the pre-quarantine `relative_path` once quarantined). `FileRecord.original_path: Option<String>`. An idempotent `ensure_column` migration so existing catalogs upgrade with no data loss.

- [ ] **Step 1: Write the failing test**

Add to `src/catalog/schema.rs` `mod tests`:

```rust
    #[test]
    fn migration_adds_original_path_to_preexisting_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        // Simulate an OLD catalog created WITHOUT original_path.
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE files (id INTEGER PRIMARY KEY, volume_id TEXT NOT NULL,
                    relative_path TEXT NOT NULL, filename TEXT NOT NULL, extension TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL, content_hash TEXT NOT NULL, created_time INTEGER,
                    modified_time INTEGER, accessed_time INTEGER, category TEXT NOT NULL,
                    container_chain TEXT, status TEXT NOT NULL, first_seen_at INTEGER NOT NULL,
                    last_seen_at INTEGER NOT NULL);",
            ).unwrap();
        }
        // Opening through Catalog must migrate it in, not fail.
        let cat = crate::catalog::Catalog::open(&db).unwrap();
        let has_col: i64 = cat.conn.query_row(
            "SELECT count(*) FROM pragma_table_info('files') WHERE name='original_path'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(has_col, 1);
        assert!(cat.integrity_ok().unwrap());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib schema`
Expected: FAIL — `original_path` column not added to the pre-existing table.

- [ ] **Step 3: Implement the migration**

In `src/catalog/schema.rs`, add `original_path TEXT` to the `CREATE TABLE files` (after `last_seen_at INTEGER NOT NULL`, i.e. `last_seen_at INTEGER NOT NULL,\n            original_path  TEXT` — remember to add the comma after `last_seen_at`). Then, at the END of `apply` (after the `execute_batch`), add the idempotent migration for already-existing catalogs:

```rust
pub fn apply(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch( /* ... existing DDL, with original_path added to CREATE TABLE files ... */ )?;
    ensure_column(conn, "files", "original_path", "TEXT")?;
    Ok(())
}

/// Add `<table>.<column> <decl>` if it does not already exist (idempotent, data-preserving).
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |r| r.get(0),
    )?;
    if exists == 0 {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}
```

(`ALTER TABLE ADD COLUMN` with no default adds a NULL column to every existing row — exactly what we want; existing data is untouched.)

- [ ] **Step 4: Add the field to `FileRecord` and read it**

In `src/catalog/models.rs`, add to `FileRecord` (after `last_seen_at`):

```rust
    pub original_path: Option<String>,
```

In `src/catalog/store.rs` `map_file_record`, the SELECT column list in `search_filtered` currently ends at `last_seen_at`. Add `original_path` to BOTH the SELECT lists (in `search_filtered`) and map it. Concretely: append `, original_path` to the column list in the `search_filtered` SQL string, and in `map_file_record` add `original_path: r.get(15)?,` (index 15, following `last_seen_at` at 14).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib schema` then `cargo test`
Expected: PASS — migration test passes; all existing tests still pass (a fresh catalog now has `original_path` from the CREATE TABLE, and `map_file_record` reads it as NULL).

- [ ] **Step 6: Commit**

```bash
git checkout -b feat/phase2a-quarantine   # only if not already on it
git add src/catalog/schema.rs src/catalog/models.rs src/catalog/store.rs
git commit -m "feat(catalog): add original_path column with idempotent migration

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Store methods — duplicates, survivor check, quarantine/purge transitions, audit, recoverable space

**Files:**
- Modify: `src/catalog/store.rs`
- Test: inline `#[cfg(test)]`

**Interfaces (methods on `Catalog`):**
- `pub fn get_file(&self, id: i64) -> anyhow::Result<Option<FileRecord>>`
- `pub fn duplicate_groups(&self) -> anyhow::Result<Vec<Vec<FileRecord>>>` — each inner vec is ≥2 active files (loose or archived) sharing a `content_hash`, ordered by hash then id.
- `pub fn active_survivor_exists(&self, hash: &str, excluding: &[i64]) -> anyhow::Result<bool>` — true if some `status='active'` row has this hash whose id is not in `excluding`.
- `pub fn mark_quarantined(&self, id: i64, new_relative_path: &str, original_path: &str, now: i64) -> anyhow::Result<()>` — sets `status='quarantined'`, `relative_path=new_relative_path`, `original_path=original_path`, `last_seen_at=now`.
- `pub fn quarantined_rows(&self, volume_id: &str) -> anyhow::Result<Vec<FileRecord>>`
- `pub fn mark_purged(&self, id: i64, now: i64) -> anyhow::Result<()>` — sets `status='purged'`, `last_seen_at=now`.
- `pub fn recoverable_bytes(&self, volume_id: &str) -> anyhow::Result<i64>` — sum `size_bytes` where `status='quarantined'`.
- `pub fn log_action(&self, action: &str, details_json: &str, now: i64) -> anyhow::Result<()>` — insert into `actions_log`.

- [ ] **Step 1: Write failing tests**

Add to `store.rs` `mod tests` (reuse existing `open_tmp`, `mk_file`):

```rust
    #[test]
    fn duplicate_groups_lists_members() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200).unwrap();
        let groups = cat.duplicate_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn survivor_check_respects_exclusions() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200).unwrap();
        let a = cat.active_file_id("vol-1", "a.txt").unwrap().unwrap();
        let b = cat.active_file_id("vol-1", "b.txt").unwrap().unwrap();
        // excluding one of two leaves a survivor
        assert!(cat.active_survivor_exists("same", &[a]).unwrap());
        // excluding both leaves none
        assert!(!cat.active_survivor_exists("same", &[a, b]).unwrap());
        // a unique hash has no survivor once excluded
        assert!(!cat.active_survivor_exists("nope", &[]).unwrap());
    }

    #[test]
    fn quarantine_then_purge_transitions_and_recoverable() {
        let (_t, cat) = open_tmp();
        let mut f = mk_file("vol-1", "Photos/a.jpg", "h"); f.size_bytes = 2048;
        cat.upsert_file(&f, 200).unwrap();
        let id = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();

        cat.mark_quarantined(id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 300).unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.relative_path, "_ToDelete/Photos/a.jpg");
        assert_eq!(rec.original_path.as_deref(), Some("Photos/a.jpg"));
        assert_eq!(cat.recoverable_bytes("vol-1").unwrap(), 2048);
        assert_eq!(cat.quarantined_rows("vol-1").unwrap().len(), 1);

        cat.mark_purged(id, 400).unwrap();
        assert_eq!(cat.get_file(id).unwrap().unwrap().status, FileStatus::Purged);
        assert_eq!(cat.recoverable_bytes("vol-1").unwrap(), 0);
    }

    #[test]
    fn log_action_appends() {
        let (_t, cat) = open_tmp();
        cat.log_action("quarantine", "{\"file_id\":1}", 100).unwrap();
        cat.log_action("purge", "{\"volume_id\":\"v\"}", 200).unwrap();
        let n: i64 = cat.conn.query_row("SELECT count(*) FROM actions_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store`
Expected: FAIL — new methods not found.

- [ ] **Step 3: Implement**

Add inside `impl Catalog` in `store.rs`:

```rust
    pub fn get_file(&self, id: i64) -> anyhow::Result<Option<FileRecord>> {
        let row = self.conn.query_row(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at, original_path FROM files WHERE id=?1",
            params![id], Self::map_file_record,
        );
        match row {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn duplicate_groups(&self) -> anyhow::Result<Vec<Vec<FileRecord>>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at, original_path FROM files
             WHERE status='active' AND content_hash IN (
                 SELECT content_hash FROM files WHERE status='active'
                 GROUP BY content_hash HAVING count(*) > 1)
             ORDER BY content_hash, id",
        )?;
        let rows = stmt.query_map([], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut groups: Vec<Vec<FileRecord>> = Vec::new();
        for r in rows {
            match groups.last_mut() {
                Some(g) if g[0].content_hash == r.content_hash => g.push(r),
                _ => groups.push(vec![r]),
            }
        }
        Ok(groups)
    }

    pub fn active_survivor_exists(&self, hash: &str, excluding: &[i64]) -> anyhow::Result<bool> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM files WHERE content_hash=?1 AND status='active'")?;
        let ids = stmt.query_map(params![hash], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids.iter().any(|id| !excluding.contains(id)))
    }

    pub fn mark_quarantined(&self, id: i64, new_relative_path: &str, original_path: &str, now: i64)
        -> anyhow::Result<()>
    {
        self.conn.execute(
            "UPDATE files SET status='quarantined', relative_path=?2, original_path=?3, last_seen_at=?4
             WHERE id=?1",
            params![id, new_relative_path, original_path, now],
        )?;
        Ok(())
    }

    pub fn quarantined_rows(&self, volume_id: &str) -> anyhow::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at, original_path FROM files
             WHERE volume_id=?1 AND status='quarantined' ORDER BY id",
        )?;
        Ok(stmt.query_map(params![volume_id], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn mark_purged(&self, id: i64, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE files SET status='purged', last_seen_at=?2 WHERE id=?1",
            params![id, now])?;
        Ok(())
    }

    pub fn recoverable_bytes(&self, volume_id: &str) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT IFNULL(sum(size_bytes),0) FROM files WHERE volume_id=?1 AND status='quarantined'",
            params![volume_id], |r| r.get(0))?;
        Ok(n)
    }

    pub fn log_action(&self, action: &str, details_json: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO actions_log(action, details, occurred_at) VALUES (?1,?2,?3)",
            params![action, details_json, now])?;
        Ok(())
    }
```

Update `map_file_record` (already updated in Task 1 to read index 15) — no further change.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib store` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs
git commit -m "feat(catalog): duplicate listing, survivor check, quarantine/purge transitions, audit

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Quarantine engine

**Files:**
- Modify: `src/volume.rs` (add `read_volume_id`)
- Create: `src/quarantine.rs`
- Modify: `src/lib.rs` (`pub mod quarantine;`)
- Test: inline `#[cfg(test)]` in `src/quarantine.rs`

**Interfaces:**
- `volume::read_volume_id(root: &Path) -> Option<String>` — reads the `.cleanupstorages_id` marker without ever writing one (wraps the existing private `read_marker`).
- `quarantine::QuarantineOutcome { pub quarantined: usize, pub skipped: usize }`
- `quarantine::quarantine_files(cat: &Catalog, mount_root: &Path, expected_volume_id: &str, ids: &[i64], now: i64) -> anyhow::Result<QuarantineOutcome>` — verifies the mount's marker equals `expected_volume_id` (else `anyhow::bail!`), then for each id: load the row; skip (log) unless it is loose (`container_chain IS NULL`), `active`, on `expected_volume_id`, and its file exists at `<mount>/<relative_path>`; enforce `active_survivor_exists(hash, excluding=&[id])`; move to `_ToDelete`; on success `mark_quarantined` + `log_action("quarantine", …)`. Any per-file failure is logged to `scan_errors` (or `actions_log` with an error action) and the batch continues.

- [ ] **Step 1: Add `read_volume_id` to `src/volume.rs`**

```rust
/// Read the drive's existing identity marker without creating one. None if absent/unreadable.
pub fn read_volume_id(root: &Path) -> Option<String> {
    read_marker(root)
}
```

(`read_marker` already exists as a private fn in this module.)

- [ ] **Step 2: Write failing tests**

Create `src/quarantine.rs`:

```rust
//! Move confirmed-duplicate loose files to a same-drive `_ToDelete` quarantine (reversible).

use std::path::Path;
use crate::catalog::Catalog;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct QuarantineOutcome {
    pub quarantined: usize,
    pub skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::Volume;
    use std::fs;

    // A fake mounted drive with a marker and two identical files.
    fn fake_drive() -> (tempfile::TempDir, Catalog, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("Photos/a.jpg"), b"IDENTICAL").unwrap();
        fs::write(root.join("copy_a.jpg"), b"IDENTICAL").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = crate::volume::VolumeIdentity {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into() };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100).unwrap();
        (tmp, cat, root.to_string_lossy().into_owned())
    }

    #[test]
    fn quarantines_a_duplicate_and_moves_the_file() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        // pick the id of Photos/a.jpg
        let id = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(out, QuarantineOutcome { quarantined: 1, skipped: 0 });
        // file moved
        assert!(!root.join("Photos/a.jpg").exists());
        assert!(root.join("_ToDelete/Photos/a.jpg").exists());
        // row updated
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, crate::catalog::models::FileStatus::Quarantined);
        assert_eq!(rec.original_path.as_deref(), Some("Photos/a.jpg"));
        // the surviving copy is untouched
        assert!(root.join("copy_a.jpg").exists());
        let _ = tmp;
    }

    #[test]
    fn refuses_to_quarantine_the_last_copy() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let a = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let b = cat.active_file_id("vol-1", "copy_a.jpg").unwrap().unwrap();
        // trying to quarantine BOTH members leaves no survivor -> second is skipped
        let out = quarantine_files(&cat, &root, "vol-1", &[a, b], 200).unwrap();
        assert_eq!(out.quarantined, 1);
        assert_eq!(out.skipped, 1);
        // exactly one of the two files remains on disk
        let remaining = [root.join("Photos/a.jpg").exists(), root.join("copy_a.jpg").exists()]
            .iter().filter(|x| **x).count();
        assert_eq!(remaining, 1);
        let _ = tmp;
    }

    #[test]
    fn wrong_marker_aborts() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let err = quarantine_files(&cat, &root, "vol-DIFFERENT", &[id], 200);
        assert!(err.is_err());
        assert!(root.join("Photos/a.jpg").exists()); // nothing moved
        let _ = tmp;
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib quarantine`
Expected: FAIL — `quarantine_files` not found.

- [ ] **Step 4: Implement the engine**

Add to `src/quarantine.rs` (above the tests). Register `pub mod quarantine;` in `src/lib.rs` first.

```rust
use crate::catalog::models::FileStatus;

const QUARANTINE_DIR: &str = "_ToDelete";

/// Move each given file to the drive's `_ToDelete` quarantine, transactionally recording each.
/// Verifies the mount's marker equals `expected_volume_id` before touching anything.
pub fn quarantine_files(
    cat: &Catalog, mount_root: &Path, expected_volume_id: &str, ids: &[i64], now: i64,
) -> anyhow::Result<QuarantineOutcome> {
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to quarantine on an unidentified drive",
            mount_root.display()),
    }

    let mut out = QuarantineOutcome::default();
    // Track ids being removed this batch so the survivor check can't count a doomed sibling.
    let batch: Vec<i64> = ids.to_vec();

    for &id in ids {
        let skip = |cat: &Catalog, reason: String, out: &mut QuarantineOutcome| -> anyhow::Result<()> {
            cat.log_action("quarantine_skip",
                &serde_json::json!({"file_id": id, "reason": reason}).to_string(), now)?;
            out.skipped += 1;
            Ok(())
        };

        let Some(rec) = cat.get_file(id)? else { skip(cat, "no such file id".into(), &mut out)?; continue; };
        if rec.volume_id != expected_volume_id
            || rec.container_chain.is_some()
            || rec.status != FileStatus::Active
        {
            skip(cat, "not a loose active file on this volume".into(), &mut out)?; continue;
        }
        if !cat.active_survivor_exists(&rec.content_hash, &batch)? {
            skip(cat, "no surviving active copy would remain".into(), &mut out)?; continue;
        }

        let src = mount_root.join(&rec.relative_path);
        if !src.is_file() {
            skip(cat, format!("file not found on disk at {}", rec.relative_path), &mut out)?; continue;
        }
        let dest_rel = quarantine_dest(mount_root, &rec.relative_path);
        let dest = mount_root.join(&dest_rel);
        if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }

        match std::fs::rename(&src, &dest) {
            Ok(()) => {
                cat.mark_quarantined(id, &dest_rel.replace('\\', "/"), &rec.relative_path, now)?;
                cat.log_action("quarantine", &serde_json::json!({
                    "file_id": id, "volume_id": rec.volume_id,
                    "from": rec.relative_path, "to": dest_rel.replace('\\', "/"),
                    "hash": rec.content_hash,
                }).to_string(), now)?;
                out.quarantined += 1;
            }
            Err(e) => {
                // Cross-device or permission error: DO NOT copy+delete. Leave original in place.
                cat.log_action("quarantine_error", &serde_json::json!({
                    "file_id": id, "from": rec.relative_path, "error": e.to_string()
                }).to_string(), now)?;
                out.skipped += 1;
            }
        }
    }
    Ok(out)
}

/// Compute a collision-free `_ToDelete/<origin>` relative path (adds ` (n)` before the extension).
fn quarantine_dest(mount_root: &Path, origin_rel: &str) -> String {
    let base = format!("{QUARANTINE_DIR}/{origin_rel}");
    if !mount_root.join(&base).exists() {
        return base;
    }
    // Split extension off the last path segment for suffixing.
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) if !s.ends_with('/') => (s.to_string(), format!(".{e}")),
        _ => (base.clone(), String::new()),
    };
    for n in 1.. {
        let candidate = format!("{stem} ({n}){ext}");
        if !mount_root.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib quarantine` then `cargo test`
Expected: PASS (3 quarantine tests + full suite).

- [ ] **Step 6: Commit**

```bash
git add src/volume.rs src/quarantine.rs src/lib.rs
git commit -m "feat(quarantine): move duplicate loose files to same-drive _ToDelete safely

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Purge engine

**Files:**
- Create: `src/purge.rs`
- Modify: `src/lib.rs` (`pub mod purge;`)
- Test: inline `#[cfg(test)]` in `src/purge.rs`

**Interfaces:**
- `purge::PurgeOutcome { pub files_purged: usize, pub bytes_reclaimed: i64 }`
- `purge::purge_volume(cat: &Catalog, mount_root: &Path, expected_volume_id: &str, now: i64) -> anyhow::Result<PurgeOutcome>` — verifies the marker; sums recoverable bytes; **deletes the `<mount>/_ToDelete` tree** (the one hard delete); marks every `quarantined` row on the volume `purged` + logs; if `_ToDelete` is absent, it's a no-op success.

- [ ] **Step 1: Write failing tests**

Create `src/purge.rs`:

```rust
//! The one user-initiated hard delete: empty a drive's `_ToDelete` and mark rows purged.

use std::path::Path;
use crate::catalog::Catalog;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub files_purged: usize,
    pub bytes_reclaimed: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::{Volume, FileStatus};
    use std::fs;

    #[test]
    fn purge_deletes_quarantine_and_marks_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("_ToDelete/Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("_ToDelete/Photos/a.jpg"), b"DEADBEEF").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        // a quarantined row pointing into _ToDelete
        let mut f = crate::catalog::models::NewFile {
            volume_id: "vol-1".into(), relative_path: "_ToDelete/Photos/a.jpg".into(),
            filename: "a.jpg".into(), extension: "jpg".into(), size_bytes: 8,
            content_hash: "h".into(), created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Photo, container_chain: None };
        cat.upsert_file(&f, 100).unwrap();
        let id = cat.active_file_id("vol-1", "_ToDelete/Photos/a.jpg").unwrap().unwrap();
        cat.mark_quarantined(id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 150).unwrap();
        let _ = &mut f;

        let out = purge_volume(&cat, &root, "vol-1", 200).unwrap();
        assert_eq!(out, PurgeOutcome { files_purged: 1, bytes_reclaimed: 8 });
        assert!(!root.join("_ToDelete").exists());
        assert_eq!(cat.get_file(id).unwrap().unwrap().status, FileStatus::Purged);
    }

    #[test]
    fn purge_with_no_quarantine_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let out = purge_volume(&cat, &root, "vol-1", 200).unwrap();
        assert_eq!(out, PurgeOutcome::default());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib purge`
Expected: FAIL — `purge_volume` not found.

- [ ] **Step 3: Implement**

Add to `src/purge.rs` (above tests); register `pub mod purge;` in `lib.rs`.

```rust
const QUARANTINE_DIR: &str = "_ToDelete";

pub fn purge_volume(cat: &Catalog, mount_root: &Path, expected_volume_id: &str, now: i64)
    -> anyhow::Result<PurgeOutcome>
{
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to purge an unidentified drive",
            mount_root.display()),
    }

    let bytes_reclaimed = cat.recoverable_bytes(expected_volume_id)?;
    let rows = cat.quarantined_rows(expected_volume_id)?;

    let qdir = mount_root.join(QUARANTINE_DIR);
    if qdir.exists() {
        std::fs::remove_dir_all(&qdir)?; // THE hard delete — user-initiated only.
    }

    let mut files_purged = 0usize;
    for rec in &rows {
        cat.mark_purged(rec.id, now)?;
        files_purged += 1;
    }
    cat.log_action("purge", &serde_json::json!({
        "volume_id": expected_volume_id, "files_purged": files_purged,
        "bytes_reclaimed": bytes_reclaimed,
    }).to_string(), now)?;

    Ok(PurgeOutcome { files_purged, bytes_reclaimed })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib purge` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/purge.rs src/lib.rs
git commit -m "feat(purge): user-initiated hard delete of a drive's quarantine, marks rows purged

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: CLI wiring — `duplicates`, `quarantine`, `purge`, recoverable space in `status`

**Files:**
- Modify: `src/commands.rs`
- Modify: `src/main.rs`
- Create: `tests/quarantine_flow.rs`

**Interfaces:**
- `commands::cmd_duplicates()` — print each duplicate group: the shared hash and each member's `id`, display location (`original_path` if set else `relative_path`, plus ` › container_chain` for archived), volume, size, status.
- `commands::cmd_quarantine(mount: &Path, ids: &[i64])` — resolve the mount's `volume_id` via `read_volume_id`; open catalog; call `quarantine::quarantine_files`; print the outcome; snapshot the catalog after.
- `commands::cmd_purge(mount: &Path)` — resolve `volume_id`; confirm; call `purge::purge_volume`; print reclaimed space; snapshot after.
- `cmd_status` gains a per-volume "recoverable: X MiB in _ToDelete" line.

- [ ] **Step 1: Write the failing end-to-end integration test**

Create `tests/quarantine_flow.rs`:

```rust
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_cleanupstorages")) }

#[test]
fn scan_quarantine_purge_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    // two identical files (duplicates)
    std::fs::write(drive.join("a.txt"), b"SAME CONTENT").unwrap();
    std::fs::write(drive.join("b.txt"), b"SAME CONTENT").unwrap();
    let data = tmp.path().join("appdata");
    let env = |c: &mut Command| { c.env("CLEANUPSTORAGES_DATA_DIR", &data); };

    // scan
    let mut c = bin(); env(&mut c);
    let out = c.arg("scan").arg(&drive).arg("--readonly-fallback").arg("fingerprint").output().unwrap();
    assert!(out.status.success(), "scan: {}", String::from_utf8_lossy(&out.stderr));

    // duplicates lists the pair (find a file id to quarantine)
    let mut c = bin(); env(&mut c);
    let dups = c.arg("duplicates").output().unwrap();
    let dtext = String::from_utf8_lossy(&dups.stdout);
    assert!(dtext.contains("a.txt") && dtext.contains("b.txt"), "duplicates: {dtext}");

    // The scan used a fingerprint id (read-only marker path); the drive DID get a marker written
    // during scan (it was writable), so quarantine can identify it. Quarantine b.txt by id.
    // Parse the first integer id printed on the line containing "b.txt".
    let id: i64 = dtext.lines().find(|l| l.contains("b.txt"))
        .and_then(|l| l.split_whitespace().find_map(|t| t.trim_start_matches('#').parse().ok()))
        .expect("an id on the b.txt line");

    let mut c = bin(); env(&mut c);
    let q = c.arg("quarantine").arg(&drive).arg(id.to_string()).output().unwrap();
    assert!(q.status.success(), "quarantine: {}", String::from_utf8_lossy(&q.stderr));
    assert!(!drive.join("b.txt").exists(), "b.txt should be moved");
    assert!(drive.join("_ToDelete/b.txt").exists(), "b.txt should be in _ToDelete");
    assert!(drive.join("a.txt").exists(), "a.txt (survivor) stays");

    // purge reclaims
    let mut c = bin(); env(&mut c);
    let p = c.arg("purge").arg(&drive).output().unwrap();
    assert!(p.status.success(), "purge: {}", String::from_utf8_lossy(&p.stderr));
    assert!(!drive.join("_ToDelete").exists(), "_ToDelete removed");
}
```

Note for the implementer: `duplicates` must print each member's numeric id in a parseable way (e.g. prefixed with `#`), and on the same line as the filename, for this test to locate it. Keep the id the first whitespace token that parses as an integer after stripping a leading `#`.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test quarantine_flow`
Expected: FAIL — subcommands not implemented.

- [ ] **Step 3: Implement the handlers**

In `src/commands.rs`, add `use crate::{quarantine, purge};` and:

```rust
pub fn cmd_duplicates() -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let groups = cat.duplicate_groups()?;
    if groups.is_empty() { println!("No duplicate groups."); return Ok(()); }
    for group in &groups {
        println!("hash {} — {} copies:", &group[0].content_hash[..16.min(group[0].content_hash.len())], group.len());
        for f in group {
            let loc = display_location(f);
            println!("  #{}  {}  [{}]  {} bytes  {}",
                f.id, loc, f.volume_id, f.size_bytes, f.status.as_str());
        }
    }
    Ok(())
}

/// Where a file is / came from, for display.
fn display_location(f: &crate::catalog::models::FileRecord) -> String {
    let base = f.original_path.as_deref().unwrap_or(&f.relative_path);
    match &f.container_chain {
        Some(chain) => format!("{base} › {chain}"),
        None => base.to_string(),
    }
}

pub fn cmd_quarantine(mount: &Path, ids: &[i64]) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}; scan the drive first", mount.display()))?;
    let now = now_secs();
    let out = quarantine::quarantine_files(&cat, mount, &vid, ids, now)?;
    println!("Quarantined {} file(s), skipped {}.", out.quarantined, out.skipped);
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_purge(mount: &Path) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}", mount.display()))?;
    let now = now_secs();
    // snapshot BEFORE the irreversible delete
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot (pre-purge): {}", snap.display());
    let out = purge::purge_volume(&cat, mount, &vid, now)?;
    println!("Purged {} file(s), reclaimed {} MiB.", out.files_purged, out.bytes_reclaimed / (1024*1024));
    Ok(())
}
```

Extend `cmd_status`'s per-volume loop to also print recoverable space:

```rust
    for (id, label, count, bytes) in cat.volume_stats()? {
        let recoverable = cat.recoverable_bytes(&id)?;
        println!("  {label} [{id}]: {count} files, {} MiB (recoverable: {} MiB in _ToDelete)",
            bytes / (1024 * 1024), recoverable / (1024 * 1024));
    }
```

- [ ] **Step 4: Add the subcommands in `src/main.rs`**

Add to `Command`:

```rust
    /// List duplicate groups (files sharing a content hash), with ids to act on.
    Duplicates,
    /// Move confirmed-duplicate files (by id) to the drive's _ToDelete quarantine.
    Quarantine {
        /// Current mount path of the drive holding the files.
        mount: std::path::PathBuf,
        /// Catalog ids of the files to quarantine (from `duplicates`).
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Permanently delete a drive's _ToDelete quarantine and reclaim space.
    Purge {
        /// Current mount path of the drive to purge.
        mount: std::path::PathBuf,
    },
```

And dispatch:

```rust
        Command::Duplicates => commands::cmd_duplicates(),
        Command::Quarantine { mount, ids } => commands::cmd_quarantine(&mount, &ids),
        Command::Purge { mount } => commands::cmd_purge(&mount),
```

- [ ] **Step 5: Run the integration test + full suite + build**

Run: `cargo test --test quarantine_flow` then `cargo test` then `cargo build --release`
Expected: all PASS; release builds.

- [ ] **Step 6: Commit**

```bash
git add src/commands.rs src/main.rs tests/quarantine_flow.rs
git commit -m "feat(cli): duplicates/quarantine/purge commands + recoverable space in status

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (§10 policy, §11 storage, §7 lifecycle, §5 audit):**
- Soft-delete to same-drive `_ToDelete` via rename, never copy → Task 3 (`quarantine_files`, cross-device aborts) ✓
- Never remove the last copy → Task 2 (`active_survivor_exists`) + Task 3 (enforced with batch exclusion) ✓
- Only loose active files (archive internals deferred to 2c) → Task 3 guard ✓
- Quarantine is a rename → ~zero space; space reclaimed only on `purge` → Tasks 3, 4 ✓
- `purge` is the sole hard delete, per-drive, user-initiated → Task 4, Task 5 (`Purge` command) ✓
- Status lifecycle `active → quarantined → purged`, rows never deleted → Tasks 2, 3, 4 ✓
- Every action in `actions_log` (JSON details) → Tasks 2–4 (`log_action`) ✓
- Recoverable-space reporting → Task 2 (`recoverable_bytes`) + Task 5 (`status`) ✓
- Drive identity verified before touching a drive (marker match) → Tasks 3, 4 ✓
- Catalog snapshot before the irreversible purge → Task 5 (`cmd_purge`) ✓

**Catalog-integrity design note:** quarantine repoints `relative_path` to the `_ToDelete/…` location and stores origin in `original_path`, so (a) a new file later created at the origin path gets its own row instead of resurrecting the quarantined one, (b) the scanner (which skips `_ToDelete`) never re-touches quarantined rows, and (c) the missing-sweep (only `status='active'`) never flags them. No scan-path query needed changing.

**Placeholder scan:** No TBD/TODO; every step has runnable code + commands. ✓

**Type consistency:** `FileRecord.original_path` added in Task 1 and read at index 15 everywhere `map_file_record` is used (get_file, duplicate_groups, quarantined_rows, search_filtered); `QuarantineOutcome`/`PurgeOutcome` fields consistent between engine and CLI; `read_volume_id` used by engines and CLI. `map_file_record` MUST be updated in Task 1 to select+read `original_path` or every SELECT using it breaks — Task 1 Step 4 covers this. ✓

---

## Phase 2 — remaining sub-plans (outline; each gets its own detailed plan)

**Plan 2b — Duplicate review GUI** (builds on 2a's engine): web endpoints `GET /api/duplicates` (groups + per-file metadata + a suggested "keep" = earliest `created_time`/most complete metadata), `GET /api/preview/<id>` (image thumbnail for photos — decode + downscale in-memory; for archived photos, stream the entry from the zip; metadata/text for others), and `POST /api/quarantine` (body: keep-id + quarantine-ids → calls `quarantine_files`, requires the drive mounted). A review screen: Tinder-style card per group with a WinMerge-style side-by-side compare (thumbnails, metadata diff, `container_chain`). Case 3 (file only inside archives / archive fully redundant) surfaced as an **advisory** panel, no action button. These are the first **write** endpoints — they must use the read-write `Catalog::open`, verify the drive is present, and never act without an explicit confirm.

**Plan 2c — Case 4 verified archive repack** (highest risk, isolated): for an identical entry inside two *different* archives, a crash-safe repack — pre-check a surviving active copy; extract the removed entry to `_ToDelete`; build a new archive as `*.rebuilding.tmp`; re-hash every retained entry against the catalog; only on full verification, move the original archive to `_ToDelete/<name>.original.zip` and swap the temp in; record the whole chain in `actions_log`. Includes the pre-flight free-space check and the configurable scratch location (spec §11) so a near-full drive uses another disk for the temp build. Never automatic; opt-in per case from the 2b GUI.

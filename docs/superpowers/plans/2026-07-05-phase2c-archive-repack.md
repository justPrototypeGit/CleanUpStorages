# Phase 2c — Verified Archive Repack (Case 4) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user remove a duplicate that lives *inside* a top-level `.zip` (when an identical copy survives elsewhere) by rebuilding that archive without the entry — a crash-safe, verify-before-swap operation that can never lose data. This is the last piece of the deduplication story and the highest-risk one (it rewrites an archive file).

**Architecture:** A new `src/repack.rs` module performs the Case-4 sequence: validate the entry, verify the drive marker, run the disk-aware never-remove-last-copy guard (survivors may be loose or in another archive), pre-flight the drive's free space, **build a new archive as a temp file in `_ToDelete` with every OTHER entry raw-copied**, **re-hash every retained entry against the catalog to verify**, and only then extract the removed entry to `_ToDelete` (safety net 1), move the original archive to `_ToDelete/<name>.original.zip` (safety net 2), and atomically swap the verified temp into place. It records the whole chain in `actions_log`, repoints the removed entry's catalog row to its extracted loose copy, and re-hashes the rebuilt archive. Exposed via a `repack` CLI command and a CSRF-guarded `POST /api/repack` wired into the review page's archived members.

**Tech Stack:** Rust 1.88, existing deps (`zip` for raw entry copy, `sysinfo` for free space, `blake3`/`hashing` for verify). No new crates.

## Global Constraints

- **Reliability is absolute — this is the one operation that rewrites a user file, so every step below is a hard gate.** The invariants:
  - **The original archive is never modified in place and never deleted during the build/verify.** A new archive is built as a separate temp file; the original is only *moved* (a same-drive rename to `_ToDelete/<name>.original.zip`) after the temp is fully built AND verified.
  - **Verify-before-swap:** the temp archive is re-opened and every catalog-known retained entry is re-hashed and compared to its recorded `content_hash`; the removed entry must be absent. **If any check fails, or anything errors, the temp is deleted and the original archive is left byte-for-byte untouched — nothing is lost.**
  - **Never remove the last copy:** the entry may be removed only if a disk-verified surviving copy exists (loose on this drive, or in another archive on this drive whose archive file exists, or any copy on a *different* volume — trusted as genuinely separate). Same disk-aware guard as Phase 2a, generalized.
  - **Two independent recovery nets after the swap:** the removed entry's content is extracted to `_ToDelete/…` as a loose file, AND the entire original archive sits in `_ToDelete/<name>.original.zip`. Both are recoverable until the user runs `purge`.
  - **Marker-verified drive** (must be mounted and its `.cleanupstorages_id` == the expected volume) before touching anything.
  - **Pre-flight free space:** before building, require the drive's free space ≥ the original archive's size (conservative — the rebuilt archive is smaller). If insufficient, **abort with guidance to `purge` first** — never fill the drive.
- **Scope: top-level entries of a `.zip` only.** The removed entry's `container_chain` must contain no ` › ` nesting separator (a direct entry of the archive). Nested-archive repack stays out of scope (spec §10 "the tool will never automatically repack a nested archive") — such a request is refused with a clear message.
- **Raw copy preserves retained entries exactly.** Rebuild uses the zip crate's raw entry copy (no decompress/recompress) so every retained entry keeps its exact bytes, compression, and metadata. Verify re-hashes them anyway as a belt-and-suspenders check.
- **The temp is built inside `_ToDelete`** (which the scanner already skips), so a crashed repack leaves an orphan temp that `purge` cleans and the scanner never catalogs.
- **`purge` (the irreversible delete) is unchanged and still the only hard delete.** Repack itself performs no `remove_file`/`remove_dir_all` on user data — it only *moves* the original into `_ToDelete` (recoverable) and swaps the temp in.
- **Reversible-only over HTTP:** `POST /api/repack` is CSRF-guarded (same token as 2b) and, like quarantine, only produces recoverable `_ToDelete` state; it never purges.
- **Git:** branch `feat/phase2c-repack` off `main`. Conventional Commits, scope `catalog`/`repack`/`cli`/`web`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (merged):** `Catalog` (`get_file`, `active_copies`, `mark_quarantined`, `log_action`, `volume_stats`), `models::{FileRecord, FileStatus}`, `volume::{read_volume_id, QUARANTINE_DIR}`, `hashing`, `config::Config`, `backup::snapshot`, the 2a quarantine engine, the 2b web server (`AppState`, CSRF, mounts). **Out of scope:** nested-archive repack; a configurable cross-drive scratch location (near-full-drive optimization — same-drive build + pre-flight for now); near-duplicate detection.

---

## File Structure

- `src/catalog/store.rs` — add `archive_entries` (list an archive's entries), `update_archive_hash`; adjust `mark_quarantined` to also clear `container_chain`.
- `src/repack.rs` — **new**: `RepackOutcome`, primitives (`rebuild_without`, `verify_rebuilt`, `extract_entry`), free-space check, and `repack_entry` (the engine). Registered in `lib.rs`.
- `src/commands.rs` — `cmd_repack`.
- `src/main.rs` — `Repack` subcommand.
- `src/web.rs` — `POST /api/repack`; review page offers "remove from archive" on archived members.
- `src/lib.rs` — `pub mod repack;`.
- `tests/repack_flow.rs` — **new** CLI e2e.

---

### Task 1: Store support for repack

**Files:**
- Modify: `src/catalog/store.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `pub fn archive_entries(&self, volume_id: &str, archive_rel_path: &str) -> anyhow::Result<Vec<FileRecord>>` — all `status='active'` rows with this `relative_path` and `container_chain IS NOT NULL` (the archive's catalogued entries).
- `pub fn update_archive_hash(&self, id: i64, content_hash: &str, size_bytes: i64, now: i64) -> anyhow::Result<()>` — update a loose archive row's hash/size after a rebuild.
- Adjust `mark_quarantined` to ALSO set `container_chain = NULL` (so an extracted archive entry becomes a proper loose quarantined row). This is a no-op for already-loose files.

- [ ] **Step 1: Write failing tests**

Add to `store.rs` `mod tests`:

```rust
    #[test]
    fn archive_entries_lists_only_that_archives_entries() {
        let (_t, cat) = open_tmp();
        let e = |chain: &str, hash: &str| crate::archive::ArchiveEntry {
            container_chain: chain.into(), filename: chain.rsplit('/').next().unwrap().into(),
            extension: "jpg".into(), size_bytes: 5, content_hash: hash.into() };
        cat.upsert_archive_entry("vol-1", "a.zip", &e("x.jpg", "h1"), 100).unwrap();
        cat.upsert_archive_entry("vol-1", "a.zip", &e("y.jpg", "h2"), 100).unwrap();
        cat.upsert_archive_entry("vol-1", "b.zip", &e("z.jpg", "h3"), 100).unwrap();
        let es = cat.archive_entries("vol-1", "a.zip").unwrap();
        assert_eq!(es.len(), 2);
        assert!(es.iter().all(|r| r.relative_path == "a.zip" && r.container_chain.is_some()));
    }

    #[test]
    fn mark_quarantined_clears_container_chain() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "a.zip",
            &crate::archive::ArchiveEntry { container_chain: "x.jpg".into(), filename: "x.jpg".into(),
                extension: "jpg".into(), size_bytes: 5, content_hash: "h1".into() }, 100).unwrap();
        // find the entry row id
        let id = cat.archive_entries("vol-1", "a.zip").unwrap()[0].id;
        cat.mark_quarantined(id, "_ToDelete/a.zip/x.jpg", "a.zip › x.jpg", 200).unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.container_chain, None); // now a loose quarantined row
        assert_eq!(rec.relative_path, "_ToDelete/a.zip/x.jpg");
        assert_eq!(rec.original_path.as_deref(), Some("a.zip › x.jpg"));
    }

    #[test]
    fn update_archive_hash_changes_hash_and_size() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.zip", "OLD"), 100).unwrap();
        let id = cat.active_file_id("vol-1", "a.zip").unwrap().unwrap();
        cat.update_archive_hash(id, "NEW", 999, 200).unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.content_hash, "NEW");
        assert_eq!(rec.size_bytes, 999);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib store`
Expected: FAIL — methods absent / `mark_quarantined` doesn't clear container_chain.

- [ ] **Step 3: Implement**

In `store.rs`, add:

```rust
    pub fn archive_entries(&self, volume_id: &str, archive_rel_path: &str) -> anyhow::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at, original_path FROM files
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NOT NULL AND status='active'
             ORDER BY id")?;
        Ok(stmt.query_map(params![volume_id, archive_rel_path], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_archive_hash(&self, id: i64, content_hash: &str, size_bytes: i64, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE files SET content_hash=?2, size_bytes=?3, last_seen_at=?4 WHERE id=?1",
            params![id, content_hash, size_bytes, now])?;
        Ok(())
    }
```

Modify `mark_quarantined`'s UPDATE to also null `container_chain`:

```rust
    pub fn mark_quarantined(&self, id: i64, new_relative_path: &str, original_path: &str, now: i64)
        -> anyhow::Result<()>
    {
        self.conn.execute(
            "UPDATE files SET status='quarantined', relative_path=?2, original_path=?3,
                 container_chain=NULL, last_seen_at=?4 WHERE id=?1",
            params![id, new_relative_path, original_path, now])?;
        Ok(())
    }
```

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib store` then `cargo test`
Expected: PASS — new tests pass; existing quarantine/2a tests still pass (nulling an already-null container_chain is a no-op).

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/phase2c-repack   # only if not already on it
git add src/catalog/store.rs
git commit -m "feat(catalog): archive entry listing, archive-hash update, quarantine clears chain

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Repack primitives (rebuild, verify, extract)

**Files:**
- Create: `src/repack.rs`
- Modify: `src/lib.rs` (`pub mod repack;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `pub fn extract_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>>` — bytes of one top-level entry.
- `pub fn rebuild_without(src_archive: &Path, dest_tmp: &Path, exclude_entry: &str) -> anyhow::Result<()>` — write a new zip containing every entry of `src_archive` except `exclude_entry`, raw-copied (no recompression). Errors if `exclude_entry` isn't present.
- `pub fn verify_rebuilt(tmp: &Path, expected: &std::collections::HashMap<String, String>, must_be_absent: &str) -> anyhow::Result<()>` — re-open `tmp`; for each `(name, hash)` in `expected`, the entry must be present and its streamed BLAKE3 must equal `hash`; `must_be_absent` must NOT be present. Any deviation → `Err`.

Note (zip 2.x API): rebuild uses raw copy to preserve bytes/metadata — e.g. iterate `src.by_index_raw(i)` / `writer.raw_copy_file(entry)`. If the resolved zip 2.4.x method names differ, adapt minimally to the same behavior (copy every entry except the excluded one without recompressing); do NOT switch to decompress+recompress (it would change bytes/metadata and defeat verify).

- [ ] **Step 1: Write failing tests**

Create `src/repack.rs`:

```rust
//! Crash-safe removal of one entry from a top-level zip (Case 4): build a new archive without the
//! entry, verify every retained entry against the catalog, then swap — original never touched until
//! the rebuild is proven good.

use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RepackOutcome {
    pub removed_entry: String,
    pub retained_entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    fn blake3_hex(bytes: &[u8]) -> String {
        let mut b: &[u8] = bytes;
        crate::hashing::hash_reader(&mut b).unwrap()
    }

    #[test]
    fn rebuild_drops_the_entry_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP"), ("also/keep2.txt", b"K2")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        assert!(extract_entry(&out, "keep.txt").is_ok());
        assert!(extract_entry(&out, "also/keep2.txt").is_ok());
        assert!(extract_entry(&out, "drop.jpg").is_err()); // gone
    }

    #[test]
    fn verify_passes_for_matching_entries_and_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        let mut expected = std::collections::HashMap::new();
        expected.insert("keep.txt".to_string(), blake3_hex(b"KEEP"));
        verify_rebuilt(&out, &expected, "drop.jpg").unwrap();

        // a wrong expected hash must fail
        let mut bad = std::collections::HashMap::new();
        bad.insert("keep.txt".to_string(), "deadbeef".to_string());
        assert!(verify_rebuilt(&out, &bad, "drop.jpg").is_err());
        // the removed entry still present would fail
        assert!(verify_rebuilt(&src, &expected, "drop.jpg").is_err());
    }

    #[test]
    fn rebuild_errors_if_excluded_entry_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP")]);
        assert!(rebuild_without(&src, &out, "not-there.jpg").is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib repack`
Expected: FAIL — functions absent.

- [ ] **Step 3: Implement the primitives**

Add to `src/repack.rs` (above the tests). Register `pub mod repack;` in `lib.rs`.

```rust
use std::collections::HashMap;

/// Bytes of one top-level entry.
pub fn extract_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

/// Build `dest_tmp` = every entry of `src_archive` except `exclude_entry`, raw-copied (no recompress).
pub fn rebuild_without(src_archive: &Path, dest_tmp: &Path, exclude_entry: &str) -> anyhow::Result<()> {
    let src_file = std::fs::File::open(src_archive)?;
    let mut src = zip::ZipArchive::new(src_file)?;
    if let Some(parent) = dest_tmp.parent() { std::fs::create_dir_all(parent)?; }
    let out_file = std::fs::File::create(dest_tmp)?;
    let mut writer = zip::ZipWriter::new(out_file);

    let mut found = false;
    for i in 0..src.len() {
        let entry = src.by_index_raw(i)?;
        if entry.name() == exclude_entry { found = true; continue; }
        writer.raw_copy_file(entry)?;
    }
    writer.finish()?;
    if !found {
        let _ = std::fs::remove_file(dest_tmp);
        anyhow::bail!("entry '{exclude_entry}' not found in {}", src_archive.display());
    }
    Ok(())
}

/// Re-hash every expected retained entry and confirm the removed one is absent.
pub fn verify_rebuilt(tmp: &Path, expected: &HashMap<String, String>, must_be_absent: &str) -> anyhow::Result<()> {
    let file = std::fs::File::open(tmp)?;
    let mut zip = zip::ZipArchive::new(file)?;

    if zip.by_name(must_be_absent).is_ok() {
        anyhow::bail!("verify failed: removed entry '{must_be_absent}' is still present");
    }
    for (name, want) in expected {
        let mut entry = zip.by_name(name)
            .map_err(|_| anyhow::anyhow!("verify failed: retained entry '{name}' missing"))?;
        let got = crate::hashing::hash_reader(&mut entry)?;
        if &got != want {
            anyhow::bail!("verify failed: entry '{name}' hash mismatch (got {got}, want {want})");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib repack` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/repack.rs src/lib.rs
git commit -m "feat(repack): raw-copy rebuild, entry extract, verify-by-rehash primitives

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: The repack engine (crash-safe orchestration + pre-flight space)

**Files:**
- Modify: `src/repack.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `pub fn available_space(path: &Path) -> Option<u64>` — free bytes on the filesystem containing `path` (via `sysinfo`).
- `pub fn repack_entry(cat: &Catalog, mount_root: &Path, expected_volume_id: &str, entry_id: i64, now: i64) -> anyhow::Result<RepackOutcome>` — the full Case-4 sequence.

Sequence (each guard aborts leaving the original archive untouched):
1. Load `entry` = `cat.get_file(entry_id)`; require: on `expected_volume_id`, `status=Active`, `container_chain = Some(chain)` with **no** ` › ` (top-level), and the entry name = chain.
2. Marker: `read_volume_id(mount_root) == expected_volume_id`, else bail.
3. Survivor guard (disk-aware, same rule as 2a via `cat.active_copies(&entry.content_hash)`): a survivor counts if it's a different row AND (different volume OR its `relative_path` exists on this mount). Refuse otherwise.
4. `archive_path = mount_root.join(&entry.relative_path)`; require it `is_file()`.
5. Pre-flight: `available_space(&archive_path) >= archive_size`, else bail advising `purge`.
6. Build temp at `mount_root/_ToDelete/.rebuild/<archive_filename>.rebuilding.tmp` via `rebuild_without(archive_path, tmp, &chain)`.
7. Verify: `expected` = `{ e.container_chain : e.content_hash }` for every `cat.archive_entries(volume, rel)` whose chain != the removed chain; `verify_rebuilt(tmp, &expected, &chain)`. On any error: delete tmp, bail (original untouched).
8. Extract removed entry to `_ToDelete/<archive_rel>/<entry_name>` (collision-suffixed) — safety net 1.
9. Swap: move `archive_path` → `_ToDelete/<archive_rel>.original.zip` (safety net 2); move tmp → `archive_path`.
10. Catalog + audit: `mark_quarantined(entry_id, extract_rel, "<archive_rel> › <chain>", now)`; re-hash the new archive and `update_archive_hash(archive_row_id, new_hash, new_size, now)` (find the archive's loose row via `active_file_id(volume, archive_rel)`); `log_action("repack", { … })`.

- [ ] **Step 1: Write failing tests**

Add to `repack.rs` `mod tests` (build a fake drive; catalog the archive + its entries via a real scan so hashes are correct; and a surviving loose copy of the removed entry so the guard passes):

```rust
    use crate::catalog::Catalog;
    use crate::catalog::models::{Volume, FileStatus};

    fn fake_drive_with_archive() -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        // archive with two entries; "dup.txt" also exists as a loose file (the surviving copy)
        make_zip(&root.join("bundle.zip"), &[("keep.txt", b"KEEPDATA"), ("dup.txt", b"SHARED")]);
        std::fs::write(root.join("loose_dup.txt"), b"SHARED").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = crate::volume::VolumeIdentity { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into() };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100).unwrap();
        (tmp, cat, root)
    }

    #[test]
    fn repack_removes_entry_keeps_rest_and_preserves_recovery() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // id of the archived entry bundle.zip › dup.txt
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap();

        let out = repack_entry(&cat, &root, "vol-1", entry.id, 200).unwrap();
        assert_eq!(out.removed_entry, "dup.txt");

        // the rebuilt archive no longer contains dup.txt but still has keep.txt
        assert!(extract_entry(&root.join("bundle.zip"), "keep.txt").is_ok());
        assert!(extract_entry(&root.join("bundle.zip"), "dup.txt").is_err());
        // safety nets: extracted loose copy + the original archive both in _ToDelete
        assert!(root.join("_ToDelete").exists());
        assert!(std::fs::read_dir(root.join("_ToDelete")).unwrap().count() > 0);
        // catalog: the entry row is now quarantined + loose
        let rec = cat.get_file(entry.id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.container_chain, None);
        let _ = tmp;
    }

    #[test]
    fn repack_refuses_when_no_surviving_copy() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // delete the loose survivor off disk so dup.txt inside the zip is the last copy
        std::fs::remove_file(root.join("loose_dup.txt")).unwrap();
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap();
        let res = repack_entry(&cat, &root, "vol-1", entry.id, 200);
        assert!(res.is_err());
        // the archive is untouched — dup.txt still inside
        assert!(extract_entry(&root.join("bundle.zip"), "dup.txt").is_ok());
        let _ = tmp;
    }

    #[test]
    fn repack_refuses_nested_entry() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // fabricate a nested-chain entry row (container_chain with ' › ')
        cat.upsert_archive_entry("vol-1", "bundle.zip",
            &crate::archive::ArchiveEntry { container_chain: "inner.zip › deep.txt".into(),
                filename: "deep.txt".into(), extension: "txt".into(), size_bytes: 3,
                content_hash: "SHARED_H".into() }, 100).unwrap();
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("inner.zip › deep.txt")).unwrap();
        let res = repack_entry(&cat, &root, "vol-1", entry.id, 200);
        assert!(res.is_err()); // nested not supported
        let _ = tmp;
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib repack`
Expected: FAIL — `repack_entry`/`available_space` absent.

- [ ] **Step 3: Implement the engine**

Add to `src/repack.rs`:

```rust
use crate::catalog::Catalog;
use crate::catalog::models::FileStatus;

/// Free bytes on the filesystem containing `path`, via sysinfo. None if undetermined.
pub fn available_space(path: &Path) -> Option<u64> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point();
        if path.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, d.available_space()));
            }
        }
    }
    best.map(|(_, s)| s)
}

const REBUILD_DIR: &str = ".rebuild";

pub fn repack_entry(cat: &Catalog, mount_root: &Path, expected_volume_id: &str, entry_id: i64, now: i64)
    -> anyhow::Result<RepackOutcome>
{
    // 1. Load + validate the entry.
    let entry = cat.get_file(entry_id)?.ok_or_else(|| anyhow::anyhow!("no such file id {entry_id}"))?;
    if entry.volume_id != expected_volume_id || entry.status != FileStatus::Active {
        anyhow::bail!("entry {entry_id} is not an active file on volume {expected_volume_id}");
    }
    let chain = entry.container_chain.clone()
        .ok_or_else(|| anyhow::anyhow!("file {entry_id} is not an archive entry"))?;
    if chain.contains(" › ") {
        anyhow::bail!("entry is inside a nested archive; nested repack is not supported — remove it manually");
    }

    // 2. Marker gate.
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!("drive is volume {vid}, not {expected_volume_id}; aborting"),
        None => anyhow::bail!("no identity marker; refusing to repack on an unidentified drive"),
    }

    // 3. Disk-aware never-remove-last-copy guard.
    let survivor_ok = cat.active_copies(&entry.content_hash)?.iter().any(|s| {
        s.id != entry.id
            && (s.volume_id != expected_volume_id || mount_root.join(&s.relative_path).exists())
    });
    if !survivor_ok {
        anyhow::bail!("no surviving copy verified — refusing to remove the last copy of this content");
    }

    // 4. Archive on disk.
    let archive_rel = entry.relative_path.clone();
    let archive_path = mount_root.join(&archive_rel);
    if !archive_path.is_file() {
        anyhow::bail!("archive {} not found on disk", archive_rel);
    }
    let archive_size = std::fs::metadata(&archive_path)?.len();

    // 5. Pre-flight free space.
    if let Some(free) = available_space(&archive_path) {
        if free < archive_size {
            anyhow::bail!("not enough free space on the drive to repack safely ({free} < {archive_size}); \
                          run `purge` to reclaim space first");
        }
    }

    // 6. Build the temp inside _ToDelete (scanner-skipped; purge-cleaned).
    let qdir = mount_root.join(crate::volume::QUARANTINE_DIR);
    let rebuild_dir = qdir.join(REBUILD_DIR);
    std::fs::create_dir_all(&rebuild_dir)?;
    let archive_filename = Path::new(&archive_rel).file_name()
        .map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "archive.zip".into());
    let tmp = rebuild_dir.join(format!("{archive_filename}.rebuilding.tmp"));
    rebuild_without(&archive_path, &tmp, &chain)?;

    // 7. Verify every retained catalogued entry; on any failure, delete tmp and abort (original intact).
    let expected: std::collections::HashMap<String, String> = cat.archive_entries(expected_volume_id, &archive_rel)?
        .into_iter()
        .filter(|e| e.container_chain.as_deref() != Some(chain.as_str()))
        .filter_map(|e| e.container_chain.map(|c| (c, e.content_hash)))
        .collect();
    if let Err(e) = verify_rebuilt(&tmp, &expected, &chain) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // 8. Extract the removed entry to _ToDelete (safety net 1).
    let extract_rel = quarantine_dest(cat, mount_root, expected_volume_id,
        &format!("{archive_rel}/{chain}"));
    let extract_path = mount_root.join(&extract_rel);
    if let Some(p) = extract_path.parent() { std::fs::create_dir_all(p)?; }
    std::fs::write(&extract_path, extract_entry(&archive_path, &chain)?)?;

    // 9. Swap: original -> _ToDelete/<name>.original.zip (safety net 2); temp -> archive path.
    let original_rel = quarantine_dest(cat, mount_root, expected_volume_id,
        &format!("{archive_rel}.original.zip"));
    let original_dest = mount_root.join(&original_rel);
    if let Some(p) = original_dest.parent() { std::fs::create_dir_all(p)?; }
    std::fs::rename(&archive_path, &original_dest)?;
    std::fs::rename(&tmp, &archive_path)?;

    // 10. Catalog + audit.
    cat.mark_quarantined(entry_id, &extract_rel.replace('\\', "/"),
        &format!("{archive_rel} › {chain}"), now)?;
    let new_hash = crate::hashing::hash_file(&archive_path)?;
    let new_size = std::fs::metadata(&archive_path)?.len() as i64;
    if let Some(arch_id) = cat.active_file_id(expected_volume_id, &archive_rel)? {
        cat.update_archive_hash(arch_id, &new_hash, new_size, now)?;
    }
    cat.log_action("repack", &serde_json::json!({
        "volume_id": expected_volume_id, "archive": archive_rel, "removed_entry": chain,
        "extracted_to": extract_rel.replace('\\', "/"),
        "original_saved_to": original_rel.replace('\\', "/"),
        "retained_entries": expected.len(), "new_archive_hash": new_hash,
    }).to_string(), now)?;

    Ok(RepackOutcome { removed_entry: chain, retained_entries: expected.len() })
}

/// Collision-free `_ToDelete/<rel>` path avoiding disk AND catalog collisions (mirrors quarantine).
fn quarantine_dest(cat: &Catalog, mount_root: &Path, volume_id: &str, origin_rel: &str) -> String {
    let base = format!("{}/{origin_rel}", crate::volume::QUARANTINE_DIR);
    let taken = |cand: &str| mount_root.join(cand).exists()
        || cat.loose_path_taken(volume_id, cand).unwrap_or(false);
    if !taken(&base) { return base; }
    let (dir, seg) = match base.rsplit_once('/') {
        Some((d, s)) => (format!("{d}/"), s.to_string()), None => (String::new(), base.clone()) };
    let (stem, ext) = match seg.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")), _ => (seg.clone(), String::new()) };
    for n in 1.. {
        let cand = format!("{dir}{stem} ({n}){ext}");
        if !taken(&cand) { return cand; }
    }
    unreachable!()
}
```

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib repack` then `cargo test`
Expected: PASS (3 engine tests + primitives + full suite).

- [ ] **Step 5: Commit**

```bash
git add src/repack.rs
git commit -m "feat(repack): crash-safe Case-4 engine (verify-before-swap, dual recovery nets)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: CLI `repack` command + e2e

**Files:**
- Modify: `src/commands.rs`
- Modify: `src/main.rs`
- Create: `tests/repack_flow.rs`

**Interfaces:**
- `commands::cmd_repack(mount: &Path, entry_id: i64)` — resolve the mount's volume via `read_volume_id`; snapshot the catalog BEFORE; call `repack::repack_entry`; print the outcome.
- `Repack { mount, entry_id }` subcommand.

- [ ] **Step 1: Write the failing e2e test**

Create `tests/repack_flow.rs`:

```rust
use std::io::Write;
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_cleanupstorages")) }

fn write_zip(path: &std::path::Path, files: &[(&str, &[u8])]) {
    let f = std::fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in files { zw.start_file(*name, opts).unwrap(); zw.write_all(bytes).unwrap(); }
    zw.finish().unwrap();
}

fn zip_has(path: &std::path::Path, name: &str) -> bool {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    z.by_name(name).is_ok()
}

#[test]
fn scan_then_repack_removes_archived_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    write_zip(&drive.join("bundle.zip"), &[("keep.txt", b"KEEP"), ("dup.txt", b"SHARED")]);
    std::fs::write(drive.join("loose_dup.txt"), b"SHARED").unwrap(); // the surviving copy
    let data = tmp.path().join("appdata");
    let env = |c: &mut Command| { c.env("CLEANUPSTORAGES_DATA_DIR", &data); };

    let mut c = bin(); env(&mut c);
    assert!(c.arg("scan").arg(&drive).arg("--readonly-fallback").arg("fingerprint").output().unwrap().status.success());

    // find the entry id via `duplicates` (dup.txt appears loose and inside bundle.zip)
    let mut c = bin(); env(&mut c);
    let dups = String::from_utf8(c.arg("duplicates").output().unwrap().stdout).unwrap();
    // the archived member line shows "bundle.zip › dup.txt"
    let id: i64 = dups.lines().find(|l| l.contains("bundle.zip › dup.txt"))
        .and_then(|l| l.split_whitespace().find_map(|t| t.trim_start_matches('#').parse().ok()))
        .expect("archived dup.txt id in duplicates output");

    let mut c = bin(); env(&mut c);
    let out = c.arg("repack").arg(&drive).arg(id.to_string()).output().unwrap();
    assert!(out.status.success(), "repack: {}", String::from_utf8_lossy(&out.stderr));

    assert!(zip_has(&drive.join("bundle.zip"), "keep.txt"));
    assert!(!zip_has(&drive.join("bundle.zip"), "dup.txt")); // removed
    assert!(drive.join("_ToDelete").exists()); // recovery nets present
    assert!(drive.join("loose_dup.txt").exists()); // survivor untouched
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test repack_flow`
Expected: FAIL — `repack` subcommand absent.

- [ ] **Step 3: Implement the handler + subcommand**

In `src/commands.rs` add `use crate::repack;` and:

```rust
pub fn cmd_repack(mount: &Path, entry_id: i64) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}; scan the drive first", mount.display()))?;
    let now = now_secs();
    // snapshot BEFORE modifying an archive
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot (pre-repack): {}", snap.display());
    let out = repack::repack_entry(&cat, mount, &vid, entry_id, now)?;
    println!("Repacked: removed '{}', {} entries retained. Original archive and removed item saved in _ToDelete (recoverable until purge).",
        out.removed_entry, out.retained_entries);
    Ok(())
}
```

In `src/main.rs` add to `Command`:

```rust
    /// Remove one entry from a top-level zip by rebuilding it (Case 4; needs a surviving copy).
    Repack {
        /// Current mount path of the drive holding the archive.
        mount: std::path::PathBuf,
        /// Catalog id of the archived entry to remove (from `duplicates`).
        entry_id: i64,
    },
```

And dispatch: `Command::Repack { mount, entry_id } => commands::cmd_repack(&mount, entry_id),`.

- [ ] **Step 4: Run e2e + full suite + release build**

Run: `cargo test --test repack_flow` then `cargo test` then `cargo build --release`
Expected: all PASS; release builds.

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs src/main.rs tests/repack_flow.rs
git commit -m "feat(cli): repack command to remove an archived duplicate (Case 4)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Web `POST /api/repack` + review page integration

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Route `POST /api/repack`. Header `x-cleanup-token` == `state.csrf_token` (else 403). Body `{ entry_id: i64 }`. Resolves the entry's volume + mount; calls `repack::repack_entry`; snapshots after; returns `{ removed_entry, retained_entries }` or an error message. Uses read-write `Catalog::open`.
- The `/review` page: for an archived member (`is_loose == false`) that is NOT the suggested keep, offer a "Remove from archive" button that POSTs its id to `/api/repack` (with the token). Only enabled when the member's drive is `mounted`.

- [ ] **Step 1: Write failing tests**

Add to `web.rs` `mod tests` (reuse `seed_dupes`/`post_json`; build a real archive with a shared entry on the fake drive):

```rust
    #[tokio::test]
    async fn repack_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/repack", None,
            serde_json::json!({"entry_id": 1})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn repack_removes_entry_over_http() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("driveA");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join(".cleanupstorages_id"), "vol-1").unwrap();
        {
            use std::io::Write;
            let f = std::fs::File::create(drive.join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (n, b) in [("keep.txt", &b"KEEP"[..]), ("dup.txt", &b"SHARED"[..])] {
                zw.start_file(n, opts).unwrap(); zw.write_all(b).unwrap();
            }
            zw.finish().unwrap();
        }
        std::fs::write(drive.join("loose_dup.txt"), b"SHARED").unwrap();
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume { volume_id: "vol-1".into(),
                label: "D".into(), identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
            let ident = crate::volume::VolumeIdentity { volume_id: "vol-1".into(), label: "D".into(),
                identified_by: "marker".into() };
            crate::scanner::scan_volume(&cat, &drive, &ident, false, 100).unwrap();
        }
        let mut mounts = std::collections::HashMap::new();
        mounts.insert("vol-1".to_string(), drive.clone());
        let state = AppState { catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into() };

        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let entry_id = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap().id;
        drop(cat);

        let (status, json) = post_json(state, "/api/repack", Some("T"),
            serde_json::json!({"entry_id": entry_id})).await;
        assert_eq!(status, axum::http::StatusCode::OK, "body {json}");
        assert_eq!(json["removed_entry"], "dup.txt");

        let f = std::fs::File::open(drive.join("bundle.zip")).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        assert!(z.by_name("keep.txt").is_ok());
        assert!(z.by_name("dup.txt").is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib web`
Expected: FAIL — route absent.

- [ ] **Step 3: Implement the endpoint**

In `src/web.rs`:

```rust
#[derive(Deserialize)]
struct RepackReq { entry_id: i64 }

#[derive(Serialize)]
struct RepackResultDto { removed_entry: String, retained_entries: usize }

async fn api_repack(State(state): State<AppState>, headers: HeaderMap, body: Json<RepackReq>)
    -> Result<Json<RepackResultDto>, (StatusCode, String)>
{
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let rec = cat.get_file(body.entry_id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "no such entry".to_string()))?;
    let mount = state.mounts.resolve(&rec.volume_id)
        .ok_or((StatusCode::CONFLICT, "drive not connected".to_string()))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;
    let out = crate::repack::repack_entry(&cat, &mount, &rec.volume_id, body.entry_id, now)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    Ok(Json(RepackResultDto { removed_entry: out.removed_entry, retained_entries: out.retained_entries }))
}
```

Register `.route("/api/repack", post(api_repack))`.

- [ ] **Step 4: Wire the review page**

In `REVIEW_HTML`, in the `card(m)` renderer, replace the archived note with a repack button for archived non-keep members on a mounted drive, and add a handler. Change the `arch` line and add JS:

```js
  const arch = m.is_loose ? "" :
    (m.mounted ? `<button class="danger repack" data-id="${m.id}">Remove from archive</button>`
               : `<div class="arch">inside archive — drive not connected</div>`);
```

And after `paint()` wiring in `render()`, add:

```js
  for(const b of document.querySelectorAll(".repack")) b.addEventListener("click", async (ev)=>{
    ev.stopPropagation();
    const id=Number(b.dataset.id);
    b.disabled=true; $("#msg").textContent="Repacking archive…";
    try{
      const res=await fetch("/api/repack",{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify({entry_id:id})});
      if(!res.ok){ $("#msg").textContent="Repack error: "+(await res.text()); b.disabled=false; }
      else{ const j=await res.json(); $("#msg").textContent=`Removed '${j.removed_entry}' from its archive (${j.retained_entries} kept). Original saved in _ToDelete.`; idx++; render(); }
    }catch(e){ $("#msg").textContent="Repack error: "+e; b.disabled=false; }
  });
```

Keep the page self-contained (no external URLs) and XSS-safe (button text is static; `j.removed_entry` is inserted via `textContent`, not innerHTML — confirm the message uses `textContent`).

- [ ] **Step 5: Run tests + full suite + release build + manual smoke**

Run: `cargo test --lib web` then `cargo test` then `cargo build --release`.
Then best-effort background smoke: `cargo run -- browse --no-open`, open `/review`, confirm an archived duplicate shows a "Remove from archive" button; do NOT let a blocking run hang. Report what you saw or that you skipped it.

- [ ] **Step 6: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): POST /api/repack + review page 'remove from archive' action

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (§10 Case 4, §11 storage):**
- Identical item inside two different archives → remove from one, keep the other → survivor guard + repack ✓ (Tasks 3–5)
- Pre-check a surviving active copy; never remove the last copy → Task 3 (disk-aware guard) ✓
- Extract the removed item to `_ToDelete` (safety net 1) → Task 3 step 8 ✓
- Build a new archive as a temp; original untouched → Task 2/3 (temp in `_ToDelete/.rebuild`) ✓
- Verify by re-hashing every retained entry; on failure discard temp, original untouched → Task 2 `verify_rebuilt` + Task 3 step 7 ✓
- Atomic swap, original preserved in `_ToDelete/<name>.original.zip` (safety net 2) → Task 3 step 9 ✓
- Record source, removed chain, survivor, both quarantine locations, entry counts, new hash → Task 3 step 10 (`actions_log`) ✓
- Removed entry row → quarantined (as a recoverable loose file); archive row re-hashed → Task 1 + Task 3 step 10 ✓
- Pre-flight free-space; never fill the drive; advise `purge` → Task 3 step 5 ✓
- Opt-in per case (CLI id / GUI button), never automatic → Tasks 4, 5 ✓
- Nested-archive repack refused → Task 3 step 1 ✓

**Safety:** original never modified in place; verify-before-swap; two recovery nets; marker-gated; snapshot-before (CLI) / after (web); reversible-only over HTTP (CSRF-guarded); temp lives in scanner-skipped `_ToDelete`. `purge` remains the sole hard delete.

**Placeholder scan:** no TBD/TODO; every step is runnable. The zip raw-copy API (`by_index_raw`/`raw_copy_file`) may need a minor name adjustment for the resolved zip 2.4.x — the plan flags this and forbids switching to recompression.

**Type consistency:** `RepackOutcome{removed_entry, retained_entries}` consistent across engine/CLI/web; `repack_entry` signature matches all call sites; `archive_entries`/`update_archive_hash`/`mark_quarantined`(now clears chain) used as defined; `available_space` via sysinfo mirrors `volume.rs` capacity logic; `quarantine_dest` mirrors the 2a collision-avoidance (disk + catalog).

**Deferred (logged):** configurable cross-drive scratch location for near-full drives (spec §11) — same-drive build + pre-flight for now; nested-archive repack; recoverable-space accounting for the saved `.original.zip` (tracked in `actions_log`, cleaned by `purge`).

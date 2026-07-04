# Phase 1b — Recursive Archive Scanning — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the scanner to descend into `.zip` archives at any nesting depth, cataloguing every entry (with its full container chain and a streamed BLAKE3 hash) so archived content participates in duplicate detection and search exactly like loose files — with safety limits for depth, zip-bombs, and unreadable/encrypted archives.

**Architecture:** A new `src/archive.rs` module scans a zip from a `Read + Seek` source, yielding `ArchiveEntry` records (hashed leaf files) and non-fatal error notes; it recurses into nested `.zip` entries by buffering them in memory (bounded by the zip-bomb cap) and re-opening them. `src/scanner.rs` calls it after cataloguing an archive file as an ordinary loose file, upserting each entry via a new `upsert_archive_entry` store method. The `files` table's existing `container_chain` column and `idx_files_archived_identity` partial unique index (from Phase 1a) store archive-entry identity; the missing-file sweep is broadened to cover archive entries too.

**Tech Stack:** Rust 1.88, existing deps, plus `zip = "2"` (pure-Rust zip reader/writer; the writer is used only in tests).

## Global Constraints

- **Reliability is paramount — nothing may ever be lost or corrupted.** This phase, like 1a, never deletes/moves/modifies user files. Archives are opened **read-only**; their internals are never rewritten (repacking is Phase 2, spec §10 Case 4).
- **Archive entries are catalogued, never extracted to disk.** Leaf entries are hashed by streaming; a nested archive is buffered **in memory** only, bounded by the zip-bomb cap.
- **The archive file itself is still catalogued as an ordinary loose file** (`container_chain = NULL`), so two identical `.zip` files are ordinary duplicates. Its entries are *additional* rows with `container_chain` set.
- **`container_chain` format:** the internal path from just inside the top-level archive to the entry, with nested-archive boundaries joined by ` › ` (U+203A, space-guillemet-space). Examples: a direct entry `report.pdf` of `old.zip` → `container_chain = "report.pdf"`; `vacation.jpg` inside `photos.zip` inside `old.zip` → `container_chain = "photos.zip › vacation.jpg"`. The archive-entry identity is `(volume_id, relative_path = on-disk path of the top archive, container_chain)`.
- **Safety limits (all logged to `scan_errors`, never silently skipped), configurable:**
  - Max nesting depth (default **8**). Depth 1 = entries directly inside a top-level archive. Descending past the limit logs `max archive depth exceeded` and stops that branch.
  - Zip-bomb caps: skip any entry whose uncompressed size exceeds an **absolute cap** (default **2 GiB**) or whose uncompressed:compressed **ratio** exceeds a cap (default **200**); log `zip bomb: <detail>`.
  - Unreadable/encrypted entry or archive: log `unreadable archive entry: <detail>` (or `password-protected`) and continue.
- **Only `.zip` is handled** (detected by extension, case-insensitive). Other formats (`.rar`, `.7z`, `.tar.gz`) are out of scope — a loose `.rar` is still catalogued as an ordinary file, just not descended into.
- **Timestamps:** archive entries have no meaningful on-disk mtime/ctime/atime — store `NULL` for those; `size_bytes` is the entry's uncompressed size.
- **Git:** work on branch `feat/phase1b-archives` off `main`. Conventional Commits, scope `archive`/`scanner`/`catalog`/`cli`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (already merged in Phase 1a):** `Catalog` (+ `conn`, store methods), `models::{NewFile, FileRecord, Category, FileStatus}`, `hashing::{hash_reader, hash_file}`, `scanner::scan_volume`, `config::Config`. **Out of scope:** near-duplicate detection, any quarantine/delete/repack (Case 1–4), the web browse screen — later plans.

---

## File Structure

- `Cargo.toml` — add `zip = "2"` dependency.
- `src/config.rs` — add archive-limit fields to `Config` (`max_archive_depth`, `archive_entry_max_bytes`, `archive_ratio_cap`).
- `src/archive.rs` — **new module**: `ArchiveLimits`, `ArchiveEntry`, `ArchiveScanResult`, `is_archive_name`, and `scan_archive` (recursive). Registered in `src/lib.rs`.
- `src/catalog/store.rs` — add `upsert_archive_entry`, `touch_archive_entries`; broaden `mark_missing_scanned` to cover archive entries.
- `src/scanner.rs` — after hashing a loose file, if it is an archive, descend and upsert entries; on incremental skip of an unchanged archive, refresh its entries' `last_seen`.
- `src/commands.rs` — `cmd_search` displays the container chain for archived hits.
- `src/lib.rs` — add `pub mod archive;`.
- `tests/archive_scan.rs` — **new** end-to-end integration test: scan a directory containing a real nested zip, search an inner filename.

---

### Task 1: Config limits + archive dependency + name helper

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/config.rs`
- Create: `src/archive.rs` (minimal: `is_archive_name` + limit struct)
- Modify: `src/lib.rs` (register `pub mod archive;`)

**Interfaces:**
- Produces:
  - `Config` gains `pub max_archive_depth: usize`, `pub archive_entry_max_bytes: u64`, `pub archive_ratio_cap: u64` (defaults 8, `2 * 1024 * 1024 * 1024`, 200).
  - `archive::is_archive_name(name: &str) -> bool` — true iff `name` ends with `.zip` (case-insensitive).
  - `archive::ArchiveLimits { pub max_depth: usize, pub entry_max_bytes: u64, pub ratio_cap: u64 }` with `impl ArchiveLimits { pub fn from_config(cfg: &crate::config::Config) -> ArchiveLimits }`.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, under `[dependencies]`, add:

```toml
zip = "2"
```

- [ ] **Step 2: Register the module**

In `src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod archive;
```

- [ ] **Step 3: Write failing tests**

Create `src/archive.rs`:

```rust
//! Reading into zip archives (recursively) to catalog their contents.

use crate::config::Config;

/// Tunable safety limits for archive descent.
#[derive(Debug, Clone)]
pub struct ArchiveLimits {
    pub max_depth: usize,
    pub entry_max_bytes: u64,
    pub ratio_cap: u64,
}

impl ArchiveLimits {
    pub fn from_config(cfg: &Config) -> ArchiveLimits {
        ArchiveLimits {
            max_depth: cfg.max_archive_depth,
            entry_max_bytes: cfg.archive_entry_max_bytes,
            ratio_cap: cfg.archive_ratio_cap,
        }
    }
}

/// True if `name` looks like a zip archive (by extension, case-insensitive).
pub fn is_archive_name(name: &str) -> bool {
    name.rsplit('.').next().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false)
        && name.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zip_names() {
        assert!(is_archive_name("old.zip"));
        assert!(is_archive_name("OLD.ZIP"));
        assert!(is_archive_name("a.b.Zip"));
        assert!(!is_archive_name("notes.txt"));
        assert!(!is_archive_name("zip")); // no extension dot
        assert!(!is_archive_name("archive.zipx"));
    }

    #[test]
    fn limits_from_config() {
        let cfg = Config::default_paths().unwrap();
        let l = ArchiveLimits::from_config(&cfg);
        assert_eq!(l.max_depth, 8);
        assert_eq!(l.entry_max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(l.ratio_cap, 200);
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --lib archive`
Expected: FAIL — `Config` has no `max_archive_depth` field yet.

- [ ] **Step 5: Add the config fields**

In `src/config.rs`, add the three fields to `struct Config`:

```rust
pub struct Config {
    pub catalog_path: PathBuf,
    pub snapshot_retention: usize,
    pub max_archive_depth: usize,
    pub archive_entry_max_bytes: u64,
    pub archive_ratio_cap: u64,
}
```

And set them in **both** return sites of `default_paths` (the env-override branch and the normal branch). Add these three fields to each `Config { ... }` literal:

```rust
            max_archive_depth: 8,
            archive_entry_max_bytes: 2 * 1024 * 1024 * 1024,
            archive_ratio_cap: 200,
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib archive config`
Expected: PASS (archive + config tests). Then `cargo build`.

- [ ] **Step 7: Commit**

```bash
git checkout -b feat/phase1b-archives   # only if not already on it
git add Cargo.toml Cargo.lock src/config.rs src/archive.rs src/lib.rs
git commit -m "feat(archive): add zip dep, archive limits config, name detection

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Scan a flat archive (leaf entries, hashing, zip-bomb caps)

**Files:**
- Modify: `src/archive.rs`
- Test: inline `#[cfg(test)]` in `src/archive.rs` (build test zips with `zip::ZipWriter`)

**Interfaces:**
- Consumes: `hashing::hash_reader`, `ArchiveLimits`.
- Produces:
  - `pub struct ArchiveEntry { pub container_chain: String, pub filename: String, pub extension: String, pub size_bytes: i64, pub content_hash: String }`
  - `pub struct ArchiveScanResult { pub entries: Vec<ArchiveEntry>, pub errors: Vec<(String, String)> }` — each error is `(internal_path_context, reason)`.
  - `pub fn scan_archive<R: std::io::Read + std::io::Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult` — scans ONE archive level (no nested descent yet — that's Task 3). Never returns `Err`; archive-open failure is recorded as an error note with context `""`.

Note on the `zip` crate API: this plan targets `zip = "2"`. The exact method names below (`ZipArchive::new`, `.len()`, `.by_index(i)`, `ZipFile::name/is_file/size/compressed_size`, `Read` impl) are correct for zip 2.x, but if the resolved 2.x patch differs, adapt minimally to the same behavior and note it in the report — do NOT change the hashing (`hashing::hash_reader`) or the `container_chain`/limit semantics.

- [ ] **Step 1: Write failing tests**

Add to `src/archive.rs`:

```rust
use std::io::{Read, Seek};
use crate::hashing;

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub container_chain: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
}

#[derive(Debug, Default)]
pub struct ArchiveScanResult {
    pub entries: Vec<ArchiveEntry>,
    pub errors: Vec<(String, String)>,
}
```

Add these tests inside the existing `mod tests`:

```rust
    use std::io::{Cursor, Write};

    // Build an in-memory zip: Vec of (name, bytes).
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, bytes) in files {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits { max_depth: 8, entry_max_bytes: 2 * 1024 * 1024 * 1024, ratio_cap: 200 }
    }

    #[test]
    fn hashes_flat_entries() {
        let zip = make_zip(&[("a.txt", b"alpha"), ("dir/b.pdf", b"beta")]);
        let res = scan_archive(Cursor::new(zip), &limits());
        assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
        assert_eq!(res.entries.len(), 2);
        let a = res.entries.iter().find(|e| e.filename == "a.txt").unwrap();
        // hash matches hashing::hash_reader over the same bytes
        let mut raw: &[u8] = b"alpha";
        assert_eq!(a.content_hash, hashing::hash_reader(&mut raw).unwrap());
        assert_eq!(a.container_chain, "a.txt");
        assert_eq!(a.size_bytes, 5);
        let b = res.entries.iter().find(|e| e.filename == "b.pdf").unwrap();
        assert_eq!(b.container_chain, "dir/b.pdf");
        assert_eq!(b.extension, "pdf");
    }

    #[test]
    fn rejects_oversized_entry() {
        // entry_max_bytes tiny -> the entry is skipped and logged, not hashed.
        let zip = make_zip(&[("big.bin", b"0123456789")]);
        let small = ArchiveLimits { max_depth: 8, entry_max_bytes: 4, ratio_cap: 200 };
        let res = scan_archive(Cursor::new(zip), &small);
        assert!(res.entries.is_empty());
        assert_eq!(res.errors.len(), 1);
        assert!(res.errors[0].1.contains("zip bomb"), "reason: {}", res.errors[0].1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib archive`
Expected: FAIL — `scan_archive` not found.

- [ ] **Step 3: Implement `scan_archive` (flat)**

Add to `src/archive.rs`:

```rust
/// Extension (lowercased, no dot) of an internal entry name, or "" if none.
fn entry_extension(name: &str) -> String {
    let leaf = name.rsplit('/').next().unwrap_or(name);
    match leaf.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Scan ONE archive level from a seekable reader. Leaf files are hashed; entries exceeding the
/// zip-bomb caps are skipped with an error note. Nested archives are NOT descended here (Task 3).
pub fn scan_archive<R: Read + Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult {
    let mut result = ArchiveScanResult::default();
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            result.errors.push((String::new(), format!("unreadable archive: {e}")));
            return result;
        }
    };

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                result.errors.push((format!("#{i}"), format!("unreadable archive entry: {e}")));
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let uncompressed = entry.size();
        let compressed = entry.compressed_size().max(1);

        // Zip-bomb guards (declared sizes).
        if uncompressed > limits.entry_max_bytes {
            result.errors.push((name.clone(),
                format!("zip bomb: {uncompressed} bytes exceeds cap {}", limits.entry_max_bytes)));
            continue;
        }
        if uncompressed / compressed > limits.ratio_cap {
            result.errors.push((name.clone(),
                format!("zip bomb: ratio {} exceeds cap {}", uncompressed / compressed, limits.ratio_cap)));
            continue;
        }

        let filename = name.rsplit('/').next().unwrap_or(&name).to_string();
        let extension = entry_extension(&name);
        let content_hash = match hashing::hash_reader(&mut entry) {
            Ok(h) => h,
            Err(e) => {
                result.errors.push((name.clone(), format!("read error: {e}")));
                continue;
            }
        };
        result.entries.push(ArchiveEntry {
            container_chain: name,
            filename,
            extension,
            size_bytes: uncompressed as i64,
            content_hash,
        });
    }

    result
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib archive`
Expected: PASS. Then `cargo test` (no regressions).

- [ ] **Step 5: Commit**

```bash
git add src/archive.rs
git commit -m "feat(archive): scan flat zip entries with streamed hashing and zip-bomb caps

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Recurse into nested archives (depth limit)

**Files:**
- Modify: `src/archive.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Changes `scan_archive` to descend into entries whose name `is_archive_name`, prefixing their entries' `container_chain` with the parent chain joined by ` › `. Adds a private recursive helper carrying `depth` and a `chain_prefix`.
- The public signature stays `pub fn scan_archive<R: Read + Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult` (depth starts at 1 internally).

- [ ] **Step 1: Write failing tests**

Add to `mod tests`:

```rust
    // Wrap an existing zip's bytes as a single entry inside an outer zip.
    fn nest_zip(inner_name: &str, inner_zip: Vec<u8>, alongside: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file(inner_name, opts).unwrap();
            zw.write_all(&inner_zip).unwrap();
            for (name, bytes) in alongside {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn descends_into_nested_archive() {
        let inner = make_zip(&[("vacation.jpg", b"pixels")]);
        let outer = nest_zip("photos.zip", inner, &[("readme.txt", b"hi")]);
        let res = scan_archive(Cursor::new(outer), &limits());
        assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
        // readme.txt (direct) + vacation.jpg (nested); the inner photos.zip itself is also an entry
        let jpg = res.entries.iter().find(|e| e.filename == "vacation.jpg").unwrap();
        assert_eq!(jpg.container_chain, "photos.zip › vacation.jpg");
        assert!(res.entries.iter().any(|e| e.container_chain == "readme.txt"));
        // the nested archive is itself catalogued as an entry (an identical inner zip is a dup)
        assert!(res.entries.iter().any(|e| e.container_chain == "photos.zip"));
    }

    #[test]
    fn stops_at_max_depth() {
        let inner = make_zip(&[("deep.txt", b"x")]);
        let outer = nest_zip("mid.zip", inner, &[]);
        // max_depth = 1: the top archive's direct entries are scanned, but mid.zip is not descended.
        let shallow = ArchiveLimits { max_depth: 1, entry_max_bytes: 2 * 1024 * 1024 * 1024, ratio_cap: 200 };
        let res = scan_archive(Cursor::new(outer), &shallow);
        assert!(res.entries.iter().any(|e| e.container_chain == "mid.zip")); // still catalogued as a file
        assert!(!res.entries.iter().any(|e| e.filename == "deep.txt")); // not descended
        assert!(res.errors.iter().any(|(_, r)| r.contains("max archive depth")));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib archive`
Expected: FAIL — nested entries not produced; no depth error.

- [ ] **Step 3: Implement recursion**

Replace the body of `scan_archive` with a thin wrapper over a recursive helper, and add the helper. Replace the whole `pub fn scan_archive` from Task 2 with:

```rust
/// Join a parent chain and a child name with the guillemet separator.
fn join_chain(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_string() } else { format!("{prefix} › {name}") }
}

/// Read up to `cap` bytes; `Err` if the stream exceeds `cap` (bomb guard for buffering).
fn read_capped<R: Read>(mut reader: R, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut limited = (&mut reader).take(cap + 1);
    limited.read_to_end(&mut buf).map_err(|e| format!("read error: {e}"))?;
    if buf.len() as u64 > cap {
        return Err(format!("zip bomb: nested archive exceeds cap {cap}"));
    }
    Ok(buf)
}

pub fn scan_archive<R: Read + Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult {
    let mut result = ArchiveScanResult::default();
    scan_level(reader, "", 1, limits, &mut result);
    result
}

/// Scan one archive level. `chain_prefix` is the container chain of THIS archive ("" at top level);
/// `depth` is 1 for a top-level archive. Recurses into nested `.zip` entries until `max_depth`.
fn scan_level<R: Read + Seek>(reader: R, chain_prefix: &str, depth: usize,
    limits: &ArchiveLimits, result: &mut ArchiveScanResult)
{
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            result.errors.push((chain_prefix.to_string(), format!("unreadable archive: {e}")));
            return;
        }
    };

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                result.errors.push((join_chain(chain_prefix, &format!("#{i}")),
                    format!("unreadable archive entry: {e}")));
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let chain = join_chain(chain_prefix, &name);
        let uncompressed = entry.size();
        let compressed = entry.compressed_size().max(1);

        if uncompressed > limits.entry_max_bytes {
            result.errors.push((chain,
                format!("zip bomb: {uncompressed} bytes exceeds cap {}", limits.entry_max_bytes)));
            continue;
        }
        if uncompressed / compressed > limits.ratio_cap {
            result.errors.push((chain,
                format!("zip bomb: ratio {} exceeds cap {}", uncompressed / compressed, limits.ratio_cap)));
            continue;
        }

        let filename = name.rsplit('/').next().unwrap_or(&name).to_string();
        let extension = entry_extension(&name);

        if is_archive_name(&name) {
            // Nested archive: buffer it (bounded) so we can BOTH hash it and re-open it with Seek
            // to recurse. Only archives are buffered — large leaf files stream (see else branch).
            let bytes = match read_capped(&mut entry, limits.entry_max_bytes) {
                Ok(b) => b,
                Err(reason) => { result.errors.push((chain, reason)); continue; }
            };
            let mut slice: &[u8] = &bytes;
            let content_hash = match hashing::hash_reader(&mut slice) {
                Ok(h) => h,
                Err(e) => { result.errors.push((chain, format!("read error: {e}"))); continue; }
            };
            result.entries.push(ArchiveEntry {
                container_chain: chain.clone(), filename, extension,
                size_bytes: uncompressed as i64, content_hash,
            });
            if depth >= limits.max_depth {
                result.errors.push((chain, format!("max archive depth exceeded ({} levels)", limits.max_depth)));
                continue;
            }
            scan_level(std::io::Cursor::new(bytes), &chain, depth + 1, limits, result);
        } else {
            // Leaf file: stream-hash directly, never buffering the whole entry into memory.
            let content_hash = match hashing::hash_reader(&mut entry) {
                Ok(h) => h,
                Err(e) => { result.errors.push((chain, format!("read error: {e}"))); continue; }
            };
            result.entries.push(ArchiveEntry {
                container_chain: chain, filename, extension,
                size_bytes: uncompressed as i64, content_hash,
            });
        }
    }
}
```

Delete the now-unused flat `scan_archive` body and the standalone leaf-only hashing path from Task 2 (the recursive `scan_level` replaces it). Keep `entry_extension`, `is_archive_name`, `ArchiveLimits`, the structs, and all tests.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib archive`
Expected: PASS (flat tests from Task 2 still pass — a flat archive is just depth-1 with no nested entries; plus the two new nested/depth tests). Then `cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/archive.rs
git commit -m "feat(archive): recurse into nested zips with depth limit and bomb-safe buffering

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Store — upsert archive entries, touch, broaden missing sweep

**Files:**
- Modify: `src/catalog/store.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces (methods on `Catalog`):
  - `pub fn upsert_archive_entry(&self, volume_id: &str, archive_rel_path: &str, e: &crate::archive::ArchiveEntry, now: i64) -> anyhow::Result<()>` — inserts/updates a row with `container_chain = e.container_chain`, targeting the `idx_files_archived_identity` partial index.
  - `pub fn touch_archive_entries(&self, volume_id: &str, archive_rel_path: &str, now: i64) -> anyhow::Result<usize>` — refresh `last_seen_at`/`status='active'` for all entries whose `relative_path = archive_rel_path AND container_chain IS NOT NULL`. Returns count.
  - `mark_missing_scanned` broadened to also sweep archive entries (drop the `container_chain IS NULL` restriction).

- [ ] **Step 1: Write failing tests**

Add to `store.rs` `mod tests`:

```rust
    use crate::archive::ArchiveEntry;

    fn mk_entry(chain: &str, hash: &str) -> ArchiveEntry {
        ArchiveEntry {
            container_chain: chain.into(),
            filename: chain.rsplit(['/', '›']).next().unwrap().trim().into(),
            extension: "jpg".into(),
            size_bytes: 42,
            content_hash: hash.into(),
        }
    }

    #[test]
    fn archive_entry_upsert_is_idempotent_and_searchable() {
        let (_t, cat) = open_tmp();
        let e = mk_entry("photos.zip › vacation.jpg", "h-vac");
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 200).unwrap();
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 250).unwrap(); // same identity again
        let hits = cat.search("vacation", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].container_chain.as_deref(), Some("photos.zip › vacation.jpg"));
        assert_eq!(hits[0].relative_path, "backups/old.zip");
    }

    #[test]
    fn archive_entry_dedupes_against_loose_file_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "loose/vacation.jpg", "same"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("vacation.jpg", "same"), 200).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1); // loose + archived share a hash
    }

    #[test]
    fn missing_sweep_covers_archive_entries() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("gone.jpg", "h1"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 200).unwrap();
        // rescan at 300 re-sees only kept.jpg
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 300).unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
    }

    #[test]
    fn touch_archive_entries_refreshes_all_under_archive() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("b.jpg", "h2"), 200).unwrap();
        let touched = cat.touch_archive_entries("vol-1", "old.zip", 300).unwrap();
        assert_eq!(touched, 2);
        // after touch, a later sweep starting at 300 does NOT mark them missing
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store`
Expected: FAIL — `upsert_archive_entry`/`touch_archive_entries` not found.

- [ ] **Step 3: Implement the methods**

Add inside `impl Catalog` in `store.rs`:

```rust
    /// Insert/update one archive entry (a file inside an archive). Identity is
    /// (volume_id, archive_rel_path, container_chain) via idx_files_archived_identity.
    pub fn upsert_archive_entry(&self, volume_id: &str, archive_rel_path: &str,
        e: &crate::archive::ArchiveEntry, now: i64) -> anyhow::Result<()>
    {
        self.conn.execute(
            "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                 content_hash, created_time, modified_time, accessed_time, category,
                 container_chain, status, first_seen_at, last_seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,NULL,?7,?8,'active',?9,?9)
             ON CONFLICT(volume_id, relative_path, container_chain)
                 WHERE container_chain IS NOT NULL DO UPDATE SET
                 filename=excluded.filename, extension=excluded.extension,
                 size_bytes=excluded.size_bytes, content_hash=excluded.content_hash,
                 category=excluded.category, status='active', last_seen_at=excluded.last_seen_at",
            params![volume_id, archive_rel_path, e.filename, e.extension, e.size_bytes,
                e.content_hash, Category::from_extension(&e.extension).as_str(), e.container_chain, now],
        )?;
        Ok(())
    }

    /// Refresh last_seen/status for every archive entry under one archive file (unchanged-archive skip).
    pub fn touch_archive_entries(&self, volume_id: &str, archive_rel_path: &str, now: i64)
        -> anyhow::Result<usize>
    {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NOT NULL",
            params![volume_id, archive_rel_path, now],
        )?;
        Ok(n)
    }
```

And broaden `mark_missing_scanned` — replace its SQL body with (drop the `container_chain IS NULL` clause so BOTH loose files and archive entries are swept):

```rust
    pub fn mark_missing_scanned(&self, volume_id: &str, scan_started_at: i64, _now: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "UPDATE files SET status='missing'
             WHERE volume_id=?1 AND status='active' AND last_seen_at < ?2",
            params![volume_id, scan_started_at],
        )?;
        Ok(n)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib store`
Expected: PASS (new tests + existing `mark_missing_flags_files_not_seen_this_scan` still passes — loose-only case is a subset). Then `cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs
git commit -m "feat(catalog): upsert/touch archive entries; sweep them in missing pass

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Scanner integration — descend archives during a scan

**Files:**
- Modify: `src/scanner.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `archive::{scan_archive, ArchiveLimits, is_archive_name}`, `catalog` archive methods.
- `scan_volume` signature UNCHANGED. Add a private helper `descend_archive(cat, path, rel, identity, limits, now, summary, in_batch)` that opens the on-disk archive file, runs `scan_archive`, upserts each entry, logs each error (prefixing the internal context with the archive's `rel`), and advances the batch counter per entry.
- `ScanSummary` gains `pub archive_entries: usize` (count of archive entries hashed/updated this pass).
- `scan_volume` needs access to limits: change `scan_volume` to build `ArchiveLimits` from a `Config`. To avoid a signature change, load limits with `ArchiveLimits::from_config(&Config::default_paths()?)` at the top of `scan_volume`. (The scanner already implicitly depends on default config via the caller; this keeps the test callable without extra args.)

- [ ] **Step 1: Write failing tests**

Add to `scanner.rs` `mod tests` (uses `zip` as a dev-dependency — already available since Task 1 added it as a normal dep):

```rust
    use std::io::Write as _;

    fn write_zip_file(path: &std::path::Path, files: &[(&str, &[u8])]) {
        let f = fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn scan_catalogs_archive_entries() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        write_zip_file(&root.join("photos.zip"), &[("trip/beach.jpg", b"sand"), ("note.txt", b"hi")]);

        let s = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        // the zip file itself is a loose hashed file
        assert_eq!(s.hashed, 1);
        // its two entries are catalogued
        assert_eq!(s.archive_entries, 2);
        // inner file is searchable, with its container chain
        let hits = cat.search("beach", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relative_path, "photos.zip");
        assert_eq!(hits[0].container_chain.as_deref(), Some("trip/beach.jpg"));
    }

    #[test]
    fn unchanged_archive_entries_survive_rescan() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        write_zip_file(&root.join("a.zip"), &[("x.txt", b"one")]);
        scan_volume(&cat, &root, &ident(), false, 100).unwrap();

        // rescan unchanged: archive is skipped, but its entry must NOT be swept to missing
        let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s.marked_missing, 0);
        assert_eq!(cat.search("x", None, None, Some("active")).unwrap().len(), 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib scanner`
Expected: FAIL — `archive_entries` field / descent not present.

- [ ] **Step 3: Implement descent + incremental touch**

In `src/scanner.rs`:

1. Add imports at top:

```rust
use crate::archive::{self, ArchiveLimits};
use crate::config::Config;
```

2. Add the field to `ScanSummary`:

```rust
    pub archive_entries: usize,
```

3. At the very top of `scan_volume` (after `let scan_started_at = now;`), build limits:

```rust
    let limits = ArchiveLimits::from_config(&Config::default_paths()?);
```

4. In the incremental-skip branch (where an unchanged loose file is `touch_seen` and `continue`d), before `continue`, refresh archive entries if this file is an archive:

```rust
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    if archive::is_archive_name(&rel) {
                        cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                    }
                    summary.skipped += 1;
                    in_batch += 1;
                    rotate_batch(cat, &mut in_batch)?;
                    continue;
                }
```

5. After the archive FILE is upserted (`cat.upsert_file(&nf, now)?; summary.hashed += 1; ...`) but before the `rotate_batch` at the end of the loop body, descend if it is an archive:

```rust
        cat.upsert_file(&nf, now)?;
        summary.hashed += 1;
        in_batch += 1;
        rotate_batch(cat, &mut in_batch)?;

        if archive::is_archive_name(&rel) {
            descend_archive(cat, path, &rel, identity, &limits, now, &mut summary, &mut in_batch)?;
        }
```

6. Add the helper function (module level, below `scan_volume`):

```rust
/// Open an on-disk archive file, catalog each entry, and log each non-fatal error.
fn descend_archive(
    cat: &Catalog, path: &Path, rel: &str, identity: &VolumeIdentity,
    limits: &ArchiveLimits, now: i64, summary: &mut ScanSummary, in_batch: &mut usize,
) -> anyhow::Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            cat.log_scan_error(Some(&identity.volume_id), rel, &format!("archive open: {e}"), now)?;
            summary.errors += 1;
            return Ok(());
        }
    };
    let res = archive::scan_archive(file, limits);
    for entry in &res.entries {
        cat.upsert_archive_entry(&identity.volume_id, rel, entry, now)?;
        summary.archive_entries += 1;
        *in_batch += 1;
        rotate_batch(cat, in_batch)?;
    }
    for (ctx, reason) in &res.errors {
        let where_ = if ctx.is_empty() { rel.to_string() } else { format!("{rel} › {ctx}") };
        cat.log_scan_error(Some(&identity.volume_id), &where_, reason, now)?;
        summary.errors += 1;
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib scanner`
Expected: PASS. Then `cargo test` (all pass).

- [ ] **Step 5: Commit**

```bash
git add src/scanner.rs
git commit -m "feat(scanner): descend into archives during scan, touch entries on skip

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: CLI display of archived hits + end-to-end integration test

**Files:**
- Modify: `src/commands.rs`
- Create: `tests/archive_scan.rs`

**Interfaces:**
- `cmd_search` prints the container chain for archived hits so a user can see *where inside* an archive a match lives.

- [ ] **Step 1: Write the failing integration test**

Create `tests/archive_scan.rs`:

```rust
use std::io::Write;
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_cleanupstorages")) }

fn write_zip(path: &std::path::Path, files: &[(&str, &[u8])]) {
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

#[test]
fn scans_archive_and_finds_inner_file() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    write_zip(&drive.join("memories.zip"), &[("2019/thesis_backup.pdf", b"important")]);
    let data = tmp.path().join("appdata");

    let scan = bin().env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("scan").arg(&drive).arg("--readonly-fallback").arg("fingerprint")
        .output().unwrap();
    assert!(scan.status.success(), "scan failed: {}", String::from_utf8_lossy(&scan.stderr));

    let search = bin().env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("search").arg("thesis_backup").output().unwrap();
    assert!(search.status.success());
    let out = String::from_utf8_lossy(&search.stdout);
    assert!(out.contains("memories.zip"), "output: {out}");
    assert!(out.contains("2019/thesis_backup.pdf"), "expected container chain in output: {out}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test archive_scan`
Expected: FAIL — search output lacks the container chain (current `cmd_search` doesn't print it).

- [ ] **Step 3: Update `cmd_search` display**

In `src/commands.rs`, in `cmd_search`, where each hit is printed, append the container chain when present. Find the existing print loop and change the per-hit line to include the chain. Replace the existing hit-print block with:

```rust
    for f in &hits {
        let flag = match f.status {
            FileStatus::Active => "",
            FileStatus::Missing => "  [MISSING]",
            FileStatus::Quarantined => "  [QUARANTINED]",
            FileStatus::Purged => "  [PURGED]",
        };
        let location = match &f.container_chain {
            Some(chain) => format!("{} › {}", f.relative_path, chain),
            None => f.relative_path.clone(),
        };
        println!("{}  [{}]  {}  ({} bytes){}",
            location, f.volume_id, f.category.as_str(), f.size_bytes, flag);
    }
```

(If the existing loop differs slightly, preserve its `flag`/stats format and only add the `location` composition.)

- [ ] **Step 4: Run the integration test**

Run: `cargo test --test archive_scan`
Expected: PASS.

- [ ] **Step 5: Full suite + release build + manual smoke**

Run: `cargo test` then `cargo build --release`
Then manually:
```bash
# create a small zip and scan it
cargo run -- scan ./ --readonly-fallback fingerprint
cargo run -- search Cargo
```
Report the observed scan counts (should now include an "archive entries" figure if any `.zip` is present under `./`).

- [ ] **Step 6: Report scan summary line**

Ensure `cmd_scan`'s completion line includes the archive-entry count. In `src/commands.rs`, update the "Done:" line to also print `s.archive_entries` (e.g. `... {} archive entries ...`). Then re-run `cargo build`.

- [ ] **Step 7: Commit**

```bash
git add src/commands.rs tests/archive_scan.rs
git commit -m "feat(cli): show archive container chain in search and scan summary

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (§9):**
- Descend into archives at any nesting depth → Task 3 (`scan_level` recursion) ✓
- Each entry catalogued with full `container_chain`, streamed hash, participates in dup matching → Tasks 2–5 ✓
- Whole archive still catalogued as an ordinary file (identical zips are dups) → unchanged loose-file path + Task 5 ✓
- Max depth (default 8), logged → Tasks 1, 3 ✓
- Zip-bomb protection (absolute + ratio caps), logged → Tasks 1, 2, 3 ✓
- Unreadable/encrypted entry logged and skipped → Tasks 2, 3 ✓
- Nothing extracted to disk (leaf streamed, nested buffered in memory bounded by cap) → Task 3 ✓
- Search/status surface archived content → Tasks 4 (search), 6 (display); dup count already spans all rows ✓
- No repack / no deletion of archive internals (Phase 2) → not implemented here ✓

**Placeholder scan:** No TBD/TODO; every step has runnable code + exact commands. ✓

**Type consistency:** `ArchiveEntry`/`ArchiveScanResult`/`ArchiveLimits` fields, `scan_archive` signature (`Read + Seek`), `upsert_archive_entry`/`touch_archive_entries` signatures, `ScanSummary.archive_entries`, and the `Config` field names all match across tasks. The archive-entry upsert targets `idx_files_archived_identity` (created in Phase 1a) via `ON CONFLICT(volume_id, relative_path, container_chain) WHERE container_chain IS NOT NULL`. ✓

**Known follow-ups (not this plan):** re-hashing before Phase-2 destructive actions (empty-file dup collapse, mtime-1s staleness) — already logged in `docs/future-ideas.md`; a directory-level unreadable subtree still leaves prior archive entries under it swept to `missing` on the next scan (same limitation as loose files).

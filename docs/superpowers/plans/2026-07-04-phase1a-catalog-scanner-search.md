# Phase 1a — Catalog + Loose-File Scanner + CLI Search — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working, reliable Rust CLI that scans a mounted drive, catalogs every loose file (metadata + BLAKE3 hash) into a durable SQLite register, and lets the user search the register and see duplicate/status stats — even for drives not currently plugged in.

**Architecture:** One Rust binary (`cleanupstorages`) with a `clap` CLI dispatching to `scan`, `search`, and `status`. A `catalog` module owns the SQLite database (WAL mode, FTS index, timestamped snapshots). A `scanner` module walks the filesystem, hashes new/changed files incrementally, and upserts records; files seen before but now absent are marked `missing` (never deleted from the catalog). Volume identity is a hidden marker UUID on the drive root, with a fingerprint fallback for read-only drives.

**Tech Stack:** Rust 1.88, `rusqlite` (bundled SQLite), `blake3`, `walkdir`, `clap` (derive), `uuid`, `serde`/`serde_json`, `directories`, `sysinfo`, `anyhow`, `thiserror`.

## Global Constraints

- **Reliability is paramount — nothing may ever be lost or corrupted.** The catalog is append-mostly: file rows are never deleted, only their `status` changes (`active`/`missing`/`quarantined`/`purged`). This plan (Phase 1a) never deletes, moves, or modifies any user file — it is read-only against user data except for the single hidden marker file on the drive root.
- **Catalog never lives on the HDDs.** Default catalog path is the OS app-data dir on the computer.
- **SQLite runs in WAL mode**; a timestamped snapshot is taken after each scan; keep last N=10 snapshots.
- **Hashing is BLAKE3, streamed in 64 KiB chunks** — never load a whole file into memory.
- **Scans are incremental by default** (skip re-hash when size+modified-time unchanged) and **resumable** (commit in batches of 200 files). `--force` re-hashes everything.
- **Non-fatal file errors** (permission denied, unreadable) are logged to the `scan_errors` table and the scan continues.
- **Timestamps** are stored as `i64` Unix seconds (UTC). Missing OS timestamps store `NULL`.
- **Git:** trunk-based on `main`; do this plan's work on branch `feat/phase1a-catalog-scanner`. Conventional Commits, scopes from CONTRIBUTING.md (`scanner`, `catalog`, `cli`, etc.). Sign-off line `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` on each commit.

**Scope note — deferred to later plans (NOT in this plan):** recursive zip/archive scanning (Plan 1b), the web browse/search screen (Plan 1c), and all of Phase 2 (review GUI, quarantine, purge, Case 1–4 workflows). Do not implement them here. The `container_chain` column is created now so archives slot in later without a migration, but no code writes it in this plan.

---

## File Structure

- `Cargo.toml` — crate manifest + dependencies.
- `src/main.rs` — entry point; `clap` CLI definition; dispatch to command handlers.
- `src/config.rs` — `Config` struct, defaults, load/save, resolved catalog/data-dir paths.
- `src/hashing.rs` — streamed BLAKE3 hashing of any `Read`er and of a file path.
- `src/category.rs` — extension → `Category` mapping.
- `src/catalog/mod.rs` — re-exports; `Catalog` handle wrapping the connection.
- `src/catalog/models.rs` — `Volume`, `FileRecord`, `FileStatus`, `Category` (re-exported), `ScanError` row types.
- `src/catalog/schema.rs` — open connection, apply pragmas (WAL), create tables + FTS, `integrity_check`.
- `src/catalog/store.rs` — upsert volume, upsert file, mark-missing sweep, duplicate groups, search, stats, log scan error.
- `src/catalog/backup.rs` — snapshot the DB file, prune to last N.
- `src/volume.rs` — resolve a drive's `volume_id` via marker file, with fingerprint fallback.
- `src/scanner.rs` — orchestrate a scan of one mounted path (walk, incremental hash, batch upsert, mark-missing).
- `src/commands.rs` — `cmd_scan`, `cmd_search`, `cmd_status` handlers wiring everything together.
- `tests/scan_and_search.rs` — end-to-end integration test over a temp directory.

---

### Task 1: Project scaffolding + CLI skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

**Interfaces:**
- Produces: a compiling binary `cleanupstorages` with subcommands `scan`, `search`, `status` (handlers stubbed to print and exit 0). Later tasks replace the stub bodies.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "cleanupstorages"
version = "0.1.0"
edition = "2021"
description = "Reliable catalog + deduplication tool for messy external drives"

[[bin]]
name = "cleanupstorages"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
rusqlite = { version = "0.31", features = ["bundled", "backup"] }
blake3 = "1"
walkdir = "2"
uuid = { version = "1", features = ["v4"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
directories = "5"
sysinfo = "0.30"
anyhow = "1"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create `src/main.rs` with the CLI skeleton**

```rust
mod config;
mod hashing;
mod category;
mod catalog;
mod volume;
mod scanner;
mod commands;

use clap::{Parser, Subcommand};

/// Reliable catalog + deduplication tool for messy external drives.
#[derive(Parser)]
#[command(name = "cleanupstorages", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a mounted drive/path, hash files, and update the catalog.
    Scan {
        /// Path to the mounted drive or directory to scan.
        path: std::path::PathBuf,
        /// Re-hash every file, ignoring the incremental skip fast-path.
        #[arg(long)]
        force: bool,
        /// How to handle read-only drives where the marker cannot be written.
        #[arg(long, value_enum, default_value = "ask")]
        readonly_fallback: commands::ReadonlyFallback,
    },
    /// Search the catalog for files by name/path.
    Search {
        /// Free-text query matched against filename and path.
        query: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        volume: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Show catalog statistics (files, duplicate groups, per-volume totals).
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, force, readonly_fallback } => {
            commands::cmd_scan(&path, force, readonly_fallback)
        }
        Command::Search { query, category, volume, status } => {
            commands::cmd_search(&query, category.as_deref(), volume.as_deref(), status.as_deref())
        }
        Command::Status => commands::cmd_status(),
    }
}
```

- [ ] **Step 3: Create minimal stubs so it compiles**

Create each module file with just enough to compile. `src/commands.rs`:

```rust
use std::path::Path;
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadonlyFallback { Ask, Fingerprint, Skip }

pub fn cmd_scan(_path: &Path, _force: bool, _fallback: ReadonlyFallback) -> anyhow::Result<()> {
    println!("scan: not yet implemented");
    Ok(())
}

pub fn cmd_search(
    _query: &str, _category: Option<&str>, _volume: Option<&str>, _status: Option<&str>,
) -> anyhow::Result<()> {
    println!("search: not yet implemented");
    Ok(())
}

pub fn cmd_status() -> anyhow::Result<()> {
    println!("status: not yet implemented");
    Ok(())
}
```

Create empty-ish stubs: `src/config.rs`, `src/hashing.rs`, `src/category.rs`, `src/volume.rs`, `src/scanner.rs` each containing `// filled in later` and any `pub` item later tasks need (leave empty for now). Create `src/catalog/mod.rs` with:

```rust
pub mod models;
pub mod schema;
pub mod store;
pub mod backup;
```

and create `src/catalog/models.rs`, `src/catalog/schema.rs`, `src/catalog/store.rs`, `src/catalog/backup.rs` as empty files.

- [ ] **Step 4: Verify it builds**

Run: `cargo build`
Expected: compiles successfully (warnings about unused stubs are fine).

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/phase1a-catalog-scanner
git add Cargo.toml Cargo.lock src/
git commit -m "chore(cli): scaffold cleanupstorages crate and CLI skeleton

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Config module

**Files:**
- Modify: `src/config.rs`
- Test: inline `#[cfg(test)]` in `src/config.rs`

**Interfaces:**
- Produces:
  - `pub struct Config { pub catalog_path: PathBuf, pub snapshot_retention: usize, pub batch_size: usize }`
  - `impl Config { pub fn default_paths() -> anyhow::Result<Config>; pub fn backups_dir(&self) -> PathBuf }`

- [ ] **Step 1: Write the failing test**

In `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default_paths().unwrap();
        assert_eq!(cfg.snapshot_retention, 10);
        assert_eq!(cfg.batch_size, 200);
        assert!(cfg.catalog_path.ends_with("catalog.db"));
        // backups dir is a sibling "catalog.backups" of the catalog file
        assert!(cfg.backups_dir().ends_with("catalog.backups"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config`
Expected: FAIL — `Config` not found.

- [ ] **Step 3: Write the implementation**

Replace `src/config.rs` contents:

```rust
use std::path::PathBuf;
use directories::ProjectDirs;

/// Runtime configuration. Defaults live on the computer, never on scanned drives.
pub struct Config {
    pub catalog_path: PathBuf,
    pub snapshot_retention: usize,
    pub batch_size: usize,
}

impl Config {
    /// Build a Config with default paths in the OS app-data directory.
    pub fn default_paths() -> anyhow::Result<Config> {
        let dirs = ProjectDirs::from("dev", "justPrototype", "CleanUpStorages")
            .ok_or_else(|| anyhow::anyhow!("could not determine app data directory"))?;
        let data_dir = dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Config {
            catalog_path: data_dir.join("catalog.db"),
            snapshot_retention: 10,
            batch_size: 200,
        })
    }

    /// Directory holding timestamped catalog snapshots (sibling of the DB file).
    pub fn backups_dir(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(|p| p.join("catalog.backups"))
            .unwrap_or_else(|| PathBuf::from("catalog.backups"))
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(cli): add Config with default on-computer catalog paths

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Streamed BLAKE3 hashing

**Files:**
- Modify: `src/hashing.rs`
- Test: inline `#[cfg(test)]` in `src/hashing.rs`

**Interfaces:**
- Produces:
  - `pub fn hash_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<String>` — lowercase hex BLAKE3.
  - `pub fn hash_file(path: &std::path::Path) -> std::io::Result<String>`.

- [ ] **Step 1: Write the failing test**

In `src/hashing.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_matches() {
        // BLAKE3 of the empty input is a fixed, well-known digest.
        let mut empty: &[u8] = b"";
        let got = hash_reader(&mut empty).unwrap();
        assert_eq!(
            got,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn same_bytes_same_hash() {
        let mut a: &[u8] = b"hello world";
        let mut b: &[u8] = b"hello world";
        assert_eq!(hash_reader(&mut a).unwrap(), hash_reader(&mut b).unwrap());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib hashing`
Expected: FAIL — `hash_reader` not found.

- [ ] **Step 3: Write the implementation**

Replace `src/hashing.rs` contents:

```rust
use std::io::Read;
use std::path::Path;

/// Hash any reader with BLAKE3 in 64 KiB chunks. Returns lowercase hex.
pub fn hash_reader<R: Read>(reader: &mut R) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Hash a file on disk by streaming it.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    hash_reader(&mut f)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib hashing`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/hashing.rs
git commit -m "feat(scanner): add streamed BLAKE3 hashing helpers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Category mapping

**Files:**
- Modify: `src/category.rs`
- Test: inline `#[cfg(test)]` in `src/category.rs`

**Interfaces:**
- Produces:
  - `pub enum Category { Photo, Document, Academic, Video, Other }`
  - `impl Category { pub fn as_str(&self) -> &'static str; pub fn from_db(s: &str) -> Category; pub fn from_extension(ext: &str) -> Category }`

- [ ] **Step 1: Write the failing test**

In `src/category.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_extensions() {
        assert_eq!(Category::from_extension("JPG"), Category::Photo);
        assert_eq!(Category::from_extension("pdf"), Category::Document);
        assert_eq!(Category::from_extension("bib"), Category::Academic);
        assert_eq!(Category::from_extension("mp4"), Category::Video);
        assert_eq!(Category::from_extension("xyz"), Category::Other);
        assert_eq!(Category::from_extension(""), Category::Other);
    }

    #[test]
    fn db_roundtrip() {
        for c in [Category::Photo, Category::Document, Category::Academic, Category::Video, Category::Other] {
            assert_eq!(Category::from_db(c.as_str()), c);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib category`
Expected: FAIL — `Category` not found.

- [ ] **Step 3: Write the implementation**

Replace `src/category.rs` contents:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category { Photo, Document, Academic, Video, Other }

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Photo => "photo",
            Category::Document => "document",
            Category::Academic => "academic",
            Category::Video => "video",
            Category::Other => "other",
        }
    }

    pub fn from_db(s: &str) -> Category {
        match s {
            "photo" => Category::Photo,
            "document" => Category::Document,
            "academic" => Category::Academic,
            "video" => Category::Video,
            _ => Category::Other,
        }
    }

    /// Map a file extension (without dot, any case) to a category.
    pub fn from_extension(ext: &str) -> Category {
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "heic" | "tiff" | "bmp" | "raw" | "cr2" | "nef" | "webp" => Category::Photo,
            "mp4" | "mov" | "avi" | "mkv" | "wmv" | "flv" | "webm" | "m4v" => Category::Video,
            "bib" | "tex" | "ipynb" | "csv" | "parquet" | "mat" | "r" => Category::Academic,
            "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ppt" | "pptx" => Category::Document,
            _ => Category::Other,
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib category`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/category.rs
git commit -m "feat(catalog): add extension-to-category mapping

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Catalog models

**Files:**
- Modify: `src/catalog/models.rs`
- Test: inline `#[cfg(test)]` in `src/catalog/models.rs`

**Interfaces:**
- Produces:
  - `pub enum FileStatus { Active, Missing, Quarantined, Purged }` with `as_str`/`from_db`.
  - `pub use crate::category::Category;`
  - `pub struct Volume { pub volume_id: String, pub label: String, pub identified_by: String, pub first_seen_at: i64, pub last_seen_at: i64 }`
  - `pub struct NewFile { ... }` — the fields the scanner produces for one file (no `id`).
  - `pub struct FileRecord { pub id: i64, ...same fields..., pub status: FileStatus, pub first_seen_at: i64, pub last_seen_at: i64 }`

- [ ] **Step 1: Write the failing test**

In `src/catalog/models.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        for s in [FileStatus::Active, FileStatus::Missing, FileStatus::Quarantined, FileStatus::Purged] {
            assert_eq!(FileStatus::from_db(s.as_str()), s);
        }
        // unknown falls back to Active defensively but is logged elsewhere
        assert_eq!(FileStatus::from_db("weird"), FileStatus::Active);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib models`
Expected: FAIL — `FileStatus` not found.

- [ ] **Step 3: Write the implementation**

Replace `src/catalog/models.rs` contents:

```rust
pub use crate::category::Category;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus { Active, Missing, Quarantined, Purged }

impl FileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileStatus::Active => "active",
            FileStatus::Missing => "missing",
            FileStatus::Quarantined => "quarantined",
            FileStatus::Purged => "purged",
        }
    }
    pub fn from_db(s: &str) -> FileStatus {
        match s {
            "missing" => FileStatus::Missing,
            "quarantined" => FileStatus::Quarantined,
            "purged" => FileStatus::Purged,
            _ => FileStatus::Active,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Volume {
    pub volume_id: String,
    pub label: String,
    /// "marker" or "fingerprint".
    pub identified_by: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

/// A file the scanner found, ready to upsert. No DB id yet.
#[derive(Debug, Clone)]
pub struct NewFile {
    pub volume_id: String,
    pub relative_path: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub created_time: Option<i64>,
    pub modified_time: Option<i64>,
    pub accessed_time: Option<i64>,
    pub category: Category,
    /// None for loose files. Reserved for archive entries in a later plan.
    pub container_chain: Option<String>,
}

/// A file row as stored, including identity and lifecycle.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub volume_id: String,
    pub relative_path: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub created_time: Option<i64>,
    pub modified_time: Option<i64>,
    pub accessed_time: Option<i64>,
    pub category: Category,
    pub container_chain: Option<String>,
    pub status: FileStatus,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib models`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/models.rs
git commit -m "feat(catalog): add volume/file models and status lifecycle enum

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Catalog schema, open, WAL, FTS, integrity

**Files:**
- Modify: `src/catalog/schema.rs`
- Modify: `src/catalog/mod.rs` (add `Catalog` handle)
- Test: inline `#[cfg(test)]` in `src/catalog/schema.rs`

**Interfaces:**
- Produces:
  - In `mod.rs`: `pub struct Catalog { pub conn: rusqlite::Connection }` and `impl Catalog { pub fn open(path: &std::path::Path) -> anyhow::Result<Catalog>; pub fn integrity_ok(&self) -> anyhow::Result<bool> }`.
  - `schema::apply(&Connection) -> rusqlite::Result<()>` creating all tables + FTS.

- [ ] **Step 1: Write the failing test**

In `src/catalog/schema.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;

    #[test]
    fn open_creates_schema_and_passes_integrity() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let cat = Catalog::open(&db).unwrap();
        assert!(cat.integrity_ok().unwrap());
        // WAL mode is active
        let mode: String = cat.conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // core tables exist
        let count: i64 = cat.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('volumes','files','scan_errors','actions_log')",
            [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib schema`
Expected: FAIL — `Catalog` not found.

- [ ] **Step 3: Write the schema**

Replace `src/catalog/schema.rs` contents:

```rust
use rusqlite::Connection;

/// Create all tables and indexes if they do not exist.
pub fn apply(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS volumes (
            volume_id     TEXT PRIMARY KEY,
            label         TEXT NOT NULL,
            identified_by TEXT NOT NULL,
            first_seen_at INTEGER NOT NULL,
            last_seen_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS files (
            id             INTEGER PRIMARY KEY,
            volume_id      TEXT NOT NULL REFERENCES volumes(volume_id),
            relative_path  TEXT NOT NULL,
            filename       TEXT NOT NULL,
            extension      TEXT NOT NULL,
            size_bytes     INTEGER NOT NULL,
            content_hash   TEXT NOT NULL,
            created_time   INTEGER,
            modified_time  INTEGER,
            accessed_time  INTEGER,
            category       TEXT NOT NULL,
            container_chain TEXT,
            status         TEXT NOT NULL,
            first_seen_at  INTEGER NOT NULL,
            last_seen_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
        CREATE INDEX IF NOT EXISTS idx_files_volume ON files(volume_id);
        CREATE INDEX IF NOT EXISTS idx_files_status ON files(status);

        -- File identity uniqueness. SQLite treats NULL as DISTINCT in a plain
        -- UNIQUE constraint, so a table-level UNIQUE(..., container_chain) would
        -- NOT dedupe loose files (container_chain always NULL) and ON CONFLICT
        -- would never fire. Partial unique indexes express the real key:
        --   loose files    -> unique on (volume_id, relative_path)
        --   archive entries -> unique on (volume_id, relative_path, container_chain)
        CREATE UNIQUE INDEX IF NOT EXISTS idx_files_loose_identity
            ON files(volume_id, relative_path) WHERE container_chain IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_files_archived_identity
            ON files(volume_id, relative_path, container_chain) WHERE container_chain IS NOT NULL;

        CREATE TABLE IF NOT EXISTS scan_errors (
            id         INTEGER PRIMARY KEY,
            volume_id  TEXT,
            path       TEXT NOT NULL,
            reason     TEXT NOT NULL,
            occurred_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS actions_log (
            id          INTEGER PRIMARY KEY,
            action      TEXT NOT NULL,
            details     TEXT,
            occurred_at INTEGER NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts
            USING fts5(filename, relative_path, content='files', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, filename, relative_path)
            VALUES (new.id, new.filename, new.relative_path);
        END;
        CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path)
            VALUES('delete', old.id, old.filename, old.relative_path);
        END;
        CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path)
            VALUES('delete', old.id, old.filename, old.relative_path);
            INSERT INTO files_fts(rowid, filename, relative_path)
            VALUES (new.id, new.filename, new.relative_path);
        END;
        "#,
    )
}
```

Note: loose-file identity is enforced by the partial unique index `idx_files_loose_identity` on `(volume_id, relative_path) WHERE container_chain IS NULL`. Task 7's `upsert_file` targets this index via `ON CONFLICT(volume_id, relative_path) WHERE container_chain IS NULL`, which is why re-scanning updates the existing row instead of inserting a duplicate.

- [ ] **Step 4: Add the `Catalog` handle in `src/catalog/mod.rs`**

Replace `src/catalog/mod.rs` contents:

```rust
pub mod models;
pub mod schema;
pub mod store;
pub mod backup;

use std::path::Path;
use rusqlite::Connection;

/// An open handle to the catalog database.
pub struct Catalog {
    pub conn: Connection,
}

impl Catalog {
    /// Open (creating if needed) the catalog at `path`, enabling WAL and the schema.
    pub fn open(path: &Path) -> anyhow::Result<Catalog> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::apply(&conn)?;
        Ok(Catalog { conn })
    }

    /// Run PRAGMA integrity_check; true if the DB reports "ok".
    pub fn integrity_ok(&self) -> anyhow::Result<bool> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result == "ok")
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib schema`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/catalog/
git commit -m "feat(catalog): open DB in WAL mode with schema, FTS index, integrity check

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: Catalog store — upserts, mark-missing, duplicates, search, stats

**Files:**
- Modify: `src/catalog/store.rs`
- Test: inline `#[cfg(test)]` in `src/catalog/store.rs`

**Interfaces:**
- Consumes: `Catalog`, `Volume`, `NewFile`, `FileRecord`, `FileStatus`, `Category`.
- Produces (methods on `Catalog`, implemented in `store.rs` via `impl Catalog`):
  - `pub fn upsert_volume(&self, v: &Volume) -> anyhow::Result<()>`
  - `pub fn get_file_meta(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<(i64, i64)>>` — returns `(size_bytes, modified_time)` for a loose file if present (modified_time NULL → 0).
  - `pub fn upsert_file(&self, f: &NewFile, now: i64) -> anyhow::Result<()>`
  - `pub fn mark_missing_except(&self, volume_id: &str, seen_ids: &std::collections::HashSet<i64>, now: i64) -> anyhow::Result<usize>` — but to avoid holding all ids, we instead sweep by scan timestamp (see impl).
  - `pub fn active_file_id(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<i64>>`
  - `pub fn log_scan_error(&self, volume_id: Option<&str>, path: &str, reason: &str, now: i64) -> anyhow::Result<()>`
  - `pub fn search(&self, query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>) -> anyhow::Result<Vec<FileRecord>>`
  - `pub fn duplicate_group_count(&self) -> anyhow::Result<i64>`
  - `pub fn volume_stats(&self) -> anyhow::Result<Vec<(String, String, i64, i64)>>` — (volume_id, label, active_file_count, active_total_bytes).

Mark-missing strategy (resumable, no giant id set): `upsert_file` always sets `last_seen_at = now` (the scan's start timestamp, passed in). After the walk completes for a volume, `mark_missing_scanned(volume_id, scan_started_at)` sets any `active` loose file whose `last_seen_at < scan_started_at` to `missing`. Replace the `mark_missing_except` interface above with this:
  - `pub fn mark_missing_scanned(&self, volume_id: &str, scan_started_at: i64, now: i64) -> anyhow::Result<usize>`

- [ ] **Step 1: Write the failing test**

In `src/catalog/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;
    use crate::catalog::models::*;

    fn mk_file(vol: &str, path: &str, hash: &str) -> NewFile {
        NewFile {
            volume_id: vol.into(),
            relative_path: path.into(),
            filename: path.rsplit('/').next().unwrap().into(),
            extension: "txt".into(),
            size_bytes: 10,
            content_hash: hash.into(),
            created_time: Some(1),
            modified_time: Some(2),
            accessed_time: Some(3),
            category: Category::Document,
            container_chain: None,
        }
    }

    fn open_tmp() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(), label: "Test HDD".into(),
            identified_by: "marker".into(), first_seen_at: 100, last_seen_at: 100,
        }).unwrap();
        (tmp, cat)
    }

    #[test]
    fn upsert_is_idempotent_and_search_finds_it() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 250).unwrap(); // same path again
        let hits = cat.search("thesis", None, None, None).unwrap();
        assert_eq!(hits.len(), 1); // one row, not two
        assert_eq!(hits[0].relative_path, "docs/thesis.txt");
    }

    #[test]
    fn duplicate_groups_counted_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1);
    }

    #[test]
    fn mark_missing_flags_files_not_seen_this_scan() {
        let (_t, cat) = open_tmp();
        // seen in an earlier scan at t=200
        cat.upsert_file(&mk_file("vol-1", "gone.txt", "h1"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 200).unwrap();
        // new scan starts at t=300; only kept.txt is re-seen
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 300).unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        let missing = cat.search("gone", None, None, Some("missing")).unwrap();
        assert_eq!(missing.len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib store`
Expected: FAIL — methods not found.

- [ ] **Step 3: Write the implementation**

Replace `src/catalog/store.rs` contents:

```rust
use crate::catalog::Catalog;
use crate::catalog::models::*;
use rusqlite::params;

impl Catalog {
    pub fn upsert_volume(&self, v: &Volume) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO volumes(volume_id, label, identified_by, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id) DO UPDATE SET label=excluded.label,
                 identified_by=excluded.identified_by, last_seen_at=excluded.last_seen_at",
            params![v.volume_id, v.label, v.identified_by, v.first_seen_at, v.last_seen_at],
        )?;
        Ok(())
    }

    /// (size_bytes, modified_time-or-0) for a loose file, if catalogued.
    pub fn get_file_meta(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<(i64, i64)>> {
        let row = self.conn.query_row(
            "SELECT size_bytes, IFNULL(modified_time,0) FROM files
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            params![volume_id, relative_path],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or update one loose file; sets status=active and last_seen_at=now.
    pub fn upsert_file(&self, f: &NewFile, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                 content_hash, created_time, modified_time, accessed_time, category,
                 container_chain, status, first_seen_at, last_seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active',?12,?12)
             ON CONFLICT(volume_id, relative_path) WHERE container_chain IS NULL DO UPDATE SET
                 filename=excluded.filename, extension=excluded.extension,
                 size_bytes=excluded.size_bytes, content_hash=excluded.content_hash,
                 created_time=excluded.created_time, modified_time=excluded.modified_time,
                 accessed_time=excluded.accessed_time, category=excluded.category,
                 status='active', last_seen_at=excluded.last_seen_at",
            params![f.volume_id, f.relative_path, f.filename, f.extension, f.size_bytes,
                f.content_hash, f.created_time, f.modified_time, f.accessed_time,
                f.category.as_str(), f.container_chain, now],
        )?;
        Ok(())
    }

    /// Flag active loose files on this volume not touched by the current scan as missing.
    pub fn mark_missing_scanned(&self, volume_id: &str, scan_started_at: i64, _now: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "UPDATE files SET status='missing'
             WHERE volume_id=?1 AND container_chain IS NULL
               AND status='active' AND last_seen_at < ?2",
            params![volume_id, scan_started_at],
        )?;
        Ok(n)
    }

    pub fn active_file_id(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<i64>> {
        let row = self.conn.query_row(
            "SELECT id FROM files WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            params![volume_id, relative_path],
            |r| r.get::<_, i64>(0),
        );
        match row {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn log_scan_error(&self, volume_id: Option<&str>, path: &str, reason: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_errors(volume_id, path, reason, occurred_at) VALUES (?1,?2,?3,?4)",
            params![volume_id, path, reason, now],
        )?;
        Ok(())
    }

    pub fn duplicate_group_count(&self) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT count(*) FROM (SELECT content_hash FROM files
                 WHERE status IN ('active','missing') GROUP BY content_hash HAVING count(*) > 1)",
            [], |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn volume_stats(&self) -> anyhow::Result<Vec<(String, String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.volume_id, v.label,
                    count(f.id) FILTER (WHERE f.status='active'),
                    IFNULL(sum(f.size_bytes) FILTER (WHERE f.status='active'),0)
             FROM volumes v LEFT JOIN files f ON f.volume_id=v.volume_id
             GROUP BY v.volume_id, v.label ORDER BY v.label",
        )?;
        let rows = stmt.query_map([], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?,
        )))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Search by free text plus optional filters. Empty query returns all (filtered).
    pub fn search(&self, query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>)
        -> anyhow::Result<Vec<FileRecord>>
    {
        let mut sql = String::from(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at FROM files WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            sql.push_str(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)");
            // FTS prefix match on each token
            let match_expr = q.split_whitespace().map(|t| format!("{t}*")).collect::<Vec<_>>().join(" ");
            args.push(Box::new(match_expr));
        }
        if let Some(c) = category { sql.push_str(" AND category = ?"); args.push(Box::new(c.to_string())); }
        if let Some(v) = volume { sql.push_str(" AND volume_id = ?"); args.push(Box::new(v.to_string())); }
        if let Some(s) = status { sql.push_str(" AND status = ?"); args.push(Box::new(s.to_string())); }
        sql.push_str(" ORDER BY relative_path LIMIT 1000");

        let mut stmt = self.conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), Self::map_file_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn map_file_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get(0)?,
            volume_id: r.get(1)?,
            relative_path: r.get(2)?,
            filename: r.get(3)?,
            extension: r.get(4)?,
            size_bytes: r.get(5)?,
            content_hash: r.get(6)?,
            created_time: r.get(7)?,
            modified_time: r.get(8)?,
            accessed_time: r.get(9)?,
            category: Category::from_db(&r.get::<_, String>(10)?),
            container_chain: r.get(11)?,
            status: FileStatus::from_db(&r.get::<_, String>(12)?),
            first_seen_at: r.get(13)?,
            last_seen_at: r.get(14)?,
        })
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib store`
Expected: PASS (all three tests).

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs
git commit -m "feat(catalog): add upsert, mark-missing, duplicate/search/stats queries

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: Catalog snapshots (durability)

**Files:**
- Modify: `src/catalog/backup.rs`
- Test: inline `#[cfg(test)]` in `src/catalog/backup.rs`

**Interfaces:**
- Consumes: `Config` (for `backups_dir` + `snapshot_retention`), catalog path.
- Produces:
  - `pub fn snapshot(catalog_path: &Path, backups_dir: &Path, retention: usize, now: i64) -> anyhow::Result<PathBuf>` — copies the live DB to `backups_dir/catalog-<now>.db`, prunes to the newest `retention` snapshots, returns the new snapshot path.

Use SQLite's online backup API (via `rusqlite::backup`) so a snapshot is consistent even with WAL. Open the live DB read-only for the backup source.

- [ ] **Step 1: Write the failing test**

In `src/catalog/backup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn snapshot_creates_file_and_prunes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let backups = tmp.path().join("catalog.backups");
        { Catalog::open(&db).unwrap(); } // create a real DB

        let mut made = Vec::new();
        for t in 1..=3 {
            made.push(snapshot(&db, &backups, 2, t).unwrap());
        }
        // retention=2 keeps only the two newest
        let kept: Vec<_> = std::fs::read_dir(&backups).unwrap()
            .filter_map(|e| e.ok()).map(|e| e.path()).collect();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|p| p == made.last().unwrap()));
        assert!(!kept.iter().any(|p| p == &made[0])); // oldest pruned
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib backup`
Expected: FAIL — `snapshot` not found.

- [ ] **Step 3: Write the implementation**

Replace `src/catalog/backup.rs` contents:

```rust
use std::path::{Path, PathBuf};
use rusqlite::{Connection, OpenFlags};

/// Copy the live catalog to a timestamped snapshot, then keep only the newest `retention`.
pub fn snapshot(catalog_path: &Path, backups_dir: &Path, retention: usize, now: i64) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(backups_dir)?;
    let dest = backups_dir.join(format!("catalog-{now}.db"));

    let src = Connection::open_with_flags(catalog_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut dst = Connection::open(&dest)?;
    let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
    backup.run_to_completion(64, std::time::Duration::from_millis(0), None)?;
    drop(backup);

    prune(backups_dir, retention)?;
    Ok(dest)
}

fn prune(backups_dir: &Path, retention: usize) -> anyhow::Result<()> {
    let mut snaps: Vec<PathBuf> = std::fs::read_dir(backups_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "db").unwrap_or(false))
        .collect();
    // Sort by filename (embeds the timestamp), newest last.
    snaps.sort();
    if snaps.len() > retention {
        for old in &snaps[..snaps.len() - retention] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib backup`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/backup.rs
git commit -m "feat(catalog): add consistent DB snapshots with retention pruning

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 9: Volume identity (marker + fingerprint fallback)

**Files:**
- Modify: `src/volume.rs`
- Test: inline `#[cfg(test)]` in `src/volume.rs`

**Interfaces:**
- Consumes: `sysinfo` for disk capacity; `uuid` for marker generation.
- Produces:
  - `pub struct VolumeIdentity { pub volume_id: String, pub label: String, pub identified_by: String }`
  - `pub enum ReadonlyMode { Ask, Fingerprint, Skip }`
  - `pub fn resolve(root: &Path, fallback: ReadonlyMode) -> anyhow::Result<Option<VolumeIdentity>>` — returns `None` only when a read-only drive is skipped. Logic: if marker file exists and is readable, use it (`identified_by="marker"`). Else try to write a new marker UUID; on success use it. On write failure, consult `fallback`: `Fingerprint` → compute fingerprint id; `Skip` → `Ok(None)`; `Ask` → prompt on stdin (default Fingerprint if not a TTY / EOF).
  - `pub fn fingerprint(root: &Path) -> anyhow::Result<String>` — stable id from label + total capacity bytes, hashed with BLAKE3, prefixed `fp-`.

- [ ] **Step 1: Write the failing test**

In `src/volume.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_marker_and_reuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let id1 = resolve(tmp.path(), ReadonlyMode::Fingerprint).unwrap().unwrap();
        assert_eq!(id1.identified_by, "marker");
        // marker file now exists and second resolve returns the same id
        let id2 = resolve(tmp.path(), ReadonlyMode::Fingerprint).unwrap().unwrap();
        assert_eq!(id1.volume_id, id2.volume_id);
        assert_eq!(id2.identified_by, "marker");
    }

    #[test]
    fn fingerprint_is_stable_and_prefixed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fingerprint(tmp.path()).unwrap();
        let b = fingerprint(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("fp-"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib volume`
Expected: FAIL — items not found.

- [ ] **Step 3: Write the implementation**

Replace `src/volume.rs` contents:

```rust
use std::io::Write;
use std::path::Path;

const MARKER: &str = ".cleanupstorages_id";

#[derive(Debug, Clone)]
pub struct VolumeIdentity {
    pub volume_id: String,
    pub label: String,
    pub identified_by: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ReadonlyMode { Ask, Fingerprint, Skip }

/// Best-effort human label for the drive (its root folder name, else the path).
fn label_for(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// Total capacity (bytes) of the filesystem containing `root`, or 0 if unknown.
fn total_capacity(root: &Path) -> u64 {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None; // (mount path len, capacity)
    for d in disks.list() {
        let mp = d.mount_point();
        if root.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, d.total_space()));
            }
        }
    }
    best.map(|(_, c)| c).unwrap_or(0)
}

pub fn fingerprint(root: &Path) -> anyhow::Result<String> {
    let label = label_for(root);
    let cap = total_capacity(root);
    let mut hasher = blake3::Hasher::new();
    hasher.update(label.as_bytes());
    hasher.update(&cap.to_le_bytes());
    Ok(format!("fp-{}", &hasher.finalize().to_hex()[..24]))
}

fn read_marker(root: &Path) -> Option<String> {
    let p = root.join(MARKER);
    std::fs::read_to_string(&p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn try_write_marker(root: &Path) -> std::io::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let p = root.join(MARKER);
    let mut f = std::fs::File::create(&p)?;
    f.write_all(id.as_bytes())?;
    f.sync_all()?;
    Ok(id)
}

/// Resolve the identity of the drive rooted at `root`. `None` = read-only drive skipped.
pub fn resolve(root: &Path, fallback: ReadonlyMode) -> anyhow::Result<Option<VolumeIdentity>> {
    let label = label_for(root);
    if let Some(existing) = read_marker(root) {
        return Ok(Some(VolumeIdentity { volume_id: existing, label, identified_by: "marker".into() }));
    }
    match try_write_marker(root) {
        Ok(id) => Ok(Some(VolumeIdentity { volume_id: id, label, identified_by: "marker".into() })),
        Err(_) => {
            let mode = match fallback {
                ReadonlyMode::Ask => prompt_readonly(root),
                other => other,
            };
            match mode {
                ReadonlyMode::Skip => Ok(None),
                _ => {
                    let fp = fingerprint(root)?;
                    Ok(Some(VolumeIdentity { volume_id: fp, label, identified_by: "fingerprint".into() }))
                }
            }
        }
    }
}

/// Ask the user how to handle a read-only drive. Defaults to Fingerprint on non-interactive input.
fn prompt_readonly(root: &Path) -> ReadonlyMode {
    use std::io::{self, BufRead};
    eprintln!(
        "Drive at {} is read-only; cannot write identity marker.\n  [f] proceed read-only (fingerprint)  [s] skip  (default: f): ",
        root.display()
    );
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return ReadonlyMode::Fingerprint;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "s" | "skip" => ReadonlyMode::Skip,
        _ => ReadonlyMode::Fingerprint,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib volume`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/volume.rs
git commit -m "feat(scanner): resolve volume identity via marker with fingerprint fallback

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 10: Scanner orchestration (walk + incremental + mark-missing)

**Files:**
- Modify: `src/scanner.rs`
- Test: inline `#[cfg(test)]` in `src/scanner.rs`

**Interfaces:**
- Consumes: `Catalog`, `VolumeIdentity`, `hashing`, `category::Category`, `models::NewFile`.
- Produces:
  - `pub struct ScanSummary { pub hashed: usize, pub skipped: usize, pub errors: usize, pub marked_missing: usize }`
  - `pub fn scan_volume(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64) -> anyhow::Result<ScanSummary>`

Behavior: walk `root` recursively (skipping the marker file and any `_ToDelete` directory). For each regular file: compute `relative_path` (forward-slash, relative to `root`); read metadata (size, mtime/ctime/atime as unix seconds via `std::fs::Metadata`); if not `force` and `get_file_meta` matches current (size, mtime), skip re-hash but still `upsert_file` with the existing hash so `last_seen_at` refreshes (fetch existing hash cheaply — see impl uses a lightweight update path). On new/changed: hash via `hashing::hash_file`; on hash/read error, `log_scan_error` and continue. Commit every `batch_size` files (wrap in transactions). After walking, call `mark_missing_scanned`.

To keep incremental skips from re-hashing, add a helper on `Catalog` used here: `touch_seen(volume_id, relative_path, now)` that sets `last_seen_at=now, status='active'` without changing the hash. Add it to `store.rs`.

- [ ] **Step 1: Add `touch_seen` to `src/catalog/store.rs`**

Inside the `impl Catalog` block in `store.rs`, add:

```rust
    /// Refresh last_seen/status for an unchanged file without re-hashing. Returns true if a row matched.
    pub fn touch_seen(&self, volume_id: &str, relative_path: &str, now: i64) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            rusqlite::params![volume_id, relative_path, now],
        )?;
        Ok(n > 0)
    }
```

- [ ] **Step 2: Write the failing test**

In `src/scanner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::catalog::models::Volume;
    use crate::volume::VolumeIdentity;
    use std::fs;

    fn ident() -> VolumeIdentity {
        VolumeIdentity { volume_id: "vol-1".into(), label: "T".into(), identified_by: "marker".into() }
    }

    fn setup() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(), label: "T".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        }).unwrap();
        (tmp, cat)
    }

    #[test]
    fn scans_hashes_and_reindex_skips() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta").unwrap();

        let s1 = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        assert_eq!(s1.hashed, 2);
        assert_eq!(s1.skipped, 0);

        // second scan: nothing changed -> both skipped (no re-hash)
        let s2 = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s2.hashed, 0);
        assert_eq!(s2.skipped, 2);

        // both searchable
        assert_eq!(cat.search("a", None, None, None).unwrap().len(), 1);
    }

    #[test]
    fn deleted_file_becomes_missing() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.txt"), b"x").unwrap();
        fs::write(root.join("gone.txt"), b"y").unwrap();
        scan_volume(&cat, &root, &ident(), false, 100).unwrap();

        fs::remove_file(root.join("gone.txt")).unwrap();
        let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s.marked_missing, 1);
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
        assert_eq!(cat.search("keep", None, None, Some("active")).unwrap().len(), 1);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib scanner`
Expected: FAIL — `scan_volume` not found.

- [ ] **Step 4: Write the implementation**

Replace `src/scanner.rs` contents:

```rust
use std::path::Path;
use walkdir::WalkDir;

use crate::catalog::Catalog;
use crate::catalog::models::NewFile;
use crate::category::Category;
use crate::hashing;
use crate::volume::VolumeIdentity;

const MARKER: &str = ".cleanupstorages_id";
const QUARANTINE_DIR: &str = "_ToDelete";

#[derive(Debug, Default)]
pub struct ScanSummary {
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub marked_missing: usize,
}

fn unix_secs(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

pub fn scan_volume(
    cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64,
) -> anyhow::Result<ScanSummary> {
    let scan_started_at = now;
    let mut summary = ScanSummary::default();
    let mut in_batch = 0usize;
    cat.conn.execute_batch("BEGIN")?;

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        if name == MARKER {
            continue;
        }
        // Skip anything under a _ToDelete quarantine folder.
        if path.components().any(|c| c.as_os_str() == QUARANTINE_DIR) {
            continue;
        }

        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &format!("metadata: {e}"), now)?;
                summary.errors += 1;
                continue;
            }
        };
        let size = meta.len() as i64;
        let mtime = unix_secs(meta.modified());

        // Incremental skip: same size + mtime as catalogued -> just touch.
        if !force {
            if let Some((old_size, old_mtime)) = cat.get_file_meta(&identity.volume_id, &rel)? {
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    summary.skipped += 1;
                    in_batch += 1;
                    if in_batch >= 200 { cat.conn.execute_batch("COMMIT; BEGIN")?; in_batch = 0; }
                    continue;
                }
            }
        }

        let hash = match hashing::hash_file(path) {
            Ok(h) => h,
            Err(e) => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &format!("read: {e}"), now)?;
                summary.errors += 1;
                continue;
            }
        };

        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let nf = NewFile {
            volume_id: identity.volume_id.clone(),
            relative_path: rel.clone(),
            filename: name.into_owned(),
            extension: ext.clone(),
            size_bytes: size,
            content_hash: hash,
            created_time: unix_secs(meta.created()),
            modified_time: mtime,
            accessed_time: unix_secs(meta.accessed()),
            category: Category::from_extension(&ext),
            container_chain: None,
        };
        cat.upsert_file(&nf, now)?;
        summary.hashed += 1;
        in_batch += 1;
        if in_batch >= 200 { cat.conn.execute_batch("COMMIT; BEGIN")?; in_batch = 0; }
    }

    cat.conn.execute_batch("COMMIT")?;
    summary.marked_missing = cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    Ok(summary)
}
```

Note: because `touch_seen`/`upsert_file` set `last_seen_at=now` and `mark_missing_scanned` flags rows with `last_seen_at < scan_started_at`, and here `now == scan_started_at`, every file touched this scan is safe and only genuinely-absent files are flagged.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib scanner`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add src/scanner.rs src/catalog/store.rs
git commit -m "feat(scanner): incremental resumable volume scan with missing-file sweep

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 11: CLI command wiring + end-to-end integration test

**Files:**
- Modify: `src/commands.rs`
- Create: `tests/scan_and_search.rs`

**Interfaces:**
- Consumes: everything above (`Config`, `Catalog`, `volume::resolve`, `scanner::scan_volume`, `backup::snapshot`).
- Produces: fully working `cmd_scan`, `cmd_search`, `cmd_status`. `ReadonlyFallback` maps to `volume::ReadonlyMode`.

- [ ] **Step 1: Write the failing integration test**

Create `tests/scan_and_search.rs`:

```rust
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cleanupstorages"))
}

#[test]
fn scan_then_search_finds_file() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("thesis_final.pdf"), b"hello thesis").unwrap();

    // Point the catalog at a temp location via env override (see Step 2).
    let data = tmp.path().join("appdata");
    let scan = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("scan").arg(&drive)
        .arg("--readonly-fallback").arg("fingerprint")
        .output().unwrap();
    assert!(scan.status.success(), "scan failed: {}", String::from_utf8_lossy(&scan.stderr));

    let search = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("search").arg("thesis")
        .output().unwrap();
    assert!(search.status.success());
    let out = String::from_utf8_lossy(&search.stdout);
    assert!(out.contains("thesis_final.pdf"), "search output was: {out}");
}
```

- [ ] **Step 2: Add a data-dir override to `Config`**

In `src/config.rs`, modify `default_paths` to honor an env override (add this at the top of the function body, before computing `dirs`):

```rust
        if let Ok(dir) = std::env::var("CLEANUPSTORAGES_DATA_DIR") {
            let data_dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&data_dir)?;
            return Ok(Config {
                catalog_path: data_dir.join("catalog.db"),
                snapshot_retention: 10,
                batch_size: 200,
            });
        }
```

- [ ] **Step 3: Implement the command handlers**

Replace `src/commands.rs` contents:

```rust
use std::path::Path;
use clap::ValueEnum;

use crate::config::Config;
use crate::catalog::Catalog;
use crate::catalog::models::{Volume, FileStatus};
use crate::volume::{self, ReadonlyMode};
use crate::scanner;
use crate::catalog::backup;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadonlyFallback { Ask, Fingerprint, Skip }

impl From<ReadonlyFallback> for ReadonlyMode {
    fn from(f: ReadonlyFallback) -> Self {
        match f {
            ReadonlyFallback::Ask => ReadonlyMode::Ask,
            ReadonlyFallback::Fingerprint => ReadonlyMode::Fingerprint,
            ReadonlyFallback::Skip => ReadonlyMode::Skip,
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub fn cmd_scan(path: &Path, force: bool, fallback: ReadonlyFallback) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }

    let identity = match volume::resolve(path, fallback.into())? {
        Some(id) => id,
        None => { println!("Skipped read-only drive at {}", path.display()); return Ok(()); }
    };
    let now = now_secs();
    cat.upsert_volume(&Volume {
        volume_id: identity.volume_id.clone(),
        label: identity.label.clone(),
        identified_by: identity.identified_by.clone(),
        first_seen_at: now, last_seen_at: now,
    })?;

    println!("Scanning {} (volume {}, id by {})...",
        path.display(), identity.label, identity.identified_by);
    let s = scanner::scan_volume(&cat, path, &identity, force, now)?;
    println!("Done: {} hashed, {} unchanged, {} errors, {} newly missing.",
        s.hashed, s.skipped, s.errors, s.marked_missing);

    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_search(query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>)
    -> anyhow::Result<()>
{
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let hits = cat.search(query, category, volume, status)?;
    if hits.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    for f in &hits {
        let flag = match f.status {
            FileStatus::Active => "",
            FileStatus::Missing => "  [MISSING]",
            FileStatus::Quarantined => "  [QUARANTINED]",
            FileStatus::Purged => "  [PURGED]",
        };
        println!("{}  [{}]  {}  ({} bytes){}",
            f.relative_path, f.volume_id, f.category.as_str(), f.size_bytes, flag);
    }
    println!("{} match(es).", hits.len());
    Ok(())
}

pub fn cmd_status() -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let groups = cat.duplicate_group_count()?;
    println!("Duplicate groups (same content hash): {groups}");
    println!("Per-volume (active files):");
    for (id, label, count, bytes) in cat.volume_stats()? {
        println!("  {label} [{id}]: {count} files, {} MiB", bytes / (1024 * 1024));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the integration test**

Run: `cargo test --test scan_and_search`
Expected: PASS.

- [ ] **Step 5: Run the whole suite + build release**

Run: `cargo test` then `cargo build --release`
Expected: all tests PASS; release binary builds.

- [ ] **Step 6: Manual smoke check**

Run:
```bash
cargo run -- scan ./ --readonly-fallback fingerprint
cargo run -- status
cargo run -- search Cargo
```
Expected: scan reports counts and a snapshot path; status lists a volume; search finds `Cargo.toml`. (This scans the repo dir; the marker file `.cleanupstorages_id` will be created here — it is already git-ignored via the pattern below.)

- [ ] **Step 7: Ignore the marker file and commit**

Add to `.gitignore`:
```
.cleanupstorages_id
```

```bash
git add src/commands.rs src/config.rs tests/scan_and_search.rs .gitignore Cargo.lock
git commit -m "feat(cli): wire scan/search/status end-to-end with snapshot on scan

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (Phase 1a subset):**
- Rust single binary, SQLite catalog on computer, BLAKE3 streamed → Tasks 1, 3, 6 ✓
- Volume identity marker + fingerprint fallback + read-only prompt (§4) → Task 9 ✓
- Data model `volumes`/`files`/`scan_errors`/`actions_log`, container_chain column reserved (§5) → Task 6 ✓
- Durability: WAL + snapshots + integrity_check (§5a) → Tasks 6, 8, 11 ✓
- Incremental + force + resumable batches + non-fatal errors (§6) → Task 10 ✓
- Status lifecycle active/missing (quarantined/purged reserved for Phase 2) (§7) → Tasks 5, 7, 10 ✓
- Exact duplicate detection by hash (§8) → Task 7 (`duplicate_group_count`) ✓
- CLI search + FTS index (§12 CLI half) → Tasks 6, 7, 11 ✓
- `status` stats (§3) → Task 11 ✓
- Config defaults incl. snapshot retention, batch size, readonly fallback (§15) → Tasks 2, 11 ✓
- **Deferred, intentionally NOT covered here:** recursive archives (§9), quarantine/purge/Case1-4 (§10-11), web browse screen (§12 web half), review GUI (§13). Tracked for Plans 1b/1c/2.

**Placeholder scan:** No TBD/TODO; every step has runnable code and exact commands. ✓

**Type consistency:** `Catalog.conn`, `NewFile`/`FileRecord` fields, `Category::from_db`/`as_str`, `FileStatus::from_db`/`as_str`, `VolumeIdentity`, `ReadonlyMode`↔`ReadonlyFallback`, `scan_volume` signature, `snapshot` signature all match across tasks. `container_chain IS NULL` used consistently for the loose-file key. ✓

---

## Follow-on plans (not this plan)

- **Plan 1b — Recursive archive scanning:** descend zips at any depth, hash entries, write `container_chain`, enforce depth/zip-bomb/encrypted safety limits (spec §9).
- **Plan 1c — Web browse/search screen:** `axum` server on `127.0.0.1`, filters, offline-drive results (spec §12 web half).
- **Plan 2 — Review GUI + quarantine + Case 1–4 workflows** (spec §10, §11, §13).

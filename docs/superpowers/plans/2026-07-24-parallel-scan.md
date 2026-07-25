# Parallel Scan Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the scan into a walker → workers → writer pipeline so disk reads and hashing overlap, driven by the #22 finding that the scan is I/O-bound on small files.

**Architecture:** A walker thread walks + stats + skip-checks (own read-only connection) and emits jobs on a bounded channel; N below-normal-priority worker threads read+hash loose files or run the pure `archive::scan_archive` (no DB access) and emit results; one writer thread owns the write connection and the transaction, applies results, runs the missing-sweep, and returns the summary. `--jobs=1` is a single worker and is the correctness anchor: it must produce a catalogue bit-identical to the old serial scan.

**Tech Stack:** Rust, `std::thread`, `crossbeam-channel` (bounded mpmc), `thread-priority` (below-normal workers), rusqlite, clap.

**Spec:** [docs/superpowers/specs/2026-07-24-parallel-scan-design.md](../specs/2026-07-24-parallel-scan-design.md)

**Two deliberate deviations from the spec (behaviour and stored data unchanged):**

1. **Connection ownership.** The spec described `run_scan` *moving* its owned `Catalog` into the
   writer thread and getting it back on join. This plan instead uses `std::thread::scope` and opens
   **fresh connections from the catalog path** — read-only in the walker, read-write in the writer —
   while the passed `&Catalog` supplies only the path. It's simpler (no ownership refactor of
   `run_scan`, no returning the connection), still a single writer, and WAL-safe (the caller's idle
   connection and the read-only walker connection do not contend with the one writer). The writer's
   `Catalog::open` re-runs the idempotent `apply()` — cheap and harmless.
2. **"All pre-existing tests pass unmodified" has one exception:** the scanner test
   `scan_records_phase_timings_and_the_size_histogram` asserts `total_phase_ms() <= wall_ms`. Under
   the pipeline the phases run on different threads and *overlap*, so sum-of-phases can exceed
   wall-clock — that is the feature working, not a bug. Task 5 removes that one assertion (the
   counter/histogram assertions in the same test stay). Every other pre-existing test is untouched.

## Global Constraints

- **One scan implementation.** The pipeline is the only scan; `--jobs=1` runs one worker. No separate serial loop survives. `descend_archive` and the old inline loop in `scan_volume_with_progress` are deleted.
- **Byte-identical hashes and bit-identical catalogue.** A fixed tree must yield the same rows (path, hash, size, status, container_chain, modified_time), same archive entries, and same summary counts at `--jobs=1` and `--jobs=8`. This is the anchor test.
- **Workers do NO SQLite I/O.** They read bytes and hash only. The writer thread is the single writer.
- **Reliability preserved exactly:** a read/stat/hash error on one file is logged via `log_scan_error`, increments `summary.errors`, touches the row (`touch_seen`), and does not abort the scan — identical to today. A writer DB error aborts with `ROLLBACK` and propagates. A worker panic is contained (join detects it) and aborts cleanly, never a silent partial scan.
- **Metrics phases fire on their thread:** `walk` + `skip_check` on the walker, `hash` + `archive` on workers, `db_write` on the writer. `record_file_seen` on the walker; `add_bytes_skipped` on the walker; `add_bytes_hashed` on the worker.
- **Default `--jobs` is 4.** Adaptive/load-based scaling is out of scope.
- **No parallelism inside one archive.** A worker runs a whole `archive::scan_archive`; the #18 buffer budget stays a local `&mut` and is untouched.
- **Archive entries inherit the archive's mtime (#10) and the FTS/date behaviour is preserved.**
- Conventional Commits; both trailers on every commit:
  `Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Every task ends green: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Branch `feat/parallel-scan`. Do not merge, tag or push.

## File structure

| File | Responsibility in this change |
| --- | --- |
| `Cargo.toml` | Add `crossbeam-channel` and `thread-priority`. |
| `src/scan_pipeline.rs` **(new)** | `Job` / `ScanResult` enums; `run_job` (worker logic, pure, no DB); `walk` (producer); `write_results` (consumer/writer). One cohesive module; keeps `scanner.rs` a thin orchestrator. |
| `src/scanner.rs` | `scan_volume_with_progress` becomes the orchestrator (spawn threads, wire channels, set priority, join). Old inline loop + `descend_archive` **deleted**. `run_scan`/`scan_volume` gain a `jobs` parameter (`scan_volume` keeps its old signature, defaulting to 1). Helpers `unix_secs`, `should_skip`, `relative_path`, `rotate_batch` stay (made `pub(crate)` where the pipeline needs them). |
| `src/commands.rs` | `cmd_scan` gains `jobs`; passes it to `run_scan`. |
| `src/main.rs` | `Scan` subcommand gains `--jobs` (default 4). |
| `src/scan_queue.rs` | Web scan passes the default jobs to `run_scan`. |
| `src/catalog/schema.rs` | `scan_runs` gains a `jobs` column. |
| `src/catalog/scan_runs.rs` | `start_scan_run` records `jobs`; `ScanRun` exposes it. |
| `src/lib.rs` | Declare `pub mod scan_pipeline;`. |

---

### Task 1: Dependencies, module, and the Job/ScanResult types

**Files:**
- Modify: `Cargo.toml`
- Create: `src/scan_pipeline.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `scan_pipeline::{Job, ScanResult}`. Tasks 2–5 use these exact shapes.

- [ ] **Step 1: Add the dependencies**

In `Cargo.toml`, under `[dependencies]`, add:

```toml
crossbeam-channel = "0.5"
thread-priority = "1"
```

- [ ] **Step 2: Create the module with the types and a compile test**

Create `src/scan_pipeline.rs`:

```rust
//! The scan pipeline: a walker produces jobs, workers read+hash them, a single writer persists the
//! results. Workers never touch SQLite — the writer is the sole writer. See the design spec.

use crate::catalog::models::NewFile;
use std::path::PathBuf;

/// Work the walker hands to a worker. `Touch` and `Error` carry no I/O — they pass through a worker
/// unchanged so the topology stays one-in/one-out (walker has one output, writer one input).
#[derive(Debug, Clone)]
pub(crate) enum Job {
    /// Unchanged file (skip-check matched). `is_archive` triggers touch of the archive's entries.
    Touch { rel: String, is_archive: bool },
    /// The walker already failed this file (e.g. stat error); just record it.
    Error { rel: String, reason: String },
    /// A new/changed loose file to read and hash.
    HashLoose {
        path: PathBuf,
        rel: String,
        filename: String,
        size: i64,
        created: Option<i64>,
        modified: Option<i64>,
        accessed: Option<i64>,
    },
    /// An archive to hash (its own loose row) and descend (its entries).
    ScanArchive {
        path: PathBuf,
        rel: String,
        filename: String,
        size: i64,
        created: Option<i64>,
        modified: Option<i64>,
        accessed: Option<i64>,
    },
}

/// What a worker sends to the writer. One `ScanArchive` job produces both an `Upsert` (the archive's
/// own loose row) and an `ArchiveEntries` (its contents).
#[derive(Debug)]
pub(crate) enum ScanResult {
    Touch { rel: String, is_archive: bool },
    Error { rel: String, reason: String },
    Upsert(NewFile),
    ArchiveEntries {
        rel: String,
        modified: Option<i64>,
        scan: crate::archive::ArchiveScanResult,
    },
}
```

In `src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod scan_pipeline;
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: compiles (dead-code warnings for the unused enums are fine at this stage; they are used in Task 2+ and `cargo clippy` is only required green at task end, by which point they are used).

If clippy is run now it will warn `dead_code`. To keep this task independently green, add `#![allow(dead_code)]`? **No** — instead do not run clippy as a gate for Task 1 alone; the enums are consumed in Task 2 which lands before any push. If you must silence it, add `#[allow(dead_code)]` on the two enums and remove it in Task 2.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/scan_pipeline.rs src/lib.rs
git commit -m "feat(scanner): add pipeline deps and the Job/ScanResult types

crossbeam-channel for the bounded mpmc job/result channels, thread-priority
for below-normal workers. The two enums are the walker->worker->writer
contract.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: The worker (`run_job`) — pure, no DB

**Files:**
- Modify: `src/scan_pipeline.rs`
- Modify: `src/catalog/store.rs` (make `FILE_COLUMNS` reuse not needed here; only `Category` import) — no change needed; see code.

**Interfaces:**
- Consumes: `Job`, `ScanResult` (Task 1); `crate::hashing::hash_file`; `crate::archive::{scan_archive, is_archive_name}`; `crate::category::Category`; `crate::scan_metrics::{ScanMetrics, Phase}`.
- Produces: `fn run_job(job: Job, volume_id: &str, metrics: &ScanMetrics) -> Vec<ScanResult>`. Task 5 calls this on each worker thread. Returns a `Vec` because one `ScanArchive` yields two results.

- [ ] **Step 1: Write the failing tests**

Append to `src/scan_pipeline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_metrics::ScanMetrics;

    fn tmp_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn hash_loose_job_produces_an_upsert_with_the_right_hash() {
        let t = tempfile::tempdir().unwrap();
        let p = tmp_file(t.path(), "a.txt", b"hello");
        let m = ScanMetrics::new();
        let job = Job::HashLoose {
            path: p.clone(),
            rel: "a.txt".into(),
            filename: "a.txt".into(),
            size: 5,
            created: Some(1),
            modified: Some(2),
            accessed: None,
        };
        let out = run_job(job, "v1", &m);
        assert_eq!(out.len(), 1);
        match &out[0] {
            ScanResult::Upsert(nf) => {
                assert_eq!(nf.relative_path, "a.txt");
                assert_eq!(nf.volume_id, "v1");
                assert_eq!(nf.size_bytes, 5);
                let mut raw: &[u8] = b"hello";
                assert_eq!(nf.content_hash, crate::hashing::hash_reader(&mut raw).unwrap());
                assert!(nf.container_chain.is_none());
                assert_eq!(nf.modified_time, Some(2));
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
        assert_eq!(m.snapshot().bytes_hashed, 5);
    }

    #[test]
    fn a_missing_loose_file_becomes_an_error_not_a_panic() {
        let m = ScanMetrics::new();
        let job = Job::HashLoose {
            path: "/no/such/file".into(),
            rel: "gone.txt".into(),
            filename: "gone.txt".into(),
            size: 0,
            created: None,
            modified: None,
            accessed: None,
        };
        let out = run_job(job, "v1", &m);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], ScanResult::Error { rel, .. } if rel == "gone.txt"));
    }

    #[test]
    fn touch_and_error_jobs_pass_through_unchanged() {
        let m = ScanMetrics::new();
        let t = run_job(Job::Touch { rel: "x".into(), is_archive: true }, "v1", &m);
        assert!(matches!(&t[0], ScanResult::Touch { rel, is_archive: true } if rel == "x"));
        let e = run_job(Job::Error { rel: "y".into(), reason: "bad".into() }, "v1", &m);
        assert!(matches!(&e[0], ScanResult::Error { rel, .. } if rel == "y"));
    }

    #[test]
    fn a_zip_job_yields_the_loose_row_and_its_entries() {
        let t = tempfile::tempdir().unwrap();
        // Build a real zip with one entry.
        let zip_path = t.path().join("bundle.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("inner.txt", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"inner bytes").unwrap();
            zw.finish().unwrap();
        }
        let size = std::fs::metadata(&zip_path).unwrap().len() as i64;
        let m = ScanMetrics::new();
        let job = Job::ScanArchive {
            path: zip_path,
            rel: "bundle.zip".into(),
            filename: "bundle.zip".into(),
            size,
            created: None,
            modified: Some(99),
            accessed: None,
        };
        let out = run_job(job, "v1", &m);
        // First result: the zip's own loose row. Second: its entries.
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], ScanResult::Upsert(nf) if nf.relative_path == "bundle.zip"));
        match &out[1] {
            ScanResult::ArchiveEntries { rel, modified, scan } => {
                assert_eq!(rel, "bundle.zip");
                assert_eq!(*modified, Some(99));
                assert_eq!(scan.entries.len(), 1);
                assert_eq!(scan.entries[0].filename, "inner.txt");
            }
            other => panic!("expected ArchiveEntries, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib scan_pipeline::tests`
Expected: FAIL — `run_job` not defined.

- [ ] **Step 3: Implement `run_job`**

Insert above the test module in `src/scan_pipeline.rs`:

```rust
use crate::category::Category;
use crate::scan_metrics::{Phase, ScanMetrics};
use std::path::Path;

/// Build the loose `NewFile` for a path already read off disk. Shared by the loose and archive
/// jobs (an archive is first catalogued as its own loose row).
fn loose_record(
    volume_id: &str,
    rel: &str,
    filename: &str,
    path: &Path,
    size: i64,
    hash: String,
    created: Option<i64>,
    modified: Option<i64>,
    accessed: Option<i64>,
) -> NewFile {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    NewFile {
        volume_id: volume_id.to_string(),
        relative_path: rel.to_string(),
        filename: filename.to_string(),
        extension: ext.clone(),
        size_bytes: size,
        content_hash: hash,
        created_time: created,
        modified_time: modified,
        accessed_time: accessed,
        category: Category::from_extension(&ext),
        container_chain: None,
    }
}

/// Do one job's I/O and hashing. No DB access — the writer persists what this returns. Runs on a
/// worker thread; `hash`/`archive` timing is charged here.
pub(crate) fn run_job(job: Job, volume_id: &str, metrics: &ScanMetrics) -> Vec<ScanResult> {
    match job {
        Job::Touch { rel, is_archive } => vec![ScanResult::Touch { rel, is_archive }],
        Job::Error { rel, reason } => vec![ScanResult::Error { rel, reason }],
        Job::HashLoose { path, rel, filename, size, created, modified, accessed } => {
            let hashed = {
                let _t = metrics.timer(Phase::Hash);
                crate::hashing::hash_file(&path)
            };
            match hashed {
                Ok(hash) => {
                    metrics.add_bytes_hashed(size);
                    let nf = loose_record(
                        volume_id, &rel, &filename, &path, size, hash, created, modified, accessed,
                    );
                    vec![ScanResult::Upsert(nf)]
                }
                Err(e) => vec![ScanResult::Error { rel, reason: format!("read: {e}") }],
            }
        }
        Job::ScanArchive { path, rel, filename, size, created, modified, accessed } => {
            // Hash the archive's own bytes for its loose row (identical to the pre-parallel scan,
            // which hashed the zip then descended it — two reads).
            let hashed = {
                let _t = metrics.timer(Phase::Hash);
                crate::hashing::hash_file(&path)
            };
            let hash = match hashed {
                Ok(h) => h,
                Err(e) => return vec![ScanResult::Error { rel, reason: format!("read: {e}") }],
            };
            metrics.add_bytes_hashed(size);
            let nf = loose_record(
                volume_id, &rel, &filename, &path, size, hash, created, modified, accessed,
            );
            // Descend. archive::scan_archive is pure (no DB); the writer persists its entries.
            let scan = {
                let _t = metrics.timer(Phase::Archive);
                // ArchiveLimits::from_config only reads three static numbers; a failure to resolve
                // the data dir must not abort a scan, so fall back to the config defaults.
                let limits = crate::config::Config::default_paths()
                    .map(|c| crate::archive::ArchiveLimits::from_config(&c))
                    .unwrap_or_else(|_| crate::archive::ArchiveLimits {
                        max_depth: 8,
                        entry_max_bytes: 2 * 1024 * 1024 * 1024,
                        ratio_cap: 200,
                        total_buffer_bytes: 2 * 1024 * 1024 * 1024,
                    });
                match std::fs::File::open(&path) {
                    Ok(f) => crate::archive::scan_archive(f, &limits),
                    Err(e) => {
                        let mut r = crate::archive::ArchiveScanResult::default();
                        r.errors.push((rel.clone(), format!("archive open: {e}")));
                        r
                    }
                }
            };
            vec![
                ScanResult::Upsert(nf),
                ScanResult::ArchiveEntries { rel, modified, scan },
            ]
        }
    }
}
```

> **Verify the fallback constants** still match `src/config.rs` defaults (`8 / 2 GiB / 200 / 2 GiB`).

- [ ] **Step 4: Remove any `#[allow(dead_code)]` added in Task 1** (the enums are now used).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib scan_pipeline::tests`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/scan_pipeline.rs
git commit -m "feat(scanner): worker job runner (pure read+hash, no DB)

run_job does one file's I/O and hashing off the worker thread and returns
records for the writer to persist. An archive yields both its own loose row
and its entries, matching the pre-parallel two-read behaviour.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: The walker (`walk`) — produce jobs

**Files:**
- Modify: `src/scan_pipeline.rs`
- Modify: `src/scanner.rs` (make helpers `pub(crate)`)

**Interfaces:**
- Consumes: `Job` (Task 1); `crate::scanner::{should_skip, relative_path, unix_secs}` (make these `pub(crate)`); `crate::catalog::Catalog` (read-only); `crate::volume::VolumeIdentity`; `crate::archive::is_archive_name`; `crossbeam_channel::Sender`.
- Produces: `fn walk(ro: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64, metrics: &ScanMetrics, jobs: &crossbeam_channel::Sender<Job>) -> ()`. Task 5 runs this on the walker thread. It never returns an error — walk/stat failures become `Job::Error`, matching today.

- [ ] **Step 1: Make the scanner helpers reachable**

In `src/scanner.rs`, change these three free functions from private to `pub(crate)`:
`fn unix_secs`, `fn should_skip`, `fn relative_path`. (They are currently `fn`; make them `pub(crate) fn`.) Leave their bodies unchanged.

- [ ] **Step 2: Write the failing tests**

Append to the `#[cfg(test)] mod tests` in `src/scan_pipeline.rs`:

```rust
    use crate::catalog::Catalog;
    use crate::volume::VolumeIdentity;

    fn ident() -> VolumeIdentity {
        VolumeIdentity { volume_id: "v1".into(), label: "D".into(), identified_by: "marker".into() }
    }

    /// Drain the walker's jobs into a Vec.
    fn walk_to_vec(cat: &Catalog, root: &std::path::Path, force: bool) -> Vec<Job> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let m = ScanMetrics::new();
        walk(cat, root, &ident(), force, 100, &m, &tx);
        drop(tx);
        rx.into_iter().collect()
    }

    #[test]
    fn walker_emits_hash_jobs_for_new_files_and_scan_jobs_for_archives() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("doc.txt"), b"hi").unwrap();
        {
            let f = std::fs::File::create(t.path().join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("e.txt", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"x").unwrap();
            zw.finish().unwrap();
        }
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        let jobs = walk_to_vec(&cat, t.path(), false);
        assert!(jobs.iter().any(|j| matches!(j, Job::HashLoose { rel, .. } if rel == "doc.txt")));
        assert!(jobs.iter().any(|j| matches!(j, Job::ScanArchive { rel, .. } if rel == "bundle.zip")));
    }

    #[test]
    fn walker_emits_touch_for_an_unchanged_catalogued_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("same.txt");
        std::fs::write(&p, b"stable").unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let size = meta.len() as i64;
        let mtime = crate::scanner::unix_secs(meta.modified());
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(), label: "D".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        }).unwrap();
        cat.upsert_file(&crate::catalog::models::NewFile {
            volume_id: "v1".into(), relative_path: "same.txt".into(), filename: "same.txt".into(),
            extension: "txt".into(), size_bytes: size, content_hash: "h".into(),
            created_time: None, modified_time: mtime, accessed_time: None,
            category: crate::category::Category::Other, container_chain: None,
        }, 50).unwrap();

        let jobs = walk_to_vec(&cat, t.path(), false); // not forced
        assert!(jobs.iter().any(|j| matches!(j, Job::Touch { rel, .. } if rel == "same.txt")),
            "unchanged file should be a Touch, got {jobs:?}");

        // --force re-hashes it instead.
        let forced = walk_to_vec(&cat, t.path(), true);
        assert!(forced.iter().any(|j| matches!(j, Job::HashLoose { rel, .. } if rel == "same.txt")));
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib scan_pipeline::tests::walker_emits`
Expected: FAIL — `walk` not defined.

- [ ] **Step 4: Implement `walk`**

Add to `src/scan_pipeline.rs` (above the tests). This mirrors the walk/stat/skip-check half of today's loop; the emit points replace the inline hash/upsert:

```rust
use crate::catalog::Catalog;
use crate::volume::VolumeIdentity;
use crossbeam_channel::Sender;
use walkdir::WalkDir;

/// Walk `root`, stat + skip-check each file, and emit a `Job` per file. Never returns an error:
/// walk and stat failures become `Job::Error`, matching the pre-parallel scan. Runs on its own
/// thread with a read-only catalog connection; `walk`/`skip_check` timing is charged here.
pub(crate) fn walk(
    ro: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    now: i64,
    metrics: &ScanMetrics,
    jobs: &Sender<Job>,
) {
    let send = |j: Job| {
        // The only reason send fails is the writer/workers went away (fatal). Stop walking.
        let _ = jobs.send(j);
    };
    let mut walker = WalkDir::new(root).into_iter();
    loop {
        let next = {
            let _t = metrics.timer(Phase::Walk);
            walker.next()
        };
        let Some(entry) = next else { break };
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let rel = err
                    .path()
                    .map(|p| p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|| "<unknown>".to_string());
                send(Job::Error { rel, reason: format!("walk: {err}") });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if crate::scanner::should_skip(path, name) {
            continue;
        }
        let Some(rel) = crate::scanner::relative_path(path, root) else { continue };

        let stat = {
            let _t = metrics.timer(Phase::Walk);
            entry.metadata()
        };
        let meta = match stat {
            Ok(m) => m,
            Err(e) => {
                metrics.record_file_seen(0);
                send(Job::Error { rel, reason: format!("metadata: {e}") });
                continue;
            }
        };
        let size = meta.len() as i64;
        let mtime = crate::scanner::unix_secs(meta.modified());
        metrics.record_file_seen(size);

        let is_archive = crate::archive::is_archive_name(&rel);

        if !force {
            let _t = metrics.timer(Phase::SkipCheck);
            if let Ok(Some((old_size, old_mtime))) = ro.get_file_meta(&identity.volume_id, &rel) {
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    metrics.add_bytes_skipped(size);
                    send(Job::Touch { rel, is_archive });
                    continue;
                }
            }
        }

        let filename = name.to_string_lossy().into_owned();
        let created = crate::scanner::unix_secs(meta.created());
        let accessed = crate::scanner::unix_secs(meta.accessed());
        if is_archive {
            send(Job::ScanArchive { path: path.to_path_buf(), rel, filename, size, created, modified: mtime, accessed });
        } else {
            send(Job::HashLoose { path: path.to_path_buf(), rel, filename, size, created, modified: mtime, accessed });
        }
    }
}
```

> **Note:** the skip-check uses `if let Ok(Some(..))` — a read error on the RO connection degrades to
> "not catalogued" (re-hash), which is safe (never skips a file we shouldn't). Today's code used `?`
> on `get_file_meta`; here the walker cannot propagate, and re-hashing on a transient read error is
> the conservative direction.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib scan_pipeline::tests`
Expected: PASS — all pipeline tests (Task 2 + Task 3).

- [ ] **Step 6: Commit**

```bash
git add src/scan_pipeline.rs src/scanner.rs
git commit -m "feat(scanner): pipeline walker produces jobs with skip-check

Walks + stats + incremental skip-check on a read-only connection, emitting a
Touch for unchanged files and a hash/scan job otherwise. Walk and stat
failures become Job::Error, matching the pre-parallel scan.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: The writer (`write_results`) — apply results in the transaction

**Files:**
- Modify: `src/scan_pipeline.rs`
- Modify: `src/scanner.rs` (make `rotate_batch` `pub(crate)`; expose `ScanSummary` already public)

**Interfaces:**
- Consumes: `ScanResult` (Task 1); `crate::scanner::{ScanSummary, rotate_batch, Progress}`; `crate::catalog::Catalog` (read-write); `crossbeam_channel::Receiver`.
- Produces: `fn write_results(cat: &Catalog, results: crossbeam_channel::Receiver<ScanResult>, identity: &VolumeIdentity, scan_started_at: i64, now: i64, metrics: &ScanMetrics, progress: Option<&dyn Progress>) -> anyhow::Result<ScanSummary>`. Owns `BEGIN`..`COMMIT`, batches, runs the missing-sweep. Task 5 runs this on the writer thread.

- [ ] **Step 1: Make `rotate_batch` reachable**

In `src/scanner.rs`, change `fn rotate_batch` to `pub(crate) fn rotate_batch`. Body unchanged.

- [ ] **Step 2: Write the failing test**

Append to `src/scan_pipeline.rs` tests:

```rust
    #[test]
    fn writer_applies_results_and_counts_them() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(), label: "D".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        }).unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(ScanResult::Upsert(crate::catalog::models::NewFile {
            volume_id: "v1".into(), relative_path: "a.txt".into(), filename: "a.txt".into(),
            extension: "txt".into(), size_bytes: 3, content_hash: "H".into(),
            created_time: None, modified_time: Some(7), accessed_time: None,
            category: crate::category::Category::Other, container_chain: None,
        })).unwrap();
        tx.send(ScanResult::Error { rel: "bad.txt".into(), reason: "read: nope".into() }).unwrap();
        drop(tx);

        let m = ScanMetrics::new();
        let ident = VolumeIdentity { volume_id: "v1".into(), label: "D".into(), identified_by: "marker".into() };
        let summary = write_results(&cat, rx, &ident, 100, 100, &m, None).unwrap();

        assert_eq!(summary.hashed, 1);
        assert_eq!(summary.errors, 1);
        // The row landed.
        let hits = cat.search("a.txt", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content_hash, "H");
        // The error was logged.
        assert!(cat.volume_has_scan_errors("v1").unwrap());
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib scan_pipeline::tests::writer_applies`
Expected: FAIL — `write_results` not defined.

- [ ] **Step 4: Implement `write_results`**

Add to `src/scan_pipeline.rs`. `BATCH_SIZE` mirrors the scanner's; import or redefine as a `const`:

```rust
use crate::scanner::{Progress, ScanSummary};
use crossbeam_channel::Receiver;

const BATCH_SIZE: usize = 200;

/// Drain every result into the catalogue inside one batched transaction, then run the missing-sweep.
/// The single writer: only this function writes to SQLite during a scan. `db_write` timing is charged
/// here. On any DB error it aborts (the caller rolls back).
pub(crate) fn write_results(
    cat: &Catalog,
    results: Receiver<ScanResult>,
    identity: &VolumeIdentity,
    scan_started_at: i64,
    now: i64,
    metrics: &ScanMetrics,
    progress: Option<&dyn Progress>,
) -> anyhow::Result<ScanSummary> {
    let mut summary = ScanSummary::default();
    let mut in_batch = 0usize;
    cat.conn.execute_batch("BEGIN")?;

    for result in results {
        let _t = metrics.timer(Phase::DbWrite);
        match result {
            ScanResult::Touch { rel, is_archive } => {
                cat.touch_seen(&identity.volume_id, &rel, now)?;
                if is_archive {
                    cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                }
                summary.skipped += 1;
                if let Some(p) = progress { p.on_skipped(); }
                in_batch += 1;
            }
            ScanResult::Error { rel, reason } => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &reason, now)?;
                summary.errors += 1;
                if let Some(p) = progress { p.on_error(); }
                // Touch so a readable-but-unhashable file is not swept to 'missing' (matches today).
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                in_batch += 1;
            }
            ScanResult::Upsert(nf) => {
                cat.upsert_file(&nf, now)?;
                summary.hashed += 1;
                if let Some(p) = progress { p.on_hashed(); }
                in_batch += 1;
            }
            ScanResult::ArchiveEntries { rel, modified, scan } => {
                for entry in &scan.entries {
                    cat.upsert_archive_entry(&identity.volume_id, &rel, entry, modified, now)?;
                    summary.archive_entries += 1;
                    if let Some(p) = progress { p.on_archive_entry(); }
                    in_batch += 1;
                }
                for (chain, reason) in &scan.errors {
                    cat.log_scan_error(Some(&identity.volume_id), chain, reason, now)?;
                    summary.errors += 1;
                    if let Some(p) = progress { p.on_error(); }
                }
            }
        }
        crate::scanner::rotate_batch(cat, &mut in_batch)?;
    }

    {
        let _t = metrics.timer(Phase::DbWrite);
        cat.conn.execute_batch("COMMIT")?;
        summary.marked_missing = cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    }
    summary.metrics = metrics.snapshot();
    Ok(summary)
}
```

> **Behaviour note:** the pre-parallel scan logged archive-level errors (`res.errors`) too — this
> preserves that. Verify `ArchiveScanResult.errors` is `Vec<(String, String)>` (chain, reason) in
> `src/archive.rs`; if the tuple order differs, match it.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib scan_pipeline::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/scan_pipeline.rs src/scanner.rs
git commit -m "feat(scanner): pipeline writer applies results in one transaction

The sole SQLite writer during a scan: drains results, batches upserts and
touches, logs errors, runs the missing-sweep on close. Reproduces the
pre-parallel error handling (touch-on-error, archive-level error logging).

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Orchestrator — spawn the pipeline, delete the old loop

**Files:**
- Modify: `src/scanner.rs` (rewrite `scan_volume_with_progress`; delete `descend_archive`; add `jobs` param; `scan_volume` keeps its signature, passes `jobs=1`)

**Interfaces:**
- Consumes: `scan_pipeline::{walk, run_job, write_results, Job, ScanResult}`; `thread_priority`.
- Produces: `pub fn scan_volume_with_progress(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64, progress: Option<&dyn Progress>, metrics: &ScanMetrics, jobs: usize) -> anyhow::Result<ScanSummary>` (adds `jobs`). `scan_volume` signature unchanged. Task 6 passes `jobs` from the CLI.

**Threading model — read before implementing.** The writer needs its own read-write `Catalog` moved into its thread, but the caller passes `&Catalog`. To avoid a large ownership refactor of `run_scan`, the orchestrator opens **fresh connections from the catalog path** for the walker (read-only) and the writer (read-write), and uses `std::thread::scope` so the `metrics`, `progress`, and `identity` borrows can be shared across threads without `'static` bounds. The passed `&Catalog` (`cat`) is used only to learn the path.

- [ ] **Step 1: Confirm the catalog path is reachable**

`Catalog` must expose its path. Check `src/catalog/mod.rs` for a `path` field or accessor. If absent, add:
```rust
// in the Catalog struct: it already holds `conn`; add the db path.
pub struct Catalog { pub conn: Connection, pub path: std::path::PathBuf }
```
and set `path` in both `open` and `open_readonly`. (If a path field/accessor already exists, use it and skip this.) This is a prerequisite; do it first and run `cargo build`.

- [ ] **Step 2: Write the anchor test**

Append to the `#[cfg(test)] mod tests` in `src/scanner.rs`:

```rust
/// A scan must produce an identical catalogue regardless of --jobs. This is what makes the single
/// parallel implementation safe.
#[test]
fn identical_catalogue_at_any_job_count() {
    fn scan_into(jobs: usize) -> Vec<(String, String, i64, String)> {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("drive");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..50 {
            std::fs::write(root.join(format!("f{i}.txt")), format!("content-{i}")).unwrap();
        }
        std::fs::write(root.join("sub/dup.txt"), b"content-0").unwrap(); // a duplicate
        {
            let f = std::fs::File::create(root.join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file("inner.txt", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"zipped").unwrap();
            zw.finish().unwrap();
        }
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, jobs).unwrap();
        // Snapshot every file row, ordered, comparable.
        let mut stmt = cat.conn.prepare(
            "SELECT relative_path, content_hash, size_bytes, IFNULL(container_chain,'') \
             FROM files ORDER BY relative_path, container_chain").unwrap();
        stmt.query_map([], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?,
        ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    }
    let one = scan_into(1);
    let eight = scan_into(8);
    assert!(!one.is_empty());
    assert_eq!(one, eight, "the catalogue must not depend on --jobs");
}
```

> The existing scanner tests call `scan_volume(&cat, &root, &ident(), false, 100)` — that signature is
> unchanged, so they keep compiling. Only `scan_volume_with_progress` gains `jobs`; update its
> existing test call site(s) (search for `scan_volume_with_progress(` in `src/scanner.rs`).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib scanner::tests::identical_catalogue`
Expected: FAIL — arity mismatch (`scan_volume_with_progress` has no `jobs` param yet).

- [ ] **Step 4: Rewrite `scan_volume_with_progress` and delete the old loop**

Replace the entire body of `scan_volume_with_progress` (the `for`/`loop` and everything through the final `Ok(summary)`) with the orchestrator below, and **delete `descend_archive` entirely**. Keep the doc comment; add `jobs` to the signature.

```rust
pub fn scan_volume_with_progress(
    cat: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    now: i64,
    progress: Option<&dyn Progress>,
    metrics: &crate::scan_metrics::ScanMetrics,
    jobs: usize,
) -> anyhow::Result<ScanSummary> {
    use crate::scan_pipeline::{run_job, walk, write_results};

    let jobs = jobs.max(1);
    let db_path = cat.path.clone();

    // Bounded channels give backpressure: the walker blocks when workers are saturated.
    let (job_tx, job_rx) = crossbeam_channel::bounded::<crate::scan_pipeline::Job>(jobs * 4);
    let (res_tx, res_rx) = crossbeam_channel::bounded::<crate::scan_pipeline::ScanResult>(jobs * 4);

    let volume_id = identity.volume_id.clone();

    let summary = std::thread::scope(|scope| -> anyhow::Result<ScanSummary> {
        // Writer: owns a fresh read-write connection.
        let writer_path = db_path.clone();
        let writer = scope.spawn(move || -> anyhow::Result<ScanSummary> {
            let wcat = Catalog::open(&writer_path)?;
            match write_results(&wcat, res_rx, identity, now, now, metrics, progress) {
                Ok(s) => Ok(s),
                Err(e) => {
                    let _ = wcat.conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        });

        // Workers: no DB. Below-normal priority so the machine stays usable.
        let mut worker_handles = Vec::new();
        for _ in 0..jobs {
            let jr = job_rx.clone();
            let rt = res_tx.clone();
            let vid = volume_id.clone();
            worker_handles.push(scope.spawn(move || {
                let _ = thread_priority::set_current_thread_priority(
                    thread_priority::ThreadPriority::Min,
                );
                for job in jr {
                    for result in run_job(job, &vid, metrics) {
                        if rt.send(result).is_err() {
                            return; // writer gone; stop
                        }
                    }
                }
            }));
        }
        drop(job_rx);
        drop(res_tx); // workers hold the remaining senders; results channel closes when they finish

        // Walker: own read-only connection.
        let rocat = Catalog::open_readonly(&db_path)?;
        walk(&rocat, root, identity, force, now, metrics, &job_tx);
        drop(job_tx); // signal workers no more jobs

        for h in worker_handles {
            h.join().map_err(|_| anyhow::anyhow!("scan worker panicked"))?;
        }
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("scan writer panicked"))?
    })?;

    Ok(summary)
}
```

> **Why `thread::scope`:** it lets the worker closures borrow `metrics`, `progress`, and `identity`
> (non-`'static`) without `Arc`, and guarantees all threads join before the function returns — no
> detached thread can outlive the borrowed data. The writer and walker each open their **own**
> connection from `db_path`; the passed `cat` is used only for its `.path`.
>
> **Ordering of drops matters:** `res_tx` is dropped in the parent after spawning workers, but each
> worker holds a clone; the results channel only closes once every worker exits, which is what lets
> the writer drain to completion. Do not drop the worker `rt` clones early.

- [ ] **Step 4b: Fix the one pre-existing test the pipeline invalidates**

In `src/scanner.rs`'s test module, the test `scan_records_phase_timings_and_the_size_histogram`
asserts `m.total_phase_ms() <= m.wall_ms`. Under the pipeline the phases run on separate threads and
overlap, so that sum can legitimately exceed wall-clock. **Delete that assertion and its comment**
(the block that reads "Upper bound only: on a fast disk these phases legitimately round to 0 ms …"
through the `assert!(m.total_phase_ms() <= m.wall_ms, …)`). Keep every counter and histogram
assertion in the test (`files_seen`, `histogram[..]`, `bytes_hashed`, `bytes_skipped`). Those remain
true and are what the test is really for.

- [ ] **Step 5: Update `scan_volume` to pass `jobs=1`**

Ensure `scan_volume` (the no-progress wrapper) calls the new signature:
```rust
pub fn scan_volume(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64) -> anyhow::Result<ScanSummary> {
    let metrics = crate::scan_metrics::ScanMetrics::new();
    scan_volume_with_progress(cat, root, identity, force, now, None, &metrics, 1)
}
```

- [ ] **Step 6: Run the anchor test and the whole scanner suite**

Run: `cargo test --lib scanner`
Expected: PASS, including `identical_catalogue_at_any_job_count` and every pre-existing scanner test unmodified.

- [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: PASS. Any failure here means a behaviour regression — fix the pipeline, not the test.

- [ ] **Step 8: Commit**

```bash
git add src/scanner.rs src/catalog/mod.rs
git commit -m "feat(scanner): parallel pipeline is now the scan; delete the serial loop

scan_volume_with_progress spawns a walker + N workers + a writer via
thread::scope. descend_archive and the inline read->hash->write loop are
gone. --jobs=1 is one worker and is proven to produce a catalogue identical
to --jobs=8 by the anchor test.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Plumb `--jobs` through run_scan / CLI / web, and record it

**Files:**
- Modify: `src/scanner.rs` (`run_scan` gains `jobs`)
- Modify: `src/commands.rs` (`cmd_scan` gains `jobs`)
- Modify: `src/main.rs` (`Scan { --jobs }`)
- Modify: `src/scan_queue.rs` (web passes default jobs)
- Modify: `src/catalog/schema.rs` (`scan_runs.jobs` column)
- Modify: `src/catalog/scan_runs.rs` (`start_scan_run` records jobs; `ScanRun.jobs`)

**Interfaces:**
- Consumes: `scan_volume_with_progress(..., jobs)` (Task 5).
- Produces: `run_scan(cat, root, force, fallback, now, progress, jobs)`; CLI `--jobs`.

- [ ] **Step 1: Add `jobs` to `run_scan`**

In `src/scanner.rs`, add `jobs: usize` as the last parameter of `run_scan`, and pass it to the
`scan_volume_with_progress(...)` call inside it. Update `run_scan`'s doc comment to mention it.

- [ ] **Step 2: Record `jobs` on the scan_runs row**

In `src/catalog/schema.rs`, add to the `scan_runs` `CREATE TABLE` (after `forced`):
```sql
            jobs            INTEGER NOT NULL DEFAULT 1,
```
This is an additive column on a fresh-or-migrated table; because `scan_runs` is created with
`CREATE TABLE IF NOT EXISTS`, an existing catalogue needs the column added. Add an `ensure_column`
call next to the others in `apply`:
```rust
    ensure_column(conn, "scan_runs", "jobs", "INTEGER NOT NULL DEFAULT 1")?;
```

In `src/catalog/scan_runs.rs`:
- Add `pub jobs: i64` to `ScanRun`.
- Change `start_scan_run` to accept `jobs: i64` and insert it (add `jobs` to the column list and a
  `?` bind). Update its callers.
- Add `jobs` to the `SELECT` in `recent_scan_runs` and read it into `ScanRun`.

In `src/scanner.rs`, `run_scan` calls `start_scan_run(...)` — pass `jobs as i64`.

- [ ] **Step 3: CLI wiring**

In `src/main.rs`, add to the `Scan` subcommand:
```rust
        /// Number of parallel read+hash workers (default 4; use 1 to disable parallelism).
        #[arg(long, default_value_t = 4)]
        jobs: usize,
```
and pass it: `Command::Scan { path, force, readonly_fallback, jobs } => commands::cmd_scan(&path, force, readonly_fallback, jobs),`.

In `src/commands.rs`, add `jobs: usize` to `cmd_scan` and pass it to `run_scan(..., jobs)`.

- [ ] **Step 4: Web wiring**

In `src/scan_queue.rs`, the `run_scan(...)` call passes the default: add `4` as the `jobs` argument.
(The web UI does not expose `--jobs`; 4 is the default.)

- [ ] **Step 5: Fix all other `run_scan` / `scan_volume_with_progress` callers**

Search and update every call site the compiler flags:
```
cargo build 2>&1 | grep -E "error\[E0061\]" 
```
Test callers of `run_scan` pass `1` (or `4`) as `jobs`; test callers of `scan_volume_with_progress`
add the `jobs` argument. `scan_volume` callers are unaffected (its signature is unchanged).

- [ ] **Step 6: Add a test that jobs is recorded**

In `src/catalog/scan_runs.rs` tests, extend a start/finish test to assert `recent_scan_runs()[0].jobs`
equals what was passed to `start_scan_run`.

- [ ] **Step 7: Full gates**

Run: `cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add -A src
git commit -m "feat(cli): --jobs for the scan, recorded on the run row

scan --jobs N (default 4) sets the worker count; the web scan uses 4. The
chosen value is stored on scan_runs so a later comparison knows the
concurrency each run used.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## After the plan: the measurement that proves it

Building the pipeline is not the same as proving it helped — the same discipline as #22:

1. On the real drive (or a faithful large synthetic tree), `scan --force --jobs 1` then `--jobs 4`,
   both with the Defender exclusion in place (so the two effects don't confound).
2. Compare wall-clock and `overlap_ratio()` (which should rise above 1.0 with `--jobs 4`).
3. Post the before/after on #23. If concurrency did **not** help on the spinning disk (seek thrash),
   that is itself the finding — record it and reconsider the default.

Do not claim the win from the design; claim it from the measurement.

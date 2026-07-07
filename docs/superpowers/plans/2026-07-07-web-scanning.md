# Web Scanning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user start a drive scan from the web UI — pick or type a folder (native folder picker + detected-drives list), run it in the background one-at-a-time with a queue, and watch live per-file progress.

**Architecture:** An in-memory single-worker scan queue (`Arc<ScanQueue>`) lives in `AppState`; a background task spawned at server start runs queued scans on `spawn_blocking`, updating atomic progress counters and a capped history. The scanner core gains one purely-additive `scan_volume_with_progress` (the existing `scan_volume` becomes a `None` wrapper — no existing call site changes). New CSRF-guarded endpoints enqueue scans and open a native folder dialog; a `/scan` page drives it all.

**Tech Stack:** Rust 1.88, existing deps, plus `rfd = "0.15"` (native folder dialog). Reuses `axum`, `tokio`, `sysinfo`, the `mounts`/`volume`/`scanner`/`backup` modules.

## Global Constraints

- **Reliability unchanged:** the only scanner-core change is an additive `Option<&dyn Progress>` path; `scan_volume` keeps its exact current behavior (delegates with `None`). All catalog mutations go through the existing `scan_volume` + `backup::snapshot` path.
- **Localhost only:** server still binds `127.0.0.1`. `POST /api/scan` and `POST /api/pick-folder` are **CSRF-guarded** (header `x-cleanup-token` == `state.csrf_token`, checked first → 403). Read endpoints need no token.
- **No mid-scan prompts:** web scans always use the fingerprint read-only fallback (`volume::ReadonlyMode::Fingerprint`).
- **One scan at a time**, extras queued FIFO. A failed scan is isolated (recorded with an `error_message`) and never wedges the queue or crashes the worker/server.
- **Self-contained, XSS-safe pages** (inlined CSS/JS, no external requests, all dynamic strings escaped), CSRF token in a `<meta>` tag — same conventions as the existing browse/review pages.
- **Git:** branch `feat/web-scanning` off `main`. Conventional Commits, scopes `scanner`/`web`/`cli`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (merged):** `scanner::scan_volume`/`ScanSummary`, `volume::{resolve, ReadonlyMode, VolumeIdentity}`, `catalog::{Catalog, backup::snapshot, models::Volume}`, `mounts::{MountResolver, scan_mounts, live_mounts}`, `config::Config`, the 2b web server (`AppState`, `build_router_with`, CSRF pattern, `serve`). **Out of scope:** concurrent scans; cancelling a running scan; per-file path display / ETA; scheduling.

---

## File Structure

- `Cargo.toml` — add `rfd = "0.15"`.
- `src/scanner.rs` — add `Progress` trait + `scan_volume_with_progress`; `scan_volume` becomes a wrapper. Add a shared `run_scan` helper.
- `src/catalog/mod.rs` — add a `busy_timeout` to `Catalog::open` (concurrency hardening).
- `src/commands.rs` — refactor `cmd_scan` to call the shared `run_scan`.
- `src/scan_queue.rs` — **new**: `ScanQueue`, `Counters` (a `Progress` impl), the worker loop, status snapshot types. Registered in `lib.rs`.
- `src/web.rs` — `AppState` gains `scan_queue`; spawn the worker in `serve`; new endpoints + `/scan` page; nav links.
- `src/lib.rs` — `pub mod scan_queue;`.
- `tests/web_scan_flow.rs` — **new** real-TCP e2e.

---

### Task 1: Scanner progress hook

**Files:**
- Modify: `src/scanner.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub trait Progress: Send + Sync { fn on_hashed(&self); fn on_skipped(&self); fn on_error(&self); fn on_archive_entry(&self); }`
  - `pub fn scan_volume_with_progress(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64, progress: Option<&dyn Progress>) -> anyhow::Result<ScanSummary>`
  - `pub fn scan_volume(cat, root, identity, force, now) -> anyhow::Result<ScanSummary>` — unchanged signature; now delegates to `scan_volume_with_progress(..., None)`. **No existing caller changes.**

- [ ] **Step 1: Write the failing test**

Add to `src/scanner.rs` `mod tests`:

```rust
    struct CountingProgress {
        hashed: std::sync::atomic::AtomicUsize,
        skipped: std::sync::atomic::AtomicUsize,
        errors: std::sync::atomic::AtomicUsize,
        arch: std::sync::atomic::AtomicUsize,
    }
    impl CountingProgress { fn new() -> Self { Self {
        hashed: 0.into(), skipped: 0.into(), errors: 0.into(), arch: 0.into() } } }
    impl Progress for CountingProgress {
        fn on_hashed(&self) { self.hashed.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_skipped(&self) { self.skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_error(&self) { self.errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_archive_entry(&self) { self.arch.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    }

    #[test]
    fn progress_callbacks_match_summary() {
        use std::sync::atomic::Ordering::Relaxed;
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta").unwrap();

        let p = CountingProgress::new();
        let s = scan_volume_with_progress(&cat, &root, &ident(), false, 100, Some(&p)).unwrap();
        assert_eq!(p.hashed.load(Relaxed), s.hashed);
        assert_eq!(p.skipped.load(Relaxed), s.skipped);
        assert_eq!(p.errors.load(Relaxed), s.errors);
        assert_eq!(p.arch.load(Relaxed), s.archive_entries);
        assert_eq!(s.hashed, 2);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib scanner::tests::progress_callbacks_match_summary`
Expected: FAIL — `Progress`/`scan_volume_with_progress` not found.

- [ ] **Step 3: Implement**

In `src/scanner.rs`:

1. Add the trait near the top (after the imports):

```rust
/// Optional live-progress sink for a scan. Each method fires once per counted event.
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
}
```

2. Rename the current `pub fn scan_volume(...) -> anyhow::Result<ScanSummary> { ... }` body to `scan_volume_with_progress` by adding the parameter, and add a thin `scan_volume` wrapper. Concretely, change the signature line to:

```rust
pub fn scan_volume_with_progress(
    cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64,
    progress: Option<&dyn Progress>,
) -> anyhow::Result<ScanSummary> {
```

3. Inside that function body, at EACH existing count site, also fire the callback. There are four sites — after each `summary.<field> += 1`, add the matching call guarded by `if let Some(p) = progress`:
   - where `summary.skipped += 1;` (incremental-skip branch): add `if let Some(p) = progress { p.on_skipped(); }`
   - where `summary.errors += 1;` (the walk-error arm, the metadata-error arm, and the hash-error arm — every place errors is incremented): add `if let Some(p) = progress { p.on_error(); }`
   - where `summary.hashed += 1;` (after `upsert_file`): add `if let Some(p) = progress { p.on_hashed(); }`
   - In `descend_archive`, where `summary.archive_entries += 1;`: add `if let Some(p) = progress { p.on_archive_entry(); }`. Since `descend_archive` doesn't currently receive `progress`, thread it through: add `progress: Option<&dyn Progress>` as a parameter to `descend_archive` and pass it at the call site in `scan_volume_with_progress`.

4. Add the wrapper below the function:

```rust
/// Scan without progress reporting (CLI and tests). Delegates with `None`.
pub fn scan_volume(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64)
    -> anyhow::Result<ScanSummary>
{
    scan_volume_with_progress(cat, root, identity, force, now, None)
}
```

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib scanner` then `cargo test`
Expected: PASS — the new progress test passes; all existing scanner/quarantine/repack/integration tests (which call `scan_volume`) are unchanged and green.

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/web-scanning   # only if not already on it
git add src/scanner.rs
git commit -m "feat(scanner): additive progress hook (scan_volume_with_progress)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Shared `run_scan` helper + `Catalog::open` busy-timeout + `cmd_scan` refactor

**Files:**
- Modify: `src/scanner.rs` (add `run_scan`)
- Modify: `src/catalog/mod.rs` (busy_timeout on `open`)
- Modify: `src/commands.rs` (`cmd_scan` uses `run_scan`)
- Test: inline in `src/scanner.rs`

**Interfaces:**
- Produces:
  - `pub fn run_scan(cat: &Catalog, mount_root: &Path, force: bool, fallback: crate::volume::ReadonlyMode, now: i64, progress: Option<&dyn Progress>) -> anyhow::Result<Option<(VolumeIdentity, ScanSummary)>>` — resolves the volume identity (with `fallback`), upserts the volume, runs `scan_volume_with_progress`. `Ok(None)` iff the drive was a skipped read-only drive. Shared by `cmd_scan` and the web worker so there is ONE definition of "how a scan works."

- [ ] **Step 1: Add busy-timeout to `Catalog::open`**

In `src/catalog/mod.rs`, inside `open` after the WAL/foreign-keys pragmas and before `schema::apply`, add:

```rust
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
```

(So a read-write open retries briefly instead of failing immediately if another writer holds the lock — matches `open_readonly`'s existing busy-timeout.)

- [ ] **Step 2: Write the failing test**

Add to `src/scanner.rs` `mod tests`:

```rust
    #[test]
    fn run_scan_resolves_upserts_and_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hello").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let out = run_scan(&cat, &root, false, crate::volume::ReadonlyMode::Fingerprint, 100, None)
            .unwrap();
        let (identity, summary) = out.expect("not skipped");
        assert_eq!(summary.hashed, 1);
        // the volume row exists after run_scan upserted it
        let stats = cat.volume_stats().unwrap();
        assert!(stats.iter().any(|(id, _, _, _)| id == &identity.volume_id));
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --lib scanner::tests::run_scan_resolves_upserts_and_scans`
Expected: FAIL — `run_scan` not found.

- [ ] **Step 4: Implement `run_scan`**

Add to `src/scanner.rs` (needs `use crate::catalog::models::Volume;` — add if not present):

```rust
/// Resolve identity, upsert the volume, and scan. `Ok(None)` iff a read-only drive was skipped.
pub fn run_scan(
    cat: &Catalog, mount_root: &Path, force: bool, fallback: crate::volume::ReadonlyMode,
    now: i64, progress: Option<&dyn Progress>,
) -> anyhow::Result<Option<(VolumeIdentity, ScanSummary)>> {
    let identity = match crate::volume::resolve(mount_root, fallback)? {
        Some(id) => id,
        None => return Ok(None),
    };
    cat.upsert_volume(&crate::catalog::models::Volume {
        volume_id: identity.volume_id.clone(),
        label: identity.label.clone(),
        identified_by: identity.identified_by.clone(),
        first_seen_at: now, last_seen_at: now,
    })?;
    let summary = scan_volume_with_progress(cat, mount_root, &identity, force, now, progress)?;
    Ok(Some((identity, summary)))
}
```

- [ ] **Step 5: Refactor `cmd_scan` to use `run_scan`**

In `src/commands.rs`, replace the body of `cmd_scan` between opening the catalog and the snapshot with a `run_scan` call. The new `cmd_scan`:

```rust
pub fn cmd_scan(path: &Path, force: bool, fallback: ReadonlyFallback) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }
    let now = now_secs();
    match scanner::run_scan(&cat, path, force, fallback.into(), now, None)? {
        None => { println!("Skipped read-only drive at {}", path.display()); return Ok(()); }
        Some((identity, s)) => {
            println!("Scanned {} (volume {}, id by {})", path.display(), identity.label, identity.identified_by);
            println!("Done: {} hashed, {} unchanged, {} errors, {} newly missing, {} archive entries.",
                s.hashed, s.skipped, s.errors, s.marked_missing, s.archive_entries);
        }
    }
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}
```

(The `use` for `Volume` in `commands.rs` may now be unused — remove it if the compiler warns.)

- [ ] **Step 6: Run tests + full suite**

Run: `cargo test`
Expected: PASS — the `scan_and_search` integration test (drives `cmd_scan`) still passes; new `run_scan` test passes.

- [ ] **Step 7: Commit**

```bash
git add src/scanner.rs src/catalog/mod.rs src/commands.rs
git commit -m "refactor(scanner): shared run_scan helper; busy_timeout on Catalog::open

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: The scan queue + worker

**Files:**
- Create: `src/scan_queue.rs`
- Modify: `src/lib.rs` (`pub mod scan_queue;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct Counters { hashed, skipped, errors, archive_entries: AtomicUsize }` implementing `scanner::Progress`; `pub fn snapshot(&self) -> (usize,usize,usize,usize)`.
  - `pub struct ScanResult { pub path: String, pub hashed, skipped, errors, archive_entries, marked_missing: usize, pub error_message: Option<String> }` (Clone, Serialize).
  - `pub struct RunningDto { pub path: String, pub hashed, skipped, errors, archive_entries: usize }` and `pub struct StatusSnapshot { pub running: Option<RunningDto>, pub queued: Vec<String>, pub recent: Vec<ScanResult> }` (Serialize).
  - `pub struct ScanQueue { catalog_path: PathBuf, inner: Mutex<Inner>, notify: tokio::sync::Notify }`.
  - `impl ScanQueue { pub fn new(catalog_path: PathBuf) -> Arc<ScanQueue>; pub fn enqueue(self: &Arc<Self>, path: PathBuf, force: bool) -> usize /*queue position*/; pub fn status(&self) -> StatusSnapshot; pub async fn run_worker(self: Arc<Self>) }`.
- Consumes: `scanner::run_scan`, `catalog::{Catalog, backup}`, `config::Config`, `volume::ReadonlyMode`.

- [ ] **Step 1: Write failing tests**

Create `src/scan_queue.rs`:

```rust
//! In-memory single-worker scan queue: runs drive scans one at a time in the background,
//! exposing live progress and a small history for the web UI.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use serde::Serialize;

pub struct Counters {
    pub hashed: AtomicUsize, pub skipped: AtomicUsize,
    pub errors: AtomicUsize, pub archive_entries: AtomicUsize,
}
impl Counters {
    fn new() -> Arc<Counters> { Arc::new(Counters {
        hashed: 0.into(), skipped: 0.into(), errors: 0.into(), archive_entries: 0.into() }) }
    pub fn snapshot(&self) -> (usize, usize, usize, usize) {
        (self.hashed.load(Ordering::Relaxed), self.skipped.load(Ordering::Relaxed),
         self.errors.load(Ordering::Relaxed), self.archive_entries.load(Ordering::Relaxed))
    }
}
impl crate::scanner::Progress for Counters {
    fn on_hashed(&self) { self.hashed.fetch_add(1, Ordering::Relaxed); }
    fn on_skipped(&self) { self.skipped.fetch_add(1, Ordering::Relaxed); }
    fn on_error(&self) { self.errors.fetch_add(1, Ordering::Relaxed); }
    fn on_archive_entry(&self) { self.archive_entries.fetch_add(1, Ordering::Relaxed); }
}

#[derive(Clone, Serialize)]
pub struct ScanResult {
    pub path: String,
    pub hashed: usize, pub skipped: usize, pub errors: usize,
    pub archive_entries: usize, pub marked_missing: usize,
    pub error_message: Option<String>,
}

#[derive(Serialize)]
pub struct RunningDto { pub path: String, pub hashed: usize, pub skipped: usize,
    pub errors: usize, pub archive_entries: usize }

#[derive(Serialize)]
pub struct StatusSnapshot {
    pub running: Option<RunningDto>,
    pub queued: Vec<String>,
    pub recent: Vec<ScanResult>,
}

struct Job { path: PathBuf, force: bool }
struct Running { path: String, counters: Arc<Counters> }

struct Inner {
    pending: VecDeque<Job>,
    running: Option<Running>,
    recent: VecDeque<ScanResult>,
}

pub struct ScanQueue {
    catalog_path: PathBuf,
    inner: Mutex<Inner>,
    notify: tokio::sync::Notify,
}

const RECENT_CAP: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_drive() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let drive = tmp.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join("a.txt"), b"one").unwrap();
        std::fs::write(drive.join("b.txt"), b"two").unwrap();
        let db = tmp.path().join("c.db");
        { crate::catalog::Catalog::open(&db).unwrap(); } // create the catalog file
        (tmp, drive, db)
    }

    #[tokio::test]
    async fn worker_runs_a_queued_scan_and_records_result() {
        let (_t, drive, db) = make_drive();
        let q = ScanQueue::new(db);
        let worker = tokio::spawn(q.clone().run_worker());
        q.enqueue(drive.clone(), false);
        // poll until the scan lands in recent
        let result = loop {
            let s = q.status();
            if let Some(r) = s.recent.first() { break r.clone(); }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(result.hashed, 2);
        assert_eq!(result.error_message, None);
        worker.abort();
    }

    #[tokio::test]
    async fn failing_scan_records_error_and_queue_continues() {
        let (_t, drive, db) = make_drive();
        let q = ScanQueue::new(db);
        let worker = tokio::spawn(q.clone().run_worker());
        // a bad path fails; a good path after it still runs
        q.enqueue(PathBuf::from("Z:/does/not/exist/at/all"), false);
        q.enqueue(drive.clone(), false);
        let good = loop {
            let s = q.status();
            if s.recent.iter().any(|r| r.error_message.is_none() && r.hashed == 2) {
                break true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert!(good);
        let s = q.status();
        assert!(s.recent.iter().any(|r| r.error_message.is_some())); // the bad one recorded
        worker.abort();
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib scan_queue`
Expected: FAIL — `ScanQueue::new`/`run_worker`/`enqueue`/`status` not implemented.

- [ ] **Step 3: Implement**

Add to `src/scan_queue.rs` (above the tests). Register `pub mod scan_queue;` in `src/lib.rs`.

```rust
impl ScanQueue {
    pub fn new(catalog_path: PathBuf) -> Arc<ScanQueue> {
        Arc::new(ScanQueue {
            catalog_path,
            inner: Mutex::new(Inner { pending: VecDeque::new(), running: None, recent: VecDeque::new() }),
            notify: tokio::sync::Notify::new(),
        })
    }

    /// Enqueue a scan; returns the number of jobs ahead of it (0 = will start next).
    pub fn enqueue(self: &Arc<Self>, path: PathBuf, force: bool) -> usize {
        let pos = {
            let mut inner = self.inner.lock().unwrap();
            inner.pending.push_back(Job { path, force });
            inner.pending.len() - 1 + inner.running.is_some() as usize
        };
        self.notify.notify_one();
        pos
    }

    pub fn status(&self) -> StatusSnapshot {
        let inner = self.inner.lock().unwrap();
        let running = inner.running.as_ref().map(|r| {
            let (hashed, skipped, errors, archive_entries) = r.counters.snapshot();
            RunningDto { path: r.path.clone(), hashed, skipped, errors, archive_entries }
        });
        StatusSnapshot {
            running,
            queued: inner.pending.iter().map(|j| j.path.display().to_string()).collect(),
            recent: inner.recent.iter().cloned().collect(),
        }
    }

    /// Background loop: run pending jobs one at a time forever.
    pub async fn run_worker(self: Arc<Self>) {
        loop {
            let job = {
                let mut inner = self.inner.lock().unwrap();
                inner.pending.pop_front()
            };
            match job {
                Some(job) => self.run_job(job).await,
                None => self.notify.notified().await,
            }
        }
    }

    async fn run_job(self: &Arc<Self>, job: Job) {
        let counters = Counters::new();
        let path_str = job.path.display().to_string();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = Some(Running { path: path_str.clone(), counters: counters.clone() });
        }

        // Run the blocking scan off the async runtime.
        let catalog_path = self.catalog_path.clone();
        let counters_for_job = counters.clone();
        let path = job.path.clone();
        let force = job.force;
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<ScanResult> {
            let cat = crate::catalog::Catalog::open(&catalog_path)?;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
            let progress: &dyn crate::scanner::Progress = counters_for_job.as_ref();
            let scanned = crate::scanner::run_scan(&cat, &path, force,
                crate::volume::ReadonlyMode::Fingerprint, now, Some(progress))?;
            // snapshot the catalog after a successful scan (best-effort)
            if let Ok(cfg) = crate::config::Config::default_paths() {
                let _ = crate::catalog::backup::snapshot(&catalog_path, &cfg.backups_dir(),
                    cfg.snapshot_retention, now);
            }
            let (hashed, skipped, errors, archive_entries) = counters_for_job.snapshot();
            Ok(match scanned {
                Some((_id, s)) => ScanResult { path: path.display().to_string(),
                    hashed: s.hashed, skipped: s.skipped, errors: s.errors,
                    archive_entries: s.archive_entries, marked_missing: s.marked_missing,
                    error_message: None },
                None => ScanResult { path: path.display().to_string(),
                    hashed, skipped, errors, archive_entries, marked_missing: 0,
                    error_message: Some("drive is read-only and was skipped".into()) },
            })
        }).await;

        let result = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => error_result(&path_str, &counters, e.to_string()),
            Err(join_err) => error_result(&path_str, &counters, format!("scan task failed: {join_err}")),
        };

        let mut inner = self.inner.lock().unwrap();
        inner.running = None;
        inner.recent.push_front(result);
        while inner.recent.len() > RECENT_CAP { inner.recent.pop_back(); }
    }
}

fn error_result(path: &str, counters: &Counters, message: String) -> ScanResult {
    let (hashed, skipped, errors, archive_entries) = counters.snapshot();
    ScanResult { path: path.to_string(), hashed, skipped, errors, archive_entries,
        marked_missing: 0, error_message: Some(message) }
}
```

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib scan_queue` then `cargo test`
Expected: PASS (both async tests) and no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/scan_queue.rs src/lib.rs
git commit -m "feat(scanner): in-memory single-worker scan queue with live counters + history

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: AppState wiring + spawn worker + `POST /api/scan` + `GET /api/scan/status`

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `AppState` gains `pub scan_queue: std::sync::Arc<crate::scan_queue::ScanQueue>`.
- `AppState::new_live` builds it via `ScanQueue::new(catalog_path.clone())`.
- `serve` spawns `tokio::spawn(state.scan_queue.clone().run_worker())` before serving.
- Routes `POST /api/scan` (CSRF; body `{ path: String, force: bool }` → enqueue → `{ queued_position }`) and `GET /api/scan/status` (→ `StatusSnapshot`).

- [ ] **Step 1: Add the field + update ALL `AppState` construction sites**

In `src/web.rs`, add to `AppState`:

```rust
    pub scan_queue: std::sync::Arc<crate::scan_queue::ScanQueue>,
```

Update `AppState::new_live` to set it:

```rust
    pub fn new_live(catalog_path: PathBuf) -> AppState {
        AppState {
            mounts: crate::mounts::MountResolver::Live,
            csrf_token: uuid::Uuid::new_v4().to_string(),
            scan_queue: crate::scan_queue::ScanQueue::new(catalog_path.clone()),
            catalog_path,
        }
    }
```

Then update EVERY OTHER place that constructs `AppState { ... }` literally (in `src/web.rs` tests: `seed_dupes`, and each inline test that builds `AppState { catalog_path, mounts, csrf_token }` — e.g. the quarantine/repack tests; and in `tests/review_flow.rs`). Add the field to each: `scan_queue: crate::scan_queue::ScanQueue::new(db.clone())` (or the appropriate catalog-path variable; in the integration test use `cleanupstorages::scan_queue::ScanQueue::new(db.clone())`). Grep first: `grep -rn "AppState {" src/ tests/` and fix each construction. (There is no `Default`; Rust requires all fields, so the compiler will flag any you miss.)

- [ ] **Step 2: Spawn the worker in `serve`**

In `serve`, after building `state` and before `axum::serve`, spawn the worker. Refactor `serve` so it constructs the state once, keeps a clone for the worker, and passes it to the router:

```rust
pub async fn serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()> {
    let state = AppState::new_live(catalog_path);
    tokio::spawn(state.scan_queue.clone().run_worker());
    let app = build_router_with(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("CleanUpStorages web UI at {url}");
    println!("(browse is read-only; scan/review can modify the catalog. Press Ctrl+C to stop)");
    if open {
        if let Err(e) = open_browser(&url) {
            eprintln!("could not open a browser automatically ({e}); open {url} yourself");
        }
    }
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 3: Write failing tests**

Add to `web.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn scan_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/scan", None,
            serde_json::json!({"path":"whatever","force":false})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn scan_enqueues_and_status_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join("a.txt"), b"hi").unwrap();
        { crate::catalog::Catalog::open(&db).unwrap(); }
        let state = AppState {
            catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(std::collections::HashMap::new()),
            csrf_token: "T".into(),
            scan_queue: crate::scan_queue::ScanQueue::new(db.clone()),
        };
        // must run the worker for the enqueued job to progress
        tokio::spawn(state.scan_queue.clone().run_worker());

        let (status, json) = post_json(state.clone(), "/api/scan", Some("T"),
            serde_json::json!({"path": drive.to_string_lossy(), "force": false})).await;
        assert_eq!(status, axum::http::StatusCode::OK, "body {json}");

        // poll status until the scan finishes
        let done = {
            let mut found = false;
            for _ in 0..200 {
                let v = get_json_state(state.clone(), "/api/scan/status").await;
                if v["recent"].as_array().map(|a| !a.is_empty()).unwrap_or(false) { found = true; break; }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            found
        };
        assert!(done, "scan should have completed and appeared in recent");
    }
```

(`get_json_state`/`post_json` already exist in the test module; `AppState` must derive `Clone` — it already does. `post_json`/`get_json_state` take `state` by value, so `.clone()` it.)

- [ ] **Step 4: Run to verify they fail**

Run: `cargo test --lib web`
Expected: FAIL — routes absent.

- [ ] **Step 5: Implement the endpoints**

In `src/web.rs`:

```rust
#[derive(Deserialize)]
struct ScanReq { path: String, force: bool }

#[derive(Serialize)]
struct ScanEnqueuedDto { queued_position: usize }

async fn api_scan(State(state): State<AppState>, headers: HeaderMap, body: Json<ScanReq>)
    -> Result<Json<ScanEnqueuedDto>, (StatusCode, String)>
{
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }
    let path = std::path::PathBuf::from(body.path.trim());
    if body.path.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".into()));
    }
    let pos = state.scan_queue.enqueue(path, body.force);
    Ok(Json(ScanEnqueuedDto { queued_position: pos }))
}

async fn api_scan_status(State(state): State<AppState>)
    -> Json<crate::scan_queue::StatusSnapshot>
{
    Json(state.scan_queue.status())
}
```

Register in `build_router_with`: `.route("/api/scan", post(api_scan)).route("/api/scan/status", get(api_scan_status))`.

- [ ] **Step 6: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): scan queue in AppState; POST /api/scan + GET /api/scan/status

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: `GET /api/detected-drives`

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Route `/api/detected-drives` → `Json<Vec<DetectedDriveDto>>`.
- `DetectedDriveDto { mount_path: String, volume_id: Option<String>, catalogued: bool, volume_label: Option<String> }`.
- For each mounted drive (from the resolver's snapshot when Live, or the Fixed map), read its marker (`volume::read_volume_id`); if present, look up the volume in the catalog for `catalogued`/`volume_label`.

- [ ] **Step 1: Write the failing test**

Add to `web.rs` `mod tests` (uses the `seed_dupes` fake drive which has a `.cleanupstorages_id` = "vol-1" and a catalogued "Photos HDD"):

```rust
    #[tokio::test]
    async fn detected_drives_flags_catalogued() {
        let (_t, _db, state) = seed_dupes(); // Fixed mount vol-1 -> driveA (marker vol-1), catalogued
        let v = get_json_state(state, "/api/detected-drives").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["catalogued"], true);
        assert_eq!(arr[0]["volume_label"], "Photos HDD");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — route absent.

- [ ] **Step 3: Implement**

Add a `snapshot()` use: `MountResolver` already has `snapshot()` (from Phase 2b). Implement:

```rust
#[derive(Serialize)]
struct DetectedDriveDto {
    mount_path: String, volume_id: Option<String>, catalogued: bool, volume_label: Option<String>,
}

async fn api_detected_drives(State(state): State<AppState>)
    -> Result<Json<Vec<DetectedDriveDto>>, (StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let labels: std::collections::HashMap<String, String> = cat.volume_stats().map_err(err500)?
        .into_iter().map(|(id, label, _, _)| (id, label)).collect();
    let mut out = Vec::new();
    for (_vid_key, root) in state.mounts.snapshot() {
        let volume_id = crate::volume::read_volume_id(&root);
        let (catalogued, volume_label) = match &volume_id {
            Some(vid) => (labels.contains_key(vid), labels.get(vid).cloned()),
            None => (false, None),
        };
        out.push(DetectedDriveDto {
            mount_path: root.display().to_string(), volume_id, catalogued, volume_label,
        });
    }
    out.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
    Ok(Json(out))
}
```

Register `.route("/api/detected-drives", get(api_detected_drives))`.

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): GET /api/detected-drives (connected drives, catalogued flag)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: `POST /api/pick-folder` (native folder dialog)

**Files:**
- Modify: `Cargo.toml` (add `rfd = "0.15"`)
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]` (CSRF gate only — the dialog needs a desktop)

**Interfaces:**
- Route `POST /api/pick-folder` (CSRF) → `Json<PickFolderDto { path: Option<String> }>`. Opens the native folder dialog via `rfd` on a blocking thread; returns the chosen path or `null` on cancel.

- [ ] **Step 1: Add dependency**

In `Cargo.toml` `[dependencies]`: `rfd = "0.15"`.

- [ ] **Step 2: Write the failing test (CSRF gate)**

Add to `web.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn pick_folder_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/pick-folder", None, serde_json::json!({})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — route absent.

- [ ] **Step 4: Implement**

```rust
#[derive(Serialize)]
struct PickFolderDto { path: Option<String> }

async fn api_pick_folder(State(state): State<AppState>, headers: HeaderMap)
    -> Result<Json<PickFolderDto>, (StatusCode, String)>
{
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }
    // The native dialog is blocking; run it off the async runtime.
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new().set_title("Choose a drive or folder to scan").pick_folder()
    }).await.map_err(err500)?;
    Ok(Json(PickFolderDto { path: picked.map(|p| p.display().to_string()) }))
}
```

Register `.route("/api/pick-folder", post(api_pick_folder))`.

- [ ] **Step 5: Run tests + build**

Run: `cargo test --lib web` then `cargo build`
Expected: PASS; builds (rfd pulls platform deps — first build slower).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/web.rs
git commit -m "feat(web): POST /api/pick-folder native folder dialog (CSRF-guarded)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: The `/scan` page + nav links + end-to-end

**Files:**
- Modify: `src/web.rs` (add `/scan` route + `SCAN_HTML`; add nav links on browse + review pages)
- Create: `tests/web_scan_flow.rs`
- Test: inline page test + real-TCP e2e

**Interfaces:**
- Route `/scan` → `Html<String>` (self-contained page, CSRF token in a `<meta>`).

- [ ] **Step 1: Write the failing page test**

Add to `web.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn scan_page_is_self_contained_and_wired() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/scan").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"csrf\""));
        assert!(body.contains("/api/scan"));
        assert!(body.contains("/api/detected-drives"));
        assert!(body.contains("/api/pick-folder"));
        assert!(!body.contains("http://") && !body.contains("https://"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — `/scan` route absent.

- [ ] **Step 3: Implement the page + route + nav links**

Add the handler and const:

```rust
async fn scan_page(State(state): State<AppState>) -> Html<String> {
    Html(SCAN_HTML.replace("{{CSRF}}", &state.csrf_token))
}

const SCAN_HTML: &str = r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf" content="{{CSRF}}">
<title>CleanUpStorages — Scan a drive</title>
<style>
  :root{color-scheme:light dark;--bg:#111;--fg:#eee;--mut:#999;--line:#333;--accent:#5aa0ff;}
  @media (prefers-color-scheme:light){:root{--bg:#fff;--fg:#111;--mut:#666;--line:#ddd;}}
  *{box-sizing:border-box;} body{margin:0;font:14px/1.4 system-ui,sans-serif;background:var(--bg);color:var(--fg);}
  header{padding:12px 16px;border-bottom:1px solid var(--line);display:flex;gap:12px;align-items:center;}
  header a{color:var(--accent);text-decoration:none;font-size:12px;}
  main{padding:16px;max-width:820px;margin:0 auto;}
  h2{font-size:14px;color:var(--mut);margin:18px 0 6px;}
  input,button{font:inherit;color:var(--fg);background:transparent;border:1px solid var(--line);border-radius:6px;padding:8px 10px;}
  input#path{width:100%;} .row{display:flex;gap:8px;margin:6px 0;}
  button.primary{border-color:var(--accent);color:var(--accent);cursor:pointer;}
  button{cursor:pointer;}
  .drive{border:1px solid var(--line);border-radius:8px;padding:8px 10px;margin:6px 0;cursor:pointer;}
  .drive:hover{border-color:var(--accent);}
  .drive .tag{font-size:11px;color:var(--mut);}
  label.chk{font-size:13px;color:var(--mut);display:flex;gap:6px;align-items:center;margin:6px 0;}
  .status{margin-top:14px;border-top:1px solid var(--line);padding-top:12px;}
  .bar{color:var(--mut);} .err{color:#e06c6c;}
  .recent div{padding:2px 0;border-bottom:1px solid var(--line);font-size:13px;}
</style></head>
<body>
<header><strong>Scan a drive</strong><a href="/">Browse</a><a href="/review">Review</a></header>
<main>
  <h2>Detected drives</h2>
  <div id="drives" class="bar">Looking for connected drives…</div>

  <h2>Or enter a path</h2>
  <div class="row"><input id="path" type="text" placeholder="e.g. D:\ or /Volumes/MyDrive"><button id="browse">Browse…</button></div>
  <label class="chk"><input id="force" type="checkbox"> Force full rescan (re-hash every file, slower)</label>
  <div class="row"><button class="primary" id="scan">Scan</button></div>

  <div class="status">
    <div id="running" class="bar"></div>
    <div id="queued" class="bar"></div>
    <h2>Recent scans</h2>
    <div id="recent" class="recent bar">None yet.</div>
  </div>
</main>
<script>
const $=s=>document.querySelector(s);
const CSRF=document.querySelector('meta[name="csrf"]').content;
function esc(s){return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));}
async function loadDrives(){
  try{
    const ds=await (await fetch("/api/detected-drives")).json();
    if(!ds.length){ $("#drives").textContent="No drives detected. Type a path below."; return; }
    $("#drives").innerHTML=ds.map(d=>`<div class="drive" data-path="${esc(d.mount_path)}">${esc(d.mount_path)} <span class="tag">${d.catalogued?("· "+esc(d.volume_label||"catalogued")+" (rescan)"):"· new"}</span></div>`).join("");
    for(const el of document.querySelectorAll(".drive")) el.addEventListener("click",()=>{ $("#path").value=el.dataset.path; });
  }catch(e){ $("#drives").textContent="Could not list drives: "+e; }
}
$("#browse").addEventListener("click",async()=>{
  try{
    const r=await fetch("/api/pick-folder",{method:"POST",headers:{"x-cleanup-token":CSRF}});
    const j=await r.json(); if(j.path) $("#path").value=j.path;
  }catch(e){ $("#running").textContent="Folder picker error: "+e; }
});
$("#scan").addEventListener("click",async()=>{
  const path=$("#path").value.trim(); if(!path){ $("#running").textContent="Enter a path first."; return; }
  const force=$("#force").checked;
  try{
    const r=await fetch("/api/scan",{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify({path,force})});
    if(!r.ok){ $("#running").innerHTML=`<span class="err">Scan error: ${esc(await r.text())}</span>`; return; }
    poll();
  }catch(e){ $("#running").textContent="Scan error: "+e; }
});
async function poll(){
  try{
    const s=await (await fetch("/api/scan/status")).json();
    if(s.running){ const r=s.running; $("#running").textContent=`Scanning ${r.path} — ${r.hashed} hashed · ${r.skipped} unchanged · ${r.errors} errors · ${r.archive_entries} archive entries`; }
    else $("#running").textContent="";
    $("#queued").textContent = s.queued.length ? ("Queued: "+s.queued.join(", ")) : "";
    $("#recent").innerHTML = s.recent.length ? s.recent.map(r=>{
      const msg = r.error_message ? `<span class="err">${esc(r.error_message)}</span>` : `${r.hashed} hashed · ${r.skipped} unchanged · ${r.errors} errors · ${r.archive_entries} archive entries · ${r.marked_missing} newly missing`;
      return `<div>${esc(r.path)} — ${msg}</div>`;
    }).join("") : "None yet.";
    if(s.running || s.queued.length) setTimeout(poll, 1500);
  }catch(e){ /* stop polling on error */ }
}
loadDrives(); poll();
</script>
</body></html>
"##;
```

Register `.route("/scan", get(scan_page))`. Add a `<a href="/scan">Scan a drive</a>` link to the browse page header (in `INDEX_HTML`, near the existing "Review duplicates →" link) and to the review page header (in `REVIEW_HTML`, near its "Back to browse" link). Both are relative — the self-contained tests for those pages still pass.

- [ ] **Step 4: Write the real-TCP e2e**

Create `tests/web_scan_flow.rs`:

```rust
use std::io::{Read, Write};
use std::net::TcpStream;

fn start(db: std::path::PathBuf) -> std::net::SocketAddr {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async move {
            let state = cleanupstorages::web::AppState {
                catalog_path: db.clone(),
                mounts: cleanupstorages::mounts::MountResolver::Fixed(std::collections::HashMap::new()),
                csrf_token: "TESTTOKEN".to_string(),
                scan_queue: cleanupstorages::scan_queue::ScanQueue::new(db.clone()),
            };
            tokio::spawn(state.scan_queue.clone().run_worker());
            let app = cleanupstorages::web::build_router_with(state);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn req(addr: std::net::SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf
}

#[test]
fn scan_a_folder_over_http_and_see_it_finish() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("c.db");
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("a.txt"), b"one").unwrap();
    std::fs::write(drive.join("b.txt"), b"two").unwrap();
    { cleanupstorages::catalog::Catalog::open(&db).unwrap(); }
    std::mem::forget(tmp);

    let addr = start(db.clone());

    // enqueue a scan (with token)
    let payload = format!("{{\"path\":{:?},\"force\":false}}", drive.to_string_lossy());
    let post = format!("POST /api/scan HTTP/1.0\r\nHost: x\r\ncontent-type: application/json\r\nx-cleanup-token: TESTTOKEN\r\ncontent-length: {}\r\n\r\n{}", payload.len(), payload);
    let resp = req(addr, &post);
    assert!(resp.contains("200 OK"), "scan enqueue: {resp}");

    // poll status until the scan appears in recent with 2 hashed
    let mut done = false;
    for _ in 0..200 {
        let s = req(addr, "GET /api/scan/status HTTP/1.0\r\nHost: x\r\n\r\n");
        if s.contains("\"hashed\":2") && s.contains("\"error_message\":null") { done = true; break; }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(done, "scan should complete and report 2 hashed");
}
```

- [ ] **Step 5: Run tests + release build + manual smoke**

Run: `cargo test --lib web` then `cargo test --test web_scan_flow` then `cargo test` then `cargo build --release`.
Then best-effort background smoke: `cargo run -- browse --no-open`, open `/scan`, click a detected drive or "Browse…", Scan, watch the counts; do NOT let a blocking run hang. Report what you saw or that you skipped it.

- [ ] **Step 6: Commit**

```bash
git add src/web.rs tests/web_scan_flow.rs
git commit -m "feat(web): /scan page (detected drives, folder picker, live progress) + nav

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Scanner progress hook, additive, no-op for existing callers (§4) → Task 1 ✓
- Queue + single worker + live counters + capped history + failure isolation (§5) → Task 3 ✓
- `POST /api/scan` (CSRF), `GET /api/scan/status` (§3, §5) → Task 4 ✓
- Worker spawned at server start; AppState holds the queue (§3, §5) → Task 4 ✓
- `GET /api/detected-drives` with catalogued flag (§6) → Task 5 ✓
- `POST /api/pick-folder` native dialog, CSRF, spawn_blocking, `rfd` (§8) → Task 6 ✓
- `/scan` page: detected drives, path field, Browse…, force checkbox, Scan, live status; nav links (§7) → Task 7 ✓
- Read-only → fingerprint (no prompt) (§2) → Tasks 2, 3 (`ReadonlyMode::Fingerprint` in the worker) ✓
- Force full rescan (§7) → Tasks 4, 7 (`force` flows path → enqueue → job → `scan_volume`) ✓
- One-at-a-time + queue (§5) → Task 3 ✓
- Localhost bind unchanged; CSRF on mutating endpoints (§2) → Tasks 4, 6 ✓
- Testing (§9): scanner-hook count test (T1), queue unit tests incl. failure (T3), endpoint oneshot + CSRF tests (T4–6), e2e (T7) ✓
- Catalog concurrency: `busy_timeout` on `Catalog::open` (hardening) → Task 2 ✓

**Placeholder scan:** No TBD/TODO; every step has runnable code + commands. The one non-literal instruction (Task 4 "update every `AppState { … }` construction") is bounded by a `grep` and the compiler (all fields required).

**Type consistency:** `Progress` trait methods (`on_hashed`/`on_skipped`/`on_error`/`on_archive_entry`) identical across scanner (T1), `Counters` (T3); `scan_volume_with_progress`/`run_scan` signatures consistent T1→T2→T3; `ScanQueue::{new,enqueue,status,run_worker}` consistent T3→T4→T7; `AppState.scan_queue` field name consistent T4→T7 + integration test; DTO field names (`hashed/skipped/errors/archive_entries/marked_missing/error_message/path`) match between `scan_queue` structs and the page JS. `MountResolver::snapshot()` (from Phase 2b) reused in T5.

**Deferred (logged):** concurrent scans; cancel a running/queued scan; per-file path + ETA; disabling action buttons during a scan (the `busy_timeout` + WAL make an overlapping quarantine/repack retry rather than fail, but a long scan holding the write lock could still time out a concurrent write — acceptable for a single local user, noted).

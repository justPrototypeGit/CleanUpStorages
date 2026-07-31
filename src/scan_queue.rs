//! In-memory single-worker scan queue: runs drive scans one at a time in the background,
//! exposing live progress and a small history for the web UI.

use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Counters {
    pub hashed: AtomicUsize,
    pub skipped: AtomicUsize,
    pub errors: AtomicUsize,
    pub archive_entries: AtomicUsize,
    pub done_bytes: AtomicU64,
    /// 0 means "not yet known": either the counting pass hasn't reported in, or it was skipped.
    /// `total_bytes == 0` is otherwise indistinguishable from "an empty tree", but an empty tree
    /// finishes instantly, so treating the sentinel as absent costs nothing real.
    total_files: AtomicU64,
    total_bytes: AtomicU64,
    eta: Mutex<crate::scan_control::EtaTracker>,
}
impl Counters {
    fn new() -> Arc<Counters> {
        Arc::new(Counters {
            hashed: 0.into(),
            skipped: 0.into(),
            errors: 0.into(),
            archive_entries: 0.into(),
            done_bytes: 0.into(),
            total_files: 0.into(),
            total_bytes: 0.into(),
            eta: Mutex::new(crate::scan_control::EtaTracker::new()),
        })
    }
    pub fn snapshot(&self) -> (usize, usize, usize, usize) {
        (
            self.hashed.load(Ordering::Relaxed),
            self.skipped.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.archive_entries.load(Ordering::Relaxed),
        )
    }
    /// Bytes done, plus totals/ETA as `None` until the counting pass has reported in (or ETA
    /// hasn't got two samples yet) — a percentage must be absent rather than wrong.
    fn progress_snapshot(&self) -> (u64, Option<u64>, Option<u64>, Option<u64>) {
        let done_bytes = self.done_bytes.load(Ordering::Relaxed);
        let total_files = self.total_files.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        let total_files = (total_files > 0).then_some(total_files);
        let total_bytes = (total_bytes > 0).then_some(total_bytes);
        let eta_seconds = total_bytes.and_then(|tb| {
            self.eta
                .lock()
                .unwrap()
                .eta_seconds(tb.saturating_sub(done_bytes))
        });
        (done_bytes, total_files, total_bytes, eta_seconds)
    }
}
impl crate::scanner::Progress for Counters {
    fn on_hashed(&self) {
        self.hashed.fetch_add(1, Ordering::Relaxed);
    }
    fn on_skipped(&self) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }
    fn on_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    fn on_archive_entry(&self) {
        self.archive_entries.fetch_add(1, Ordering::Relaxed);
    }
    fn on_total(&self, files: u64, bytes: u64) {
        self.total_files.store(files, Ordering::Relaxed);
        self.total_bytes.store(bytes, Ordering::Relaxed);
    }
    fn on_bytes(&self, bytes: u64) {
        self.done_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.eta.lock().unwrap().record(bytes);
    }
}

#[derive(Clone, Serialize)]
pub struct ScanResult {
    pub path: String,
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub archive_entries: usize,
    pub marked_missing: usize,
    pub error_message: Option<String>,
}

#[derive(Serialize)]
pub struct RunningDto {
    pub path: String,
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub archive_entries: usize,
    pub done_bytes: u64,
    pub total_files: Option<u64>,
    pub total_bytes: Option<u64>,
    pub eta_seconds: Option<u64>,
}

#[derive(Serialize)]
pub struct StatusSnapshot {
    pub running: Option<RunningDto>,
    pub queued: Vec<String>,
    pub recent: Vec<ScanResult>,
}

struct Job {
    path: PathBuf,
    force: bool,
}
struct Running {
    path: String,
    counters: Arc<Counters>,
    /// Fresh per job: sharing one flag across jobs would let a stop from a previous scan silently
    /// cancel the next one queued behind it.
    stop: crate::scan_control::StopFlag,
}

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

impl ScanQueue {
    pub fn new(catalog_path: PathBuf) -> Arc<ScanQueue> {
        Arc::new(ScanQueue {
            catalog_path,
            inner: Mutex::new(Inner {
                pending: VecDeque::new(),
                running: None,
                recent: VecDeque::new(),
            }),
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
            let (done_bytes, total_files, total_bytes, eta_seconds) =
                r.counters.progress_snapshot();
            RunningDto {
                path: r.path.clone(),
                hashed,
                skipped,
                errors,
                archive_entries,
                done_bytes,
                total_files,
                total_bytes,
                eta_seconds,
            }
        });
        StatusSnapshot {
            running,
            queued: inner
                .pending
                .iter()
                .map(|j| j.path.display().to_string())
                .collect(),
            recent: inner.recent.iter().cloned().collect(),
        }
    }

    /// Ask the running scan to stop. Returns false when nothing is running.
    ///
    /// Cooperative, not `tokio::task::JoinHandle::abort`: the scan runs inside `spawn_blocking`,
    /// which an abort cannot interrupt -- it only stops the task from being *awaited*, so the
    /// blocking work would run to completion regardless. The `StopFlag` is checked by the scanner
    /// itself between files, which is the only place it is safe to stop.
    pub fn request_stop(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        match inner.running.as_ref() {
            Some(r) => {
                r.stop.request();
                true
            }
            None => false,
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
        let stop = crate::scan_control::StopFlag::new();
        let path_str = job.path.display().to_string();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = Some(Running {
                path: path_str.clone(),
                counters: counters.clone(),
                stop: stop.clone(),
            });
        }

        // Run the blocking scan off the async runtime.
        let catalog_path = self.catalog_path.clone();
        let counters_for_job = counters.clone();
        let stop_for_job = stop.clone();
        let path = job.path.clone();
        let force = job.force;
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<ScanResult> {
            // `scanner::run_scan` fingerprints an identity for any path (even a missing one) and
            // reports per-file walk errors rather than failing outright, so a nonexistent or
            // unreadable root would otherwise look like an empty successful scan. Reject it here
            // instead of handing it to the scanner.
            if !path.is_dir() {
                anyhow::bail!(
                    "path does not exist or is not a directory: {}",
                    path.display()
                );
            }
            let cat = crate::catalog::Catalog::open(&catalog_path)?;
            if !cat.integrity_ok()? {
                anyhow::bail!(
                    "catalog failed integrity check; restore the latest snapshot before scanning"
                );
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            let progress: &dyn crate::scanner::Progress = counters_for_job.as_ref();
            // Counting first is what makes a real percentage possible from the very first byte
            // hashed, rather than only once the scan itself has walked the whole tree.
            let totals = crate::scanner::count_tree(&path, &stop_for_job);
            crate::scanner::Progress::on_total(progress, totals.files, totals.bytes);
            let scanned = crate::scanner::run_scan(
                &cat,
                &path,
                force,
                crate::volume::ReadonlyMode::Fingerprint,
                now,
                Some(progress),
                &stop_for_job,
            )?;
            // snapshot the catalog after a successful scan (best-effort)
            if let Ok(cfg) = crate::config::Config::default_paths() {
                let _ = crate::catalog::backup::snapshot(
                    &catalog_path,
                    &cfg.backups_dir(),
                    cfg.snapshot_retention,
                    now,
                );
            }
            let (hashed, skipped, errors, archive_entries) = counters_for_job.snapshot();
            Ok(match scanned {
                Some((_id, s)) => ScanResult {
                    path: path.display().to_string(),
                    hashed: s.hashed,
                    skipped: s.skipped,
                    errors: s.errors,
                    archive_entries: s.archive_entries,
                    marked_missing: s.marked_missing,
                    error_message: None,
                },
                None => ScanResult {
                    path: path.display().to_string(),
                    hashed,
                    skipped,
                    errors,
                    archive_entries,
                    marked_missing: 0,
                    error_message: Some("drive is read-only and was skipped".into()),
                },
            })
        })
        .await;

        let result = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => error_result(&path_str, &counters, e.to_string()),
            Err(join_err) => error_result(
                &path_str,
                &counters,
                format!("scan task failed: {join_err}"),
            ),
        };

        let mut inner = self.inner.lock().unwrap();
        inner.running = None;
        inner.recent.push_front(result);
        while inner.recent.len() > RECENT_CAP {
            inner.recent.pop_back();
        }
    }
}

fn error_result(path: &str, counters: &Counters, message: String) -> ScanResult {
    let (hashed, skipped, errors, archive_entries) = counters.snapshot();
    ScanResult {
        path: path.to_string(),
        hashed,
        skipped,
        errors,
        archive_entries,
        marked_missing: 0,
        error_message: Some(message),
    }
}

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
        {
            crate::catalog::Catalog::open(&db).unwrap();
        } // create the catalog file
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
            if let Some(r) = s.recent.first() {
                break r.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(result.hashed, 2);
        assert_eq!(result.error_message, None);
        worker.abort();
    }

    #[test]
    fn request_stop_flips_the_running_jobs_own_flag() {
        // The deterministic half of the pair below: no wall-clock racing, so this one cannot go
        // red on a loaded CI runner. It covers the wiring bug that matters -- request_stop()
        // reaching the flag the worker actually handed to the scanner -- by installing a known
        // flag as the running job and asserting that same flag comes back set.
        let q = ScanQueue::new(std::path::PathBuf::from("unused.db"));
        assert!(!q.request_stop(), "nothing running yet");

        let stop = crate::scan_control::StopFlag::new();
        q.inner.lock().unwrap().running = Some(Running {
            path: "D:/drive".into(),
            counters: Counters::new(),
            stop: stop.clone(),
        });

        assert!(!stop.is_requested(), "clean before the request");
        assert!(q.request_stop(), "a running job accepts a stop");
        assert!(
            stop.is_requested(),
            "the request must reach the flag the scanner is holding, not a copy of it"
        );
    }

    #[tokio::test]
    async fn stop_request_ends_a_running_scan_before_it_finishes_the_tree() {
        // Enough files that the walk+hash takes measurably longer than the sub-millisecond poll
        // below, so request_stop() reliably lands mid-scan instead of racing straight past it.
        let tmp = tempfile::tempdir().unwrap();
        let drive = tmp.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        for i in 0..2000 {
            std::fs::write(drive.join(format!("f{i}.bin")), b"x").unwrap();
        }
        let db = tmp.path().join("c.db");
        {
            crate::catalog::Catalog::open(&db).unwrap();
        }

        let q = ScanQueue::new(db);
        let worker = tokio::spawn(q.clone().run_worker());
        q.enqueue(drive.clone(), false);
        // Poll request_stop() itself rather than status(): it only returns true once the job is
        // marked running, which is exactly the moment the flag it flips starts being checked by
        // the counting pass and the scanner.
        loop {
            if q.request_stop() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let result = loop {
            let s = q.status();
            if let Some(r) = s.recent.first() {
                break r.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        // The reliability guard: a scan that did not finish must never sweep unreached files to
        // missing. If request_stop() were wired to nothing, this would still hold (no scan ever
        // reaches its end early) but the count below would not.
        assert_eq!(
            result.marked_missing, 0,
            "a stopped scan must never mark files missing"
        );
        // The load-bearing assertion: without request_stop() actually reaching the per-job
        // StopFlag, this scan runs to completion and hashes all 2000 files.
        assert!(
            result.hashed < 2000,
            "expected the scan to stop before finishing all 2000 files, got {}",
            result.hashed
        );
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
            if s.recent
                .iter()
                .any(|r| r.error_message.is_none() && r.hashed == 2)
            {
                break true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert!(good);
        let s = q.status();
        assert!(s.recent.iter().any(|r| r.error_message.is_some())); // the bad one recorded
        worker.abort();
    }
}

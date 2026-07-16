//! In-memory single-worker scan queue: runs drive scans one at a time in the background,
//! exposing live progress and a small history for the web UI.

use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Counters {
    pub hashed: AtomicUsize,
    pub skipped: AtomicUsize,
    pub errors: AtomicUsize,
    pub archive_entries: AtomicUsize,
}
impl Counters {
    fn new() -> Arc<Counters> {
        Arc::new(Counters {
            hashed: 0.into(),
            skipped: 0.into(),
            errors: 0.into(),
            archive_entries: 0.into(),
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
            RunningDto {
                path: r.path.clone(),
                hashed,
                skipped,
                errors,
                archive_entries,
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
            inner.running = Some(Running {
                path: path_str.clone(),
                counters: counters.clone(),
            });
        }

        // Run the blocking scan off the async runtime.
        let catalog_path = self.catalog_path.clone();
        let counters_for_job = counters.clone();
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
            let scanned = crate::scanner::run_scan(
                &cat,
                &path,
                force,
                crate::volume::ReadonlyMode::Fingerprint,
                now,
                Some(progress),
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

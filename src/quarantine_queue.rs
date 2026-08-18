//! Serial background queue for folder quarantines, so reviewing does not mean waiting (#66).
//!
//! **Serial is required, not a simplification.** SQLite has a single writer, and every item
//! re-checks that its files are still `active` immediately before moving them — a check that only
//! means anything if nothing else is mutating the catalogue at the same time.
//!
//! What the queue changes is *who* waits. The reviewer confirms an item and moves on; the worker
//! drains the list in order. The expensive part — rebuilding the directory tree over every row —
//! happens **once when the queue empties**, not once per item, because twenty quarantines in a row
//! used to mean twenty full rebuilds.

use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize)]
pub struct QuarantineResult {
    pub volume_id: String,
    pub path: String,
    /// Files whose catalogue rows were updated, or 0 when the item failed.
    pub files_updated: usize,
    pub dest: Option<String>,
    /// Present exactly when the item failed. A refusal — drive swapped, tree no longer all active —
    /// is reported here rather than swallowed, because the user needs to know their click did not
    /// take effect.
    pub error_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct QuarantineJobDto {
    pub volume_id: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct QuarantineStatus {
    pub running: Option<QuarantineJobDto>,
    pub pending: Vec<QuarantineJobDto>,
    pub recent: Vec<QuarantineResult>,
}

struct Job {
    volume_id: String,
    path: String,
}

struct Inner {
    pending: VecDeque<Job>,
    running: Option<Job>,
    recent: VecDeque<QuarantineResult>,
}

pub struct QuarantineQueue {
    catalog_path: PathBuf,
    mounts: crate::mounts::MountResolver,
    inner: Mutex<Inner>,
    notify: tokio::sync::Notify,
}

const RECENT_CAP: usize = 50;

impl QuarantineQueue {
    pub fn new(
        catalog_path: PathBuf,
        mounts: crate::mounts::MountResolver,
    ) -> Arc<QuarantineQueue> {
        Arc::new(QuarantineQueue {
            catalog_path,
            mounts,
            inner: Mutex::new(Inner {
                pending: VecDeque::new(),
                running: None,
                recent: VecDeque::new(),
            }),
            notify: tokio::sync::Notify::new(),
        })
    }

    /// Add an item; returns how many are ahead of it (0 = starts next).
    ///
    /// Deliberately does no validation beyond de-duplication: the worker re-checks everything
    /// immediately before acting, and a check here would be stale by the time the item ran.
    pub fn enqueue(self: &Arc<Self>, volume_id: String, path: String) -> usize {
        let pos = {
            let mut inner = self.inner.lock().unwrap();
            // Double-clicking a row must not queue the same folder twice. The second attempt would
            // fail harmlessly (the path is gone by then), but reporting it as an error would be
            // noise about something the user did not do wrong.
            let dup = inner
                .running
                .iter()
                .chain(inner.pending.iter())
                .any(|j| j.volume_id == volume_id && j.path == path);
            if dup {
                return inner.pending.len();
            }
            inner.pending.push_back(Job { volume_id, path });
            inner.pending.len() - 1 + inner.running.is_some() as usize
        };
        self.notify.notify_one();
        pos
    }

    pub fn status(&self) -> QuarantineStatus {
        let inner = self.inner.lock().unwrap();
        QuarantineStatus {
            running: inner.running.as_ref().map(|j| QuarantineJobDto {
                volume_id: j.volume_id.clone(),
                path: j.path.clone(),
            }),
            pending: inner
                .pending
                .iter()
                .map(|j| QuarantineJobDto {
                    volume_id: j.volume_id.clone(),
                    path: j.path.clone(),
                })
                .collect(),
            recent: inner.recent.iter().cloned().collect(),
        }
    }

    /// Background loop: drain the queue one item at a time, forever.
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
        let (volume_id, path) = (job.volume_id.clone(), job.path.clone());
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = Some(job);
        }

        let mount = self.mounts.resolve(&volume_id);
        let catalog_path = self.catalog_path.clone();
        let (vid, p) = (volume_id.clone(), path.clone());

        // Off the async runtime: the rename is instant but the per-file bookkeeping is not, and the
        // largest single group here is 326,569 files.
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<(usize, String)> {
            let mount = mount.ok_or_else(|| anyhow::anyhow!("drive not connected"))?;
            let cat = crate::catalog::Catalog::open(&catalog_path)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            let out = crate::tree_quarantine::quarantine_tree(&cat, &mount, &vid, &p, now)?;
            Ok((out.files_updated, out.dest_relative_path))
        })
        .await;

        let result = match outcome {
            Ok(Ok((files_updated, dest))) => QuarantineResult {
                volume_id: volume_id.clone(),
                path: path.clone(),
                files_updated,
                dest: Some(dest),
                error_message: None,
            },
            Ok(Err(e)) => QuarantineResult {
                volume_id: volume_id.clone(),
                path: path.clone(),
                files_updated: 0,
                dest: None,
                error_message: Some(e.to_string()),
            },
            Err(e) => QuarantineResult {
                volume_id: volume_id.clone(),
                path: path.clone(),
                files_updated: 0,
                dest: None,
                error_message: Some(format!("quarantine task failed: {e}")),
            },
        };

        let drained = {
            let mut inner = self.inner.lock().unwrap();
            inner.running = None;
            inner.recent.push_front(result);
            while inner.recent.len() > RECENT_CAP {
                inner.recent.pop_back();
            }
            inner.pending.is_empty()
        };

        // Rebuild ONCE, when there is nothing left to do. Rebuilding per item meant reprocessing
        // every row in the catalogue for each folder moved; the review list is stale in exactly the
        // same way after one item or twenty, so the work only needs doing when the user is about to
        // look again.
        if drained {
            let catalog_path = self.catalog_path.clone();
            let vid = volume_id.clone();
            let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let cat = crate::catalog::Catalog::open(&catalog_path)?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs() as i64;
                cat.rebuild_directory_trees(&vid, now)?;
                cat.refresh_volume_totals(&vid)?;
                Ok(())
            })
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Arc<QuarantineQueue> {
        QuarantineQueue::new(
            PathBuf::from("unused.db"),
            crate::mounts::MountResolver::Fixed(Default::default()),
        )
    }

    #[test]
    fn enqueue_reports_position_and_preserves_order() {
        let q = queue();
        assert_eq!(q.enqueue("v".into(), "a".into()), 0, "first starts next");
        assert_eq!(q.enqueue("v".into(), "b".into()), 1);
        assert_eq!(q.enqueue("v".into(), "c".into()), 2);
        let s = q.status();
        let paths: Vec<&str> = s.pending.iter().map(|j| j.path.as_str()).collect();
        assert_eq!(paths, vec!["a", "b", "c"], "order must be preserved");
        assert!(s.running.is_none());
    }

    #[test]
    fn the_same_folder_cannot_be_queued_twice() {
        // Double-clicking a row would otherwise queue it again; the second attempt fails once the
        // path is gone, and reporting that as an error blames the user for a stutter.
        let q = queue();
        q.enqueue("v".into(), "same".into());
        q.enqueue("v".into(), "same".into());
        assert_eq!(q.status().pending.len(), 1);
    }

    #[test]
    fn the_same_path_on_a_different_drive_is_a_different_item() {
        // Both drives here were first seen as `D:\` and share folder names, so keying on path alone
        // would silently drop a real second decision.
        let q = queue();
        q.enqueue("uni-big".into(), "Lezioni/Google Drive".into());
        q.enqueue("uni-small".into(), "Lezioni/Google Drive".into());
        assert_eq!(q.status().pending.len(), 2);
    }
}

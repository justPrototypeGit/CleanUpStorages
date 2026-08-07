//! Liveness signal for a running scan, so a hard-killed one stops looking alive forever (#36).
//!
//! **Deliberately not in SQLite.** The scanner holds a write transaction and commits only at batch
//! rotation, so a heartbeat written from a second connection blocks on the write lock -- with no
//! commit to release it for the ~48 minutes a 100 GB file takes to hash -- and one written on the
//! scan's own connection is invisible to other processes until that transaction commits. Either way
//! a live scan would be reported dead, which is the one outcome worth avoiding: it invites the user
//! to start a second scan over the same drive.
//!
//! So the signal is a file whose mtime a ticker thread refreshes. The scan's own write lock cannot
//! silence it, and no process-liveness API is needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How often the ticker refreshes the file.
const TICK_SECS: u64 = 15;
/// How long a heartbeat may go unrefreshed before its run is presumed dead. Eight missed ticks:
/// long enough to survive a stalled machine, short enough to be useful.
pub const STALE_AFTER_SECS: i64 = 120;

/// Where heartbeats live: beside the catalogue, like snapshots and settings.
fn heartbeats_dir(catalog_path: &Path) -> PathBuf {
    catalog_path
        .parent()
        .map(|p| p.join("scan-heartbeats"))
        .unwrap_or_else(|| PathBuf::from("scan-heartbeats"))
}

fn heartbeat_path(catalog_path: &Path, run_id: i64) -> PathBuf {
    heartbeats_dir(catalog_path).join(format!("{run_id}"))
}

/// Refreshes a run's heartbeat until dropped.
///
/// `Drop` covers the normal, error and panic-unwind paths. Only a hard kill leaves the file behind
/// -- and a file that stops being refreshed is exactly the signal this exists to produce.
pub struct Heartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl Heartbeat {
    /// Begin beating for `run_id`. Never fails the scan: a heartbeat that cannot be written leaves
    /// the run looking interrupted, which is a display problem, whereas failing here would stop a
    /// scan the user wants.
    pub fn start(catalog_path: &Path, run_id: i64) -> Heartbeat {
        let path = heartbeat_path(catalog_path, run_id);
        // Failures are warned about, not swallowed. A heartbeat that never establishes makes a
        // perfectly healthy scan report as interrupted two minutes in, and the read path cannot
        // tell that apart from a hard kill -- so the log line is the only way anyone would ever
        // work out why.
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(
                    "could not create {}: {e}; this scan will report as interrupted",
                    dir.display()
                );
            }
        }
        if let Err(e) = touch(&path) {
            tracing::warn!(
                "could not write the scan heartbeat at {}: {e}; this scan will report as interrupted",
                path.display()
            );
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let path_for_thread = path.clone();
        let handle = std::thread::spawn(move || {
            // Wake often, act rarely: this lets Drop join promptly instead of waiting out a full
            // tick, which would add seconds to the end of every scan.
            let mut waited = 0u64;
            while !stop_for_thread.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                waited += 250;
                if waited >= TICK_SECS * 1000 {
                    waited = 0;
                    let _ = touch(&path_for_thread);
                }
            }
        });
        Heartbeat {
            stop,
            handle: Some(handle),
            path,
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn touch(path: &Path) -> std::io::Result<()> {
    // Rewriting the file is the portable way to move its mtime; the content is irrelevant.
    std::fs::write(path, b"")
}

/// Does this run still look alive?
///
/// True when its heartbeat file was refreshed within the staleness window, **or** when the run only
/// just started: `start_scan_run` commits the `running` row before the heartbeat exists, so without
/// the `started_at` floor a run could be called interrupted in the first instants of its life.
pub fn is_alive(catalog_path: &Path, run_id: i64, started_at: i64, now: i64) -> bool {
    if now.saturating_sub(started_at) <= STALE_AFTER_SECS {
        return true;
    }
    let Ok(meta) = std::fs::metadata(heartbeat_path(catalog_path, run_id)) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return true; // cannot tell: prefer "alive" (see below)
    };
    let mtime = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return true,
    };
    // A clock that moved backwards makes `now - mtime` negative. Treat that as fresh: guessing
    // "alive" costs a row that stays `running` a little longer, while guessing "dead" tells the
    // user a scan that is still working has died. The first is the one to prefer.
    now.saturating_sub(mtime) <= STALE_AFTER_SECS
}

/// The status to SHOW for a run, given what is stored.
///
/// Read-only by design: nothing rewrites a stored `running` value, so a restarted `browse` can never
/// declare a concurrently-running CLI scan dead. One helper, so the Scan page and the CLI cannot
/// disagree.
pub fn display_status(
    catalog_path: &Path,
    run_id: i64,
    stored_status: &str,
    started_at: i64,
    now: i64,
) -> String {
    if stored_status == "running" && !is_alive(catalog_path, run_id, started_at, now) {
        return "interrupted".to_string();
    }
    stored_status.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_enough() -> i64 {
        STALE_AFTER_SECS + 1
    }

    #[test]
    fn a_run_with_a_fresh_heartbeat_is_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let _hb = Heartbeat::start(&db, 7);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // started long ago, so only the heartbeat itself can be keeping it alive
        assert!(is_alive(&db, 7, now - 10_000, now));
        assert_eq!(
            display_status(&db, 7, "running", now - 10_000, now),
            "running"
        );
    }

    #[test]
    fn a_run_whose_heartbeat_went_stale_is_interrupted() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        {
            let _hb = Heartbeat::start(&db, 7);
            // dropped here, which also removes the file
        }
        // Recreate the file to simulate a hard kill: the file survives, nothing refreshes it.
        let p = heartbeat_path(&db, 7);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // The file's mtime is now, so ask from a point far in the future instead of sleeping.
        let later = now + old_enough();
        assert!(!is_alive(&db, 7, now - 10_000, later));
        assert_eq!(
            display_status(&db, 7, "running", now - 10_000, later),
            "interrupted"
        );
    }

    #[test]
    fn a_run_with_no_heartbeat_file_at_all_is_interrupted() {
        // The hard-kill case where even the file is gone, plus every row written before this
        // feature existed.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        assert!(!is_alive(&db, 99, 1_000, 1_000 + old_enough()));
    }

    #[test]
    fn a_just_started_run_is_never_interrupted() {
        // start_scan_run commits the row before the heartbeat exists; without the started_at floor
        // a run could be declared dead in its first instants.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        assert!(is_alive(&db, 1, 1_000, 1_000));
        assert!(is_alive(&db, 1, 1_000, 1_000 + STALE_AFTER_SECS));
    }

    #[test]
    fn a_clock_moving_backwards_never_kills_a_live_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let p = heartbeat_path(&db, 3);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        // `now` far in the past relative to the file's mtime.
        assert!(is_alive(&db, 3, 0, 1));
    }

    #[test]
    fn a_finished_run_is_reported_exactly_as_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        for stored in ["completed", "failed", "cancelled"] {
            assert_eq!(
                display_status(&db, 5, stored, 0, 1_000_000),
                stored,
                "a terminal status must never be rewritten"
            );
        }
    }

    #[test]
    fn dropping_the_heartbeat_removes_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let p = heartbeat_path(&db, 11);
        {
            let _hb = Heartbeat::start(&db, 11);
            assert!(p.exists(), "the file must exist while the scan runs");
        }
        assert!(
            !p.exists(),
            "a clean end must leave nothing behind; only a hard kill does"
        );
    }
}

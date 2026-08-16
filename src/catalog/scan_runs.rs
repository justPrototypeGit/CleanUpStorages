//! One row per scan: what it cost and how it ended.
//!
//! Written outside the scan's long transaction, so an interrupted multi-day scan still leaves a
//! record. Every write here is best-effort at the call site: losing a measurement is acceptable,
//! losing a scan is not.

use crate::catalog::Catalog;
use crate::scan_metrics::{MetricsSnapshot, BUCKET_COUNT};
use crate::scanner::ScanSummary;
use rusqlite::params;

/// A persisted scan run, as shown by the CLI and the Scan page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanRun {
    pub id: i64,
    pub volume_id: Option<String>,
    pub root_path: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub forced: bool,
    pub status: String,
    pub error_message: Option<String>,
    pub hashed: i64,
    pub skipped: i64,
    pub errors: i64,
    pub archive_entries: i64,
    pub metrics: MetricsSnapshot,
}

impl Catalog {
    /// Record a scan as started. Returns the row id to pass to `finish_scan_run`.
    ///
    /// Called before the scan opens its transaction, so the row is committed immediately and a
    /// killed scan leaves a visible `running` row rather than silence.
    pub fn start_scan_run(
        &self,
        volume_id: Option<&str>,
        root_path: &str,
        started_at: i64,
        forced: bool,
    ) -> anyhow::Result<i64> {
        self.conn.execute(
            "INSERT INTO scan_runs(volume_id, root_path, started_at, forced, status)
             VALUES (?1, ?2, ?3, ?4, 'running')",
            params![volume_id, root_path, started_at, forced as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close out a run with its final counters and timings. `status` is one of
    /// `completed` | `failed` | `cancelled` (the last is reserved for #5 and unreachable today).
    pub fn finish_scan_run(
        &self,
        id: i64,
        finished_at: i64,
        status: &str,
        error_message: Option<&str>,
        summary: &ScanSummary,
    ) -> anyhow::Result<()> {
        let m = &summary.metrics;
        let histogram = serde_json::to_string(&m.histogram)?;
        self.conn.execute(
            "UPDATE scan_runs SET finished_at=?2, status=?3, error_message=?4, wall_ms=?5,
                 files_seen=?6, hashed=?7, skipped=?8, errors=?9, archive_entries=?10,
                 bytes_hashed=?11, bytes_skipped=?12, walk_ms=?13, skip_check_ms=?14,
                 hash_ms=?15, db_write_ms=?16, archive_ms=?17, size_histogram=?18
             WHERE id=?1",
            params![
                id,
                finished_at,
                status,
                error_message,
                m.wall_ms as i64,
                m.files_seen as i64,
                summary.hashed as i64,
                summary.skipped as i64,
                summary.errors as i64,
                summary.archive_entries as i64,
                m.bytes_hashed as i64,
                m.bytes_skipped as i64,
                m.walk_ms as i64,
                m.skip_check_ms as i64,
                m.hash_ms as i64,
                m.db_write_ms as i64,
                m.archive_ms as i64,
                histogram,
            ],
        )?;
        Ok(())
    }

    /// Most recent runs, newest first.
    /// A scan that is genuinely running right now, if any: `(id, root_path, started_at)`.
    ///
    /// One writer is the correct design for a SQLite catalogue -- the scanner holds a write
    /// transaction almost continuously, so a second scanner simply cannot proceed. The defect this
    /// exists to fix is that the collision used to be discovered four minutes in, as
    /// `database is locked`, which tells the user nothing about what happened (#60).
    ///
    /// Liveness comes from the heartbeat added for #36, so a run left `running` by a hard kill is
    /// correctly NOT treated as blocking.
    pub fn running_scan(&self) -> anyhow::Result<Option<(i64, String, i64)>> {
        let db_path = match self.db_path() {
            Some(p) => p,
            // An in-memory catalogue has no directory to hold heartbeats, so it cannot host a
            // second process either. Nothing to block on.
            None => return Ok(None),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut stmt = self.conn.prepare(
            "SELECT id, root_path, started_at FROM scan_runs
              WHERE status='running' ORDER BY started_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().find(|(id, _, started_at)| {
            crate::scan_heartbeat::is_alive(&db_path, *id, *started_at, now)
        }))
    }

    pub fn recent_scan_runs(&self, limit: usize) -> anyhow::Result<Vec<ScanRun>> {
        // A 'running' row whose heartbeat has gone stale is REPORTED as interrupted here and
        // nowhere else, so the Scan page and the CLI cannot disagree. Nothing is written back: a
        // second process must never relabel a scan another process may still be running (#36).
        let db_path = self.db_path();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, root_path, started_at, finished_at, forced, status,
                    error_message, files_seen, hashed, skipped, errors, archive_entries,
                    bytes_hashed, bytes_skipped, wall_ms, walk_ms, skip_check_ms, hash_ms,
                    db_write_ms, archive_ms, size_histogram
             FROM scan_runs ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        // Read by column NAME, not position. Inserting one column into the SELECT above shifts
        // every later index, which silently puts the wrong number in the wrong field — and no test
        // catches it where two columns share a type. Names cannot drift that way.
        let rows = stmt.query_map(params![limit as i64], |r| {
            // Display data must never fail a read: a corrupt histogram degrades to zeroes.
            let histogram = r
                .get::<_, Option<String>>("size_histogram")?
                .and_then(|s| serde_json::from_str::<[u64; BUCKET_COUNT]>(&s).ok())
                .unwrap_or([0; BUCKET_COUNT]);
            Ok(ScanRun {
                id: r.get("id")?,
                volume_id: r.get("volume_id")?,
                root_path: r.get("root_path")?,
                started_at: r.get("started_at")?,
                finished_at: r.get("finished_at")?,
                forced: r.get::<_, i64>("forced")? != 0,
                status: {
                    let id: i64 = r.get("id")?;
                    let stored: String = r.get("status")?;
                    let started_at: i64 = r.get("started_at")?;
                    match &db_path {
                        Some(p) => {
                            crate::scan_heartbeat::display_status(p, id, &stored, started_at, now)
                        }
                        // An in-memory catalogue has no directory to hold heartbeats; report what
                        // is stored rather than guessing.
                        None => stored,
                    }
                },
                error_message: r.get("error_message")?,
                hashed: r.get("hashed")?,
                skipped: r.get("skipped")?,
                errors: r.get("errors")?,
                archive_entries: r.get("archive_entries")?,
                metrics: MetricsSnapshot {
                    files_seen: r.get::<_, i64>("files_seen")? as u64,
                    bytes_hashed: r.get::<_, i64>("bytes_hashed")? as u64,
                    bytes_skipped: r.get::<_, i64>("bytes_skipped")? as u64,
                    wall_ms: r.get::<_, Option<i64>>("wall_ms")?.unwrap_or(0) as u64,
                    walk_ms: r.get::<_, i64>("walk_ms")? as u64,
                    skip_check_ms: r.get::<_, i64>("skip_check_ms")? as u64,
                    hash_ms: r.get::<_, i64>("hash_ms")? as u64,
                    db_write_ms: r.get::<_, i64>("db_write_ms")? as u64,
                    archive_ms: r.get::<_, i64>("archive_ms")? as u64,
                    histogram,
                },
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;
    use crate::scan_metrics::MetricsSnapshot;
    use crate::scanner::ScanSummary;

    fn open() -> (tempfile::TempDir, Catalog) {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        (t, cat)
    }

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn a_started_run_is_visible_as_running_before_it_finishes() {
        let (_t, cat) = open();
        // A real just-started run carries a current timestamp. With a 1970 one it would -- rightly
        // -- be reported interrupted, since nothing has beaten for it in fifty years (#36).
        let id = cat
            .start_scan_run(Some("v1"), "D:/drive", now_secs(), false)
            .unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].finished_at, None);
        assert_eq!(runs[0].root_path, "D:/drive");
    }

    #[test]
    fn a_live_scan_blocks_a_second_one_and_a_dead_one_does_not() {
        // #60: a second scan used to run for minutes and then die on `database is locked`. It now
        // refuses immediately. The other half matters just as much -- a run left `running` by a
        // hard kill must NOT block scanning forever, or one power cut locks the tool out.
        let (tmp, cat) = open();
        let db = tmp.path().join("c.db");

        assert!(cat.running_scan().unwrap().is_none(), "nothing running yet");

        // Started long ago -- like the real 12.75-hour scan -- so the only thing keeping it alive
        // is the heartbeat itself, not `is_alive`'s grace window for freshly-started runs.
        let id = cat
            .start_scan_run(Some("v1"), "D:/drive", now_secs() - 86_400, false)
            .unwrap();
        let _hb = crate::scan_heartbeat::Heartbeat::start(&db, id);
        let live = cat.running_scan().unwrap();
        assert!(live.is_some(), "a beating scan must block a second one");
        assert_eq!(
            live.unwrap().1,
            "D:/drive",
            "and must name the other scan's path"
        );

        // A run stamped long ago with nothing beating for it is a hard kill, not a live scan.
        let dead = cat
            .start_scan_run(Some("v1"), "E:/other", now_secs() - 86_400, false)
            .unwrap();
        assert_ne!(dead, id);
        drop(_hb);
        assert!(
            cat.running_scan().unwrap().is_none(),
            "a hard-killed run must not lock the tool out of ever scanning again"
        );
    }

    #[test]
    fn an_old_running_row_with_no_heartbeat_reads_as_interrupted() {
        // The #36 case: a scan killed by power loss or Task Manager leaves status='running'
        // forever. Nothing beats for it, so it is reported interrupted rather than appearing to
        // run indefinitely. The STORED value is deliberately untouched -- another process may be
        // running a scan of its own, and no process may relabel another's work.
        let (_t, cat) = open();
        let id = cat
            .start_scan_run(Some("v1"), "D:/drive", now_secs() - 86_400, false)
            .unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs[0].status, "interrupted");

        let stored: String = cat
            .conn
            .query_row("SELECT status FROM scan_runs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, "running", "the stored value must not be rewritten");
    }

    #[test]
    fn a_live_heartbeat_keeps_an_old_run_reported_as_running() {
        // The case a SQLite-based heartbeat gets wrong: a scan that started long ago and is still
        // working, holding its write transaction the whole time.
        let (tmp, cat) = open();
        let started = now_secs() - 86_400;
        cat.start_scan_run(Some("v1"), "D:/drive", started, false)
            .unwrap();
        let db = tmp.path().join("c.db");
        let _hb = crate::scan_heartbeat::Heartbeat::start(&db, 1);
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(
            runs[0].status, "running",
            "a beating scan must keep reporting as running however long it has run"
        );
    }

    #[test]
    fn finishing_a_run_stores_counters_phases_and_histogram() {
        let (_t, cat) = open();
        let id = cat
            .start_scan_run(Some("v1"), "D:/drive", 100, true)
            .unwrap();
        let summary = ScanSummary {
            hashed: 7,
            skipped: 3,
            errors: 1,
            marked_missing: 0,
            archive_entries: 2,
            stopped: false,
            metrics: MetricsSnapshot {
                wall_ms: 1234,
                walk_ms: 100,
                skip_check_ms: 50,
                hash_ms: 900,
                db_write_ms: 80,
                archive_ms: 40,
                files_seen: 10,
                bytes_hashed: 5000,
                bytes_skipped: 300,
                histogram: [1, 2, 3, 0, 0, 0, 0],
            },
        };
        cat.finish_scan_run(id, 200, "completed", None, &summary)
            .unwrap();

        let r = &cat.recent_scan_runs(10).unwrap()[0];
        assert_eq!(r.status, "completed");
        assert_eq!(r.finished_at, Some(200));
        assert!(r.forced);
        assert_eq!(r.hashed, 7);
        assert_eq!(r.errors, 1);
        assert_eq!(r.archive_entries, 2);
        assert_eq!(r.metrics.hash_ms, 900);
        assert_eq!(r.metrics.files_seen, 10);
        assert_eq!(r.metrics.histogram, [1, 2, 3, 0, 0, 0, 0]);
        assert_eq!(r.error_message, None);
    }

    #[test]
    fn a_failed_run_keeps_its_error_and_its_partial_numbers() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(None, "D:/x", 100, false).unwrap();
        let summary = ScanSummary {
            hashed: 4,
            metrics: MetricsSnapshot {
                hash_ms: 25,
                ..Default::default()
            },
            ..Default::default()
        };
        cat.finish_scan_run(id, 150, "failed", Some("disk fell out"), &summary)
            .unwrap();

        let r = &cat.recent_scan_runs(10).unwrap()[0];
        assert_eq!(r.status, "failed");
        assert_eq!(r.error_message.as_deref(), Some("disk fell out"));
        assert_eq!(r.hashed, 4, "partial work is still recorded");
        assert_eq!(r.metrics.hash_ms, 25);
        assert_eq!(r.volume_id, None);
    }

    #[test]
    fn recent_runs_are_newest_first_and_bounded() {
        let (_t, cat) = open();
        for i in 0..5 {
            cat.start_scan_run(Some("v"), "D:/d", 100 + i, false)
                .unwrap();
        }
        let runs = cat.recent_scan_runs(3).unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].started_at, 104, "newest first");
        assert!(runs[0].started_at > runs[1].started_at);
    }

    #[test]
    fn a_corrupt_histogram_column_reads_back_as_zeroes_not_an_error() {
        let (_t, cat) = open();
        let id = cat.start_scan_run(Some("v"), "D:/d", 100, false).unwrap();
        cat.conn
            .execute(
                "UPDATE scan_runs SET size_histogram='not json' WHERE id=?1",
                [id],
            )
            .unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(
            runs[0].metrics.histogram, [0; 7],
            "display data must never fail a read"
        );
    }
}

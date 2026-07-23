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
    pub fn recent_scan_runs(&self, limit: usize) -> anyhow::Result<Vec<ScanRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, volume_id, root_path, started_at, finished_at, forced, status,
                    error_message, files_seen, hashed, skipped, errors, archive_entries,
                    bytes_hashed, bytes_skipped, wall_ms, walk_ms, skip_check_ms, hash_ms,
                    db_write_ms, archive_ms, size_histogram
             FROM scan_runs ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            // Display data must never fail a read: a corrupt histogram degrades to zeroes.
            let histogram = r
                .get::<_, Option<String>>(21)?
                .and_then(|s| serde_json::from_str::<[u64; BUCKET_COUNT]>(&s).ok())
                .unwrap_or([0; BUCKET_COUNT]);
            Ok(ScanRun {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                root_path: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                forced: r.get::<_, i64>(5)? != 0,
                status: r.get(6)?,
                error_message: r.get(7)?,
                hashed: r.get(9)?,
                skipped: r.get(10)?,
                errors: r.get(11)?,
                archive_entries: r.get(12)?,
                metrics: MetricsSnapshot {
                    files_seen: r.get::<_, i64>(8)? as u64,
                    bytes_hashed: r.get::<_, i64>(13)? as u64,
                    bytes_skipped: r.get::<_, i64>(14)? as u64,
                    wall_ms: r.get::<_, Option<i64>>(15)?.unwrap_or(0) as u64,
                    walk_ms: r.get::<_, i64>(16)? as u64,
                    skip_check_ms: r.get::<_, i64>(17)? as u64,
                    hash_ms: r.get::<_, i64>(18)? as u64,
                    db_write_ms: r.get::<_, i64>(19)? as u64,
                    archive_ms: r.get::<_, i64>(20)? as u64,
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

    #[test]
    fn a_started_run_is_visible_as_running_before_it_finishes() {
        let (_t, cat) = open();
        let id = cat
            .start_scan_run(Some("v1"), "D:/drive", 100, false)
            .unwrap();
        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].finished_at, None);
        assert_eq!(runs[0].root_path, "D:/drive");
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

//! Zip-format files whose extension nobody has classified yet.
//!
//! The scanner will not guess: a five-day unattended run cannot ask, so an unfamiliar zip-format
//! extension is left whole and recorded here for the user to approve or dismiss.
//!
//! Keyed per FILE (`volume_id`, `relative_path`), not per scan: a rescan of an unchanged file never
//! reaches this code at all (the skip path must never open a file), so there is nothing to
//! re-derive on every pass. A row is written once when a file is first hashed and found unfamiliar,
//! and simply persists -- upserted in place if the same file is hashed again (a real rescan with
//! `force`, or the file having actually changed). That also means there is no scan-start clear:
//! with per-file keying there is nothing to reset, and a stopped scan just contributes whatever it
//! reached, with the rest arriving on a later pass.

use crate::catalog::Catalog;
use rusqlite::params;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingFormat {
    pub extension: String,
    pub count: i64,
    pub total_bytes: i64,
    pub first_seen_at: i64,
}

impl Catalog {
    /// Record (or refresh) one unfamiliar file. Upserts on `(volume_id, relative_path)`, so
    /// re-hashing the same file (an incremental rescan that found it changed, or a forced rescan)
    /// replaces its row instead of adding another -- double-counting is impossible by construction.
    pub fn record_pending_format(
        &self,
        volume_id: &str,
        relative_path: &str,
        extension: &str,
        size_bytes: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO pending_archive_formats(volume_id, relative_path, extension, size_bytes, first_seen_at)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(volume_id, relative_path) DO UPDATE SET
                 extension = excluded.extension,
                 size_bytes = excluded.size_bytes,
                 first_seen_at = MIN(first_seen_at, excluded.first_seen_at)",
            params![
                volume_id,
                relative_path,
                extension.to_ascii_lowercase(),
                size_bytes,
                now
            ],
        )?;
        Ok(())
    }

    /// Aggregated across volumes -- the decision is about a file format, not one drive.
    pub fn pending_formats(&self) -> anyhow::Result<Vec<PendingFormat>> {
        let mut stmt = self.conn.prepare(
            "SELECT extension, COUNT(*), SUM(size_bytes), MIN(first_seen_at)
               FROM pending_archive_formats GROUP BY extension
              ORDER BY SUM(size_bytes) DESC, extension",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingFormat {
                extension: r.get(0)?,
                count: r.get(1)?,
                total_bytes: r.get(2)?,
                first_seen_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn clear_pending_format(&self, extension: &str) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM pending_archive_formats WHERE extension=?1",
            params![extension.to_ascii_lowercase()],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_the_same_extension_accumulates_per_volume_and_aggregates_across() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.record_pending_format("v1", "a.bak", "bak", 100, 10)
            .unwrap();
        cat.record_pending_format("v1", "b.bak", "bak", 200, 20)
            .unwrap();
        cat.record_pending_format("v2", "c.bak", "bak", 400, 30)
            .unwrap();
        cat.record_pending_format("v1", "d.kra", "kra", 50, 40)
            .unwrap();

        let rows = cat.pending_formats().unwrap();
        let bak = rows.iter().find(|r| r.extension == "bak").unwrap();
        assert_eq!(bak.count, 3, "aggregated across both volumes");
        assert_eq!(bak.total_bytes, 700);
        assert_eq!(bak.first_seen_at, 10, "earliest sighting wins");
        assert_eq!(rows[0].extension, "bak", "biggest first");

        assert_eq!(
            cat.clear_pending_format("bak").unwrap(),
            3,
            "all three bak rows go, across both volumes"
        );
        let rows = cat.pending_formats().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].extension, "kra");
    }

    #[test]
    fn re_recording_the_same_file_replaces_its_row_instead_of_accumulating() {
        // The bug this keying exists to make impossible: hashing the same file twice (e.g. a
        // forced rescan) must not double-count it.
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.record_pending_format("v1", "a.bak", "bak", 100, 10)
            .unwrap();
        cat.record_pending_format("v1", "a.bak", "bak", 100, 20)
            .unwrap();
        let rows = cat.pending_formats().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 1, "same file, one row");
        assert_eq!(rows[0].first_seen_at, 10, "earliest sighting still wins");
    }
}

//! Zip-format files whose extension nobody has classified yet.
//!
//! The scanner will not guess: a five-day unattended run cannot ask, so an unfamiliar zip-format
//! extension is left whole and recorded here for the user to approve or dismiss.

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
    pub fn record_pending_format(
        &self,
        volume_id: &str,
        extension: &str,
        size_bytes: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO pending_archive_formats(extension, volume_id, count, total_bytes, first_seen_at)
             VALUES (?1,?2,1,?3,?4)
             ON CONFLICT(extension, volume_id) DO UPDATE SET
                 count = count + 1,
                 total_bytes = total_bytes + excluded.total_bytes,
                 first_seen_at = MIN(first_seen_at, excluded.first_seen_at)",
            params![extension.to_ascii_lowercase(), volume_id, size_bytes, now],
        )?;
        Ok(())
    }

    /// Aggregated across volumes -- the decision is about a file format, not one drive.
    pub fn pending_formats(&self) -> anyhow::Result<Vec<PendingFormat>> {
        let mut stmt = self.conn.prepare(
            "SELECT extension, SUM(count), SUM(total_bytes), MIN(first_seen_at)
               FROM pending_archive_formats GROUP BY extension
              ORDER BY SUM(total_bytes) DESC, extension",
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

    /// Drop one volume's rows, so a rescan re-counts instead of accumulating.
    pub fn clear_pending_formats_for_volume(&self, volume_id: &str) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM pending_archive_formats WHERE volume_id=?1",
            params![volume_id],
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
        cat.record_pending_format("v1", "bak", 100, 10).unwrap();
        cat.record_pending_format("v1", "bak", 200, 20).unwrap();
        cat.record_pending_format("v2", "bak", 400, 30).unwrap();
        cat.record_pending_format("v1", "kra", 50, 40).unwrap();

        let rows = cat.pending_formats().unwrap();
        let bak = rows.iter().find(|r| r.extension == "bak").unwrap();
        assert_eq!(bak.count, 3, "aggregated across both volumes");
        assert_eq!(bak.total_bytes, 700);
        assert_eq!(bak.first_seen_at, 10, "earliest sighting wins");
        assert_eq!(rows[0].extension, "bak", "biggest first");

        assert_eq!(
            cat.clear_pending_format("bak").unwrap(),
            2,
            "both volumes' rows go"
        );
        let rows = cat.pending_formats().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].extension, "kra");
    }
}

//! Recording scan errors, and classifying them so they can be grouped.
//!
//! Separate from `store.rs` for the same reason `scan_runs.rs` is: one table, one cohesive set of
//! operations, in a file small enough to hold in your head.

use crate::catalog::Catalog;
use rusqlite::params;

/// Classify an I/O failure for grouping.
///
/// From `ErrorKind` and the raw OS code, **never** the message: Windows renders `io::Error`
/// messages in the OS language (this project's dev machine is Italian), so text matching would
/// misclassify on exactly the machines this feature exists to serve.
pub fn classify_io(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => "permission",
        ErrorKind::NotFound => "not_found",
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION: another process holds the file. Checked
        // by raw code because the ErrorKind mapping for these is not stable across Rust versions.
        _ => match e.raw_os_error() {
            Some(32) | Some(33) => "locked",
            _ => "io",
        },
    }
}

impl Catalog {
    /// Record (or refresh) the failure for one path. One row per `(volume_id, path)`: a path that
    /// fails on twenty scans is one problem, not twenty.
    pub fn log_scan_error(
        &self,
        volume_id: Option<&str>,
        path: &str,
        reason: &str,
        phase: &str,
        kind: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_errors(volume_id, path, reason, occurred_at, phase, kind)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(volume_id, path) DO UPDATE SET
                 reason=excluded.reason, occurred_at=excluded.occurred_at,
                 phase=excluded.phase, kind=excluded.kind",
            params![volume_id, path, reason, now, phase, kind],
        )?;
        Ok(())
    }

    /// Drop errors this scan resolved. Returns how many rows went.
    ///
    /// Two rules, because two kinds of path:
    ///
    /// * `metadata`/`read`/`archive_open` store a real file path, so an error clears when that
    ///   path was re-seen this scan. Keyed on `last_seen_at` -- the same predicate that makes the
    ///   missing-file sweep safe -- so **a stopped scan cannot over-clear**: a path the walk never
    ///   reached never had its stamp bumped.
    /// * `walk` stores a directory and `archive_entry` stores a composite `archive › inner` path;
    ///   neither exists in `files`. Only a *completed* scan proves the location was visited and is
    ///   readable again, so those clear only when `completed` and not re-recorded this run.
    ///
    /// Legacy rows (written before `phase` existed) have `phase IS NULL`, i.e. `IFNULL(phase,'')
    /// = ''`. They belong to *both* IN-lists below: since we don't know which kind of path they
    /// hold, a legacy row clears under whichever rule fits it -- the file rule if its path turns
    /// out to match a re-seen file, otherwise the completed-scan rule. Leaving `''` out of both
    /// lists (as an earlier version of this function did) makes every legacy row immortal --
    /// never delete without one of `''` or {phase already known to be one of the two buckets}.
    pub fn clear_resolved_scan_errors(
        &self,
        volume_id: &str,
        scan_started_at: i64,
        completed: bool,
    ) -> anyhow::Result<usize> {
        let mut removed = self.conn.execute(
            "DELETE FROM scan_errors
              WHERE volume_id=?1
                AND IFNULL(phase,'') IN ('metadata','read','archive_open','')
                AND path IN (SELECT relative_path FROM files
                              WHERE volume_id=?1 AND last_seen_at >= ?2)",
            params![volume_id, scan_started_at],
        )?;

        if completed {
            // The upsert refreshes `occurred_at` to this scan's stamp, so anything still older
            // was not re-recorded -- the directory opened cleanly this time.
            removed += self.conn.execute(
                "DELETE FROM scan_errors
                  WHERE volume_id=?1 AND IFNULL(phase,'') IN ('walk','archive_entry','')
                    AND occurred_at < ?2",
                params![volume_id, scan_started_at],
            )?;
        }
        Ok(removed)
    }

    /// Per-volume answer to "is this catalogue complete, and what is missing?"
    pub fn volume_completeness(&self, volume_id: &str) -> anyhow::Result<Completeness> {
        let sql = format!(
            "SELECT {BUCKET_SQL} AS bucket, count(*) FROM scan_errors e
               LEFT JOIN files f
                 ON f.volume_id=e.volume_id AND f.relative_path=e.path AND f.status='active'
              WHERE e.volume_id=?1 GROUP BY bucket"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut out = Completeness::default();
        let rows = stmt.query_map(params![volume_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (bucket, n) = row?;
            match bucket.as_str() {
                "absent" => out.absent = n,
                "unverified" => out.unverified = n,
                "unreadable_dir" => out.unreadable_dirs = n,
                _ => {}
            }
        }
        Ok(out)
    }

    /// Bounded, deterministically-ordered list of scan errors for one volume, optionally filtered
    /// by `bucket` (`"absent"` / `"unverified"` / `"unreadable_dir"`) and/or `kind`.
    pub fn volume_scan_errors(
        &self,
        volume_id: &str,
        bucket: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<ScanErrorRow>> {
        let sql = format!(
            "SELECT e.path, e.reason, e.kind, e.phase, e.occurred_at, {BUCKET_SQL} AS bucket
               FROM scan_errors e
               LEFT JOIN files f
                 ON f.volume_id=e.volume_id AND f.relative_path=e.path AND f.status='active'
              WHERE e.volume_id=?1
                AND (?2 IS NULL OR bucket = ?2)
                AND (?3 IS NULL OR IFNULL(e.kind,'') = ?3)
              ORDER BY e.path LIMIT ?4 OFFSET ?5"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![volume_id, bucket, kind, limit as i64, offset as i64],
            |r| {
                Ok(ScanErrorRow {
                    path: r.get(0)?,
                    reason: r.get(1)?,
                    kind: r.get(2)?,
                    phase: r.get(3)?,
                    occurred_at: r.get(4)?,
                    bucket: r.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Per-volume answer to "is this catalogue complete, and what is missing?"
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Completeness {
    /// Files with no catalogue row: invisible to search and dedup. The real hole.
    pub absent: i64,
    /// Files with a row from an earlier scan that this scan could not re-read, so the stored hash
    /// may be stale -- which can pair the wrong files during duplicate review.
    pub unverified: i64,
    /// Directories the walk could not open. Never counted as one missing file: the number of files
    /// beneath an unopenable directory is unknown, and printing 1 would make a denied folder of
    /// 40,000 photos look like a denied `System Volume Information`.
    pub unreadable_dirs: i64,
}

impl Completeness {
    pub fn is_complete(&self) -> bool {
        self.absent == 0 && self.unverified == 0 && self.unreadable_dirs == 0
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanErrorRow {
    pub path: String,
    pub reason: String,
    pub kind: Option<String>,
    pub phase: Option<String>,
    pub occurred_at: i64,
    pub bucket: String,
}

/// `IFNULL(phase,'')` throughout: rows written before classification have a NULL phase, and
/// `phase <> 'walk'` is NULL for those -- which would drop them from *both* buckets and
/// under-report the very thing this feature measures.
const BUCKET_SQL: &str = "CASE WHEN IFNULL(e.phase,'')='walk' THEN 'unreadable_dir' \
                               WHEN f.id IS NULL THEN 'absent' ELSE 'unverified' END";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use std::io::{Error, ErrorKind};

    #[test]
    fn classification_never_reads_the_message_text() {
        // The dev machine's Windows is Italian, so io::Error messages arrive translated.
        // Classification must come from ErrorKind and the raw OS code, never the string --
        // these errors carry deliberately misleading text to prove it.
        assert_eq!(
            classify_io(&Error::new(
                ErrorKind::PermissionDenied,
                "totally unrelated words"
            )),
            "permission"
        );
        assert_eq!(
            classify_io(&Error::new(ErrorKind::NotFound, "permission denied")),
            "not_found"
        );
        assert_eq!(classify_io(&Error::from_raw_os_error(32)), "locked");
        assert_eq!(classify_io(&Error::from_raw_os_error(33)), "locked");
        assert_eq!(
            classify_io(&Error::other("file is locked")),
            "io",
            "text saying 'locked' must not make it 'locked'"
        );
    }

    #[test]
    fn recording_the_same_path_twice_updates_rather_than_duplicates() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();

        cat.log_scan_error(Some("v"), "a/x.pst", "read: locked", "read", "locked", 100)
            .unwrap();
        cat.log_scan_error(Some("v"), "a/x.pst", "read: i/o error", "read", "io", 200)
            .unwrap();

        let (n, reason, kind, at): (i64, String, String, i64) = cat
            .conn
            .query_row(
                "SELECT count(*), max(reason), max(kind), max(occurred_at) FROM scan_errors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "one row per path, however many scans hit it");
        assert_eq!(reason, "read: i/o error", "the latest failure wins");
        assert_eq!(kind, "io");
        assert_eq!(at, 200);
    }

    #[test]
    fn completeness_splits_absent_unverified_and_unreadable_directories() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "V".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        // A file that IS catalogued but could not be re-read -> unverified (its hash may be stale).
        cat.upsert_file(
            &crate::catalog::models::NewFile {
                volume_id: "v".into(),
                relative_path: "have.pst".into(),
                filename: "have.pst".into(),
                extension: "pst".into(),
                size_bytes: 10,
                content_hash: "H".into(),
                created_time: Some(1),
                modified_time: Some(1),
                accessed_time: Some(1),
                // NOTE: Category lives in `crate::category`, not in `catalog::models`.
                category: crate::category::Category::Other,
                container_chain: None,
            },
            1,
        )
        .unwrap();

        cat.log_scan_error(Some("v"), "have.pst", "read: locked", "read", "locked", 10)
            .unwrap();
        cat.log_scan_error(Some("v"), "gone.jpg", "read: i/o", "read", "io", 10)
            .unwrap();
        cat.log_scan_error(
            Some("v"),
            "sysvol",
            "walk: denied",
            "walk",
            "permission",
            10,
        )
        .unwrap();
        // A legacy row from before classification existed: phase IS NULL. It must still be counted.
        cat.conn
            .execute(
                "INSERT INTO scan_errors(volume_id,path,reason,occurred_at) VALUES ('v','old.bin','read: ?',5)",
                [],
            )
            .unwrap();

        let c = cat.volume_completeness("v").unwrap();
        assert_eq!(c.unverified, 1, "have.pst is catalogued but unverified");
        assert_eq!(c.absent, 2, "gone.jpg and the legacy old.bin are absent");
        assert_eq!(
            c.unreadable_dirs, 1,
            "a walk error is never counted as a missing file"
        );
        assert!(!c.is_complete());

        let rows = cat
            .volume_scan_errors("v", Some("absent"), None, 50, 0)
            .unwrap();
        let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["gone.jpg", "old.bin"], "ordered by path");
    }

    #[test]
    fn a_volume_with_no_errors_is_complete() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        let c = cat.volume_completeness("v").unwrap();
        assert_eq!((c.absent, c.unverified, c.unreadable_dirs), (0, 0, 0));
        assert!(c.is_complete(), "no errors means complete, not unknown");
    }
}

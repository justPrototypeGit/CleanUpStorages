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
}

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
}

//! The one user-initiated hard delete: empty a drive's `_ToDelete` and mark rows purged.

use std::path::Path;
use crate::catalog::Catalog;

const QUARANTINE_DIR: &str = "_ToDelete";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub files_purged: usize,
    pub bytes_reclaimed: i64,
}

/// Empty the drive's `_ToDelete` quarantine and mark every quarantined row on the volume
/// `purged`. Verifies the mount's marker equals `expected_volume_id` before touching anything.
/// If `_ToDelete` is absent, this is a no-op success.
pub fn purge_volume(
    cat: &Catalog, mount_root: &Path, expected_volume_id: &str, now: i64,
) -> anyhow::Result<PurgeOutcome> {
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to purge an unidentified drive",
            mount_root.display()),
    }

    let bytes_reclaimed = cat.recoverable_bytes(expected_volume_id)?;
    let rows = cat.quarantined_rows(expected_volume_id)?;

    let qdir = mount_root.join(QUARANTINE_DIR);
    if qdir.exists() {
        std::fs::remove_dir_all(&qdir)?; // THE hard delete — user-initiated only.
    }

    let mut files_purged = 0usize;
    for rec in &rows {
        cat.mark_purged(rec.id, now)?;
        files_purged += 1;
    }
    cat.log_action("purge", &serde_json::json!({
        "volume_id": expected_volume_id, "files_purged": files_purged,
        "bytes_reclaimed": bytes_reclaimed,
    }).to_string(), now)?;

    Ok(PurgeOutcome { files_purged, bytes_reclaimed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::{Volume, FileStatus};
    use std::fs;

    #[test]
    fn purge_deletes_quarantine_and_marks_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("_ToDelete/Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("_ToDelete/Photos/a.jpg"), b"DEADBEEF").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        // a quarantined row pointing into _ToDelete
        let f = crate::catalog::models::NewFile {
            volume_id: "vol-1".into(), relative_path: "_ToDelete/Photos/a.jpg".into(),
            filename: "a.jpg".into(), extension: "jpg".into(), size_bytes: 8,
            content_hash: "h".into(), created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Photo, container_chain: None };
        cat.upsert_file(&f, 100).unwrap();
        let id = cat.active_file_id("vol-1", "_ToDelete/Photos/a.jpg").unwrap().unwrap();
        cat.mark_quarantined(id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 150).unwrap();

        let out = purge_volume(&cat, &root, "vol-1", 200).unwrap();
        assert_eq!(out, PurgeOutcome { files_purged: 1, bytes_reclaimed: 8 });
        assert!(!root.join("_ToDelete").exists());
        assert_eq!(cat.get_file(id).unwrap().unwrap().status, FileStatus::Purged);
    }

    #[test]
    fn purge_with_no_quarantine_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let out = purge_volume(&cat, &root, "vol-1", 200).unwrap();
        assert_eq!(out, PurgeOutcome::default());
    }
}

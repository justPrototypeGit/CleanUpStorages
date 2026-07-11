//! The one user-initiated hard delete: empty a drive's `_ToDelete` and mark rows purged.

use std::path::Path;
use crate::catalog::Catalog;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub files_purged: usize,
    pub bytes_reclaimed: i64,
}

/// Empty the drive's `_ToDelete` quarantine and mark every quarantined row on the volume
/// `purged`. Verifies the mount's marker equals `expected_volume_id` before touching anything.
/// If `_ToDelete` is absent, the delete itself is a no-op, but any rows still `quarantined`
/// for this volume are reconciled to `purged` — a quarantined row with no corresponding file
/// on disk means the file is already gone (e.g. the user emptied the folder manually), so the
/// catalog is brought in line with reality rather than left stale.
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

    let qdir = mount_root.join(crate::volume::QUARANTINE_DIR);
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

#[derive(Debug, Default)]
pub struct PurgeAllOutcome {
    pub purged: Vec<(String, usize, i64)>,
    pub skipped_unmounted: Vec<String>,
    pub errors: Vec<String>,
}

/// Purge every volume that has reclaimable quarantine. Mounted volumes are purged via
/// `purge_volume`; volumes with reclaimable space that aren't currently mounted are reported in
/// `skipped_unmounted` (you can't delete files on a disk that isn't connected).
pub fn purge_all(
    cat: &Catalog,
    mounts: &std::collections::HashMap<String, std::path::PathBuf>,
    now: i64,
) -> anyhow::Result<PurgeAllOutcome> {
    let mut out = PurgeAllOutcome::default();
    for (volume_id, _label, _files, _bytes) in cat.volume_stats()? {
        let reclaimable = cat.recoverable_bytes(&volume_id)?;
        if reclaimable == 0 { continue; }
        match mounts.get(&volume_id) {
            Some(root) => match purge_volume(cat, root, &volume_id, now) {
                Ok(o) => out.purged.push((volume_id, o.files_purged, o.bytes_reclaimed)),
                Err(e) => out.errors.push(format!("{volume_id}: {e}")),
            },
            None => out.skipped_unmounted.push(volume_id),
        }
    }
    Ok(out)
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
        let id = cat.loose_file_id("vol-1", "_ToDelete/Photos/a.jpg").unwrap().unwrap();
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

    #[test]
    fn wrong_marker_aborts_and_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("_ToDelete/Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("_ToDelete/Photos/a.jpg"), b"DATA").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();

        // expected volume id does NOT match the marker -> must bail, delete nothing
        let res = purge_volume(&cat, &root, "vol-DIFFERENT", 200);
        assert!(res.is_err());
        assert!(root.join("_ToDelete/Photos/a.jpg").exists());
    }

    #[test]
    fn missing_marker_aborts_and_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("_ToDelete")).unwrap();
        fs::write(root.join("_ToDelete/x"), b"DATA").unwrap();
        // NO marker file written
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();

        let res = purge_volume(&cat, &root, "vol-1", 200);
        assert!(res.is_err());
        assert!(root.join("_ToDelete/x").exists());
    }

    /// Build a marked drive with one quarantined loose file, matching the setup style of
    /// `purge_deletes_quarantine_and_marks_rows` above (marker + a directly-inserted quarantined
    /// row, rather than going through `quarantine::quarantine_files`, which enforces a
    /// "not the last copy" guard we don't need here).
    fn setup_quarantined_drive() -> (tempfile::TempDir, std::path::PathBuf, String, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("_ToDelete/Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("_ToDelete/Photos/a.jpg"), b"DEADBEEF").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let f = crate::catalog::models::NewFile {
            volume_id: "vol-1".into(), relative_path: "_ToDelete/Photos/a.jpg".into(),
            filename: "a.jpg".into(), extension: "jpg".into(), size_bytes: 8,
            content_hash: "h".into(), created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Photo, container_chain: None };
        cat.upsert_file(&f, 100).unwrap();
        let id = cat.loose_file_id("vol-1", "_ToDelete/Photos/a.jpg").unwrap().unwrap();
        cat.mark_quarantined(id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 150).unwrap();

        let vid = "vol-1".to_string();
        (tmp, root, vid, cat)
    }

    #[test]
    fn purge_all_purges_mounted_and_reports_unmounted() {
        // Reuse this module's helper that builds a marked drive with one quarantined file.
        let (_tmp, root, vid, cat) = setup_quarantined_drive(); // existing-style helper
        let mut mounts = std::collections::HashMap::new();
        mounts.insert(vid.clone(), root.clone());
        let out = purge_all(&cat, &mounts, 1000).unwrap();
        assert_eq!(out.purged.len(), 1);
        assert_eq!(out.purged[0].0, vid);
        assert!(out.skipped_unmounted.is_empty());
        // Second run: nothing left to reclaim.
        let out2 = purge_all(&cat, &mounts, 1001).unwrap();
        assert!(out2.purged.is_empty());
    }

    #[test]
    fn purge_all_reports_unmounted_volume_without_touching_it() {
        // A second volume has reclaimable quarantine but is NOT in the mounts map.
        let (_tmp, root, vid, cat) = setup_quarantined_drive();

        let tmp2 = tempfile::tempdir().unwrap();
        let root2 = tmp2.path().join("drive2");
        fs::create_dir_all(root2.join("_ToDelete")).unwrap();
        fs::write(root2.join(".cleanupstorages_id"), "vol-2").unwrap();
        fs::write(root2.join("_ToDelete/b.jpg"), b"DATA").unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-2".into(), label: "E".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let f2 = crate::catalog::models::NewFile {
            volume_id: "vol-2".into(), relative_path: "_ToDelete/b.jpg".into(),
            filename: "b.jpg".into(), extension: "jpg".into(), size_bytes: 4,
            content_hash: "h2".into(), created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Photo, container_chain: None };
        cat.upsert_file(&f2, 100).unwrap();
        let id2 = cat.loose_file_id("vol-2", "_ToDelete/b.jpg").unwrap().unwrap();
        cat.mark_quarantined(id2, "_ToDelete/b.jpg", "b.jpg", 150).unwrap();

        // Only vol-1 is mounted; vol-2 is not.
        let mut mounts = std::collections::HashMap::new();
        mounts.insert(vid.clone(), root.clone());

        let out = purge_all(&cat, &mounts, 1000).unwrap();
        assert_eq!(out.purged.len(), 1);
        assert_eq!(out.purged[0].0, vid);
        assert_eq!(out.skipped_unmounted, vec!["vol-2".to_string()]);
        assert!(out.errors.is_empty());
        // vol-2's quarantine directory is untouched.
        assert!(root2.join("_ToDelete/b.jpg").exists());
    }

    #[test]
    fn reconciles_quarantined_rows_when_todelete_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        // NO _ToDelete directory on disk, but a quarantined row exists in the catalog
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let f = crate::catalog::models::NewFile {
            volume_id: "vol-1".into(), relative_path: "_ToDelete/gone.jpg".into(),
            filename: "gone.jpg".into(), extension: "jpg".into(), size_bytes: 4,
            content_hash: "h".into(), created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Photo, container_chain: None };
        cat.upsert_file(&f, 100).unwrap();
        let id = cat.loose_file_id("vol-1", "_ToDelete/gone.jpg").unwrap().unwrap();
        cat.mark_quarantined(id, "_ToDelete/gone.jpg", "gone.jpg", 150).unwrap();

        // _ToDelete absent -> delete is a no-op, but the quarantined row reconciles to purged
        let out = purge_volume(&cat, &root, "vol-1", 200).unwrap();
        assert_eq!(out.files_purged, 1);
        assert_eq!(cat.get_file(id).unwrap().unwrap().status, FileStatus::Purged);
    }
}

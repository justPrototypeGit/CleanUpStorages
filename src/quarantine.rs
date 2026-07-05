//! Move confirmed-duplicate loose files to a same-drive `_ToDelete` quarantine (reversible).

use std::path::Path;
use crate::catalog::Catalog;
use crate::catalog::models::FileStatus;

const QUARANTINE_DIR: &str = "_ToDelete";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct QuarantineOutcome {
    pub quarantined: usize,
    pub skipped: usize,
}

/// Move each given file to the drive's `_ToDelete` quarantine, transactionally recording each.
/// Verifies the mount's marker equals `expected_volume_id` before touching anything.
pub fn quarantine_files(
    cat: &Catalog, mount_root: &Path, expected_volume_id: &str, ids: &[i64], now: i64,
) -> anyhow::Result<QuarantineOutcome> {
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to quarantine on an unidentified drive",
            mount_root.display()),
    }

    let mut out = QuarantineOutcome::default();

    for &id in ids {
        let skip = |cat: &Catalog, reason: String, out: &mut QuarantineOutcome| -> anyhow::Result<()> {
            cat.log_action("quarantine_skip",
                &serde_json::json!({"file_id": id, "reason": reason}).to_string(), now)?;
            out.skipped += 1;
            Ok(())
        };

        let Some(rec) = cat.get_file(id)? else { skip(cat, "no such file id".into(), &mut out)?; continue; };
        if rec.volume_id != expected_volume_id
            || rec.container_chain.is_some()
            || rec.status != FileStatus::Active
        {
            skip(cat, "not a loose active file on this volume".into(), &mut out)?; continue;
        }
        // Exclude only this id (not the whole batch): each successful quarantine commits
        // immediately, so a doomed sibling processed earlier in this same batch is already
        // non-active by the time we get here and can't be mistaken for a survivor.
        if !cat.active_survivor_exists(&rec.content_hash, &[id])? {
            skip(cat, "no surviving active copy would remain".into(), &mut out)?; continue;
        }

        let src = mount_root.join(&rec.relative_path);
        if !src.is_file() {
            skip(cat, format!("file not found on disk at {}", rec.relative_path), &mut out)?; continue;
        }
        let dest_rel = quarantine_dest(mount_root, &rec.relative_path);
        let dest = mount_root.join(&dest_rel);
        if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }

        match std::fs::rename(&src, &dest) {
            Ok(()) => {
                cat.mark_quarantined(id, &dest_rel.replace('\\', "/"), &rec.relative_path, now)?;
                cat.log_action("quarantine", &serde_json::json!({
                    "file_id": id, "volume_id": rec.volume_id,
                    "from": rec.relative_path, "to": dest_rel.replace('\\', "/"),
                    "hash": rec.content_hash,
                }).to_string(), now)?;
                out.quarantined += 1;
            }
            Err(e) => {
                // Cross-device or permission error: DO NOT copy+delete. Leave original in place.
                cat.log_action("quarantine_error", &serde_json::json!({
                    "file_id": id, "from": rec.relative_path, "error": e.to_string()
                }).to_string(), now)?;
                out.skipped += 1;
            }
        }
    }
    Ok(out)
}

/// Compute a collision-free `_ToDelete/<origin>` relative path (adds ` (n)` before the extension).
fn quarantine_dest(mount_root: &Path, origin_rel: &str) -> String {
    let base = format!("{QUARANTINE_DIR}/{origin_rel}");
    if !mount_root.join(&base).exists() {
        return base;
    }
    // Split extension off the last path segment for suffixing.
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) if !s.ends_with('/') => (s.to_string(), format!(".{e}")),
        _ => (base.clone(), String::new()),
    };
    for n in 1.. {
        let candidate = format!("{stem} ({n}){ext}");
        if !mount_root.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::Volume;
    use std::fs;

    // A fake mounted drive with a marker and two identical files.
    fn fake_drive() -> (tempfile::TempDir, Catalog, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("Photos")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("Photos/a.jpg"), b"IDENTICAL").unwrap();
        fs::write(root.join("copy_a.jpg"), b"IDENTICAL").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = crate::volume::VolumeIdentity {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into() };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100).unwrap();
        (tmp, cat, root.to_string_lossy().into_owned())
    }

    #[test]
    fn quarantines_a_duplicate_and_moves_the_file() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        // pick the id of Photos/a.jpg
        let id = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(out, QuarantineOutcome { quarantined: 1, skipped: 0 });
        // file moved
        assert!(!root.join("Photos/a.jpg").exists());
        assert!(root.join("_ToDelete/Photos/a.jpg").exists());
        // row updated
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, crate::catalog::models::FileStatus::Quarantined);
        assert_eq!(rec.original_path.as_deref(), Some("Photos/a.jpg"));
        // the surviving copy is untouched
        assert!(root.join("copy_a.jpg").exists());
        let _ = tmp;
    }

    #[test]
    fn refuses_to_quarantine_the_last_copy() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let a = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let b = cat.active_file_id("vol-1", "copy_a.jpg").unwrap().unwrap();
        // trying to quarantine BOTH members leaves no survivor -> second is skipped
        let out = quarantine_files(&cat, &root, "vol-1", &[a, b], 200).unwrap();
        assert_eq!(out.quarantined, 1);
        assert_eq!(out.skipped, 1);
        // exactly one of the two files remains on disk
        let remaining = [root.join("Photos/a.jpg").exists(), root.join("copy_a.jpg").exists()]
            .iter().filter(|x| **x).count();
        assert_eq!(remaining, 1);
        let _ = tmp;
    }

    #[test]
    fn wrong_marker_aborts() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.active_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let err = quarantine_files(&cat, &root, "vol-DIFFERENT", &[id], 200);
        assert!(err.is_err());
        assert!(root.join("Photos/a.jpg").exists()); // nothing moved
        let _ = tmp;
    }
}

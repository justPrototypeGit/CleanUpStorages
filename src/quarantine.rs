//! Move confirmed-duplicate loose files to a same-drive `_ToDelete` quarantine (reversible).

use crate::catalog::models::FileStatus;
use crate::catalog::Catalog;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct QuarantineOutcome {
    pub quarantined: usize,
    pub skipped: usize,
}

/// Move each given file to the drive's `_ToDelete` quarantine, transactionally recording each.
/// Verifies the mount's marker equals `expected_volume_id` before touching anything.
pub fn quarantine_files(
    cat: &Catalog,
    mount_root: &Path,
    expected_volume_id: &str,
    ids: &[i64],
    now: i64,
) -> anyhow::Result<QuarantineOutcome> {
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()
        ),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to quarantine on an unidentified drive",
            mount_root.display()
        ),
    }

    let mut out = QuarantineOutcome::default();

    let mut cache = crate::verify::HashCache::default();

    for &id in ids {
        let skip =
            |cat: &Catalog, reason: String, out: &mut QuarantineOutcome| -> anyhow::Result<()> {
                cat.log_action(
                    "quarantine_skip",
                    &serde_json::json!({"file_id": id, "reason": reason}).to_string(),
                    now,
                )?;
                out.skipped += 1;
                Ok(())
            };

        let Some(rec) = cat.get_file(id)? else {
            skip(cat, "no such file id".into(), &mut out)?;
            continue;
        };
        if rec.volume_id != expected_volume_id
            || rec.container_chain.is_some()
            || rec.status != FileStatus::Active
        {
            skip(
                cat,
                "not a loose active file on this volume".into(),
                &mut out,
            )?;
            continue;
        }
        let src = mount_root.join(&rec.relative_path);
        if !src.is_file() {
            skip(
                cat,
                format!("file not found on disk at {}", rec.relative_path),
                &mut out,
            )?;
            continue;
        }

        // Re-hash what we are about to move, rather than trusting the catalogue. The incremental
        // scan skips re-hashing when size and second-granularity mtime match, so a same-size edit
        // made within one second of the recorded mtime leaves a stale hash (#4) — and a stale hash
        // is exactly how a unique file gets mistaken for a duplicate.
        let live_hash = match cache.file(&src) {
            Ok(h) => h,
            Err(e) => {
                skip(
                    cat,
                    format!("could not re-read {}: {e}", rec.relative_path),
                    &mut out,
                )?;
                continue;
            }
        };

        // Disk-aware "never remove the last copy" guard, shared with repack so the two cannot
        // drift. Exclude only this id (not the whole batch): each successful quarantine commits
        // immediately, so a doomed sibling processed earlier is already non-active by now and
        // cannot be mistaken for a survivor.
        match crate::verify::find_surviving_copy(
            cat,
            mount_root,
            expected_volume_id,
            id,
            &rec.content_hash,
            &live_hash,
            &mut cache,
        )? {
            crate::verify::Survivor::Verified => {}
            crate::verify::Survivor::NotFound(reason) => {
                skip(cat, reason, &mut out)?;
                continue;
            }
        }

        let dest_rel = quarantine_dest(cat, mount_root, expected_volume_id, &rec.relative_path)?;
        let dest = mount_root.join(&dest_rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match std::fs::rename(&src, &dest) {
            Ok(()) => {
                cat.mark_quarantined(id, &dest_rel.replace('\\', "/"), &rec.relative_path, now)?;
                cat.log_action(
                    "quarantine",
                    &serde_json::json!({
                        "file_id": id, "volume_id": rec.volume_id,
                        "from": rec.relative_path, "to": dest_rel.replace('\\', "/"),
                        "hash": rec.content_hash,
                    })
                    .to_string(),
                    now,
                )?;
                out.quarantined += 1;
            }
            Err(e) => {
                // Cross-device or permission error: DO NOT copy+delete. Leave original in place.
                cat.log_action(
                    "quarantine_error",
                    &serde_json::json!({
                        "file_id": id, "from": rec.relative_path, "error": e.to_string()
                    })
                    .to_string(),
                    now,
                )?;
                out.skipped += 1;
            }
        }
    }
    Ok(out)
}

/// Compute a collision-free `_ToDelete/<origin>` relative path (adds ` (n)` before the
/// extension of the LAST path segment only, preserving the directory). A candidate is only
/// acceptable when NEITHER the file exists on disk NOR a loose catalog row already claims it
/// (e.g. a purged row still occupying the loose unique index) — avoiding a post-rename orphan.
pub(crate) fn quarantine_dest(
    cat: &Catalog,
    mount_root: &Path,
    volume_id: &str,
    origin_rel: &str,
) -> anyhow::Result<String> {
    let base = format!("{}/{origin_rel}", crate::volume::QUARANTINE_DIR);
    let taken = |cat: &Catalog, cand: &str| -> anyhow::Result<bool> {
        Ok(mount_root.join(cand).exists() || cat.loose_path_taken(volume_id, cand)?)
    };
    if !taken(cat, &base)? {
        return Ok(base);
    }
    let (dir, seg) = match base.rsplit_once('/') {
        Some((d, s)) => (format!("{d}/"), s.to_string()),
        None => (String::new(), base.clone()),
    };
    let (stem, ext) = match seg.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (seg.clone(), String::new()),
    };
    for n in 1.. {
        let cand = format!("{dir}{stem} ({n}){ext}");
        if !taken(cat, &cand)? {
            return Ok(cand);
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
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        let ident = crate::volume::VolumeIdentity {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
        };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100).unwrap();
        (tmp, cat, root.to_string_lossy().into_owned())
    }

    #[test]
    fn quarantines_a_duplicate_and_moves_the_file() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        // pick the id of Photos/a.jpg
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(
            out,
            QuarantineOutcome {
                quarantined: 1,
                skipped: 0
            }
        );
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
    fn refuses_when_the_file_no_longer_matches_its_catalogued_hash() {
        // The #4 scenario: the incremental scan skips re-hashing when size and second-granularity
        // mtime match, so a same-size edit can leave a stale hash. Acting on that stale verdict
        // would quarantine a file whose content is now unique.
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let before = cat.get_file(id).unwrap().unwrap().content_hash;

        // Same byte count, different content — exactly what the size+mtime skip cannot see.
        let len = std::fs::read(root.join("Photos/a.jpg")).unwrap().len();
        std::fs::write(root.join("Photos/a.jpg"), vec![b'Z'; len]).unwrap();

        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(
            out,
            QuarantineOutcome {
                quarantined: 0,
                skipped: 1
            },
            "a file that no longer matches its catalogued hash must not be moved"
        );
        assert!(
            root.join("Photos/a.jpg").exists(),
            "the file stays exactly where it was"
        );
        assert_eq!(
            cat.get_file(id).unwrap().unwrap().status,
            crate::catalog::models::FileStatus::Active
        );
        // The skip reason must name the drift, not blame a missing survivor.
        let reason = last_skip_reason(&cat);
        assert!(
            reason.contains("content changed since the last scan"),
            "unhelpful skip reason: {reason}"
        );
        assert_eq!(before, cat.get_file(id).unwrap().unwrap().content_hash);
        let _ = tmp;
    }

    #[test]
    fn refuses_when_the_survivor_on_disk_no_longer_matches() {
        // The victim is unchanged, but the copy we were relying on has drifted. Quarantining now
        // would leave zero copies of these bytes outside _ToDelete.
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();

        let len = std::fs::read(root.join("copy_a.jpg")).unwrap().len();
        std::fs::write(root.join("copy_a.jpg"), vec![b'Q'; len]).unwrap();

        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(
            out.quarantined, 0,
            "the survivor no longer holds these bytes"
        );
        assert_eq!(out.skipped, 1);
        assert!(root.join("Photos/a.jpg").exists());
        assert!(last_skip_reason(&cat).contains("no surviving copy verified"));
        let _ = tmp;
    }

    #[test]
    fn a_verified_copy_inside_a_zip_counts_as_a_survivor() {
        // Most of a real corpus is archive entries, so "the twin is zipped" is the common case,
        // not an edge one. The bytes genuinely survive inside the archive, so the move is allowed
        // — but only after decompressing that entry and confirming it really holds them.
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let payload = std::fs::read(root.join("Photos/a.jpg")).unwrap();

        // A zip on the drive holding the same bytes, catalogued as an archive entry.
        let zip_rel = "archive.zip";
        {
            let f = std::fs::File::create(root.join(zip_rel)).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("a.jpg", opts).unwrap();
            std::io::Write::write_all(&mut zw, &payload).unwrap();
            zw.finish().unwrap();
        }
        let mut slice: &[u8] = &payload;
        let hash = crate::hashing::hash_reader(&mut slice).unwrap();
        cat.upsert_archive_entry(
            "vol-1",
            zip_rel,
            &crate::archive::ArchiveEntry {
                container_chain: "a.jpg".into(),
                filename: "a.jpg".into(),
                extension: "jpg".into(),
                size_bytes: payload.len() as i64,
                content_hash: hash,
            },
            None,
            100,
        )
        .unwrap();

        // Remove the loose twin so the zip entry is the ONLY other copy.
        let sibling = cat.loose_file_id("vol-1", "copy_a.jpg").unwrap().unwrap();
        cat.conn
            .execute("DELETE FROM files WHERE id=?1", [sibling])
            .unwrap();
        std::fs::remove_file(root.join("copy_a.jpg")).unwrap();

        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(
            out.quarantined,
            1,
            "an archived copy preserves the bytes, so the loose duplicate may be quarantined: {}",
            last_skip_reason(&cat)
        );
        assert!(root.join("_ToDelete/Photos/a.jpg").exists());
        let _ = tmp;
    }

    #[test]
    fn a_zip_entry_that_does_not_match_is_not_a_survivor() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();

        // A zip catalogued under the same hash whose entry actually holds different bytes — the
        // exact lie a stale catalogue tells.
        {
            let f = std::fs::File::create(root.join("archive.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("a.jpg", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"totally different bytes").unwrap();
            zw.finish().unwrap();
        }
        cat.upsert_archive_entry(
            "vol-1",
            "archive.zip",
            &crate::archive::ArchiveEntry {
                container_chain: "a.jpg".into(),
                filename: "a.jpg".into(),
                extension: "jpg".into(),
                size_bytes: 23,
                content_hash: rec.content_hash.clone(),
            },
            None,
            100,
        )
        .unwrap();

        let sibling = cat.loose_file_id("vol-1", "copy_a.jpg").unwrap().unwrap();
        cat.conn
            .execute("DELETE FROM files WHERE id=?1", [sibling])
            .unwrap();
        std::fs::remove_file(root.join("copy_a.jpg")).unwrap();

        let out = quarantine_files(&cat, &root, "vol-1", &[id], 200).unwrap();
        assert_eq!(out.quarantined, 0, "the zip does not hold these bytes");
        assert!(root.join("Photos/a.jpg").exists());
        let _ = tmp;
    }

    /// The reason recorded by the most recent `quarantine_skip` action.
    fn last_skip_reason(cat: &Catalog) -> String {
        cat.conn
            .query_row(
                "SELECT details FROM actions_log WHERE action='quarantine_skip'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    }

    #[test]
    fn refuses_to_quarantine_the_last_copy() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let a = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let b = cat.loose_file_id("vol-1", "copy_a.jpg").unwrap().unwrap();
        // trying to quarantine BOTH members leaves no survivor -> second is skipped
        let out = quarantine_files(&cat, &root, "vol-1", &[a, b], 200).unwrap();
        assert_eq!(out.quarantined, 1);
        assert_eq!(out.skipped, 1);
        // exactly one of the two files remains on disk
        let remaining = [
            root.join("Photos/a.jpg").exists(),
            root.join("copy_a.jpg").exists(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        assert_eq!(remaining, 1);
        let _ = tmp;
    }

    #[test]
    fn wrong_marker_aborts() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let err = quarantine_files(&cat, &root, "vol-DIFFERENT", &[id], 200);
        assert!(err.is_err());
        assert!(root.join("Photos/a.jpg").exists()); // nothing moved
        let _ = tmp;
    }

    #[test]
    fn collision_suffix_targets_last_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cat = Catalog::open(&root.join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        // dotted ANCESTOR dir, final segment has no extension
        std::fs::create_dir_all(root.join("_ToDelete/my.backup")).unwrap();
        std::fs::write(root.join("_ToDelete/my.backup/README"), b"x").unwrap();
        let dest = quarantine_dest(&cat, root, "vol-1", "my.backup/README").unwrap();
        assert_eq!(dest, "_ToDelete/my.backup/README (1)");

        // normal case: extension on the final segment
        std::fs::write(root.join("_ToDelete/my.backup/note.txt"), b"y").unwrap();
        let dest2 = quarantine_dest(&cat, root, "vol-1", "my.backup/note.txt").unwrap();
        assert_eq!(dest2, "_ToDelete/my.backup/note (1).txt");
    }

    #[test]
    fn refuses_when_only_sibling_was_deleted_off_disk() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        // user deletes the OTHER copy outside the tool; catalog still thinks it's active
        std::fs::remove_file(root.join("copy_a.jpg")).unwrap();
        let a = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[a], 200).unwrap();
        assert_eq!(out.quarantined, 0);
        assert_eq!(out.skipped, 1);
        assert!(root.join("Photos/a.jpg").exists()); // the last real copy is untouched
        let _ = tmp;
    }

    #[test]
    fn dest_avoids_catalog_collision_with_purged_row() {
        let (tmp, cat, root) = fake_drive();
        let root = std::path::PathBuf::from(root);
        // Simulate a prior purge: a purged row already holds _ToDelete/Photos/a.jpg
        let mut ghost = crate::catalog::models::NewFile {
            volume_id: "vol-1".into(),
            relative_path: "_ToDelete/Photos/a.jpg".into(),
            filename: "a.jpg".into(),
            extension: "jpg".into(),
            size_bytes: 9,
            content_hash: "old".into(),
            created_time: None,
            modified_time: None,
            accessed_time: None,
            category: crate::category::Category::Photo,
            container_chain: None,
        };
        cat.upsert_file(&ghost, 50).unwrap();
        let ghost_id = cat
            .loose_file_id("vol-1", "_ToDelete/Photos/a.jpg")
            .unwrap()
            .unwrap();
        cat.mark_quarantined(ghost_id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 60)
            .unwrap();
        cat.mark_purged(ghost_id, 70).unwrap();
        let _ = &mut ghost;

        // Now quarantine the live Photos/a.jpg (survivor copy_a.jpg exists on disk)
        let a = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();
        let out = quarantine_files(&cat, &root, "vol-1", &[a], 200).unwrap();
        assert_eq!(out.quarantined, 1);
        let rec = cat.get_file(a).unwrap().unwrap();
        // must NOT reuse the purged row's key; suffix goes before the extension of the
        // last segment (established by `collision_suffix_targets_last_segment`).
        assert_ne!(rec.relative_path, "_ToDelete/Photos/a.jpg");
        assert_eq!(rec.relative_path, "_ToDelete/Photos/a (1).jpg");
        assert!(root.join("_ToDelete/Photos/a (1).jpg").exists());
        let _ = tmp;
    }
}

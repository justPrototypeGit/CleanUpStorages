//! Quarantine a whole redundant directory with a single rename.
//!
//! The rename is an optimisation, NOT a shortcut around the bookkeeping: every file beneath the
//! tree still gets its catalogue row updated exactly as N individual quarantines would have. If the
//! rows were left stale the next scan would report present files as missing.

use crate::catalog::models::FileStatus;
use crate::catalog::Catalog;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct TreeQuarantineOutcome {
    pub files_updated: usize,
    pub dest_relative_path: String,
}

/// Move `tree_path` (relative to `mount_root`) into `_ToDelete`, then update every row beneath it.
pub fn quarantine_tree(
    cat: &Catalog,
    mount_root: &Path,
    expected_volume_id: &str,
    tree_path: &str,
    now: i64,
) -> anyhow::Result<TreeQuarantineOutcome> {
    let tree_path = tree_path.trim_end_matches('/');
    if tree_path.is_empty()
        || tree_path == crate::volume::QUARANTINE_DIR
        || tree_path.starts_with(&format!("{}/", crate::volume::QUARANTINE_DIR))
    {
        anyhow::bail!(
            "refusing to quarantine {tree_path:?}: the volume root and the quarantine itself are off limits"
        );
    }

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

    // Everything beneath the tree, re-read NOW rather than trusting what the UI was showing when
    // the user clicked. Archive entry rows are included: they live under the same relative_path as
    // their container, and leaving them stale would make the next scan call present files missing.
    let rows: Vec<(i64, String, Option<String>, FileStatus)> = {
        let mut stmt = cat.conn.prepare(
            "SELECT id, relative_path, container_chain, status FROM files
              WHERE volume_id=?1
                AND (relative_path=?2 OR relative_path LIKE ?3 ESCAPE '\\')",
        )?;
        let like = format!("{tree_path}/")
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
            + "%";
        let mapped = stmt.query_map(
            rusqlite::params![expected_volume_id, tree_path, like],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    FileStatus::from_db(&r.get::<_, String>(3)?),
                ))
            },
        )?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };

    // A folder INSIDE an archive can never be renamed out of it; the only correct remedy is a
    // repack. The UI already hides the button for these, but the guard belongs here too: the API is
    // reachable without the UI, and "safe because the row query happens to return nothing" is not a
    // safety property worth relying on.
    if let Some(container) = enclosing_archive(cat, expected_volume_id, tree_path)? {
        anyhow::bail!(
            "{tree_path} is inside the archive {container}; it cannot be moved by renaming — \
             use repack to remove entries from an archive"
        );
    }

    if rows.is_empty() {
        anyhow::bail!("no catalogued files under {tree_path:?} on volume {expected_volume_id}");
    }
    // Guards the window between the UI rendering a group and the user confirming it.
    if let Some((_, p, _, s)) = rows.iter().find(|(_, _, _, s)| *s != FileStatus::Active) {
        anyhow::bail!(
            "{p} is {}, no longer active; refusing to quarantine this tree",
            s.as_str()
        );
    }

    let src = mount_root.join(tree_path);
    // A whole redundant archive is a directory in the catalogue (its entries are its tree) but a
    // FILE on disk. Moving it is still one rename, so accept either shape.
    if !src.is_dir() && !src.is_file() {
        anyhow::bail!("{} does not exist on disk", src.display());
    }

    let dest_rel = tree_dest(mount_root, tree_path);
    let dest = mount_root.join(&dest_rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Record the intent BEFORE the rename. If the process dies between the rename and the row
    // updates, this line is what tells the user where their files went.
    cat.log_action(
        "quarantine_tree_begin",
        &serde_json::json!({"volume_id": expected_volume_id, "from": tree_path,
                           "to": dest_rel, "files": rows.len()})
        .to_string(),
        now,
    )?;

    std::fs::rename(&src, &dest).map_err(|e| {
        anyhow::anyhow!(
            "could not move {} to {}: {e}",
            src.display(),
            dest.display()
        )
    })?;

    let mut files_updated = 0usize;
    for (id, rel, chain, _) in &rows {
        let suffix = rel.strip_prefix(tree_path).unwrap_or(rel);
        let new_rel = format!("{dest_rel}{suffix}");
        match chain {
            // Still inside its archive: keep the chain, or every entry would collapse onto the
            // container's own path and break the loose-identity unique index.
            Some(_) => cat.mark_quarantined_in_place(*id, &new_rel, rel, now)?,
            None => cat.mark_quarantined(*id, &new_rel, rel, now)?,
        }
        files_updated += 1;
    }

    cat.log_action(
        "quarantine_tree",
        &serde_json::json!({"volume_id": expected_volume_id, "from": tree_path,
                           "to": dest_rel, "files": files_updated})
        .to_string(),
        now,
    )?;

    Ok(TreeQuarantineOutcome {
        files_updated,
        dest_relative_path: dest_rel,
    })
}

/// The archive strictly containing `tree_path`, if any.
///
/// Derived from the data -- a path with entry rows IS an archive -- rather than from a list of
/// archive extensions, so a newly allow-listed format is covered without a code change, and a real
/// directory that merely happens to be named `stuff.zip` is not.
fn enclosing_archive(
    cat: &Catalog,
    volume_id: &str,
    tree_path: &str,
) -> anyhow::Result<Option<String>> {
    let mut stmt = cat.conn.prepare(
        "SELECT DISTINCT relative_path FROM files
          WHERE volume_id=?1 AND container_chain IS NOT NULL",
    )?;
    let archives = stmt
        .query_map(rusqlite::params![volume_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(archives
        .into_iter()
        .filter(|a| tree_path.starts_with(&format!("{a}/")))
        .max_by_key(|a| a.len()))
}

/// `_ToDelete/<tree_path>`, suffixed ` (n)` if that path is already taken.
fn tree_dest(mount_root: &Path, tree_path: &str) -> String {
    let base = format!("{}/{tree_path}", crate::volume::QUARANTINE_DIR);
    if !mount_root.join(&base).exists() {
        return base;
    }
    for n in 1.. {
        let cand = format!("{base} ({n})");
        if !mount_root.join(&cand).exists() {
            return cand;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::{Category, NewFile, Volume};
    use std::fs;

    fn drive_with_tree() -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("copy/sub")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("copy/a.txt"), b"AAA").unwrap();
        fs::write(root.join("copy/sub/b.txt"), b"BBB").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        for (p, h) in [("copy/a.txt", "HA"), ("copy/sub/b.txt", "HB")] {
            cat.upsert_file(
                &NewFile {
                    volume_id: "vol-1".into(),
                    relative_path: p.into(),
                    filename: p.rsplit('/').next().unwrap().into(),
                    extension: "txt".into(),
                    size_bytes: 3,
                    content_hash: h.into(),
                    created_time: None,
                    modified_time: None,
                    accessed_time: None,
                    category: Category::Document,
                    container_chain: None,
                },
                100,
            )
            .unwrap();
        }
        (tmp, cat, root)
    }

    #[test]
    fn moves_the_whole_tree_with_one_rename_and_updates_every_row() {
        let (_t, cat, root) = drive_with_tree();
        let out = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap();

        assert!(
            !root.join("copy").exists(),
            "the original tree must be gone from its place"
        );
        assert!(
            root.join("_ToDelete/copy/a.txt").is_file(),
            "moved, preserving structure"
        );
        assert!(
            root.join("_ToDelete/copy/sub/b.txt").is_file(),
            "including subfolders"
        );
        assert_eq!(out.files_updated, 2);

        // The bookkeeping is the point: a rename that left the catalogue stale would make the next
        // scan report two present files as missing.
        let rows = cat.quarantined_rows("vol-1").unwrap();
        assert_eq!(rows.len(), 2);
        let mut paths: Vec<String> = rows.iter().map(|r| r.relative_path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["_ToDelete/copy/a.txt", "_ToDelete/copy/sub/b.txt"]
        );
        assert!(rows.iter().all(|r| r.status == FileStatus::Quarantined));
        let mut origins: Vec<String> = rows
            .iter()
            .filter_map(|r| r.original_path.clone())
            .collect();
        origins.sort();
        assert_eq!(
            origins,
            vec!["copy/a.txt", "copy/sub/b.txt"],
            "original paths must survive, or the move is not reversible by hand"
        );
    }

    #[test]
    fn refuses_a_tree_whose_files_are_not_all_active() {
        // Guards the window between the UI rendering a group and the user confirming it: the file
        // goes missing after the group was drawn but before the user clicks. Set the status in
        // place -- `mark_quarantined` would also rewrite relative_path, moving the row out of the
        // tree and testing nothing.
        let (_t, cat, root) = drive_with_tree();
        cat.conn
            .execute(
                "UPDATE files SET status='missing' WHERE relative_path='copy/a.txt'",
                [],
            )
            .unwrap();

        let err = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap_err();
        assert!(err.to_string().contains("no longer active"), "got: {err}");
        assert!(
            root.join("copy/sub/b.txt").exists(),
            "a refusal must not move anything"
        );
    }

    #[test]
    fn refuses_when_the_drive_is_not_the_expected_volume() {
        let (_t, cat, root) = drive_with_tree();
        fs::write(root.join(".cleanupstorages_id"), "vol-OTHER").unwrap();
        let err = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap_err();
        assert!(err.to_string().contains("vol-OTHER"), "got: {err}");
        assert!(
            root.join("copy/a.txt").exists(),
            "nothing may move on the wrong drive"
        );
    }

    #[test]
    fn refuses_to_quarantine_the_quarantine_or_the_volume_root() {
        let (_t, cat, root) = drive_with_tree();
        for bad in ["", "_ToDelete", "_ToDelete/copy"] {
            let err = quarantine_tree(&cat, &root, "vol-1", bad, 200).unwrap_err();
            assert!(
                err.to_string().contains("refusing"),
                "path {bad:?} gave: {err}"
            );
        }
    }

    #[test]
    fn a_whole_redundant_archive_is_quarantined_as_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("backup.zip"), b"ZIPBYTES").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        cat.upsert_file(
            &NewFile {
                volume_id: "vol-1".into(),
                relative_path: "backup.zip".into(),
                filename: "backup.zip".into(),
                extension: "zip".into(),
                size_bytes: 8,
                content_hash: "ZIPHASH".into(),
                created_time: None,
                modified_time: None,
                accessed_time: None,
                category: Category::Other,
                container_chain: None,
            },
            100,
        )
        .unwrap();
        // Two entries, so the collapse-onto-one-path hazard is actually exercised.
        for chain in ["Project/a.txt", "Project/b.txt"] {
            cat.conn
                .execute(
                    "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                         content_hash, category, container_chain, status, first_seen_at,
                         last_seen_at)
                     VALUES ('vol-1','backup.zip','x','txt',3,'HI','document',?1,'active',100,100)",
                    rusqlite::params![chain],
                )
                .unwrap();
        }

        let out = quarantine_tree(&cat, &root, "vol-1", "backup.zip", 200).unwrap();
        assert!(!root.join("backup.zip").exists());
        assert!(
            root.join("_ToDelete/backup.zip").is_file(),
            "moved as one file"
        );
        assert_eq!(
            out.files_updated, 3,
            "the archive row AND both entry rows must update"
        );

        // The entries must still be entries: clearing their container_chain would collapse them
        // onto the archive's own path and violate the loose-identity unique index.
        let still_entries: i64 = cat
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE relative_path='_ToDelete/backup.zip'
                   AND container_chain IS NOT NULL AND status='quarantined'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_entries, 2);
    }

    /// Limits for tests: the compiled-in defaults, with NO ambient environment read.
    fn test_limits() -> crate::archive::ArchiveLimits {
        crate::archive::ArchiveLimits {
            max_depth: 8,
            buffer_max_bytes: 2 * 1024 * 1024 * 1024,
            total_buffer_bytes: 2 * 1024 * 1024 * 1024,
            entry_max_bytes: Some(64 * 1024 * 1024 * 1024),
            ratio_cap: 10_000,
            deny_extensions: crate::config::DEFAULT_DENY
                .iter()
                .map(|s| s.to_string())
                .collect(),
            allow_extensions: Vec::new(),
        }
    }

    #[test]
    fn a_scan_after_a_tree_quarantine_reports_nothing_missing() {
        // THE check that matters for the reliability constraint. A rename that left the catalogue
        // stale would make the very next scan declare present files missing -- and "missing" is the
        // state that makes a user go looking for data they think they have lost. Nothing short of
        // running a real scan afterwards proves this.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("orig/sub")).unwrap();
        fs::create_dir_all(root.join("copy/sub")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        for base in ["orig", "copy"] {
            fs::write(root.join(base).join("a.txt"), b"SAME-A").unwrap();
            fs::write(root.join(base).join("sub/b.txt"), b"SAME-B").unwrap();
        }

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
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100, &test_limits()).unwrap();

        quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap();

        // Scan again. The files are physically in _ToDelete now, and the catalogue must already
        // say so -- so nothing is newly missing.
        let s =
            crate::scanner::scan_volume(&cat, &root, &ident, false, 300, &test_limits()).unwrap();
        assert_eq!(
            s.marked_missing, 0,
            "a tree quarantine must leave the catalogue and the disk in agreement"
        );
        assert_eq!(s.errors, 0);

        let still_missing: i64 = cat
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE status='missing'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_missing, 0);
    }

    #[test]
    fn refuses_a_folder_that_lives_inside_an_archive() {
        // The UI hides the button for these, but the API is reachable without the UI. A file inside
        // a zip cannot be renamed out of it, so this must be refused explicitly rather than left to
        // fail incidentally because no rows happened to match.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("backup.zip"), b"ZIP").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        cat.conn
            .execute(
                "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                     content_hash, category, container_chain, status, first_seen_at, last_seen_at)
                 VALUES ('vol-1','backup.zip','a.txt','txt',3,'H','document','Photos/a.txt',
                         'active',100,100)",
                [],
            )
            .unwrap();

        let err = quarantine_tree(&cat, &root, "vol-1", "backup.zip/Photos", 200).unwrap_err();
        assert!(
            err.to_string().contains("inside the archive") && err.to_string().contains("repack"),
            "the refusal must name the container and point at repack; got: {err}"
        );
        assert!(root.join("backup.zip").exists(), "nothing may move");
    }

    #[test]
    fn a_tree_containing_an_archive_keeps_that_archives_entries_addressable() {
        // The mixed case: a loose folder that happens to hold a zip. Both the zip's own row and its
        // entry rows sit under the folder and must move with it.
        let (_t, cat, root) = drive_with_tree();
        fs::write(root.join("copy/inner.zip"), b"ZZ").unwrap();
        cat.upsert_file(
            &NewFile {
                volume_id: "vol-1".into(),
                relative_path: "copy/inner.zip".into(),
                filename: "inner.zip".into(),
                extension: "zip".into(),
                size_bytes: 2,
                content_hash: "ZH".into(),
                created_time: None,
                modified_time: None,
                accessed_time: None,
                category: Category::Other,
                container_chain: None,
            },
            100,
        )
        .unwrap();
        cat.conn
            .execute(
                "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                     content_hash, category, container_chain, status, first_seen_at, last_seen_at)
                 VALUES ('vol-1','copy/inner.zip','e.txt','txt',1,'EH','document','e.txt',
                         'active',100,100)",
                [],
            )
            .unwrap();

        let out = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap();
        assert_eq!(out.files_updated, 4, "2 loose + the zip + its entry");
        let entry_path: String = cat
            .conn
            .query_row(
                "SELECT relative_path FROM files WHERE container_chain='e.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entry_path, "_ToDelete/copy/inner.zip");
    }
}

//! Crash-safe removal of one entry from a top-level zip (Case 4): build a new archive without the
//! entry, verify every retained entry against the catalog, then swap — original never touched until
//! the rebuild is proven good.

use std::collections::HashMap;
use std::path::Path;

use crate::catalog::Catalog;
use crate::catalog::models::FileStatus;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RepackOutcome {
    pub removed_entry: String,
    pub retained_entries: usize,
}

const REBUILD_DIR: &str = ".rebuild";

/// Free bytes on the filesystem containing `path`, via sysinfo. None if undetermined.
pub fn available_space(path: &Path) -> Option<u64> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point();
        if path.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, d.available_space()));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Crash-safe Case-4 repack: remove one top-level archive entry from `archive_path`, verified
/// against the catalog before anything touches the original. Each guard below aborts leaving the
/// original archive completely untouched; only after `verify_rebuilt` passes (step 7) does the
/// function extract a recovery copy, swap the files, and update the catalog.
pub fn repack_entry(
    cat: &Catalog, mount_root: &Path, expected_volume_id: &str, entry_id: i64, now: i64,
) -> anyhow::Result<RepackOutcome> {
    let entry = load_repackable_entry(cat, expected_volume_id, entry_id)?;
    let chain = entry.container_chain.clone().expect("validated above");

    // 2. Marker gate: refuse if this isn't actually the expected drive.
    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!("drive is volume {vid}, not {expected_volume_id}; aborting"),
        None => anyhow::bail!("no identity marker; refusing to repack on an unidentified drive"),
    }

    // 3. Disk-aware survivor guard: never remove the last remaining copy of this content.
    require_surviving_copy(cat, mount_root, expected_volume_id, &entry)?;

    // 4. Archive must actually be on disk where the catalog says it is.
    let archive_rel = entry.relative_path.clone();
    let archive_path = mount_root.join(&archive_rel);
    if !archive_path.is_file() {
        anyhow::bail!("archive {} not found on disk", archive_rel);
    }
    let archive_size = std::fs::metadata(&archive_path)?.len();

    // 5. Pre-flight free space check.
    if let Some(free) = available_space(&archive_path) {
        if free < archive_size {
            anyhow::bail!(
                "not enough free space on the drive to repack safely ({free} < {archive_size}); \
                 run `purge` to reclaim space first"
            );
        }
    }

    // 6+7. Build the rebuilt archive in a temp file and verify it before anything else happens.
    // Any error here — including a mid-copy IO failure in `rebuild_without` itself, not just a
    // verify mismatch — must leave the original archive untouched and the temp file cleaned up.
    let tmp = rebuild_dir(mount_root).join(format!("{}.rebuilding.tmp", archive_filename(&archive_rel)));
    if let Err(e) = rebuild_and_verify(cat, expected_volume_id, &archive_rel, &archive_path, &tmp, &chain) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let retained = cat.archive_entries(expected_volume_id, &archive_rel)?
        .into_iter()
        .filter(|e| e.container_chain.as_deref() != Some(chain.as_str()))
        .count();

    // 8. Extract the removed entry into _ToDelete (safety net 1) before the swap.
    let extract_rel = quarantine_dest(cat, mount_root, expected_volume_id, &format!("{archive_rel}/{chain}"));
    let extract_path = mount_root.join(&extract_rel);
    if let Some(p) = extract_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&extract_path, extract_entry(&archive_path, &chain)?)?;

    // 9. Swap: original -> _ToDelete/<name>.original.zip (safety net 2); temp -> archive path.
    let original_rel = quarantine_dest(cat, mount_root, expected_volume_id, &format!("{archive_rel}.original.zip"));
    let original_dest = mount_root.join(&original_rel);
    if let Some(p) = original_dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::rename(&archive_path, &original_dest)?;
    std::fs::rename(&tmp, &archive_path)?;

    // 10. Catalog + audit.
    cat.mark_quarantined(
        entry_id,
        &extract_rel.replace('\\', "/"),
        &format!("{archive_rel} › {chain}"),
        now,
    )?;
    let new_hash = crate::hashing::hash_file(&archive_path)?;
    let new_size = std::fs::metadata(&archive_path)?.len() as i64;
    if let Some(arch_id) = cat.active_file_id(expected_volume_id, &archive_rel)? {
        cat.update_archive_hash(arch_id, &new_hash, new_size, now)?;
    }
    cat.log_action(
        "repack",
        &serde_json::json!({
            "volume_id": expected_volume_id, "archive": archive_rel, "removed_entry": chain,
            "extracted_to": extract_rel.replace('\\', "/"),
            "original_saved_to": original_rel.replace('\\', "/"),
            "retained_entries": retained, "new_archive_hash": new_hash,
        })
        .to_string(),
        now,
    )?;

    Ok(RepackOutcome { removed_entry: chain, retained_entries: retained })
}

/// Step 1: load + validate the entry is a top-level (non-nested) active archive entry on the
/// expected volume. Bails with a specific "nested not supported" message for nested chains.
fn load_repackable_entry(
    cat: &Catalog, expected_volume_id: &str, entry_id: i64,
) -> anyhow::Result<crate::catalog::models::FileRecord> {
    let entry = cat.get_file(entry_id)?.ok_or_else(|| anyhow::anyhow!("no such file id {entry_id}"))?;
    if entry.volume_id != expected_volume_id || entry.status != FileStatus::Active {
        anyhow::bail!("entry {entry_id} is not an active file on volume {expected_volume_id}");
    }
    let chain = entry.container_chain.as_deref()
        .ok_or_else(|| anyhow::anyhow!("file {entry_id} is not an archive entry"))?;
    if chain.contains(" › ") {
        anyhow::bail!("entry is inside a nested archive; nested repack is not supported — remove it manually");
    }
    Ok(entry)
}

/// Step 3: a survivor counts if it's a different catalog row AND (on a different volume, or its
/// relative_path still exists on this mount) — mirrors the disk-aware guard from Phase 2a.
fn require_surviving_copy(
    cat: &Catalog, mount_root: &Path, expected_volume_id: &str, entry: &crate::catalog::models::FileRecord,
) -> anyhow::Result<()> {
    let survivor_ok = cat.active_copies(&entry.content_hash)?.iter().any(|s| {
        s.id != entry.id && (s.volume_id != expected_volume_id || mount_root.join(&s.relative_path).exists())
    });
    if !survivor_ok {
        anyhow::bail!("no surviving copy verified — refusing to remove the last copy of this content");
    }
    Ok(())
}

/// Steps 6-7: rebuild into `tmp` and verify every retained catalogued entry matches by hash and
/// that the removed entry is gone. Does not touch `archive_path`; the caller deletes `tmp` on error.
fn rebuild_and_verify(
    cat: &Catalog, expected_volume_id: &str, archive_rel: &str, archive_path: &Path, tmp: &Path, chain: &str,
) -> anyhow::Result<()> {
    rebuild_without(archive_path, tmp, chain)?;
    let expected: HashMap<String, String> = cat.archive_entries(expected_volume_id, archive_rel)?
        .into_iter()
        .filter(|e| e.container_chain.as_deref() != Some(chain))
        .filter_map(|e| e.container_chain.map(|c| (c, e.content_hash)))
        .collect();
    verify_rebuilt(tmp, &expected, chain)
}

/// `mount_root/_ToDelete/.rebuild`, created if absent.
fn rebuild_dir(mount_root: &Path) -> std::path::PathBuf {
    let dir = mount_root.join(crate::volume::QUARANTINE_DIR).join(REBUILD_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn archive_filename(archive_rel: &str) -> String {
    Path::new(archive_rel)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive.zip".into())
}

/// Collision-free `_ToDelete/<rel>` path avoiding both disk AND catalog collisions (mirrors the
/// quarantine collision-avoidance helper from Phase 2a).
fn quarantine_dest(cat: &Catalog, mount_root: &Path, volume_id: &str, origin_rel: &str) -> String {
    let base = format!("{}/{origin_rel}", crate::volume::QUARANTINE_DIR);
    let taken = |cand: &str| {
        mount_root.join(cand).exists() || cat.loose_path_taken(volume_id, cand).unwrap_or(false)
    };
    if !taken(&base) {
        return base;
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
        if !taken(&cand) {
            return cand;
        }
    }
    unreachable!()
}

/// Bytes of one top-level entry.
pub fn extract_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

/// Build `dest_tmp` = every entry of `src_archive` except `exclude_entry`, raw-copied (no recompress).
pub fn rebuild_without(src_archive: &Path, dest_tmp: &Path, exclude_entry: &str) -> anyhow::Result<()> {
    let src_file = std::fs::File::open(src_archive)?;
    let mut src = zip::ZipArchive::new(src_file)?;
    if let Some(parent) = dest_tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out_file = std::fs::File::create(dest_tmp)?;
    let mut writer = zip::ZipWriter::new(out_file);

    let mut found = false;
    for i in 0..src.len() {
        let entry = src.by_index_raw(i)?;
        if entry.name() == exclude_entry {
            found = true;
            continue;
        }
        writer.raw_copy_file(entry)?;
    }
    writer.finish()?;
    if !found {
        let _ = std::fs::remove_file(dest_tmp);
        anyhow::bail!("entry '{exclude_entry}' not found in {}", src_archive.display());
    }
    Ok(())
}

/// Re-hash every expected retained entry and confirm the removed one is absent.
pub fn verify_rebuilt(
    tmp: &Path,
    expected: &HashMap<String, String>,
    must_be_absent: &str,
) -> anyhow::Result<()> {
    let file = std::fs::File::open(tmp)?;
    let mut zip = zip::ZipArchive::new(file)?;

    if zip.by_name(must_be_absent).is_ok() {
        anyhow::bail!("verify failed: removed entry '{must_be_absent}' is still present");
    }
    for (name, want) in expected {
        let mut entry = zip
            .by_name(name)
            .map_err(|_| anyhow::anyhow!("verify failed: retained entry '{name}' missing"))?;
        let got = crate::hashing::hash_reader(&mut entry)?;
        if &got != want {
            anyhow::bail!("verify failed: entry '{name}' hash mismatch (got {got}, want {want})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    fn blake3_hex(bytes: &[u8]) -> String {
        let mut b: &[u8] = bytes;
        crate::hashing::hash_reader(&mut b).unwrap()
    }

    #[test]
    fn rebuild_drops_the_entry_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP"), ("also/keep2.txt", b"K2")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        assert!(extract_entry(&out, "keep.txt").is_ok());
        assert!(extract_entry(&out, "also/keep2.txt").is_ok());
        assert!(extract_entry(&out, "drop.jpg").is_err()); // gone
    }

    #[test]
    fn verify_passes_for_matching_entries_and_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        let mut expected = std::collections::HashMap::new();
        expected.insert("keep.txt".to_string(), blake3_hex(b"KEEP"));
        verify_rebuilt(&out, &expected, "drop.jpg").unwrap();

        // a wrong expected hash must fail
        let mut bad = std::collections::HashMap::new();
        bad.insert("keep.txt".to_string(), "deadbeef".to_string());
        assert!(verify_rebuilt(&out, &bad, "drop.jpg").is_err());
        // the removed entry still present would fail
        assert!(verify_rebuilt(&src, &expected, "drop.jpg").is_err());
    }

    #[test]
    fn rebuild_errors_if_excluded_entry_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP")]);
        assert!(rebuild_without(&src, &out, "not-there.jpg").is_err());
    }

    use crate::catalog::Catalog;
    use crate::catalog::models::{Volume, FileStatus};

    fn fake_drive_with_archive() -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        // archive with two entries; "dup.txt" also exists as a loose file (the surviving copy)
        make_zip(&root.join("bundle.zip"), &[("keep.txt", b"KEEPDATA"), ("dup.txt", b"SHARED")]);
        std::fs::write(root.join("loose_dup.txt"), b"SHARED").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = crate::volume::VolumeIdentity { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into() };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100).unwrap();
        (tmp, cat, root)
    }

    #[test]
    fn repack_removes_entry_keeps_rest_and_preserves_recovery() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // id of the archived entry bundle.zip › dup.txt
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap();

        let out = repack_entry(&cat, &root, "vol-1", entry.id, 200).unwrap();
        assert_eq!(out.removed_entry, "dup.txt");

        // the rebuilt archive no longer contains dup.txt but still has keep.txt
        assert!(extract_entry(&root.join("bundle.zip"), "keep.txt").is_ok());
        assert!(extract_entry(&root.join("bundle.zip"), "dup.txt").is_err());
        // safety nets: extracted loose copy + the original archive both in _ToDelete
        assert!(root.join("_ToDelete").exists());
        assert!(std::fs::read_dir(root.join("_ToDelete")).unwrap().count() > 0);
        // catalog: the entry row is now quarantined + loose
        let rec = cat.get_file(entry.id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.container_chain, None);
        let _ = tmp;
    }

    #[test]
    fn repack_refuses_when_no_surviving_copy() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // delete the loose survivor off disk so dup.txt inside the zip is the last copy
        std::fs::remove_file(root.join("loose_dup.txt")).unwrap();
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap();
        let res = repack_entry(&cat, &root, "vol-1", entry.id, 200);
        assert!(res.is_err());
        // the archive is untouched — dup.txt still inside
        assert!(extract_entry(&root.join("bundle.zip"), "dup.txt").is_ok());
        let _ = tmp;
    }

    #[test]
    fn repack_refuses_nested_entry() {
        let (tmp, cat, root) = fake_drive_with_archive();
        // fabricate a nested-chain entry row (container_chain with ' › ')
        cat.upsert_archive_entry("vol-1", "bundle.zip",
            &crate::archive::ArchiveEntry { container_chain: "inner.zip › deep.txt".into(),
                filename: "deep.txt".into(), extension: "txt".into(), size_bytes: 3,
                content_hash: "SHARED_H".into() }, 100).unwrap();
        let entry = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("inner.zip › deep.txt")).unwrap();
        let res = repack_entry(&cat, &root, "vol-1", entry.id, 200);
        assert!(res.is_err()); // nested not supported
        let _ = tmp;
    }
}

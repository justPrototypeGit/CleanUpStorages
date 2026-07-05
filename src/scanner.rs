use std::path::Path;
use walkdir::WalkDir;

use crate::archive::{self, ArchiveLimits};
use crate::catalog::Catalog;
use crate::catalog::models::NewFile;
use crate::category::Category;
use crate::config::Config;
use crate::hashing;
use crate::volume::VolumeIdentity;

const BATCH_SIZE: usize = 200;

/// Outcome of one `scan_volume` pass.
#[derive(Debug, Default)]
pub struct ScanSummary {
    pub hashed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub marked_missing: usize,
    pub archive_entries: usize,
}

/// Metadata timestamp (best-effort) as seconds since UNIX_EPOCH.
fn unix_secs(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// True if `path` is the identity marker file or lives under a `_ToDelete` quarantine dir.
fn should_skip(path: &Path, file_name: &std::ffi::OsStr) -> bool {
    file_name == crate::volume::MARKER
        || path.components().any(|c| c.as_os_str() == crate::volume::QUARANTINE_DIR)
}

/// Path of `path` relative to `root`, normalized to forward slashes; `None` if not under `root`.
fn relative_path(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

/// Commit the current transaction and open the next one, resetting the in-batch counter.
fn rotate_batch(cat: &Catalog, in_batch: &mut usize) -> anyhow::Result<()> {
    if *in_batch >= BATCH_SIZE {
        cat.conn.execute_batch("COMMIT; BEGIN")?;
        *in_batch = 0;
    }
    Ok(())
}

/// Recursively scan `root`, hashing new/changed files and skipping (but re-touching) unchanged
/// ones, then sweep any previously-active file not seen this pass into `missing`.
///
/// `force` bypasses the incremental skip and re-hashes every file. `now` is used both as the
/// scan's `last_seen_at` stamp and as `scan_started_at` for the missing-file sweep: because every
/// file touched this scan gets `last_seen_at == now`, `mark_missing_scanned` (which flags rows
/// with `last_seen_at < scan_started_at`) only ever catches files genuinely absent this pass.
pub fn scan_volume(
    cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64,
) -> anyhow::Result<ScanSummary> {
    let scan_started_at = now;
    let limits = ArchiveLimits::from_config(&Config::default_paths()?);
    let mut summary = ScanSummary::default();
    let mut in_batch = 0usize;
    cat.conn.execute_batch("BEGIN")?;

    for entry in WalkDir::new(root) {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let p = err
                    .path()
                    .map(|p| p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|| "<unknown>".to_string());
                cat.log_scan_error(Some(&identity.volume_id), &p, &format!("walk: {err}"), now)?;
                summary.errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if should_skip(path, name) {
            continue;
        }
        let Some(rel) = relative_path(path, root) else { continue };

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &format!("metadata: {e}"), now)?;
                summary.errors += 1;
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                continue;
            }
        };
        let size = meta.len() as i64;
        let mtime = unix_secs(meta.modified());

        // Incremental skip: same size + mtime as catalogued -> just touch, don't re-hash.
        if !force {
            if let Some((old_size, old_mtime)) = cat.get_file_meta(&identity.volume_id, &rel)? {
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    if archive::is_archive_name(&rel) {
                        cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                    }
                    summary.skipped += 1;
                    in_batch += 1;
                    rotate_batch(cat, &mut in_batch)?;
                    continue;
                }
            }
        }

        let hash = match hashing::hash_file(path) {
            Ok(h) => h,
            Err(e) => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &format!("read: {e}"), now)?;
                summary.errors += 1;
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                continue;
            }
        };

        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let nf = NewFile {
            volume_id: identity.volume_id.clone(),
            relative_path: rel.clone(),
            filename: name.to_string_lossy().into_owned(),
            extension: ext.clone(),
            size_bytes: size,
            content_hash: hash,
            created_time: unix_secs(meta.created()),
            modified_time: mtime,
            accessed_time: unix_secs(meta.accessed()),
            category: Category::from_extension(&ext),
            container_chain: None,
        };
        cat.upsert_file(&nf, now)?;
        summary.hashed += 1;
        in_batch += 1;
        rotate_batch(cat, &mut in_batch)?;

        if archive::is_archive_name(&rel) {
            descend_archive(cat, path, &rel, identity, &limits, now, &mut summary, &mut in_batch)?;
        }
    }

    cat.conn.execute_batch("COMMIT")?;
    summary.marked_missing = cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    Ok(summary)
}

/// Open an on-disk archive file, catalog each entry, and log each non-fatal error.
fn descend_archive(
    cat: &Catalog, path: &Path, rel: &str, identity: &VolumeIdentity,
    limits: &ArchiveLimits, now: i64, summary: &mut ScanSummary, in_batch: &mut usize,
) -> anyhow::Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            cat.log_scan_error(Some(&identity.volume_id), rel, &format!("archive open: {e}"), now)?;
            summary.errors += 1;
            return Ok(());
        }
    };
    let res = archive::scan_archive(file, limits);
    for entry in &res.entries {
        cat.upsert_archive_entry(&identity.volume_id, rel, entry, now)?;
        summary.archive_entries += 1;
        *in_batch += 1;
        rotate_batch(cat, in_batch)?;
    }
    for (ctx, reason) in &res.errors {
        let where_ = if ctx.is_empty() { rel.to_string() } else { format!("{rel} › {ctx}") };
        cat.log_scan_error(Some(&identity.volume_id), &where_, reason, now)?;
        summary.errors += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::catalog::models::Volume;
    use crate::volume::VolumeIdentity;
    use std::fs;

    fn ident() -> VolumeIdentity {
        VolumeIdentity { volume_id: "vol-1".into(), label: "T".into(), identified_by: "marker".into() }
    }

    fn setup() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(), label: "T".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        }).unwrap();
        (tmp, cat)
    }

    #[test]
    fn scans_hashes_and_reindex_skips() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta").unwrap();

        let s1 = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        assert_eq!(s1.hashed, 2);
        assert_eq!(s1.skipped, 0);

        // second scan: nothing changed -> both skipped (no re-hash)
        let s2 = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s2.hashed, 0);
        assert_eq!(s2.skipped, 2);

        // both searchable
        assert_eq!(cat.search("a", None, None, None).unwrap().len(), 1);
    }

    #[test]
    fn deleted_file_becomes_missing() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep.txt"), b"x").unwrap();
        fs::write(root.join("gone.txt"), b"y").unwrap();
        scan_volume(&cat, &root, &ident(), false, 100).unwrap();

        fs::remove_file(root.join("gone.txt")).unwrap();
        let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s.marked_missing, 1);
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
        assert_eq!(cat.search("keep", None, None, Some("active")).unwrap().len(), 1);
    }

    use std::io::Write as _;

    fn write_zip_file(path: &std::path::Path, files: &[(&str, &[u8])]) {
        let f = fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn scan_catalogs_archive_entries() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        write_zip_file(&root.join("photos.zip"), &[("trip/beach.jpg", b"sand"), ("note.txt", b"hi")]);

        let s = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        // the zip file itself is a loose hashed file
        assert_eq!(s.hashed, 1);
        // its two entries are catalogued
        assert_eq!(s.archive_entries, 2);
        // inner file is searchable, with its container chain
        let hits = cat.search("beach", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relative_path, "photos.zip");
        assert_eq!(hits[0].container_chain.as_deref(), Some("trip/beach.jpg"));
    }

    #[test]
    fn unchanged_archive_entries_survive_rescan() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        write_zip_file(&root.join("a.zip"), &[("x.txt", b"one")]);
        scan_volume(&cat, &root, &ident(), false, 100).unwrap();

        // rescan unchanged: archive is skipped, but its entry must NOT be swept to missing
        let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();
        assert_eq!(s.marked_missing, 0);
        assert_eq!(cat.search("x", None, None, Some("active")).unwrap().len(), 1);
    }
}

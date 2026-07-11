use std::path::Path;
use walkdir::WalkDir;

use crate::archive::{self, ArchiveLimits};
use crate::catalog::Catalog;
use crate::catalog::models::{NewFile, Volume};
use crate::category::Category;
use crate::config::Config;
use crate::hashing;
use crate::volume::VolumeIdentity;

const BATCH_SIZE: usize = 200;

/// Optional live-progress sink for a scan. Each method fires once per counted event.
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
}

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
pub fn scan_volume_with_progress(
    cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64,
    progress: Option<&dyn Progress>,
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
                if let Some(p) = progress { p.on_error(); }
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
                if let Some(p) = progress { p.on_error(); }
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
                    if let Some(p) = progress { p.on_skipped(); }
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
                if let Some(p) = progress { p.on_error(); }
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
        if let Some(p) = progress { p.on_hashed(); }
        in_batch += 1;
        rotate_batch(cat, &mut in_batch)?;

        if archive::is_archive_name(&rel) {
            descend_archive(cat, path, &rel, identity, &limits, now, &mut summary, &mut in_batch, progress)?;
        }
    }

    cat.conn.execute_batch("COMMIT")?;
    summary.marked_missing = cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    Ok(summary)
}

/// Scan without progress reporting (CLI and tests). Delegates with `None`.
pub fn scan_volume(cat: &Catalog, root: &Path, identity: &VolumeIdentity, force: bool, now: i64)
    -> anyhow::Result<ScanSummary>
{
    scan_volume_with_progress(cat, root, identity, force, now, None)
}

/// Resolve identity, upsert the volume, and scan. `Ok(None)` iff a read-only drive was skipped.
///
/// The single shared definition of "how a scan works" — used by both the CLI's `cmd_scan` and
/// the web worker, so the two callers can never drift apart on volume-identity/upsert semantics.
pub fn run_scan(
    cat: &Catalog, mount_root: &Path, force: bool, fallback: crate::volume::ReadonlyMode,
    now: i64, progress: Option<&dyn Progress>,
) -> anyhow::Result<Option<(VolumeIdentity, ScanSummary)>> {
    let identity = match crate::volume::resolve(mount_root, fallback)? {
        Some(id) => id,
        None => return Ok(None),
    };
    tracing::info!(volume = %identity.volume_id, label = %identity.label,
        identified_by = %identity.identified_by, "scanning volume");
    cat.upsert_volume(&Volume {
        volume_id: identity.volume_id.clone(),
        label: identity.label.clone(),
        identified_by: identity.identified_by.clone(),
        first_seen_at: now, last_seen_at: now,
    })?;
    let summary = scan_volume_with_progress(cat, mount_root, &identity, force, now, progress)?;
    // Audit trail: one row per completed scan so the Overview "recent activity" feed can show it.
    let _ = cat.log_action("scan", &serde_json::json!({
        "volume_id": identity.volume_id, "label": identity.label,
        "hashed": summary.hashed, "skipped": summary.skipped, "errors": summary.errors,
        "marked_missing": summary.marked_missing, "archive_entries": summary.archive_entries,
    }).to_string(), now);
    Ok(Some((identity, summary)))
}

/// Open an on-disk archive file, catalog each entry, and log each non-fatal error.
fn descend_archive(
    cat: &Catalog, path: &Path, rel: &str, identity: &VolumeIdentity,
    limits: &ArchiveLimits, now: i64, summary: &mut ScanSummary, in_batch: &mut usize,
    progress: Option<&dyn Progress>,
) -> anyhow::Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            cat.log_scan_error(Some(&identity.volume_id), rel, &format!("archive open: {e}"), now)?;
            summary.errors += 1;
            if let Some(p) = progress { p.on_error(); }
            return Ok(());
        }
    };
    let res = archive::scan_archive(file, limits);
    for entry in &res.entries {
        cat.upsert_archive_entry(&identity.volume_id, rel, entry, now)?;
        summary.archive_entries += 1;
        if let Some(p) = progress { p.on_archive_entry(); }
        *in_batch += 1;
        rotate_batch(cat, in_batch)?;
    }
    for (ctx, reason) in &res.errors {
        let where_ = if ctx.is_empty() { rel.to_string() } else { format!("{rel} › {ctx}") };
        cat.log_scan_error(Some(&identity.volume_id), &where_, reason, now)?;
        summary.errors += 1;
        if let Some(p) = progress { p.on_error(); }
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

    struct CountingProgress {
        hashed: std::sync::atomic::AtomicUsize,
        skipped: std::sync::atomic::AtomicUsize,
        errors: std::sync::atomic::AtomicUsize,
        arch: std::sync::atomic::AtomicUsize,
    }
    impl CountingProgress { fn new() -> Self { Self {
        hashed: 0.into(), skipped: 0.into(), errors: 0.into(), arch: 0.into() } } }
    impl Progress for CountingProgress {
        fn on_hashed(&self) { self.hashed.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_skipped(&self) { self.skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_error(&self) { self.errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fn on_archive_entry(&self) { self.arch.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    }

    #[test]
    fn run_scan_resolves_upserts_and_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hello").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let out = run_scan(&cat, &root, false, crate::volume::ReadonlyMode::Fingerprint, 100, None)
            .unwrap();
        let (identity, summary) = out.expect("not skipped");
        assert_eq!(summary.hashed, 1);
        // the volume row exists after run_scan upserted it
        let stats = cat.volume_stats().unwrap();
        assert!(stats.iter().any(|(id, _, _, _)| id == &identity.volume_id));
    }

    #[derive(Clone)]
    struct CaptureW(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureW {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(b); Ok(b.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureW {
        type Writer = CaptureW;
        fn make_writer(&'a self) -> Self::Writer { self.clone() }
    }

    #[test]
    fn run_scan_logs_volume_resolution() {
        // Serialize with other subscriber-installing tests (tracing's interest cache is global).
        let _tracing_lock = crate::observability::tracing_test_guard();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hi").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureW(buf.clone())).with_ansi(false).finish();
        let _guard = tracing::subscriber::set_default(sub);

        run_scan(&cat, &root, false, crate::volume::ReadonlyMode::Fingerprint, 100, None).unwrap();
        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.to_lowercase().contains("volume"), "expected a volume info line: {logged}");
    }

    #[test]
    fn run_scan_logs_a_scan_action() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hello").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let n = run_scan(&cat, &root, false, crate::volume::ReadonlyMode::Fingerprint, 1234, None)
            .unwrap();
        assert!(n.is_some());
        let acts = cat.recent_actions(10).unwrap();
        assert!(acts.iter().any(|(a, d, t)| a == "scan" && *t == 1234 && d.contains("\"hashed\"")));
    }

    #[test]
    fn progress_callbacks_match_summary() {
        use std::sync::atomic::Ordering::Relaxed;
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta").unwrap();

        let p = CountingProgress::new();
        let s = scan_volume_with_progress(&cat, &root, &ident(), false, 100, Some(&p)).unwrap();
        assert_eq!(p.hashed.load(Relaxed), s.hashed);
        assert_eq!(p.skipped.load(Relaxed), s.skipped);
        assert_eq!(p.errors.load(Relaxed), s.errors);
        assert_eq!(p.arch.load(Relaxed), s.archive_entries);
        assert_eq!(s.hashed, 2);
    }
}

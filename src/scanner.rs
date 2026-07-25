use std::path::Path;

use crate::catalog::models::Volume;
use crate::catalog::Catalog;
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
    /// Where this scan's time went. Measured always; see `scan_metrics`.
    pub metrics: crate::scan_metrics::MetricsSnapshot,
}

/// Metadata timestamp (best-effort) as seconds since UNIX_EPOCH.
pub(crate) fn unix_secs(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// True if `path` is the identity marker file or lives under a `_ToDelete` quarantine dir.
pub(crate) fn should_skip(path: &Path, file_name: &std::ffi::OsStr) -> bool {
    file_name == crate::volume::MARKER
        || path
            .components()
            .any(|c| c.as_os_str() == crate::volume::QUARANTINE_DIR)
}

/// Path of `path` relative to `root`, normalized to forward slashes; `None` if not under `root`.
pub(crate) fn relative_path(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

/// Commit the current transaction and open the next one, resetting the in-batch counter.
pub(crate) fn rotate_batch(cat: &Catalog, in_batch: &mut usize) -> anyhow::Result<()> {
    if *in_batch >= BATCH_SIZE {
        cat.conn.execute_batch("COMMIT; BEGIN")?;
        *in_batch = 0;
    }
    Ok(())
}

/// Flags the pipeline as aborted unless disarmed — how a worker panic reaches the writer.
///
/// Without this, a panicking worker simply drops its sender, the results channel closes, and the
/// writer cannot distinguish that from a completed walk: it would commit the partial scan and then
/// sweep every unreported file to `missing`.
struct AbortOnDrop<'a>(&'a crate::scan_pipeline::PipelineStatus);

impl AbortOnDrop<'_> {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for AbortOnDrop<'_> {
    fn drop(&mut self) {
        self.0
            .aborted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Recursively scan `root`, hashing new/changed files and skipping (but re-touching) unchanged
/// ones, then sweep any previously-active file not seen this pass into `missing`.
///
/// `force` bypasses the incremental skip and re-hashes every file. `now` is used both as the
/// scan's `last_seen_at` stamp and as `scan_started_at` for the missing-file sweep: because every
/// file touched this scan gets `last_seen_at == now`, `mark_missing_scanned` (which flags rows
/// with `last_seen_at < scan_started_at`) only ever catches files genuinely absent this pass.
///
/// `metrics` is owned by the caller so a scan that bails part-way still yields what it measured
/// before it died — the multi-day run that fails late is the one most worth measuring.
#[allow(
    clippy::too_many_arguments,
    reason = "each parameter is an independent scan input; grouping them into a struct would add \
        indirection without reducing real complexity"
)]
pub fn scan_volume_with_progress(
    cat: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    now: i64,
    progress: Option<&dyn Progress>,
    metrics: &crate::scan_metrics::ScanMetrics,
    jobs: usize,
) -> anyhow::Result<ScanSummary> {
    use crate::scan_pipeline::{run_job, walk, write_results, PipelineStatus};

    let jobs = jobs.max(1);
    let db_path = cat.path.clone();

    // Resolved once, not per archive: `Config::default_paths` does a directory lookup and a
    // create_dir_all, which would otherwise be a filesystem syscall per archive job, issued
    // concurrently and charged to the archive phase we are trying to measure.
    let mut limits =
        crate::archive::ArchiveLimits::from_config(&crate::config::Config::default_paths()?);
    // #18 capped how much nested-archive data the SCAN holds at once. With N workers each running
    // its own descent, a per-descent cap would multiply by N (4 workers x 2 GiB = 8 GiB), so divide
    // it: the process-wide ceiling stays what it was however many workers there are.
    limits.total_buffer_bytes = (limits.total_buffer_bytes / jobs as u64).max(1);

    // The results channel closes on success AND on every abort path, so the writer needs an explicit
    // signal to tell them apart before it commits.
    let status = PipelineStatus::default();
    // Shadow with a reference so the `move` closures below capture the borrow, not the value.
    let status = &status;

    // Bounded channels give backpressure: the walker blocks when workers are saturated, so the walk
    // cannot run millions of paths ahead of the hashing and blow up memory.
    let (job_tx, job_rx) = crossbeam_channel::bounded::<crate::scan_pipeline::Job>(jobs * 4);
    let (res_tx, res_rx) = crossbeam_channel::bounded::<crate::scan_pipeline::ScanResult>(jobs * 4);

    let volume_id = identity.volume_id.clone();

    // thread::scope lets the workers borrow `metrics`, `progress` and `identity` without `'static`
    // bounds, and guarantees every thread joins before this function returns.
    std::thread::scope(|scope| -> anyhow::Result<ScanSummary> {
        // Writer: the single SQLite writer, on its own read-write connection.
        let writer_path = db_path.clone();
        let writer = scope.spawn(move || -> anyhow::Result<ScanSummary> {
            let wcat = Catalog::open(&writer_path)?;
            match write_results(&wcat, res_rx, identity, now, now, metrics, progress, status) {
                Ok(s) => Ok(s),
                Err(e) => {
                    let _ = wcat.conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        });

        // Workers: no DB access at all. Below-normal priority so the machine stays usable while a
        // multi-hour scan runs; failing to lower priority is not worth aborting a scan over.
        let mut worker_handles = Vec::new();
        for _ in 0..jobs {
            let jr = job_rx.clone();
            let rt = res_tx.clone();
            let vid = volume_id.clone();
            let lim = &limits;
            let st = status;
            worker_handles.push(scope.spawn(move || {
                let _ = thread_priority::set_current_thread_priority(
                    thread_priority::ThreadPriority::Min,
                );
                // Declared as a body local so that on a panic it drops — and sets the flag — BEFORE
                // the captured sender drops. The writer only reads the flag after the channel
                // closes, which cannot happen until every sender is gone, so it always sees a true
                // value here rather than committing a partial scan.
                let guard = AbortOnDrop(st);
                for job in jr {
                    for result in run_job(job, &vid, lim, metrics) {
                        if rt.send(result).is_err() {
                            return; // writer gone; its own error is the one that matters
                        }
                    }
                }
                guard.disarm();
            }));
        }
        // The parent's handles must go, or the channels never close: each worker holds its own
        // clone, so the results channel closes only once every worker has exited.
        drop(job_rx);
        drop(res_tx);

        // Walker: its own read-only connection for the skip-check.
        let rocat = Catalog::open_readonly(&db_path)?;
        walk(&rocat, root, identity, force, metrics, &job_tx);
        // Set BEFORE closing the channel: the writer must never see a closed channel without also
        // seeing the final value of this flag. A panic in walk() skips this, leaving it false.
        status
            .walk_completed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        drop(job_tx); // no more jobs -> workers drain and exit

        for h in worker_handles {
            h.join()
                .map_err(|_| anyhow::anyhow!("scan worker panicked"))?;
        }
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("scan writer panicked"))?
    })
}

/// Scan without progress reporting (CLI and tests). Delegates with `None`.
pub fn scan_volume(
    cat: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    now: i64,
) -> anyhow::Result<ScanSummary> {
    let metrics = crate::scan_metrics::ScanMetrics::new();
    scan_volume_with_progress(cat, root, identity, force, now, None, &metrics, 1)
}

/// Resolve identity, upsert the volume, and scan. `Ok(None)` iff a read-only drive was skipped.
///
/// The single shared definition of "how a scan works" — used by both the CLI's `cmd_scan` and
/// the web worker, so the two callers can never drift apart on volume-identity/upsert semantics.
///
/// `jobs` is the parallel read+hash worker count (1 disables parallelism); it is recorded on the
/// scan_runs row so a later comparison knows the concurrency each run used.
pub fn run_scan(
    cat: &Catalog,
    mount_root: &Path,
    force: bool,
    fallback: crate::volume::ReadonlyMode,
    now: i64,
    progress: Option<&dyn Progress>,
    jobs: usize,
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
        first_seen_at: now,
        last_seen_at: now,
    })?;
    // Remember where this volume was scanned so a folder-drive (not a disk root) can be recognized
    // as connected later. Best-effort: a bookkeeping failure must not fail the scan.
    let _ = cat.set_volume_path(&identity.volume_id, &mount_root.display().to_string(), now);

    // Best-effort throughout: a bookkeeping failure must never fail a scan. Started before the
    // scan opens its transaction, so the 'running' row is committed immediately and an
    // interrupted multi-day scan leaves a record.
    let run_id = cat
        .start_scan_run(
            Some(&identity.volume_id),
            &mount_root.display().to_string(),
            now,
            force,
            jobs as i64,
        )
        .map_err(|e| tracing::warn!("could not record scan start: {e}"))
        .ok();

    // Owned here, not inside the scan, so a scan that bails part-way still reports what it
    // measured before it died.
    let metrics = crate::scan_metrics::ScanMetrics::new();
    let result = scan_volume_with_progress(
        cat, mount_root, &identity, force, now, progress, &metrics, jobs,
    );

    if let Some(id) = run_id {
        let finished_at = crate::commands::now_secs();
        let outcome = match &result {
            Ok(summary) => cat.finish_scan_run(id, finished_at, "completed", None, summary),
            Err(e) => {
                let msg = e.to_string();
                let partial = ScanSummary {
                    metrics: metrics.snapshot(),
                    ..Default::default()
                };
                cat.finish_scan_run(id, finished_at, "failed", Some(&msg), &partial)
            }
        };
        if let Err(e) = outcome {
            tracing::warn!("could not record scan result: {e}");
        }
    }

    let summary = result?;
    // Audit trail: one row per completed scan so the Overview "recent activity" feed can show it.
    let _ = cat.log_action(
        "scan",
        &serde_json::json!({
            "volume_id": identity.volume_id, "label": identity.label,
            "hashed": summary.hashed, "skipped": summary.skipped, "errors": summary.errors,
            "marked_missing": summary.marked_missing, "archive_entries": summary.archive_entries,
        })
        .to_string(),
        now,
    );
    Ok(Some((identity, summary)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::Volume;
    use crate::catalog::Catalog;
    use crate::volume::VolumeIdentity;
    use std::fs;

    fn ident() -> VolumeIdentity {
        VolumeIdentity {
            volume_id: "vol-1".into(),
            label: "T".into(),
            identified_by: "marker".into(),
        }
    }

    fn setup() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "T".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
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
        assert_eq!(
            cat.search("gone", None, None, Some("missing"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            cat.search("keep", None, None, Some("active"))
                .unwrap()
                .len(),
            1
        );
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
        write_zip_file(
            &root.join("photos.zip"),
            &[("trip/beach.jpg", b"sand"), ("note.txt", b"hi")],
        );

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
        assert_eq!(
            cat.search("x", None, None, Some("active")).unwrap().len(),
            1
        );
    }

    struct CountingProgress {
        hashed: std::sync::atomic::AtomicUsize,
        skipped: std::sync::atomic::AtomicUsize,
        errors: std::sync::atomic::AtomicUsize,
        arch: std::sync::atomic::AtomicUsize,
    }
    impl CountingProgress {
        fn new() -> Self {
            Self {
                hashed: 0.into(),
                skipped: 0.into(),
                errors: 0.into(),
                arch: 0.into(),
            }
        }
    }
    impl Progress for CountingProgress {
        fn on_hashed(&self) {
            self.hashed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_skipped(&self) {
            self.skipped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_error(&self) {
            self.errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_archive_entry(&self) {
            self.arch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn run_scan_resolves_upserts_and_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hello").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let out = run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            100,
            None,
            1,
        )
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
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureW {
        type Writer = CaptureW;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
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
            .with_writer(CaptureW(buf.clone()))
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(sub);

        run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            100,
            None,
            1,
        )
        .unwrap();
        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            logged.to_lowercase().contains("volume"),
            "expected a volume info line: {logged}"
        );
    }

    #[test]
    fn run_scan_logs_a_scan_action() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x.txt"), b"hello").unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();

        let n = run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            1234,
            None,
            1,
        )
        .unwrap();
        assert!(n.is_some());
        let acts = cat.recent_actions(10).unwrap();
        assert!(acts
            .iter()
            .any(|(a, d, t)| a == "scan" && *t == 1234 && d.contains("\"hashed\"")));
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
        let m = crate::scan_metrics::ScanMetrics::new();
        let s =
            scan_volume_with_progress(&cat, &root, &ident(), false, 100, Some(&p), &m, 1).unwrap();
        assert_eq!(p.hashed.load(Relaxed), s.hashed);
        assert_eq!(p.skipped.load(Relaxed), s.skipped);
        assert_eq!(p.errors.load(Relaxed), s.errors);
        assert_eq!(p.arch.load(Relaxed), s.archive_entries);
        assert_eq!(s.hashed, 2);
    }

    /// A temp dir containing `files` (name, byte length), plus an open catalog with the `ident()`
    /// volume already upserted (the `files` table's `volume_id` is FK-enforced).
    fn fixture_with_files(
        files: &[(&str, usize)],
    ) -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        for (name, len) in files {
            std::fs::write(root.join(name), vec![b'x'; *len]).unwrap();
        }
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "T".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        (t, cat, root)
    }

    /// A scan must produce an identical catalogue regardless of --jobs. This is what makes the
    /// single parallel implementation safe: --jobs=1 is not a separate serial path, it is this
    /// same pipeline with one worker, and it must agree with --jobs=8 exactly.
    #[test]
    fn identical_catalogue_at_any_job_count() {
        fn scan_into(jobs: usize) -> Vec<(String, String, i64, String)> {
            let t = tempfile::tempdir().unwrap();
            let root = t.path().join("drive");
            std::fs::create_dir_all(root.join("sub")).unwrap();
            for i in 0..50 {
                std::fs::write(root.join(format!("f{i}.txt")), format!("content-{i}")).unwrap();
            }
            std::fs::write(root.join("sub/dup.txt"), b"content-0").unwrap(); // a duplicate
            {
                let f = std::fs::File::create(root.join("bundle.zip")).unwrap();
                let mut zw = zip::ZipWriter::new(f);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inner.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"zipped").unwrap();
                zw.finish().unwrap();
            }
            let cat = Catalog::open(&t.path().join("c.db")).unwrap();
            // files.volume_id is FK-enforced, and this calls the scan directly rather than through
            // run_scan (which would upsert the volume for us).
            cat.upsert_volume(&Volume {
                volume_id: ident().volume_id.clone(),
                label: "T".into(),
                identified_by: "marker".into(),
                first_seen_at: 1,
                last_seen_at: 1,
            })
            .unwrap();
            let m = crate::scan_metrics::ScanMetrics::new();
            scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, jobs).unwrap();
            let mut stmt = cat
                .conn
                .prepare(
                    "SELECT relative_path, content_hash, size_bytes, IFNULL(container_chain,'') \
                     FROM files ORDER BY relative_path, container_chain",
                )
                .unwrap();
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        }
        let one = scan_into(1);
        let eight = scan_into(8);
        assert!(!one.is_empty());
        assert_eq!(one, eight, "the catalogue must not depend on --jobs");
    }

    #[test]
    fn scan_records_phase_timings_and_the_size_histogram() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("big.bin", 5000)]);
        let s = scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        let m = &s.metrics;

        assert_eq!(m.files_seen, 2);
        assert_eq!(m.histogram[1], 1, "the 10-byte file");
        assert_eq!(m.histogram[2], 1, "the 5000-byte file");
        assert_eq!(m.bytes_hashed, 5010);
        assert_eq!(m.bytes_skipped, 0);
    }

    #[test]
    fn rescan_attributes_bytes_to_skipped_not_hashed() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("b.txt", 20)]);
        scan_volume(&cat, &root, &ident(), false, 100).unwrap();
        let s = scan_volume(&cat, &root, &ident(), false, 200).unwrap();

        assert_eq!(s.skipped, 2, "second pass takes the incremental-skip path");
        assert_eq!(s.metrics.bytes_hashed, 0);
        assert_eq!(s.metrics.bytes_skipped, 30);
        assert_eq!(s.metrics.files_seen, 2, "skipped files are still 'seen'");
        assert_eq!(s.metrics.histogram[1], 2);
    }

    #[test]
    fn run_scan_records_a_completed_run() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10)]);
        let out = run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            100,
            None,
            1,
        )
        .unwrap();
        assert!(out.is_some());

        let runs = cat.recent_scan_runs(10).unwrap();
        assert_eq!(runs.len(), 1, "exactly one row per scan, not one per file");
        assert_eq!(runs[0].status, "completed");
        assert!(runs[0].finished_at.is_some());
        assert_eq!(runs[0].hashed, 1);
        assert_eq!(runs[0].metrics.files_seen, 1);
        assert!(!runs[0].root_path.is_empty());
    }

    #[test]
    fn a_failed_scan_records_failed_with_its_error_and_its_partial_metrics() {
        let (t, cat, root) = fixture_with_files(&[("a.txt", 10)]);
        let db = t.path().join("c.db");
        // Abort the very first file insert. RAISE(ABORT) undoes the statement but leaves the
        // scan's BEGIN open -- the exact shape that used to swallow the 'failed' row.
        cat.conn
            .execute_batch(
                "CREATE TRIGGER boom BEFORE INSERT ON files
                 BEGIN SELECT RAISE(ABORT, 'induced scan failure'); END",
            )
            .unwrap();

        let out = run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            100,
            None,
            1,
        );
        assert!(out.is_err(), "the induced trigger must fail the scan");
        drop(cat);

        // A fresh connection is the point: reading on the scan's own connection would see the
        // update inside its abandoned transaction and pass spuriously.
        let fresh = Catalog::open(&db).unwrap();
        let runs = fresh.recent_scan_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].status, "failed",
            "outcome must survive the rollback"
        );
        assert!(
            runs[0]
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("induced scan failure"),
            "error lost: {:?}",
            runs[0].error_message
        );
        assert_eq!(
            runs[0].metrics.files_seen, 1,
            "partial measurement must survive the failure"
        );
    }

    #[test]
    fn a_metrics_write_failure_never_fails_the_scan() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10)]);
        // Drop the table out from under the run: recording must degrade, not propagate.
        cat.conn.execute_batch("DROP TABLE scan_runs").unwrap();
        let out = run_scan(
            &cat,
            &root,
            false,
            crate::volume::ReadonlyMode::Fingerprint,
            100,
            None,
            1,
        );
        assert!(
            out.is_ok(),
            "a bookkeeping failure must not fail a scan: {out:?}"
        );
        assert_eq!(
            out.unwrap().unwrap().1.hashed,
            1,
            "the scan still did its work"
        );
    }
}

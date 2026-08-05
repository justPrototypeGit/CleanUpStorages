use std::path::Path;
use walkdir::WalkDir;

use crate::archive::{self, ArchiveLimits};
use crate::catalog::models::{NewFile, Volume};
use crate::catalog::Catalog;
use crate::category::Category;
use crate::hashing;
use crate::volume::VolumeIdentity;

const BATCH_SIZE: usize = 200;

/// Optional live-progress sink for a scan. Each method fires once per counted event.
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
    /// Totals from the counting pass. Never called when counting is skipped, so a percentage is
    /// absent rather than wrong.
    fn on_total(&self, _files: u64, _bytes: u64) {}
    /// Bytes of the file just finished, hashed or skipped. Drives the rate and the ETA.
    fn on_bytes(&self, _bytes: u64) {}
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
    /// True when the scan ended on a stop request rather than reaching the end of the tree.
    /// A stopped scan must not run the missing-sweep.
    pub stopped: bool,
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
        || path
            .components()
            .any(|c| c.as_os_str() == crate::volume::QUARANTINE_DIR)
}

/// Opens `path` for the top-level archive-detection peek. Factored out purely so a test can force
/// this one call to fail without also failing the hash step that runs just before it -- a real OS
/// lock (share violation, permission) held externally blocks both opens indiscriminately, so there
/// is no way to reproduce a detection-only failure with real file handles.
#[cfg(not(test))]
fn open_for_archive_detection(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
thread_local! {
    static FORCE_DETECTION_OPEN_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn open_for_archive_detection(path: &Path) -> std::io::Result<std::fs::File> {
    if FORCE_DETECTION_OPEN_ERROR.with(|f| f.get()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "forced for test",
        ));
    }
    std::fs::File::open(path)
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

/// Files and bytes a scan of `root` would process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeTotals {
    pub files: u64,
    pub bytes: u64,
}

/// Count the tree without reading any file contents — `readdir` + `stat` only.
///
/// This is what makes a real percentage possible for a folder that has never been scanned, which is
/// most of a first pass. It costs a metadata walk, so it scales with file count rather than with
/// terabytes. Errors are ignored: this is an estimate, and a directory the scan cannot read is
/// reported by the scan itself.
pub fn count_tree(root: &Path, stop: &crate::scan_control::StopFlag) -> TreeTotals {
    let mut totals = TreeTotals::default();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if stop.is_requested() {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if should_skip(entry.path(), entry.file_name()) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            totals.files += 1;
            totals.bytes += meta.len();
        }
    }
    totals
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
    stop: &crate::scan_control::StopFlag,
    limits: &ArchiveLimits,
) -> anyhow::Result<ScanSummary> {
    let scan_started_at = now;
    let mut summary = ScanSummary::default();
    let mut in_batch = 0usize;
    // Directories this pass could not enumerate. Their contents were never visited, so they must be
    // held back from the missing-sweep below (#7) — unreadable is not the same as gone.
    let mut unreadable_dirs: Vec<String> = Vec::new();
    cat.conn.execute_batch("BEGIN")?;
    // Re-count from scratch for this volume, so a rescan does not accumulate.
    cat.clear_pending_formats_for_volume(&identity.volume_id)?;

    let mut walker = WalkDir::new(root).into_iter();
    loop {
        let next = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Walk);
            walker.next()
        };
        let Some(entry) = next else { break };
        if stop.is_requested() {
            summary.stopped = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let p = err
                    .path()
                    .map(|p| {
                        p.strip_prefix(root)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .replace('\\', "/")
                    })
                    .unwrap_or_else(|| "<unknown>".to_string());
                cat.log_scan_error(
                    Some(&identity.volume_id),
                    &p,
                    &format!("walk: {err}"),
                    "walk",
                    err.io_error()
                        .map(crate::catalog::scan_errors::classify_io)
                        .unwrap_or("other"),
                    now,
                )?;
                summary.errors += 1;
                if let Some(pr) = progress {
                    pr.on_error();
                }
                // Only a known path can be scoped. An error with no path (rare) leaves the sweep
                // unrestricted, which self-heals on the next successful scan.
                if p != "<unknown>" {
                    unreadable_dirs.push(p);
                }
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
        let Some(rel) = relative_path(path, root) else {
            continue;
        };

        // The failed stat is legitimately walk cost; the two SQLite writes that follow it are not,
        // so the guard drops before the error arm runs.
        let stat = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Walk);
            entry.metadata()
        };
        let meta = match stat {
            Ok(m) => m,
            Err(e) => {
                // Still a file the walk considered, and it still cost a seek — but its size is
                // genuinely unknown, so it lands in bucket 0.
                metrics.record_file_seen(0);
                cat.log_scan_error(
                    Some(&identity.volume_id),
                    &rel,
                    &format!("metadata: {e}"),
                    "metadata",
                    e.io_error()
                        .map(crate::catalog::scan_errors::classify_io)
                        .unwrap_or("other"),
                    now,
                )?;
                summary.errors += 1;
                if let Some(p) = progress {
                    p.on_error();
                }
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                continue;
            }
        };
        let size = meta.len() as i64;
        let mtime = unix_secs(meta.modified());
        metrics.record_file_seen(size);

        // Incremental skip: same size + mtime as catalogued -> just touch, don't re-hash.
        // `skip_check` is get_file_meta + touch_seen only; the batch COMMIT this path also
        // triggers is db_write (it is the fsync #26 targets), so the guard must be dead before
        // rotate_batch runs — otherwise a rescan books 100% of its fsyncs to skip_check and reads
        // as seek-bound.
        let is_unchanged = if force {
            false
        } else {
            let _t = metrics.timer(crate::scan_metrics::Phase::SkipCheck);
            match cat.get_file_meta(&identity.volume_id, &rel)? {
                Some((old_size, old_mtime, has_archive_entries, revive_floor))
                    if old_size == size && old_mtime == mtime.unwrap_or(0) =>
                {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    // From the catalogue, not from the filename: a renamed zip has entries too,
                    // and missing them here would let the sweep mark present files missing.
                    if has_archive_entries {
                        // revive_floor is Some(last_seen_at) only if the archive's own row was
                        // ALSO missing a moment ago -- i.e. the whole archive vanished and came
                        // back. Only entries that were still present at that same moment (their own
                        // last_seen_at >= the floor) revive; an entry removed from the archive's
                        // real content by an earlier descend has a smaller last_seen_at and stays
                        // missing even though the archive returned.
                        cat.touch_archive_entries(&identity.volume_id, &rel, now, revive_floor)?;
                    }
                    true
                }
                _ => false,
            }
        };
        if is_unchanged {
            summary.skipped += 1;
            metrics.add_bytes_skipped(size);
            if let Some(p) = progress {
                p.on_bytes(size as u64);
            }
            if let Some(p) = progress {
                p.on_skipped();
            }
            in_batch += 1;
            {
                let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
                rotate_batch(cat, &mut in_batch)?;
            }
            continue;
        }

        // As with the stat above: the failed read is hash cost, its error logging is not.
        let hashed = {
            let _t = metrics.timer(crate::scan_metrics::Phase::Hash);
            hashing::hash_file(path)
        };
        let hash = match hashed {
            Ok(h) => h,
            Err(e) => {
                cat.log_scan_error(
                    Some(&identity.volume_id),
                    &rel,
                    &format!("read: {e}"),
                    "read",
                    crate::catalog::scan_errors::classify_io(&e),
                    now,
                )?;
                summary.errors += 1;
                if let Some(p) = progress {
                    p.on_error();
                }
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                continue;
            }
        };
        metrics.add_bytes_hashed(size);
        if let Some(p) = progress {
            p.on_bytes(size as u64);
        }

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
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
        {
            let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
            cat.upsert_file(&nf, now)?;
            in_batch += 1;
            rotate_batch(cat, &mut in_batch)?;
        }
        summary.hashed += 1;
        if let Some(p) = progress {
            p.on_hashed();
        }

        // By content, not by name. Cheap here because the file has just been hashed, so it is warm
        // in the OS cache -- unlike the skip path above, which must never open anything.
        //
        // The head-magic check alone misses a prefixed/self-extracting zip (real readers locate the
        // central directory from the END of the file, not the start). The extension is used as a
        // HINT to do that extra tail read, never as the decision: `._Video.zip` (AppleDouble) also
        // reaches the tail check, finds no EOCD signature, and is correctly left a leaf.
        let is_archive = {
            let ext_looks_like_zip = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("zip"))
                .unwrap_or(false);
            let detected = open_for_archive_detection(path).and_then(|mut f| {
                let (_head, head_is_zip) = archive::peek4(&mut f)?;
                if head_is_zip {
                    return Ok(true);
                }
                if ext_looks_like_zip {
                    return archive::tail_has_eocd_signature(&mut f);
                }
                Ok(false)
            });
            match detected {
                Ok(v) => v,
                Err(e) => {
                    // A detection failure must never be silently indistinguishable from "not an
                    // archive": that would leave `descend_archive` unrun with no error logged, and
                    // the entries inside would be swept to `missing` with no way to self-heal on a
                    // later clean rescan (the archive's own row stays `active`, so the revive floor
                    // is `None`). Log it so #6's completeness audit surfaces it, and fall back to
                    // the extension test rather than to `false` so a momentarily-locked `.zip` is
                    // still treated as an archive.
                    cat.log_scan_error(
                        Some(&identity.volume_id),
                        &rel,
                        &format!("archive detection: {e}"),
                        "read",
                        crate::catalog::scan_errors::classify_io(&e),
                        now,
                    )?;
                    summary.errors += 1;
                    if let Some(p) = progress {
                        p.on_error();
                    }
                    ext_looks_like_zip
                }
            }
        };
        if is_archive {
            let descent_ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            match archive::descent_for(
                &descent_ext,
                &limits.deny_extensions,
                &limits.allow_extensions,
            ) {
                archive::Descent::Descend => {
                    let _t = metrics.timer(crate::scan_metrics::Phase::Archive);
                    descend_archive(
                        cat,
                        path,
                        &rel,
                        mtime,
                        identity,
                        limits,
                        now,
                        &mut summary,
                        &mut in_batch,
                        progress,
                    )?;
                }
                archive::Descent::Leaf => {}
                archive::Descent::Unrecognised => {
                    // Left whole AND recorded: the user decides, the scanner does not guess.
                    cat.record_pending_format(&identity.volume_id, &descent_ext, size, now)?;
                }
            }
        }
    }

    // The final COMMIT and the missing-sweep are both real scan cost and both hit SQLite, so they
    // belong to db_write. Leaving them untimed would inflate the unaccounted gap and understate
    // exactly the fsync cost #26 targets.
    {
        let _t = metrics.timer(crate::scan_metrics::Phase::DbWrite);
        cat.conn.execute_batch("COMMIT")?;
        // THE rule: a scan that did not finish never sweeps. Every file the walk had not reached
        // yet looks untouched, so sweeping here would mark present files as missing.
        if !summary.stopped {
            summary.marked_missing = cat.mark_missing_scanned(
                &identity.volume_id,
                scan_started_at,
                now,
                &unreadable_dirs,
            )?;
        }
        // Outside the sweep guard on purpose: the file rule is keyed on `last_seen_at`, so a
        // stopped scan clears only paths it actually re-reached. The directory rule is the part
        // that needs a completed scan, and `completed` carries that.
        cat.clear_resolved_scan_errors(&identity.volume_id, scan_started_at, !summary.stopped)?;
    }
    summary.metrics = metrics.snapshot();
    Ok(summary)
}

/// Scan without progress reporting (CLI and tests). Delegates with `None`.
pub fn scan_volume(
    cat: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    now: i64,
    limits: &ArchiveLimits,
) -> anyhow::Result<ScanSummary> {
    let metrics = crate::scan_metrics::ScanMetrics::new();
    scan_volume_with_progress(
        cat,
        root,
        identity,
        force,
        now,
        None,
        &metrics,
        &crate::scan_control::StopFlag::new(),
        limits,
    )
}

/// Resolve identity, upsert the volume, and scan. `Ok(None)` iff a read-only drive was skipped.
///
/// The single shared definition of "how a scan works" — used by both the CLI's `cmd_scan` and
/// the web worker, so the two callers can never drift apart on volume-identity/upsert semantics.
#[allow(
    clippy::too_many_arguments,
    reason = "each parameter is an independent scan input; grouping them into a struct would add \
        indirection without reducing real complexity"
)]
pub fn run_scan(
    cat: &Catalog,
    mount_root: &Path,
    force: bool,
    fallback: crate::volume::ReadonlyMode,
    now: i64,
    progress: Option<&dyn Progress>,
    stop: &crate::scan_control::StopFlag,
    limits: &ArchiveLimits,
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
        )
        .map_err(|e| tracing::warn!("could not record scan start: {e}"))
        .ok();

    // Owned here, not inside the scan, so a scan that bails part-way still reports what it
    // measured before it died.
    let metrics = crate::scan_metrics::ScanMetrics::new();
    let result = scan_volume_with_progress(
        cat, mount_root, &identity, force, now, progress, &metrics, stop, limits,
    );

    if result.is_err() {
        // The scan bailed with its BEGIN still open; end it so the metrics UPDATE below is its
        // own transaction and survives. Nothing durable is lost -- it would have been rolled
        // back at connection close anyway.
        let _ = cat.conn.execute_batch("ROLLBACK");
    }

    if let Some(id) = run_id {
        let finished_at = crate::commands::now_secs();
        let outcome = match &result {
            Ok(summary) => {
                let status = if summary.stopped {
                    "cancelled"
                } else {
                    "completed"
                };
                cat.finish_scan_run(id, finished_at, status, None, summary)
            }
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

/// Open an on-disk archive file, catalog each entry, and log each non-fatal error.
#[allow(
    clippy::too_many_arguments,
    reason = "each parameter is an independent scan input; grouping them into a struct would add \
        indirection without reducing real complexity"
)]
fn descend_archive(
    cat: &Catalog,
    path: &Path,
    rel: &str,
    archive_mtime: Option<i64>,
    identity: &VolumeIdentity,
    limits: &ArchiveLimits,
    now: i64,
    summary: &mut ScanSummary,
    in_batch: &mut usize,
    progress: Option<&dyn Progress>,
) -> anyhow::Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            cat.log_scan_error(
                Some(&identity.volume_id),
                rel,
                &format!("archive open: {e}"),
                "archive_open",
                crate::catalog::scan_errors::classify_io(&e),
                now,
            )?;
            summary.errors += 1;
            if let Some(p) = progress {
                p.on_error();
            }
            return Ok(());
        }
    };
    let res = archive::scan_archive(file, limits);
    for entry in &res.entries {
        cat.upsert_archive_entry(&identity.volume_id, rel, entry, archive_mtime, now)?;
        summary.archive_entries += 1;
        if let Some(p) = progress {
            p.on_archive_entry();
        }
        *in_batch += 1;
        rotate_batch(cat, in_batch)?;
    }
    for (ctx, reason) in &res.errors {
        let where_ = if ctx.is_empty() {
            rel.to_string()
        } else {
            format!("{rel} › {ctx}")
        };
        // `reason` comes from the zip crate, not an io::Error, so there is no ErrorKind to read.
        // Parsing the string is exactly what classification exists to avoid.
        cat.log_scan_error(
            Some(&identity.volume_id),
            &where_,
            reason,
            "archive_entry",
            "other",
            now,
        )?;
        summary.errors += 1;
        if let Some(p) = progress {
            p.on_error();
        }
    }
    Ok(())
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

    /// Build a stored (uncompressed) zip in memory, for tests that write archive bytes to disk.
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, bytes) in files {
                zw.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut zw, bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    /// The status of the one archive entry with this filename, catalogued anywhere.
    fn status_of(cat: &Catalog, name: &str) -> String {
        cat.conn
            .query_row(
                "SELECT status FROM files WHERE filename=?1 AND container_chain IS NOT NULL",
                [name],
                |r| r.get(0),
            )
            .unwrap()
    }

    // A real scan over a file that genuinely fails to read, asserting the scanner itself (not the
    // catalogue layer) records the correct `kind` for its `"read"` call site. Platform-gated because
    // the honest way to make a read fail differs per OS, and CI runs both Windows and macOS.

    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_recorded_with_the_read_phase_and_locked_kind() {
        use std::os::windows::fs::OpenOptionsExt;

        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("locked.bin");
        std::fs::write(&victim, b"data").unwrap();

        // Hold the file open with an exclusive share mode (share_mode(0) = no sharing at all), so
        // the scanner's own open-for-hashing fails with ERROR_SHARING_VIOLATION (raw OS error 32)
        // for the duration of the scan.
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&victim)
            .unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        let (phase, kind): (String, String) = cat
            .conn
            .query_row(
                "SELECT phase, kind FROM scan_errors WHERE path='locked.bin'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(phase, "read");
        assert_eq!(kind, "locked");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_recorded_with_the_read_phase_and_permission_kind() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("noperm.bin");
        std::fs::write(&victim, b"data").unwrap();
        let orig_perms = std::fs::metadata(&victim).unwrap().permissions();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o000)).unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        let result = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        );

        // Restore permissions before any assertion can early-return/panic, so the tempdir can
        // still be cleaned up regardless of outcome.
        std::fs::set_permissions(&victim, orig_perms).unwrap();
        result.unwrap();

        let (phase, kind): (String, String) = cat
            .conn
            .query_row(
                "SELECT phase, kind FROM scan_errors WHERE path='noperm.bin'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(phase, "read");
        assert_eq!(kind, "permission");
    }

    // Regression for the false-"complete" bug: a file catalogued by an earlier scan, then unreadable
    // on a re-scan, must land in `unverified` -- not have its own fresh error silently deleted by
    // the same scan's self-heal, which would leave the catalogue reporting "complete" over a file
    // whose stored hash was never re-verified. Drives a real scan twice (not the catalogue layer
    // directly) so the reproduction matches what actually happens in production: `touch_seen` bumps
    // `last_seen_at` to `now` on the error path, which equals `scan_started_at`.

    #[cfg(windows)]
    #[test]
    fn a_file_that_becomes_locked_after_being_catalogued_is_unverified_not_complete() {
        use std::os::windows::fs::OpenOptionsExt;

        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("locked.bin");
        std::fs::write(&victim, b"data").unwrap();

        // First scan: the file is readable, so it gets catalogued normally.
        let m1 = crate::scan_metrics::ScanMetrics::new();
        let stop1 = crate::scan_control::StopFlag::new();
        let s1 = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m1,
            &stop1,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(s1.errors, 0);

        // Second scan: the file is now locked, so the re-read fails. `force=true` so the unchanged
        // fast-path (which never re-reads) doesn't hide the point of the test.
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&victim)
            .unwrap();
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let stop2 = crate::scan_control::StopFlag::new();
        let s2 = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            true,
            200,
            None,
            &m2,
            &stop2,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(s2.errors, 1, "the scan itself must count the failure");

        let c = cat.volume_completeness("vol-1").unwrap();
        assert_eq!(
            c.unverified, 1,
            "the error the scan just recorded must not be erased by its own self-heal"
        );
        assert_eq!(c.absent, 0);
        assert_eq!(c.unreadable_dirs, 0);
        assert_ne!(
            c.summary_line(),
            "Completeness: complete.",
            "a catalogue holding an unverified file must never report complete"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_becomes_unreadable_after_being_catalogued_is_unverified_not_complete() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("noperm.bin");
        std::fs::write(&victim, b"data").unwrap();

        // First scan: the file is readable, so it gets catalogued normally.
        let m1 = crate::scan_metrics::ScanMetrics::new();
        let stop1 = crate::scan_control::StopFlag::new();
        let s1 = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m1,
            &stop1,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(s1.errors, 0);

        // Second scan: the file is now unreadable, so the re-read fails. `force=true` so the
        // unchanged fast-path (which never re-reads) doesn't hide the point of the test.
        let orig_perms = std::fs::metadata(&victim).unwrap().permissions();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o000)).unwrap();
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let stop2 = crate::scan_control::StopFlag::new();
        let result = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            true,
            200,
            None,
            &m2,
            &stop2,
            &test_limits(),
        );

        // Restore permissions before any assertion can early-return/panic, so the tempdir can
        // still be cleaned up regardless of outcome.
        std::fs::set_permissions(&victim, orig_perms).unwrap();
        let s2 = result.unwrap();
        assert_eq!(s2.errors, 1, "the scan itself must count the failure");

        let c = cat.volume_completeness("vol-1").unwrap();
        assert_eq!(
            c.unverified, 1,
            "the error the scan just recorded must not be erased by its own self-heal"
        );
        assert_eq!(c.absent, 0);
        assert_eq!(c.unreadable_dirs, 0);
        assert_ne!(
            c.summary_line(),
            "Completeness: complete.",
            "a catalogue holding an unverified file must never report complete"
        );
    }

    #[test]
    fn scans_hashes_and_reindex_skips() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta").unwrap();

        let s1 = scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();
        assert_eq!(s1.hashed, 2);
        assert_eq!(s1.skipped, 0);

        // second scan: nothing changed -> both skipped (no re-hash)
        let s2 = scan_volume(&cat, &root, &ident(), false, 200, &test_limits()).unwrap();
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
        scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();

        fs::remove_file(root.join("gone.txt")).unwrap();
        let s = scan_volume(&cat, &root, &ident(), false, 200, &test_limits()).unwrap();
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

        let s = scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();
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
        scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();

        // rescan unchanged: archive is skipped, but its entry must NOT be swept to missing
        let s = scan_volume(&cat, &root, &ident(), false, 200, &test_limits()).unwrap();
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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            Some(&p),
            &m,
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
        )
        .unwrap();
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

    #[test]
    fn scan_records_phase_timings_and_the_size_histogram() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("big.bin", 5000)]);
        let s = scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();
        let m = &s.metrics;

        assert_eq!(m.files_seen, 2);
        assert_eq!(m.histogram[1], 1, "the 10-byte file");
        assert_eq!(m.histogram[2], 1, "the 5000-byte file");
        assert_eq!(m.bytes_hashed, 5010);
        assert_eq!(m.bytes_skipped, 0);
        // Upper bound only: on a fast disk these phases legitimately round to 0 ms. That the
        // timers accumulate at all is proven in scan_metrics with a controlled sleep.
        assert!(
            m.total_phase_ms() <= m.wall_ms,
            "phases {} exceeded wall {}",
            m.total_phase_ms(),
            m.wall_ms
        );
    }

    #[test]
    fn rescan_attributes_bytes_to_skipped_not_hashed() {
        let (_t, cat, root) = fixture_with_files(&[("a.txt", 10), ("b.txt", 20)]);
        scan_volume(&cat, &root, &ident(), false, 100, &test_limits()).unwrap();
        let s = scan_volume(&cat, &root, &ident(), false, 200, &test_limits()).unwrap();

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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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
            &crate::scan_control::StopFlag::new(),
            &test_limits(),
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

    #[test]
    fn count_tree_totals_files_and_bytes_and_skips_the_marker() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("drive");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.bin"), vec![b'x'; 100]).unwrap();
        std::fs::write(root.join("sub/b.bin"), vec![b'y'; 250]).unwrap();
        // The identity marker is skipped by the scan, so it must not be counted either.
        std::fs::write(root.join(crate::volume::MARKER), b"vol-1").unwrap();

        let totals = count_tree(&root, &crate::scan_control::StopFlag::new());
        assert_eq!(totals.files, 2);
        assert_eq!(totals.bytes, 350);
    }

    #[test]
    fn count_tree_returns_promptly_when_stopped() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..200 {
            std::fs::write(root.join(format!("f{i}.bin")), b"x").unwrap();
        }
        let stop = crate::scan_control::StopFlag::new();
        stop.request(); // already requested before we start
        let totals = count_tree(&root, &stop);
        assert!(
            totals.files < 200,
            "counting should stop early, got {}",
            totals.files
        );
    }

    #[test]
    fn a_stopped_scan_sweeps_nothing_and_reports_stopped() {
        // THE rule: a scan that did not finish must never mark files missing. Without the guard, every
        // file the walk had not reached yet would be flagged as gone.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        std::fs::write(root.join("b.txt"), b"two").unwrap();

        // First pass catalogues both files.
        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(cat.search("", None, None, Some("active")).unwrap().len(), 2);

        // Second pass is stopped before it starts: nothing is re-seen, so an unguarded sweep would
        // mark BOTH files missing.
        let stop2 = crate::scan_control::StopFlag::new();
        stop2.request();
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            300,
            None,
            &m2,
            &stop2,
            &test_limits(),
        )
        .unwrap();

        assert!(s.stopped, "the summary must report that it was stopped");
        assert_eq!(s.marked_missing, 0, "a stopped scan must not sweep");
        assert_eq!(
            cat.search("", None, None, Some("active")).unwrap().len(),
            2,
            "both files are still on disk and must stay active"
        );
    }

    #[test]
    fn an_unstopped_scan_still_sweeps() {
        // The guard must not disable the feature: a genuinely deleted file still becomes missing.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("gone.txt"), b"bye").unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        std::fs::remove_file(root.join("gone.txt")).unwrap();
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            300,
            None,
            &m2,
            &stop,
            &test_limits(),
        )
        .unwrap();
        assert!(!s.stopped);
        assert_eq!(s.marked_missing, 1);
    }

    #[test]
    fn a_resolved_file_error_clears_but_an_unreached_one_survives() {
        // THE rule, restated for errors: a stopped scan must clear only what it re-reached.
        // Without the last_seen_at predicate, stopping would wipe findings for the part of the
        // tree the run never visited -- silently reporting a catalogue as complete when it is not.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();

        // Two errors on record: one for a file that will be scanned, one for a path that will not.
        cat.log_scan_error(
            Some("vol-1"),
            "a.txt",
            "read: was locked",
            "read",
            "locked",
            50,
        )
        .unwrap();
        cat.log_scan_error(
            Some("vol-1"),
            "never/reached.bin",
            "read: i/o",
            "read",
            "io",
            50,
        )
        .unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        let remaining: Vec<String> = cat
            .conn
            .prepare("SELECT path FROM scan_errors ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            remaining,
            vec!["never/reached.bin".to_string()],
            "a.txt was re-catalogued so its error clears; the unreached path keeps its error"
        );
    }

    #[test]
    fn a_stopped_scan_does_not_clear_walk_errors() {
        // Only a completed scan proves a directory is readable again. A stopped scan may simply
        // never have got there.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        cat.log_scan_error(
            Some("vol-1"),
            "locked/dir",
            "walk: denied",
            "walk",
            "permission",
            50,
        )
        .unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        stop.request(); // stopped before it starts
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();
        assert!(s.stopped);

        let n: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM scan_errors WHERE path='locked/dir'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "a stopped scan never clears a walk error");
    }

    #[test]
    fn a_legacy_phase_null_error_clears_when_its_path_is_recatalogued() {
        // Rows written before `phase` existed have phase IS NULL. The spec promises they "clear
        // themselves on the next scan" -- same as any other file-path error, once re-seen.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();

        // Raw insert, no phase/kind -- mirrors a row from before this feature existed.
        cat.conn
            .execute(
                "INSERT INTO scan_errors(volume_id, path, reason, occurred_at)
                 VALUES ('vol-1', 'a.txt', 'read: was locked', 50)",
                [],
            )
            .unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        let n: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM scan_errors WHERE path='a.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "a legacy error clears once its path is re-catalogued");
    }

    #[test]
    fn a_legacy_phase_null_error_survives_an_unreached_stopped_scan() {
        // The relaxation that lets legacy rows clear must not reopen the false-complete hazard:
        // a legacy row for a path a STOPPED scan never reached must still survive.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();

        cat.conn
            .execute(
                "INSERT INTO scan_errors(volume_id, path, reason, occurred_at)
                 VALUES ('vol-1', 'never/reached.bin', 'read: i/o', 50)",
                [],
            )
            .unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        stop.request(); // stopped before it starts
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();
        assert!(s.stopped);

        let n: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM scan_errors WHERE path='never/reached.bin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "a stopped scan must not clear a legacy error for an unreached path"
        );
    }

    #[test]
    fn a_renamed_zip_keeps_its_entries_active_across_an_unchanged_rescan() {
        // THE regression for this task. Once archives are detected by content, a renamed zip has
        // entries. If the skip path does not recognise it, those entries keep an old last_seen_at
        // and the sweep marks present files missing -- silent data loss in the catalogue.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();

        // A real zip, deliberately NOT named .zip.
        let inner = {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut zw = zip::ZipWriter::new(&mut buf);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inside.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"payload").unwrap();
                zw.finish().unwrap();
            }
            buf.into_inner()
        };
        std::fs::write(root.join("archive.bak"), &inner).unwrap();

        // This regression is about the SKIP-PATH/revival mechanics for an already-descended
        // archive's entries, not about the descent decision itself -- so `bak` is explicitly
        // allow-listed here (unlike `test_limits()`) to keep that decision out of the way.
        let allow_bak = crate::archive::ArchiveLimits {
            allow_extensions: vec!["bak".to_string()],
            ..test_limits()
        };

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &allow_bak,
        )
        .unwrap();

        let entries_active = || -> i64 {
            cat.conn
                .query_row(
                    "SELECT count(*) FROM files WHERE container_chain IS NOT NULL AND status='active'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(-1)
        };
        assert_eq!(entries_active(), 1, "the renamed zip's entry is catalogued");

        // Second scan, nothing changed on disk: the skip path runs, then the sweep.
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let stop2 = crate::scan_control::StopFlag::new();
        let s = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            300,
            None,
            &m2,
            &stop2,
            &allow_bak,
        )
        .unwrap();

        assert_eq!(
            s.marked_missing, 0,
            "nothing on disk changed, so nothing may be marked missing"
        );
        assert_eq!(
            entries_active(),
            1,
            "the archive entry must still be active after an unchanged rescan"
        );
    }

    #[test]
    fn a_prefixed_zip_is_still_detected_and_descended_into() {
        // A self-extracting stub (or anything else that prepends bytes) leaves a file that does not
        // start with PK, yet is a perfectly valid zip: real readers locate the central directory
        // from the END of the file. The head-only check alone would regress this to undetected.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();

        let zip_bytes = {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut zw = zip::ZipWriter::new(&mut buf);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inside.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"payload").unwrap();
                zw.finish().unwrap();
            }
            buf.into_inner()
        };
        // Prepend a fake self-extracting stub. The file is named .zip, but does not start with PK.
        let mut prefixed = b"MZ this is a fake SFX stub, not a zip signature".to_vec();
        prefixed.extend_from_slice(&zip_bytes);
        std::fs::write(root.join("installer.zip"), &prefixed).unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        let inner_catalogued: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM files WHERE filename='inside.txt' AND container_chain IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            inner_catalogued, 1,
            "a prefixed zip's entries must be catalogued, not skipped as a leaf"
        );
    }

    /// F1: a failed open at the top-level archive-detection step must never be silently
    /// indistinguishable from "not an archive". Forces the detection `File::open` to fail (a real
    /// external lock would also fail the hash step that runs just before it, so a thread-local
    /// injection point is the only reproducible way to isolate this exact call).
    ///
    /// This test is DESIGNED to fail if the fix regresses to `.unwrap_or(false)`: with that old
    /// behaviour, `is_archive` silently becomes `false`, `descend_archive` never runs, no scan error
    /// is logged, and the archive's entry is swept to `missing` with no way to self-heal (the
    /// archive's own row stays `active`, so the revive floor is `None`).
    #[test]
    fn a_failed_detection_open_is_logged_and_does_not_lose_the_archives_entries() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.zip"), make_zip(&[("a.txt", b"payload")])).unwrap();

        FORCE_DETECTION_OPEN_ERROR.with(|f| f.set(true));
        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        let scan_result = scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        );
        FORCE_DETECTION_OPEN_ERROR.with(|f| f.set(false));
        let summary = scan_result.unwrap();

        assert_eq!(
            summary.errors, 1,
            "a detection failure is a real scan error and must be counted"
        );
        let logged: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM scan_errors WHERE path='a.zip'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            logged, 1,
            "a detection failure must be recorded, not silent (this is the F1 defect)"
        );
        assert_eq!(
            status_of(&cat, "a.txt"),
            "active",
            "the archive's entry must not be swept to missing just because detection had to \
             fall back on the extension test"
        );
    }

    #[test]
    fn a_revive_floor_does_not_resurrect_an_entry_removed_before_the_archive_went_missing() {
        // THE Finding-2 regression, reproduced end to end through the real scanner rather than the
        // store API directly:
        //
        //   t=100  bundle.zip = a.txt + gone.txt      -> both active
        //   t=200  rewritten without gone.txt         -> gone.txt correctly swept 'missing'
        //   t=300  archive moved out of the tree      -> archive row + a.txt swept 'missing'
        //   t=400  moved back, identical, same mtime  -> skip path, revive_floor = Some(...)
        //
        // A plain "was the archive missing" boolean cannot tell gone.txt (removed BEFORE the
        // archive went missing) apart from a.txt (missing only because it went missing WITH the
        // archive) -- both revive. The floor must let only a.txt come back.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let bundle_path = root.join("bundle.zip");
        let parked_path = tmp.path().join("bundle.zip.parked");

        // t=100: both entries present.
        std::fs::write(
            &bundle_path,
            make_zip(&[("a.txt", b"alpha"), ("gone.txt", b"beta")]),
        )
        .unwrap();
        let m1 = crate::scan_metrics::ScanMetrics::new();
        let stop1 = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m1,
            &stop1,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(status_of(&cat, "a.txt"), "active");
        assert_eq!(status_of(&cat, "gone.txt"), "active");

        // t=200: rewritten without gone.txt -- a real content change, so it redescends (the size
        // differs, which alone fails the skip check regardless of mtime resolution).
        std::fs::write(&bundle_path, make_zip(&[("a.txt", b"alpha")])).unwrap();
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let stop2 = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            200,
            None,
            &m2,
            &stop2,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(status_of(&cat, "a.txt"), "active");
        assert_eq!(
            status_of(&cat, "gone.txt"),
            "missing",
            "gone.txt was genuinely removed from the archive's content"
        );

        // t=300: the archive itself leaves the tree -- its own row and a.txt's entry are swept.
        std::fs::rename(&bundle_path, &parked_path).unwrap();
        let m3 = crate::scan_metrics::ScanMetrics::new();
        let stop3 = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            300,
            None,
            &m3,
            &stop3,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(status_of(&cat, "a.txt"), "missing");
        assert_eq!(status_of(&cat, "gone.txt"), "missing");

        // t=400: moved back byte-identical (same size, same mtime since it's the same underlying
        // file, never rewritten) -- the skip path fires and touches the archive's entries.
        std::fs::rename(&parked_path, &bundle_path).unwrap();
        let m4 = crate::scan_metrics::ScanMetrics::new();
        let stop4 = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            400,
            None,
            &m4,
            &stop4,
            &test_limits(),
        )
        .unwrap();

        assert_eq!(
            status_of(&cat, "a.txt"),
            "active",
            "a.txt went missing together with the archive, so it must revive with it"
        );
        assert_eq!(
            status_of(&cat, "gone.txt"),
            "missing",
            "gone.txt was removed from the archive BEFORE the archive went missing -- reviving it \
             would assert the archive contains a file it demonstrably does not"
        );
    }

    #[test]
    fn the_revive_floor_still_separates_after_a_second_round_trip() {
        // Adapted from a salvaged review attack: the archive goes missing and returns TWICE, with a
        // legitimate removal happening between the two round-trips. Does the floor correctly
        // separate "removed before the SECOND disappearance" from "present at the second
        // disappearance", given that the same entry (gone.txt) already lived through one revival
        // earlier in its history?
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let bundle_path = root.join("bundle.zip");
        let parked_path = tmp.path().join("bundle.zip.parked");

        // t=100: a.txt + gone.txt present.
        std::fs::write(
            &bundle_path,
            make_zip(&[("a.txt", b"alpha"), ("gone.txt", b"beta")]),
        )
        .unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();

        // t=200/300: archive vanishes (round-trip #1) and returns unchanged -- both entries revive
        // together (this is the ordinary, already-tested case).
        std::fs::rename(&bundle_path, &parked_path).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            200,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();
        std::fs::rename(&parked_path, &bundle_path).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            300,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(status_of(&cat, "a.txt"), "active");
        assert_eq!(
            status_of(&cat, "gone.txt"),
            "active",
            "revived together at round-trip #1"
        );

        // t=400: gone.txt is now genuinely removed via a real descend (size changes).
        std::fs::write(&bundle_path, make_zip(&[("a.txt", b"alpha")])).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            400,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();
        assert_eq!(
            status_of(&cat, "gone.txt"),
            "missing",
            "genuinely removed this time"
        );

        // t=500/600: archive vanishes AGAIN (round-trip #2) and returns unchanged.
        std::fs::rename(&bundle_path, &parked_path).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            500,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();
        std::fs::rename(&parked_path, &bundle_path).unwrap();
        let m = crate::scan_metrics::ScanMetrics::new();
        let s = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            600,
            None,
            &m,
            &s,
            &test_limits(),
        )
        .unwrap();

        assert_eq!(
            status_of(&cat, "a.txt"),
            "active",
            "a.txt was present at round-trip #2's disappearance, must revive"
        );
        assert_eq!(
            status_of(&cat, "gone.txt"),
            "missing",
            "ATTACK: gone.txt was removed BEFORE round-trip #2's disappearance (at t=400), so the \
             SECOND round-trip must not revive it either, even though it WAS revived by the FIRST \
             round-trip"
        );
    }

    #[test]
    fn a_document_container_is_catalogued_whole_and_a_renamed_zip_is_reported() {
        // The whole point of this branch: a .docx must not explode into its parts, and a zip with
        // an unfamiliar extension must be reported rather than silently descended or ignored.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut zw = zip::ZipWriter::new(&mut buf);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inside.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"payload").unwrap();
                zw.finish().unwrap();
            }
            buf.into_inner()
        };
        std::fs::write(root.join("thesis.docx"), &zip_bytes).unwrap();
        std::fs::write(root.join("backup.bak"), &zip_bytes).unwrap();
        std::fs::write(root.join("real.zip"), &zip_bytes).unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(
            &cat,
            &root,
            &ident(),
            false,
            100,
            None,
            &m,
            &stop,
            &test_limits(),
        )
        .unwrap();

        let entries: Vec<String> = cat
            .conn
            .prepare("SELECT relative_path FROM files WHERE container_chain IS NOT NULL ORDER BY relative_path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            entries,
            vec!["real.zip".to_string()],
            "only the .zip is descended into"
        );

        let pending = cat.pending_formats().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].extension, "bak");
        assert_eq!(pending[0].count, 1);

        // All three are still catalogued as ordinary files -- nothing is skipped.
        let loose: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM files WHERE container_chain IS NULL AND status='active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(loose, 3);
    }

    #[test]
    fn a_rescan_does_not_double_count_pending_formats() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut zw = zip::ZipWriter::new(&mut buf);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inside.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"payload").unwrap();
                zw.finish().unwrap();
            }
            buf.into_inner()
        };
        std::fs::write(root.join("backup.bak"), &zip_bytes).unwrap();
        let stop = crate::scan_control::StopFlag::new();
        for now in [100, 200] {
            let m = crate::scan_metrics::ScanMetrics::new();
            scan_volume_with_progress(
                &cat,
                &root,
                &ident(),
                true,
                now,
                None,
                &m,
                &stop,
                &test_limits(),
            )
            .unwrap();
        }
        let pending = cat.pending_formats().unwrap();
        assert_eq!(
            pending[0].count, 1,
            "a rescan re-counts, it does not accumulate"
        );
    }
}

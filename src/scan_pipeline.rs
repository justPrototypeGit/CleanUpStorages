//! The scan pipeline: a walker produces jobs, workers read+hash them, a single writer persists the
//! results. Workers never touch SQLite — the writer is the sole writer. See the design spec.

use crate::archive::ArchiveLimits;
use crate::catalog::models::NewFile;
use crate::catalog::Catalog;
use crate::category::Category;
use crate::scan_metrics::{Phase, ScanMetrics};
use crate::scanner::{Progress, ScanSummary};
use crate::volume::VolumeIdentity;
use crossbeam_channel::{Receiver, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

/// Work the walker hands to a worker. `Touch` and `Error` carry no I/O — they pass through a worker
/// unchanged so the topology stays one-in/one-out (walker has one output, writer one input).
#[derive(Debug, Clone)]
pub(crate) enum Job {
    /// Unchanged file (skip-check matched). `is_archive` triggers touch of the archive's entries.
    Touch { rel: String, is_archive: bool },
    /// The walker already failed this file (e.g. stat error); just record it.
    Error { rel: String, reason: String },
    /// A new/changed loose file to read and hash.
    HashLoose {
        path: PathBuf,
        rel: String,
        filename: String,
        size: i64,
        created: Option<i64>,
        modified: Option<i64>,
        accessed: Option<i64>,
    },
    /// An archive to hash (its own loose row) and descend (its entries).
    ScanArchive {
        path: PathBuf,
        rel: String,
        filename: String,
        size: i64,
        created: Option<i64>,
        modified: Option<i64>,
        accessed: Option<i64>,
    },
}

/// What a worker sends to the writer. One `ScanArchive` job produces both an `Upsert` (the archive's
/// own loose row) and an `ArchiveEntries` (its contents).
#[derive(Debug)]
pub(crate) enum ScanResult {
    Touch {
        rel: String,
        is_archive: bool,
    },
    Error {
        rel: String,
        reason: String,
    },
    Upsert(NewFile),
    ArchiveEntries {
        rel: String,
        modified: Option<i64>,
        scan: crate::archive::ArchiveScanResult,
    },
}

/// Build the loose `NewFile` for a path already read off disk. Shared by the loose and archive
/// jobs (an archive is first catalogued as its own loose row).
#[allow(clippy::too_many_arguments)]
fn loose_record(
    volume_id: &str,
    rel: &str,
    filename: &str,
    path: &Path,
    size: i64,
    hash: String,
    created: Option<i64>,
    modified: Option<i64>,
    accessed: Option<i64>,
) -> NewFile {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    NewFile {
        volume_id: volume_id.to_string(),
        relative_path: rel.to_string(),
        filename: filename.to_string(),
        extension: ext.clone(),
        size_bytes: size,
        content_hash: hash,
        created_time: created,
        modified_time: modified,
        accessed_time: accessed,
        category: Category::from_extension(&ext),
        container_chain: None,
    }
}

/// Do one job's I/O and hashing. No DB access — the writer persists what this returns. Runs on a
/// worker thread; `hash`/`archive` timing is charged here.
pub(crate) fn run_job(
    job: Job,
    volume_id: &str,
    limits: &ArchiveLimits,
    metrics: &ScanMetrics,
) -> Vec<ScanResult> {
    match job {
        Job::Touch { rel, is_archive } => vec![ScanResult::Touch { rel, is_archive }],
        Job::Error { rel, reason } => vec![ScanResult::Error { rel, reason }],
        Job::HashLoose {
            path,
            rel,
            filename,
            size,
            created,
            modified,
            accessed,
        } => {
            let hashed = {
                let _t = metrics.timer(Phase::Hash);
                crate::hashing::hash_file(&path)
            };
            match hashed {
                Ok(hash) => {
                    metrics.add_bytes_hashed(size);
                    let nf = loose_record(
                        volume_id, &rel, &filename, &path, size, hash, created, modified, accessed,
                    );
                    vec![ScanResult::Upsert(nf)]
                }
                Err(e) => vec![ScanResult::Error {
                    rel,
                    reason: format!("read: {e}"),
                }],
            }
        }
        Job::ScanArchive {
            path,
            rel,
            filename,
            size,
            created,
            modified,
            accessed,
        } => {
            // Hash the archive's own bytes for its loose row (identical to the pre-parallel scan,
            // which hashed the zip then descended it — two reads).
            let hashed = {
                let _t = metrics.timer(Phase::Hash);
                crate::hashing::hash_file(&path)
            };
            let hash = match hashed {
                Ok(h) => h,
                Err(e) => {
                    return vec![ScanResult::Error {
                        rel,
                        reason: format!("read: {e}"),
                    }]
                }
            };
            metrics.add_bytes_hashed(size);
            let nf = loose_record(
                volume_id, &rel, &filename, &path, size, hash, created, modified, accessed,
            );
            // Descend. archive::scan_archive is pure (no DB); the writer persists its entries.
            let scan = {
                let _t = metrics.timer(Phase::Archive);
                match std::fs::File::open(&path) {
                    Ok(f) => crate::archive::scan_archive(f, limits),
                    Err(e) => {
                        let mut r = crate::archive::ArchiveScanResult::default();
                        // Empty inner context: the writer qualifies these by the archive path, so
                        // naming the archive again here would log "bundle.zip › bundle.zip".
                        r.errors.push((String::new(), format!("archive open: {e}")));
                        r
                    }
                }
            };
            vec![
                ScanResult::Upsert(nf),
                ScanResult::ArchiveEntries {
                    rel,
                    modified,
                    scan,
                },
            ]
        }
    }
}

/// Walk `root`, stat + skip-check each file, and emit a `Job` per file. Never returns an error:
/// walk and stat failures become `Job::Error`, matching the pre-parallel scan. Runs on its own
/// thread with a read-only catalog connection; `walk`/`skip_check` timing is charged here.
///
/// Returns **true only if the whole tree was walked**. False means the pipeline died underneath it
/// and the walk stopped early — the caller must not then tell the writer the walk completed, or the
/// writer would commit a partial scan and sweep everything unreached to `missing`.
#[must_use]
pub(crate) fn walk(
    ro: &Catalog,
    root: &Path,
    identity: &VolumeIdentity,
    force: bool,
    metrics: &ScanMetrics,
    jobs: &Sender<Job>,
) -> bool {
    // A send only fails once the workers/writer are gone, i.e. the scan is already doomed. Returning
    // false makes the caller stop: otherwise the walker would keep stat-ing every remaining file on
    // a 20 TB drive — hours of seeking — just to throw each job away.
    let send = |j: Job| jobs.send(j).is_ok();
    let mut walker = WalkDir::new(root).into_iter();
    loop {
        let next = {
            let _t = metrics.timer(Phase::Walk);
            walker.next()
        };
        let Some(entry) = next else { break };
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let rel = err
                    .path()
                    .map(|p| {
                        p.strip_prefix(root)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .replace('\\', "/")
                    })
                    .unwrap_or_else(|| "<unknown>".to_string());
                if !send(Job::Error {
                    rel,
                    reason: format!("walk: {err}"),
                }) {
                    return false;
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if crate::scanner::should_skip(path, name) {
            continue;
        }
        let Some(rel) = crate::scanner::relative_path(path, root) else {
            continue;
        };

        let stat = {
            let _t = metrics.timer(Phase::Walk);
            entry.metadata()
        };
        let meta = match stat {
            Ok(m) => m,
            Err(e) => {
                metrics.record_file_seen(0);
                if !send(Job::Error {
                    rel,
                    reason: format!("metadata: {e}"),
                }) {
                    return false;
                }
                continue;
            }
        };
        let size = meta.len() as i64;
        let mtime = crate::scanner::unix_secs(meta.modified());
        metrics.record_file_seen(size);

        let is_archive = crate::archive::is_archive_name(&rel);

        if !force {
            let _t = metrics.timer(Phase::SkipCheck);
            // A read error on the RO connection degrades to "not catalogued", i.e. re-hash. That is
            // the conservative direction: it can never skip a file that should have been hashed.
            if let Ok(Some((old_size, old_mtime))) = ro.get_file_meta(&identity.volume_id, &rel) {
                if old_size == size && old_mtime == mtime.unwrap_or(0) {
                    metrics.add_bytes_skipped(size);
                    if !send(Job::Touch { rel, is_archive }) {
                        return false;
                    }
                    continue;
                }
            }
        }

        let filename = name.to_string_lossy().into_owned();
        let created = crate::scanner::unix_secs(meta.created());
        let accessed = crate::scanner::unix_secs(meta.accessed());
        let sent = if is_archive {
            send(Job::ScanArchive {
                path: path.to_path_buf(),
                rel,
                filename,
                size,
                created,
                modified: mtime,
                accessed,
            })
        } else {
            send(Job::HashLoose {
                path: path.to_path_buf(),
                rel,
                filename,
                size,
                created,
                modified: mtime,
                accessed,
            })
        };
        if !sent {
            return false;
        }
    }
    true
}

/// Set by the pipeline so the writer can tell "the walk finished" from "everything died".
///
/// The results channel closes on BOTH, so closure alone cannot be read as success — committing on
/// an abort would durably persist a partial scan and then sweep every unreported file to `missing`.
#[derive(Default)]
pub(crate) struct PipelineStatus {
    /// The walk ran to completion (set before the jobs channel is closed).
    pub walk_completed: AtomicBool,
    /// A worker died without finishing its jobs (set by its drop-guard, before its sender drops).
    pub aborted: AtomicBool,
}

/// Drain every result into the catalogue inside one batched transaction, then run the missing-sweep.
/// The single writer: only this function writes to SQLite during a scan. `db_write` timing is
/// charged here. On any DB error it aborts and the caller rolls back.
#[allow(
    clippy::too_many_arguments,
    reason = "each parameter is an independent scan input; grouping them into a struct would add \
        indirection without reducing real complexity"
)]
pub(crate) fn write_results(
    cat: &Catalog,
    results: Receiver<ScanResult>,
    identity: &VolumeIdentity,
    scan_started_at: i64,
    now: i64,
    metrics: &ScanMetrics,
    progress: Option<&dyn Progress>,
    status: &PipelineStatus,
) -> anyhow::Result<ScanSummary> {
    let mut summary = ScanSummary::default();
    let mut in_batch = 0usize;
    cat.conn.execute_batch("BEGIN")?;

    for result in results {
        let _t = metrics.timer(Phase::DbWrite);
        match result {
            ScanResult::Touch { rel, is_archive } => {
                cat.touch_seen(&identity.volume_id, &rel, now)?;
                if is_archive {
                    cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                }
                summary.skipped += 1;
                if let Some(p) = progress {
                    p.on_skipped();
                }
                in_batch += 1;
            }
            ScanResult::Error { rel, reason } => {
                cat.log_scan_error(Some(&identity.volume_id), &rel, &reason, now)?;
                summary.errors += 1;
                if let Some(p) = progress {
                    p.on_error();
                }
                // Touch so a readable-but-unhashable file is not swept to 'missing' (matches today).
                let _ = cat.touch_seen(&identity.volume_id, &rel, now);
                in_batch += 1;
            }
            ScanResult::Upsert(nf) => {
                cat.upsert_file(&nf, now)?;
                summary.hashed += 1;
                if let Some(p) = progress {
                    p.on_hashed();
                }
                in_batch += 1;
            }
            ScanResult::ArchiveEntries {
                rel,
                modified,
                scan,
            } => {
                for entry in &scan.entries {
                    cat.upsert_archive_entry(&identity.volume_id, &rel, entry, modified, now)?;
                    summary.archive_entries += 1;
                    if let Some(p) = progress {
                        p.on_archive_entry();
                    }
                    in_batch += 1;
                }
                for (ctx, reason) in &scan.errors {
                    // Same location format as the pre-parallel scan: the inner context qualified by
                    // the archive it came from, or the archive alone when there is no inner context.
                    let where_ = if ctx.is_empty() {
                        rel.clone()
                    } else {
                        format!("{rel} › {ctx}")
                    };
                    cat.log_scan_error(Some(&identity.volume_id), &where_, reason, now)?;
                    summary.errors += 1;
                    if let Some(p) = progress {
                        p.on_error();
                    }
                }
            }
        }
        crate::scanner::rotate_batch(cat, &mut in_batch)?;
    }

    // The channel closing does NOT mean the scan succeeded — it also closes when a worker panics or
    // the walker bails, because that drops the senders. Committing here on an abort would make a
    // partial scan durable and then sweep every file the walk never reached to `missing`: on a
    // 1.8M-file drive, an abort late in a long scan would silently un-`active` a large part of the
    // register. The pre-parallel scan could not do this — it held one transaction and rolled back.
    if !status.walk_completed.load(Ordering::SeqCst) {
        anyhow::bail!("scan did not complete its walk; no results were committed");
    }
    if status.aborted.load(Ordering::SeqCst) {
        anyhow::bail!("a scan worker died; no results were committed");
    }

    {
        let _t = metrics.timer(Phase::DbWrite);
        cat.conn.execute_batch("COMMIT")?;
        summary.marked_missing =
            cat.mark_missing_scanned(&identity.volume_id, scan_started_at, now)?;
    }
    summary.metrics = metrics.snapshot();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::scan_metrics::ScanMetrics;
    use crate::volume::VolumeIdentity;

    /// Default archive limits for tests that do not care about the bomb caps.
    fn test_limits() -> ArchiveLimits {
        ArchiveLimits {
            max_depth: 8,
            entry_max_bytes: 2 * 1024 * 1024 * 1024,
            ratio_cap: 200,
            total_buffer_bytes: 2 * 1024 * 1024 * 1024,
        }
    }

    fn ident() -> VolumeIdentity {
        VolumeIdentity {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
        }
    }

    /// Drain the walker's jobs into a Vec.
    fn walk_to_vec(cat: &Catalog, root: &std::path::Path, force: bool) -> Vec<Job> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let m = ScanMetrics::new();
        // The receiver is alive for the whole walk here, so a complete walk is also the assertion
        // that no send failed.
        assert!(
            walk(cat, root, &ident(), force, &m, &tx),
            "the walk should run to completion in this fixture"
        );
        drop(tx);
        rx.into_iter().collect()
    }

    #[test]
    fn walker_emits_hash_jobs_for_new_files_and_scan_jobs_for_archives() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("doc.txt"), b"hi").unwrap();
        {
            let f = std::fs::File::create(t.path().join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("e.txt", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"x").unwrap();
            zw.finish().unwrap();
        }
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        let jobs = walk_to_vec(&cat, t.path(), false);
        assert!(jobs
            .iter()
            .any(|j| matches!(j, Job::HashLoose { rel, .. } if rel == "doc.txt")));
        assert!(jobs
            .iter()
            .any(|j| matches!(j, Job::ScanArchive { rel, .. } if rel == "bundle.zip")));
    }

    #[test]
    fn walker_emits_touch_for_an_unchanged_catalogued_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("same.txt");
        std::fs::write(&p, b"stable").unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let size = meta.len() as i64;
        let mtime = crate::scanner::unix_secs(meta.modified());
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        cat.upsert_file(
            &crate::catalog::models::NewFile {
                volume_id: "v1".into(),
                relative_path: "same.txt".into(),
                filename: "same.txt".into(),
                extension: "txt".into(),
                size_bytes: size,
                content_hash: "h".into(),
                created_time: None,
                modified_time: mtime,
                accessed_time: None,
                category: crate::category::Category::Other,
                container_chain: None,
            },
            50,
        )
        .unwrap();

        let jobs = walk_to_vec(&cat, t.path(), false); // not forced
        assert!(
            jobs.iter()
                .any(|j| matches!(j, Job::Touch { rel, .. } if rel == "same.txt")),
            "unchanged file should be a Touch, got {jobs:?}"
        );

        // --force re-hashes it instead.
        let forced = walk_to_vec(&cat, t.path(), true);
        assert!(forced
            .iter()
            .any(|j| matches!(j, Job::HashLoose { rel, .. } if rel == "same.txt")));
    }

    #[test]
    fn writer_applies_results_and_counts_them() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(ScanResult::Upsert(crate::catalog::models::NewFile {
            volume_id: "v1".into(),
            relative_path: "a.txt".into(),
            filename: "a.txt".into(),
            extension: "txt".into(),
            size_bytes: 3,
            content_hash: "H".into(),
            created_time: None,
            modified_time: Some(7),
            accessed_time: None,
            category: crate::category::Category::Other,
            container_chain: None,
        }))
        .unwrap();
        tx.send(ScanResult::Error {
            rel: "bad.txt".into(),
            reason: "read: nope".into(),
        })
        .unwrap();
        drop(tx);

        let m = ScanMetrics::new();
        let status = PipelineStatus::default();
        status.walk_completed.store(true, Ordering::SeqCst);
        let summary = write_results(&cat, rx, &ident(), 100, 100, &m, None, &status).unwrap();

        assert_eq!(summary.hashed, 1);
        assert_eq!(summary.errors, 1);
        // The row landed.
        let hits = cat.search("a.txt", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content_hash, "H");
        // The error was logged.
        assert!(cat.volume_has_scan_errors("v1").unwrap());
    }

    /// Seed a volume with one catalogued, currently-active file.
    fn cat_with_active_file(t: &tempfile::TempDir) -> Catalog {
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        cat.upsert_file(
            &crate::catalog::models::NewFile {
                volume_id: "v1".into(),
                relative_path: "present.txt".into(),
                filename: "present.txt".into(),
                extension: "txt".into(),
                size_bytes: 1,
                content_hash: "H".into(),
                created_time: None,
                modified_time: Some(1),
                accessed_time: None,
                category: crate::category::Category::Other,
                container_chain: None,
            },
            50,
        )
        .unwrap();
        cat
    }

    #[test]
    fn a_panicking_worker_sets_the_flag_before_its_sender_closes_the_channel() {
        // The whole abort guarantee rests on a Rust drop-order detail: in a `move` closure, body
        // locals drop during unwind BEFORE the captured variables do. The abort guard is a body
        // local and the results sender is a capture, so a panicking worker flags itself while the
        // channel is still open — the writer therefore cannot observe "channel closed" without
        // also observing "aborted". If that ordering were ever reversed, the writer would commit a
        // partial scan and sweep the volume. This pins the mechanism itself.
        struct Guard<'a>(&'a AtomicBool);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded::<u8>();

        let f = flag.clone();
        // `tx` is captured and never rebound in the body — exactly how the worker holds `res_tx`.
        let h = std::thread::spawn(move || {
            let _guard = Guard(&f); // body local: drops first on unwind
            let _use_tx = &tx; // keep the capture alive to the end of the body
            panic!("worker died");
        });

        // Observe the channel closing WITHOUT joining first. Joining would let the thread finish
        // unwinding completely, so the assertion would hold no matter what order things dropped in
        // — which is what made an earlier version of this test prove nothing. Blocking on recv()
        // wakes us at the instant the last sender drops, which is the moment that matters.
        assert!(
            rx.recv().is_err(),
            "sender dropped, so the channel is closed"
        );
        assert!(
            flag.load(Ordering::SeqCst),
            "the abort flag must already be set when the channel closes; if the guard dropped after \
             the sender, the writer could observe a closed channel and commit a partial scan"
        );

        assert!(h.join().is_err(), "the worker must actually have panicked");
    }

    #[test]
    fn an_incomplete_walk_commits_nothing_and_sweeps_nothing() {
        // The results channel closes when the walk finishes AND when everything dies. If the writer
        // treats closure as success it commits a partial scan and then sweeps every file the walk
        // never reached to `missing` -- on a 1.8M-file drive, an abort late in a long scan would
        // silently un-`active` a large part of the register. The pre-parallel scan could not do
        // this: it held one transaction and rolled back.
        let t = tempfile::tempdir().unwrap();
        let cat = cat_with_active_file(&t);

        let (tx, rx) = crossbeam_channel::unbounded();
        drop(tx); // channel closes with walk_completed still false
        let m = ScanMetrics::new();
        let status = PipelineStatus::default(); // walk_completed = false

        let res = write_results(&cat, rx, &ident(), 100, 100, &m, None, &status);
        assert!(res.is_err(), "an incomplete walk must not report success");
        let _ = cat.conn.execute_batch("ROLLBACK");

        let status_now: String = cat
            .conn
            .query_row(
                "SELECT status FROM files WHERE relative_path='present.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            status_now, "active",
            "a file that is still on disk must not be swept to missing by an aborted scan"
        );
    }

    #[test]
    fn a_dead_worker_prevents_the_commit() {
        // Same protection, via the other abort path: the walk finished but a worker died before
        // reporting its files.
        let t = tempfile::tempdir().unwrap();
        let cat = cat_with_active_file(&t);

        let (tx, rx) = crossbeam_channel::unbounded();
        drop(tx);
        let m = ScanMetrics::new();
        let status = PipelineStatus::default();
        status.walk_completed.store(true, Ordering::SeqCst);
        status.aborted.store(true, Ordering::SeqCst);

        let res = write_results(&cat, rx, &ident(), 100, 100, &m, None, &status);
        assert!(res.is_err(), "a dead worker must not report success");
        let _ = cat.conn.execute_batch("ROLLBACK");

        let status_now: String = cat
            .conn
            .query_row(
                "SELECT status FROM files WHERE relative_path='present.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status_now, "active");
    }

    #[test]
    fn a_completed_walk_does_commit_and_sweep() {
        // The guard must not be so strict that a normal scan stops sweeping: a file that really is
        // gone still has to become `missing`.
        let t = tempfile::tempdir().unwrap();
        let cat = cat_with_active_file(&t);

        let (tx, rx) = crossbeam_channel::unbounded();
        drop(tx);
        let m = ScanMetrics::new();
        let status = PipelineStatus::default();
        status.walk_completed.store(true, Ordering::SeqCst);

        // scan_started_at = 100 > the row's last_seen_at of 50, so the sweep should catch it.
        let summary = write_results(&cat, rx, &ident(), 100, 100, &m, None, &status).unwrap();
        assert_eq!(summary.marked_missing, 1);

        let status_now: String = cat
            .conn
            .query_row(
                "SELECT status FROM files WHERE relative_path='present.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status_now, "missing");
    }

    #[test]
    fn writer_prefixes_archive_errors_with_the_archive_path() {
        // The pre-parallel scan logged an archive's internal error as "<archive> › <entry>", and
        // bare "<archive>" when the error had no inner context. Scan errors are user-facing, so the
        // location must not silently lose its archive prefix.
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();

        let mut scan = crate::archive::ArchiveScanResult::default();
        scan.errors.push(("inner.txt".into(), "zip bomb".into()));
        scan.errors
            .push((String::new(), "unreadable archive".into()));

        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(ScanResult::ArchiveEntries {
            rel: "bundle.zip".into(),
            modified: Some(5),
            scan,
        })
        .unwrap();
        drop(tx);

        let m = ScanMetrics::new();
        let status = PipelineStatus::default();
        status.walk_completed.store(true, Ordering::SeqCst);
        let summary = write_results(&cat, rx, &ident(), 100, 100, &m, None, &status).unwrap();
        assert_eq!(summary.errors, 2);

        let paths: Vec<String> = cat
            .conn
            .prepare("SELECT path FROM scan_errors ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["bundle.zip › inner.txt", "bundle.zip"]);
    }

    #[test]
    fn an_unopenable_archive_logs_the_archive_path_once() {
        // run_job and write_results have to agree on who names the archive. If the worker also put
        // the archive path in the inner context, the writer would log "x.zip › x.zip".
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v1".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();

        // A path that exists for stat purposes but cannot be opened as a file: use a directory.
        let dir_as_archive = t.path().join("notreally.zip");
        std::fs::create_dir(&dir_as_archive).unwrap();

        let m = ScanMetrics::new();
        let out = run_job(
            Job::ScanArchive {
                path: dir_as_archive,
                rel: "notreally.zip".into(),
                filename: "notreally.zip".into(),
                size: 0,
                created: None,
                modified: None,
                accessed: None,
            },
            "v1",
            &test_limits(),
            &m,
        );

        let (tx, rx) = crossbeam_channel::unbounded();
        for r in out {
            tx.send(r).unwrap();
        }
        drop(tx);
        let status = PipelineStatus::default();
        status.walk_completed.store(true, Ordering::SeqCst);
        write_results(&cat, rx, &ident(), 100, 100, &m, None, &status).unwrap();

        let paths: Vec<String> = cat
            .conn
            .prepare("SELECT path FROM scan_errors")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for p in &paths {
            assert!(
                !p.contains("notreally.zip › notreally.zip"),
                "archive path logged twice: {p}"
            );
        }
    }

    fn tmp_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn hash_loose_job_produces_an_upsert_with_the_right_hash() {
        let t = tempfile::tempdir().unwrap();
        let p = tmp_file(t.path(), "a.txt", b"hello");
        let m = ScanMetrics::new();
        let job = Job::HashLoose {
            path: p.clone(),
            rel: "a.txt".into(),
            filename: "a.txt".into(),
            size: 5,
            created: Some(1),
            modified: Some(2),
            accessed: None,
        };
        let out = run_job(job, "v1", &test_limits(), &m);
        assert_eq!(out.len(), 1);
        match &out[0] {
            ScanResult::Upsert(nf) => {
                assert_eq!(nf.relative_path, "a.txt");
                assert_eq!(nf.volume_id, "v1");
                assert_eq!(nf.size_bytes, 5);
                let mut raw: &[u8] = b"hello";
                assert_eq!(
                    nf.content_hash,
                    crate::hashing::hash_reader(&mut raw).unwrap()
                );
                assert!(nf.container_chain.is_none());
                assert_eq!(nf.modified_time, Some(2));
            }
            other => panic!("expected Upsert, got {other:?}"),
        }
        assert_eq!(m.snapshot().bytes_hashed, 5);
    }

    #[test]
    fn a_missing_loose_file_becomes_an_error_not_a_panic() {
        let m = ScanMetrics::new();
        let job = Job::HashLoose {
            path: "/no/such/file".into(),
            rel: "gone.txt".into(),
            filename: "gone.txt".into(),
            size: 0,
            created: None,
            modified: None,
            accessed: None,
        };
        let out = run_job(job, "v1", &test_limits(), &m);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], ScanResult::Error { rel, .. } if rel == "gone.txt"));
    }

    #[test]
    fn touch_and_error_jobs_pass_through_unchanged() {
        let m = ScanMetrics::new();
        let t = run_job(
            Job::Touch {
                rel: "x".into(),
                is_archive: true,
            },
            "v1",
            &test_limits(),
            &m,
        );
        assert!(matches!(&t[0], ScanResult::Touch { rel, is_archive: true } if rel == "x"));
        let e = run_job(
            Job::Error {
                rel: "y".into(),
                reason: "bad".into(),
            },
            "v1",
            &test_limits(),
            &m,
        );
        assert!(matches!(&e[0], ScanResult::Error { rel, .. } if rel == "y"));
    }

    #[test]
    fn a_zip_job_yields_the_loose_row_and_its_entries() {
        let t = tempfile::tempdir().unwrap();
        // Build a real zip with one entry.
        let zip_path = t.path().join("bundle.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("inner.txt", opts).unwrap();
            std::io::Write::write_all(&mut zw, b"inner bytes").unwrap();
            zw.finish().unwrap();
        }
        let size = std::fs::metadata(&zip_path).unwrap().len() as i64;
        let m = ScanMetrics::new();
        let job = Job::ScanArchive {
            path: zip_path,
            rel: "bundle.zip".into(),
            filename: "bundle.zip".into(),
            size,
            created: None,
            modified: Some(99),
            accessed: None,
        };
        let out = run_job(job, "v1", &test_limits(), &m);
        // First result: the zip's own loose row. Second: its entries.
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], ScanResult::Upsert(nf) if nf.relative_path == "bundle.zip"));
        match &out[1] {
            ScanResult::ArchiveEntries {
                rel,
                modified,
                scan,
            } => {
                assert_eq!(rel, "bundle.zip");
                assert_eq!(*modified, Some(99));
                assert_eq!(scan.entries.len(), 1);
                assert_eq!(scan.entries[0].filename, "inner.txt");
            }
            other => panic!("expected ArchiveEntries, got {other:?}"),
        }
    }
}

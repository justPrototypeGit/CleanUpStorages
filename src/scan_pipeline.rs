//! The scan pipeline: a walker produces jobs, workers read+hash them, a single writer persists the
//! results. Workers never touch SQLite — the writer is the sole writer. See the design spec.

// The pipeline is built bottom-up (worker, walker, writer) and only wired into `scanner` by the
// orchestrator task, so these are legitimately unused until then. REMOVE this attribute when the
// orchestrator lands — after that, dead code here is a real defect.
#![allow(dead_code)]

use crate::catalog::models::NewFile;
use crate::category::Category;
use crate::scan_metrics::{Phase, ScanMetrics};
use std::path::{Path, PathBuf};

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
pub(crate) fn run_job(job: Job, volume_id: &str, metrics: &ScanMetrics) -> Vec<ScanResult> {
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
                // ArchiveLimits::from_config only reads three static numbers; a failure to resolve
                // the data dir must not abort a scan, so fall back to the config defaults.
                let limits = crate::config::Config::default_paths()
                    .map(|c| crate::archive::ArchiveLimits::from_config(&c))
                    .unwrap_or_else(|_| crate::archive::ArchiveLimits {
                        max_depth: 8,
                        entry_max_bytes: 2 * 1024 * 1024 * 1024,
                        ratio_cap: 200,
                        total_buffer_bytes: 2 * 1024 * 1024 * 1024,
                    });
                match std::fs::File::open(&path) {
                    Ok(f) => crate::archive::scan_archive(f, &limits),
                    Err(e) => {
                        let mut r = crate::archive::ArchiveScanResult::default();
                        r.errors.push((rel.clone(), format!("archive open: {e}")));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_metrics::ScanMetrics;

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
        let out = run_job(job, "v1", &m);
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
        let out = run_job(job, "v1", &m);
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
            &m,
        );
        assert!(matches!(&t[0], ScanResult::Touch { rel, is_archive: true } if rel == "x"));
        let e = run_job(
            Job::Error {
                rel: "y".into(),
                reason: "bad".into(),
            },
            "v1",
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
        let out = run_job(job, "v1", &m);
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

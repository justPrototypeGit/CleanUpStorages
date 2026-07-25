//! The scan pipeline: a walker produces jobs, workers read+hash them, a single writer persists the
//! results. Workers never touch SQLite — the writer is the sole writer. See the design spec.

use crate::catalog::models::NewFile;
use std::path::PathBuf;

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

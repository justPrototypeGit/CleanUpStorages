//! Duplicate detection and reclaimable-space queries.
//!
//! Everything here derives from ONE definition of which copy is kept (`KEEP_ORDER`), so the rule
//! cannot drift between the whole-table view and the per-page query.

/// The single source of truth for "which copy do we keep?": oldest `created_time`, then
/// `modified_time`, then `id`. NULL timestamps sort LAST — SQLite would otherwise sort them first,
/// which would silently change which copy survives.
pub const KEEP_ORDER: &str =
    "IFNULL(created_time, 9223372036854775807), IFNULL(modified_time, 9223372036854775807), id";

/// Default review floor in bytes (1 MiB). Presentational only — the catalog keeps every row.
pub const DEFAULT_MIN_SIZE: i64 = 1_048_576;

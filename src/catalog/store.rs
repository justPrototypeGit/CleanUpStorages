use crate::catalog::models::*;
use crate::catalog::Catalog;
use rusqlite::params;

/// Optional filters for a catalog search/browse. Empty vec / `None` = match everything; each of the
/// `category`/`volume`/`status` vecs is OR-combined (SQL `IN`), so the filters are multi-select.
#[derive(Default, Debug, Clone)]
pub struct SearchFilters {
    pub query: String,
    pub category: Vec<String>,
    pub volume: Vec<String>,
    pub status: Vec<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
}

/// The full `files` column list, in one place. Every full-row SELECT uses this; the mapper
/// (`map_file_record`) reads results by column NAME, so this list and the mapper cannot drift.
const FILE_COLUMNS: &str =
    "id, volume_id, relative_path, filename, extension, size_bytes, content_hash, \
     created_time, modified_time, accessed_time, category, container_chain, \
     status, first_seen_at, last_seen_at, original_path";

impl Catalog {
    pub fn upsert_volume(&self, v: &Volume) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO volumes(volume_id, label, identified_by, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id) DO UPDATE SET label=excluded.label,
                 identified_by=excluded.identified_by, last_seen_at=excluded.last_seen_at",
            params![
                v.volume_id,
                v.label,
                v.identified_by,
                v.first_seen_at,
                v.last_seen_at
            ],
        )?;
        Ok(())
    }

    /// (size_bytes, modified_time-or-0) for a loose file, if catalogued.
    pub fn get_file_meta(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<Option<(i64, i64)>> {
        let row = self.conn.query_row(
            "SELECT size_bytes, IFNULL(modified_time,0) FROM files
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            params![volume_id, relative_path],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or update one loose file; sets status=active and last_seen_at=now.
    pub fn upsert_file(&self, f: &NewFile, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                 content_hash, created_time, modified_time, accessed_time, category,
                 container_chain, status, first_seen_at, last_seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active',?12,?12)
             ON CONFLICT(volume_id, relative_path) WHERE container_chain IS NULL DO UPDATE SET
                 filename=excluded.filename, extension=excluded.extension,
                 size_bytes=excluded.size_bytes, content_hash=excluded.content_hash,
                 created_time=excluded.created_time, modified_time=excluded.modified_time,
                 accessed_time=excluded.accessed_time, category=excluded.category,
                 status='active', last_seen_at=excluded.last_seen_at",
            params![
                f.volume_id,
                f.relative_path,
                f.filename,
                f.extension,
                f.size_bytes,
                f.content_hash,
                f.created_time,
                f.modified_time,
                f.accessed_time,
                f.category.as_str(),
                f.container_chain,
                now
            ],
        )?;
        Ok(())
    }

    /// Refresh last_seen/status for an unchanged file without re-hashing. Returns true if a row matched.
    pub fn touch_seen(
        &self,
        volume_id: &str,
        relative_path: &str,
        now: i64,
    ) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            rusqlite::params![volume_id, relative_path, now],
        )?;
        Ok(n > 0)
    }

    /// Flag active files (loose or archived) on this volume not touched by the current scan as missing.
    pub fn mark_missing_scanned(
        &self,
        volume_id: &str,
        scan_started_at: i64,
        _now: i64,
    ) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "UPDATE files SET status='missing'
             WHERE volume_id=?1 AND status='active' AND last_seen_at < ?2",
            params![volume_id, scan_started_at],
        )?;
        Ok(n)
    }

    /// Insert/update one archive entry (a file inside an archive). Identity is
    /// (volume_id, archive_rel_path, container_chain) via idx_files_archived_identity.
    pub fn upsert_archive_entry(
        &self,
        volume_id: &str,
        archive_rel_path: &str,
        e: &crate::archive::ArchiveEntry,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                 content_hash, created_time, modified_time, accessed_time, category,
                 container_chain, status, first_seen_at, last_seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,NULL,?7,?8,'active',?9,?9)
             ON CONFLICT(volume_id, relative_path, container_chain)
                 WHERE container_chain IS NOT NULL DO UPDATE SET
                 filename=excluded.filename, extension=excluded.extension,
                 size_bytes=excluded.size_bytes, content_hash=excluded.content_hash,
                 category=excluded.category, status='active', last_seen_at=excluded.last_seen_at",
            params![
                volume_id,
                archive_rel_path,
                e.filename,
                e.extension,
                e.size_bytes,
                e.content_hash,
                Category::from_extension(&e.extension).as_str(),
                e.container_chain,
                now
            ],
        )?;
        Ok(())
    }

    /// Refresh last_seen/status for every archive entry under one archive file (unchanged-archive skip).
    pub fn touch_archive_entries(
        &self,
        volume_id: &str,
        archive_rel_path: &str,
        now: i64,
    ) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NOT NULL AND status='active'",
            params![volume_id, archive_rel_path, now],
        )?;
        Ok(n)
    }

    pub fn log_scan_error(
        &self,
        volume_id: Option<&str>,
        path: &str,
        reason: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_errors(volume_id, path, reason, occurred_at) VALUES (?1,?2,?3,?4)",
            params![volume_id, path, reason, now],
        )?;
        Ok(())
    }

    /// The volume's last_seen_at (updated on every scan), if the volume exists.
    pub fn volume_last_seen(&self, volume_id: &str) -> anyhow::Result<Option<i64>> {
        let row = self.conn.query_row(
            "SELECT last_seen_at FROM volumes WHERE volume_id=?1",
            params![volume_id],
            |r| r.get::<_, i64>(0),
        );
        match row {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// True if this volume has any recorded scan error.
    pub fn volume_has_scan_errors(&self, volume_id: &str) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM scan_errors WHERE volume_id=?1",
            params![volume_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn duplicate_group_count(&self) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT count(*) FROM (SELECT content_hash FROM files
                 WHERE status='active' GROUP BY content_hash HAVING count(*) > 1)",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// For the given content hashes, those with >1 active copy in the catalog, mapped to their
    /// active copy count. Bounded by the passed hashes (indexed on content_hash).
    pub fn duplicate_counts(
        &self,
        hashes: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        let mut out = std::collections::HashMap::new();
        if hashes.is_empty() {
            return Ok(out);
        }
        // Deduplicate the input so the IN-list stays small.
        let uniq: std::collections::HashSet<&String> = hashes.iter().collect();
        let placeholders = std::iter::repeat("?")
            .take(uniq.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT content_hash, count(*) FROM files
             WHERE status='active' AND content_hash IN ({placeholders})
             GROUP BY content_hash HAVING count(*) > 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = uniq
            .iter()
            .map(|h| *h as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (h, n) = row?;
            out.insert(h, n);
        }
        Ok(out)
    }

    pub fn volume_stats(&self) -> anyhow::Result<Vec<(String, String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.volume_id, v.label,
                    count(f.id) FILTER (WHERE f.status='active'),
                    IFNULL(sum(f.size_bytes) FILTER (WHERE f.status='active'),0)
             FROM volumes v LEFT JOIN files f ON f.volume_id=v.volume_id
             GROUP BY v.volume_id, v.label ORDER BY v.label",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Back-compat wrapper: text + category/volume/status filters, limit 1000.
    pub fn search(
        &self,
        query: &str,
        category: Option<&str>,
        volume: Option<&str>,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<FileRecord>> {
        let f = SearchFilters {
            query: query.to_string(),
            category: category.map(str::to_string).into_iter().collect(),
            volume: volume.map(str::to_string).into_iter().collect(),
            status: status.map(str::to_string).into_iter().collect(),
            ..Default::default()
        };
        self.search_filtered(&f, 1000)
    }

    /// Build ` AND <col> IN (?,?,…)` for a multi-value filter, pushing each value as an arg.
    fn push_in_clause(
        sql: &mut String,
        args: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        col: &str,
        values: &[String],
    ) {
        if values.is_empty() {
            return;
        }
        let holders = std::iter::repeat("?")
            .take(values.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND {col} IN ({holders})"));
        for v in values {
            args.push(Box::new(v.clone()));
        }
    }

    /// Full filtered search over the catalog.
    pub fn search_filtered(
        &self,
        f: &SearchFilters,
        limit: usize,
    ) -> anyhow::Result<Vec<FileRecord>> {
        let mut sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let q = f.query.trim();
        if !q.is_empty() {
            sql.push_str(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)");
            // FTS prefix match on each token; quote as a literal string so punctuation
            // (", (, -, :) in the query can't be parsed as FTS5 query syntax.
            let match_expr = q
                .split_whitespace()
                .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" ");
            args.push(Box::new(match_expr));
        }
        Self::push_in_clause(&mut sql, &mut args, "category", &f.category);
        Self::push_in_clause(&mut sql, &mut args, "volume_id", &f.volume);
        // Purged rows are a permanently-deleted audit record — the file (and its `_ToDelete` folder)
        // is gone from disk, so hide them from the default browse/search. They remain reachable only
        // by explicitly including status = 'purged' in the filter.
        if f.status.is_empty() {
            sql.push_str(" AND status != 'purged'");
        } else {
            Self::push_in_clause(&mut sql, &mut args, "status", &f.status);
        }
        if let Some(n) = f.min_size {
            sql.push_str(" AND size_bytes >= ?");
            args.push(Box::new(n));
        }
        if let Some(n) = f.max_size {
            sql.push_str(" AND size_bytes <= ?");
            args.push(Box::new(n));
        }
        if let Some(n) = f.modified_after {
            sql.push_str(" AND modified_time >= ?");
            args.push(Box::new(n));
        }
        if let Some(n) = f.modified_before {
            sql.push_str(" AND modified_time <= ?");
            args.push(Box::new(n));
        }
        sql.push_str(" ORDER BY relative_path LIMIT ?");
        args.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), Self::map_file_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Count rows per status for the given text/category/volume context (status itself is not
    /// filtered). Lets the UI flag which kinds — active/missing/quarantined/purged — are present,
    /// including purged rows that the default search hides.
    pub fn status_counts(
        &self,
        query: &str,
        category: &[String],
        volume: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        let mut sql = String::from("SELECT status, count(*) FROM files WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            sql.push_str(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)");
            let match_expr = q
                .split_whitespace()
                .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" ");
            args.push(Box::new(match_expr));
        }
        Self::push_in_clause(&mut sql, &mut args, "category", category);
        Self::push_in_clause(&mut sql, &mut args, "volume_id", volume);
        sql.push_str(" GROUP BY status");

        let mut stmt = self.conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
    }

    /// Id of the loose file (container_chain IS NULL) at this path, if catalogued, regardless of
    /// status. Exactly one such row can exist per (volume, path) — the loose-identity partial
    /// unique index guarantees it.
    pub fn loose_file_id(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<Option<i64>> {
        let row = self.conn.query_row(
            "SELECT id FROM files WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            params![volume_id, relative_path],
            |r| r.get::<_, i64>(0),
        );
        match row {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Fetch a single file record by id.
    pub fn get_file(&self, id: i64) -> anyhow::Result<Option<FileRecord>> {
        let row = self.conn.query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE id=?1"),
            params![id],
            Self::map_file_record,
        );
        match row {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Groups of ≥2 active files (loose or archived) sharing a content_hash,
    /// ordered by hash then id. Consecutive rows with the same hash form a group.
    pub fn duplicate_groups(&self) -> anyhow::Result<Vec<Vec<FileRecord>>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE status='active' AND content_hash IN (
                 SELECT content_hash FROM files WHERE status='active'
                 GROUP BY content_hash HAVING count(*) > 1)
             ORDER BY content_hash, id"
        ))?;
        let rows = stmt
            .query_map([], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut groups: Vec<Vec<FileRecord>> = Vec::new();
        for r in rows {
            match groups.last_mut() {
                Some(g) if g[0].content_hash == r.content_hash => g.push(r),
                _ => groups.push(vec![r]),
            }
        }
        Ok(groups)
    }

    /// Bytes-per-volume of active duplicate members that are NOT their group's suggested keep.
    /// Suggested keep = earliest created_time, then earliest modified_time, then smallest id.
    pub fn reclaimable_bytes_by_volume(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        let mut out: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for group in self.duplicate_groups()? {
            let keep = group
                .iter()
                .min_by_key(|f| {
                    (
                        f.created_time.unwrap_or(i64::MAX),
                        f.modified_time.unwrap_or(i64::MAX),
                        f.id,
                    )
                })
                .map(|f| f.id)
                .unwrap_or(0);
            for f in &group {
                if f.id != keep {
                    *out.entry(f.volume_id.clone()).or_default() += f.size_bytes;
                }
            }
        }
        Ok(out)
    }

    /// All currently-active rows sharing this content hash (loose or archived).
    pub fn active_copies(&self, hash: &str) -> anyhow::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE content_hash=?1 AND status='active' ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![hash], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// True if any loose row (any status) on this volume already uses this relative_path.
    pub fn loose_path_taken(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM files WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            rusqlite::params![volume_id, relative_path], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Move a file into quarantine: records where it moved to and where it came from.
    /// Also clears container_chain, so an extracted archive entry becomes a proper
    /// loose quarantined row (a no-op for files that were already loose).
    pub fn mark_quarantined(
        &self,
        id: i64,
        new_relative_path: &str,
        original_path: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE files SET status='quarantined', relative_path=?2, original_path=?3,
                 container_chain=NULL, last_seen_at=?4 WHERE id=?1",
            params![id, new_relative_path, original_path, now],
        )?;
        Ok(())
    }

    /// An archive's currently-catalogued entries (active rows filed under this
    /// relative_path with a non-null container_chain).
    pub fn archive_entries(
        &self,
        volume_id: &str,
        archive_rel_path: &str,
    ) -> anyhow::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            &format!(
                "SELECT {FILE_COLUMNS} FROM files
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NOT NULL AND status='active'
             ORDER BY id"
            ))?;
        let rows = stmt
            .query_map(params![volume_id, archive_rel_path], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Update a loose archive row's hash/size after a rebuild (repack).
    pub fn update_archive_hash(
        &self,
        id: i64,
        content_hash: &str,
        size_bytes: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE files SET content_hash=?2, size_bytes=?3, last_seen_at=?4 WHERE id=?1",
            params![id, content_hash, size_bytes, now],
        )?;
        Ok(())
    }

    /// All quarantined rows for a volume, ordered by id.
    pub fn quarantined_rows(&self, volume_id: &str) -> anyhow::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILE_COLUMNS} FROM files
             WHERE volume_id=?1 AND status='quarantined' ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![volume_id], Self::map_file_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark a quarantined file as permanently purged.
    pub fn mark_purged(&self, id: i64, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE files SET status='purged', last_seen_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        Ok(())
    }

    /// Total bytes that would be reclaimed by purging this volume's quarantined files.
    pub fn recoverable_bytes(&self, volume_id: &str) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT IFNULL(sum(size_bytes),0) FROM files WHERE volume_id=?1 AND status='quarantined'",
            params![volume_id], |r| r.get(0))?;
        Ok(n)
    }

    /// Remove ALL catalog knowledge of a volume: its file rows (FTS cleaned up by triggers) and
    /// its `volumes` row. Never touches files on disk — a later rescan fully rebuilds the volume.
    /// Returns the number of file rows removed, and logs a `forget` audit action.
    ///
    /// All of it — the three deletes and the audit row — runs in one transaction: any error before
    /// commit rolls everything back (the `Transaction` guard rolls back on drop), so a mid-delete
    /// failure can never leave a half-forgotten volume or a delete without its audit entry.
    pub fn forget_volume(&self, volume_id: &str, now: i64) -> anyhow::Result<usize> {
        let label: String = self
            .conn
            .query_row(
                "SELECT label FROM volumes WHERE volume_id=?1",
                params![volume_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| volume_id.to_string());
        let tx = self.conn.unchecked_transaction()?;
        let removed = self
            .conn
            .execute("DELETE FROM files WHERE volume_id=?1", params![volume_id])?;
        self.conn.execute(
            "DELETE FROM scan_errors WHERE volume_id=?1",
            params![volume_id],
        )?;
        self.conn
            .execute("DELETE FROM volumes WHERE volume_id=?1", params![volume_id])?;
        self.log_action(
            "forget",
            &serde_json::json!({
            "volume_id": volume_id, "label": label, "removed_files": removed })
            .to_string(),
            now,
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// Record the absolute path a volume was last scanned at (so a folder-drive can be recognized
    /// as connected later even though it isn't a disk mount root).
    pub fn set_volume_path(&self, volume_id: &str, path: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE volumes SET last_scanned_path=?2, last_seen_at=?3 WHERE volume_id=?1",
            params![volume_id, path, now],
        )?;
        Ok(())
    }

    /// (volume_id, last_scanned_path) for every volume that has a remembered path.
    pub fn volume_paths(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT volume_id, last_scanned_path FROM volumes WHERE last_scanned_path IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Set a volume's user display name and/or description. Each field updates independently:
    /// `None` leaves that column unchanged (partial update), `Some(s)` sets it — trimmed, with an
    /// empty-after-trim value clearing to NULL (which falls back to the detected label). Logs a
    /// `rename` audit action.
    pub fn set_volume_meta(
        &self,
        volume_id: &str,
        display_name: Option<&str>,
        description: Option<&str>,
        now: i64,
    ) -> anyhow::Result<()> {
        // None = leave unchanged; Some(s) = set (trim; empty clears to NULL / detected label).
        // A rename does NOT touch last_seen_at — it isn't a scan, and the Drives card renders
        // last_seen_at as "last scan", which must stay truthful.
        let to_val = |s: &str| -> Option<String> {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        if let Some(dn) = display_name {
            let v = to_val(dn);
            self.conn.execute(
                "UPDATE volumes SET display_name=?2 WHERE volume_id=?1",
                params![volume_id, v],
            )?;
        }
        if let Some(desc) = description {
            let v = to_val(desc);
            self.conn.execute(
                "UPDATE volumes SET description=?2 WHERE volume_id=?1",
                params![volume_id, v],
            )?;
        }
        self.log_action(
            "rename",
            &serde_json::json!({
            "volume_id": volume_id, "display_name": display_name, "description": description })
            .to_string(),
            now,
        )?;
        Ok(())
    }

    /// A volume's (display_name, description); both None if unset or the volume is unknown.
    pub fn volume_meta(&self, volume_id: &str) -> anyhow::Result<(Option<String>, Option<String>)> {
        let row = self.conn.query_row(
            "SELECT display_name, description FROM volumes WHERE volume_id=?1",
            params![volume_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match row {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((None, None)),
            Err(e) => Err(e.into()),
        }
    }

    /// volume_id → the name to show: the user display_name when set, else the detected label.
    pub fn effective_labels(&self) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT volume_id, COALESCE(NULLIF(display_name,''), label) FROM volumes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
    }

    /// Append an audit entry to actions_log.
    pub fn log_action(&self, action: &str, details_json: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO actions_log(action, details, occurred_at) VALUES (?1,?2,?3)",
            params![action, details_json, now],
        )?;
        Ok(())
    }

    /// The most recent `limit` audit entries, newest first: (action, details_json, occurred_at).
    pub fn recent_actions(&self, limit: usize) -> anyhow::Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT action, IFNULL(details,''), occurred_at FROM actions_log
             ORDER BY occurred_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn map_file_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get("id")?,
            volume_id: r.get("volume_id")?,
            relative_path: r.get("relative_path")?,
            filename: r.get("filename")?,
            extension: r.get("extension")?,
            size_bytes: r.get("size_bytes")?,
            content_hash: r.get("content_hash")?,
            created_time: r.get("created_time")?,
            modified_time: r.get("modified_time")?,
            accessed_time: r.get("accessed_time")?,
            category: Category::from_db(&r.get::<_, String>("category")?),
            container_chain: r.get("container_chain")?,
            status: FileStatus::from_db(&r.get::<_, String>("status")?),
            first_seen_at: r.get("first_seen_at")?,
            last_seen_at: r.get("last_seen_at")?,
            original_path: r.get("original_path")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::archive::ArchiveEntry;
    use crate::catalog::models::*;
    use crate::catalog::store::SearchFilters;
    use crate::catalog::Catalog;

    fn mk_entry(chain: &str, hash: &str) -> ArchiveEntry {
        ArchiveEntry {
            container_chain: chain.into(),
            filename: chain.rsplit(['/', '›']).next().unwrap().trim().into(),
            extension: "jpg".into(),
            size_bytes: 42,
            content_hash: hash.into(),
        }
    }

    fn mk_file(vol: &str, path: &str, hash: &str) -> NewFile {
        NewFile {
            volume_id: vol.into(),
            relative_path: path.into(),
            filename: path.rsplit('/').next().unwrap().into(),
            extension: "txt".into(),
            size_bytes: 10,
            content_hash: hash.into(),
            created_time: Some(1),
            modified_time: Some(2),
            accessed_time: Some(3),
            category: Category::Document,
            container_chain: None,
        }
    }

    fn open_tmp() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume {
            volume_id: "vol-1".into(),
            label: "Test HDD".into(),
            identified_by: "marker".into(),
            first_seen_at: 100,
            last_seen_at: 100,
        })
        .unwrap();
        (tmp, cat)
    }

    #[test]
    fn volume_last_seen_and_scan_errors() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "V".into(),
            identified_by: "marker".into(),
            first_seen_at: 5,
            last_seen_at: 42,
        })
        .unwrap();
        assert_eq!(cat.volume_last_seen("v").unwrap(), Some(42));
        assert_eq!(cat.volume_last_seen("nope").unwrap(), None);
        assert_eq!(cat.volume_has_scan_errors("v").unwrap(), false);
        cat.log_scan_error(Some("v"), "some/path", "permission denied", 9)
            .unwrap();
        assert_eq!(cat.volume_has_scan_errors("v").unwrap(), true);
    }

    #[test]
    fn upsert_is_idempotent_and_search_finds_it() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 250)
            .unwrap(); // same path again
        let hits = cat.search("thesis", None, None, None).unwrap();
        assert_eq!(hits.len(), 1); // one row, not two
        assert_eq!(hits[0].relative_path, "docs/thesis.txt");
    }

    #[test]
    fn duplicate_groups_counted_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200)
            .unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1);
    }

    #[test]
    fn duplicate_group_count_ignores_all_missing_groups() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "V".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        // Two files sharing a hash, both marked missing -> not a reviewable group.
        let mut f = crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: "a".into(),
            filename: "a".into(),
            extension: "".into(),
            size_bytes: 1,
            content_hash: "dup".into(),
            created_time: None,
            modified_time: None,
            accessed_time: None,
            category: crate::category::Category::Other,
            container_chain: None,
        };
        cat.upsert_file(&f, 1).unwrap();
        f.relative_path = "b".into();
        f.filename = "b".into();
        cat.upsert_file(&f, 1).unwrap();
        // Both rows have last_seen_at=1; a scan starting at 300 sweeps anything not seen this pass
        // (last_seen_at < 300) to missing. Signature: mark_missing_scanned(volume_id, scan_started_at, now).
        cat.mark_missing_scanned("v", 300, 300).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 0); // active-only: no reviewable groups
    }

    #[test]
    fn duplicate_counts_reports_only_multi_active_hashes() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "V".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        let mut f = crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: "a".into(),
            filename: "a".into(),
            extension: "".into(),
            size_bytes: 1,
            content_hash: "dup".into(),
            created_time: None,
            modified_time: None,
            accessed_time: None,
            category: crate::category::Category::Other,
            container_chain: None,
        };
        cat.upsert_file(&f, 1).unwrap(); // dup copy 1
        f.relative_path = "b".into();
        f.filename = "b".into();
        cat.upsert_file(&f, 1).unwrap(); // dup copy 2
        f.relative_path = "u".into();
        f.filename = "u".into();
        f.content_hash = "uniq".into();
        cat.upsert_file(&f, 1).unwrap(); // unique
        let m = cat
            .duplicate_counts(&["dup".to_string(), "uniq".to_string(), "absent".to_string()])
            .unwrap();
        assert_eq!(m.get("dup").copied(), Some(2));
        assert_eq!(m.get("uniq"), None); // single copy -> not duplicated
        assert_eq!(m.get("absent"), None); // not in catalog
    }

    #[test]
    fn mark_missing_flags_files_not_seen_this_scan() {
        let (_t, cat) = open_tmp();
        // seen in an earlier scan at t=200
        cat.upsert_file(&mk_file("vol-1", "gone.txt", "h1"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 200)
            .unwrap();
        // new scan starts at t=300; only kept.txt is re-seen
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 300)
            .unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        let missing = cat.search("gone", None, None, Some("missing")).unwrap();
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn volume_stats_counts_active_files() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "h1"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "h2"), 200)
            .unwrap();
        let stats = cat.volume_stats().unwrap();
        assert_eq!(stats.len(), 1);
        let (volume_id, label, count, bytes) = &stats[0];
        assert_eq!(volume_id, "vol-1");
        assert_eq!(label, "Test HDD");
        assert_eq!(*count, 2);
        assert_eq!(*bytes, 20); // 2 files * size_bytes 10
    }

    #[test]
    fn archive_entry_upsert_is_idempotent_and_searchable() {
        let (_t, cat) = open_tmp();
        let e = mk_entry("photos.zip › vacation.jpg", "h-vac");
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 200)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 250)
            .unwrap(); // same identity again
        let hits = cat.search("vacation", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].container_chain.as_deref(),
            Some("photos.zip › vacation.jpg")
        );
        assert_eq!(hits[0].relative_path, "backups/old.zip");
    }

    #[test]
    fn archive_entry_dedupes_against_loose_file_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "loose/vacation.jpg", "same"), 200)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("vacation.jpg", "same"), 200)
            .unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1); // loose + archived share a hash
    }

    #[test]
    fn missing_sweep_covers_archive_entries() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("gone.jpg", "h1"), 200)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 200)
            .unwrap();
        // rescan at 300 re-sees only kept.jpg
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 300)
            .unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            cat.search("gone", None, None, Some("missing"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn touch_archive_entries_refreshes_all_under_archive() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 200)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("b.jpg", "h2"), 200)
            .unwrap();
        let touched = cat.touch_archive_entries("vol-1", "old.zip", 300).unwrap();
        assert_eq!(touched, 2);
        // after touch, a later sweep starting at 300 does NOT mark them missing
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn search_filtered_applies_size_and_status() {
        let (_t, cat) = open_tmp();
        let mut small = mk_file("vol-1", "small.txt", "h1");
        small.size_bytes = 10;
        let mut big = mk_file("vol-1", "big.txt", "h2");
        big.size_bytes = 5000;
        cat.upsert_file(&small, 200).unwrap();
        cat.upsert_file(&big, 200).unwrap();

        let f = SearchFilters {
            min_size: Some(1000),
            ..Default::default()
        };
        let hits = cat.search_filtered(&f, 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].filename, "big.txt");
    }

    #[test]
    fn search_filtered_empty_query_returns_all_filtered() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "h1"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "h2"), 200)
            .unwrap();
        let hits = cat.search_filtered(&SearchFilters::default(), 100).unwrap();
        assert_eq!(hits.len(), 2); // empty query = browse all
    }

    #[test]
    fn search_tolerates_fts_special_chars() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "docs/report(final).pdf", "h1"), 200)
            .unwrap();

        let hits = cat.search("report(final)", None, None, None);
        assert!(hits.is_ok(), "special-char query must not error: {hits:?}");
        assert_eq!(hits.unwrap().len(), 1);

        let lone_quote = cat.search("\"", None, None, None);
        assert!(
            lone_quote.is_ok(),
            "lone quote query must not error: {lone_quote:?}"
        );
    }

    #[test]
    fn duplicate_groups_lists_members() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200)
            .unwrap();
        let groups = cat.duplicate_groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn reclaimable_bytes_by_volume_excludes_the_keep() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&Volume {
            volume_id: "v".into(),
            label: "V".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        let mut f = NewFile {
            volume_id: "v".into(),
            relative_path: "a.bin".into(),
            filename: "a.bin".into(),
            extension: "bin".into(),
            size_bytes: 100,
            content_hash: "dup".into(),
            created_time: Some(10),
            modified_time: Some(10),
            accessed_time: None,
            category: Category::Other,
            container_chain: None,
        };
        cat.upsert_file(&f, 1).unwrap(); // keep (created 10)
        f.relative_path = "b.bin".into();
        f.filename = "b.bin".into();
        f.created_time = Some(20); // newer duplicate -> reclaimable
        cat.upsert_file(&f, 1).unwrap();
        f.relative_path = "u.bin".into();
        f.filename = "u.bin".into();
        f.content_hash = "uniq".into();
        f.size_bytes = 999; // unique -> not counted
        cat.upsert_file(&f, 1).unwrap();
        let map = cat.reclaimable_bytes_by_volume().unwrap();
        assert_eq!(map.get("v").copied().unwrap_or(0), 100); // only the non-keep duplicate
    }

    #[test]
    fn active_copies_returns_active_rows_for_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200)
            .unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200)
            .unwrap();
        assert_eq!(cat.active_copies("same").unwrap().len(), 2);
        assert_eq!(cat.active_copies("unique").unwrap().len(), 1);
        assert_eq!(cat.active_copies("none").unwrap().len(), 0);
    }

    #[test]
    fn quarantine_then_purge_transitions_and_recoverable() {
        let (_t, cat) = open_tmp();
        let mut f = mk_file("vol-1", "Photos/a.jpg", "h");
        f.size_bytes = 2048;
        cat.upsert_file(&f, 200).unwrap();
        let id = cat.loose_file_id("vol-1", "Photos/a.jpg").unwrap().unwrap();

        cat.mark_quarantined(id, "_ToDelete/Photos/a.jpg", "Photos/a.jpg", 300)
            .unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.relative_path, "_ToDelete/Photos/a.jpg");
        assert_eq!(rec.original_path.as_deref(), Some("Photos/a.jpg"));
        assert_eq!(cat.recoverable_bytes("vol-1").unwrap(), 2048);
        assert_eq!(cat.quarantined_rows("vol-1").unwrap().len(), 1);

        cat.mark_purged(id, 400).unwrap();
        assert_eq!(
            cat.get_file(id).unwrap().unwrap().status,
            FileStatus::Purged
        );
        assert_eq!(cat.recoverable_bytes("vol-1").unwrap(), 0);
    }

    #[test]
    fn log_action_appends() {
        let (_t, cat) = open_tmp();
        cat.log_action("quarantine", "{\"file_id\":1}", 100)
            .unwrap();
        cat.log_action("purge", "{\"volume_id\":\"v\"}", 200)
            .unwrap();
        let n: i64 = cat
            .conn
            .query_row("SELECT count(*) FROM actions_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn recent_actions_returns_newest_first() {
        let (_t, cat) = open_tmp();
        cat.log_action("quarantine", "{\"file_id\":1}", 100)
            .unwrap();
        cat.log_action("purge", "{\"volume_id\":\"v\"}", 200)
            .unwrap();
        let rows = cat.recent_actions(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "purge"); // newest first
        assert_eq!(rows[0].2, 200);
        assert_eq!(rows[1].0, "quarantine");
        // limit is respected
        assert_eq!(cat.recent_actions(1).unwrap().len(), 1);
    }

    #[test]
    fn touch_does_not_resurrect_missing_archive_entries() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 200)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("gone.jpg", "h2"), 200)
            .unwrap();
        // rescan at 300 re-sees only a.jpg -> gone.jpg swept to missing
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 300)
            .unwrap();
        cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(
            cat.search("gone", None, None, Some("missing"))
                .unwrap()
                .len(),
            1
        );
        // a later incremental-skip touch must NOT resurrect gone.jpg
        cat.touch_archive_entries("vol-1", "old.zip", 400).unwrap();
        assert_eq!(
            cat.search("gone", None, None, Some("missing"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            cat.search("gone", None, None, Some("active"))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn archive_entries_lists_only_that_archives_entries() {
        let (_t, cat) = open_tmp();
        let e = |chain: &str, hash: &str| crate::archive::ArchiveEntry {
            container_chain: chain.into(),
            filename: chain.rsplit('/').next().unwrap().into(),
            extension: "jpg".into(),
            size_bytes: 5,
            content_hash: hash.into(),
        };
        cat.upsert_archive_entry("vol-1", "a.zip", &e("x.jpg", "h1"), 100)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "a.zip", &e("y.jpg", "h2"), 100)
            .unwrap();
        cat.upsert_archive_entry("vol-1", "b.zip", &e("z.jpg", "h3"), 100)
            .unwrap();
        let es = cat.archive_entries("vol-1", "a.zip").unwrap();
        assert_eq!(es.len(), 2);
        assert!(es
            .iter()
            .all(|r| r.relative_path == "a.zip" && r.container_chain.is_some()));
    }

    #[test]
    fn mark_quarantined_clears_container_chain() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry(
            "vol-1",
            "a.zip",
            &crate::archive::ArchiveEntry {
                container_chain: "x.jpg".into(),
                filename: "x.jpg".into(),
                extension: "jpg".into(),
                size_bytes: 5,
                content_hash: "h1".into(),
            },
            100,
        )
        .unwrap();
        // find the entry row id
        let id = cat.archive_entries("vol-1", "a.zip").unwrap()[0].id;
        cat.mark_quarantined(id, "_ToDelete/a.zip/x.jpg", "a.zip › x.jpg", 200)
            .unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.status, FileStatus::Quarantined);
        assert_eq!(rec.container_chain, None); // now a loose quarantined row
        assert_eq!(rec.relative_path, "_ToDelete/a.zip/x.jpg");
        assert_eq!(rec.original_path.as_deref(), Some("a.zip › x.jpg"));
    }

    #[test]
    fn default_search_hides_purged_rows_but_status_filter_shows_them() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "D".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        let mk = |name: &str| crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: name.into(),
            filename: name.into(),
            extension: "txt".into(),
            size_bytes: 1,
            content_hash: format!("h-{name}"),
            created_time: None,
            modified_time: None,
            accessed_time: None,
            category: crate::category::Category::Other,
            container_chain: None,
        };
        cat.upsert_file(&mk("keep.txt"), 1).unwrap();
        cat.upsert_file(&mk("_ToDelete/gone.txt"), 1).unwrap();
        let gone = cat
            .loose_file_id("v", "_ToDelete/gone.txt")
            .unwrap()
            .unwrap();
        cat.mark_purged(gone, 200).unwrap();

        // Default browse (no status filter) must not show the purged `_ToDelete` row.
        let def = cat.search("", None, None, None).unwrap();
        assert_eq!(def.len(), 1);
        assert_eq!(def[0].relative_path, "keep.txt");

        // Explicitly asking for purged still surfaces them (audit view).
        let purged = cat.search("", None, None, Some("purged")).unwrap();
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].relative_path, "_ToDelete/gone.txt");
    }

    #[test]
    fn forget_volume_deletes_rows_and_fts_but_returns_count() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "Gone".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        let f = crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: "a.txt".into(),
            filename: "a.txt".into(),
            extension: "txt".into(),
            size_bytes: 1,
            content_hash: "h".into(),
            created_time: None,
            modified_time: None,
            accessed_time: None,
            category: crate::category::Category::Other,
            container_chain: None,
        };
        cat.upsert_file(&f, 1).unwrap();
        let removed = cat.forget_volume("v", 500).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(cat.volume_last_seen("v").unwrap(), None); // volume row gone
        assert!(cat.search("a", None, None, None).unwrap().is_empty()); // FTS row gone
        assert!(cat
            .recent_actions(5)
            .unwrap()
            .iter()
            .any(|(a, _, _)| a == "forget"));
    }

    #[test]
    fn volume_path_and_meta_round_trip() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "Detected".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        // path
        cat.set_volume_path("v", "/some/folder", 5).unwrap();
        assert_eq!(
            cat.volume_paths().unwrap(),
            vec![("v".to_string(), "/some/folder".to_string())]
        );
        // meta: set name + description
        cat.set_volume_meta("v", Some("My Photos"), Some("holiday pics"), 6)
            .unwrap();
        assert_eq!(
            cat.volume_meta("v").unwrap(),
            (
                Some("My Photos".to_string()),
                Some("holiday pics".to_string())
            )
        );
        assert_eq!(
            cat.effective_labels().unwrap().get("v").cloned(),
            Some("My Photos".to_string())
        );
        // clearing the name (empty) falls back to the detected label
        cat.set_volume_meta("v", Some("  "), None, 7).unwrap();
        assert_eq!(cat.volume_meta("v").unwrap().0, None);
        assert_eq!(
            cat.effective_labels().unwrap().get("v").cloned(),
            Some("Detected".to_string())
        );
    }

    #[test]
    fn set_volume_meta_partial_update_preserves_other_field() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(),
            label: "Detected".into(),
            identified_by: "marker".into(),
            first_seen_at: 1,
            last_seen_at: 1,
        })
        .unwrap();
        cat.set_volume_meta("v", Some("My Name"), Some("my desc"), 5)
            .unwrap();
        // Update only the description (name = None) -> name must survive.
        cat.set_volume_meta("v", None, Some("new desc"), 6).unwrap();
        assert_eq!(
            cat.volume_meta("v").unwrap(),
            (Some("My Name".to_string()), Some("new desc".to_string()))
        );
        // Update only the name -> description survives.
        cat.set_volume_meta("v", Some("Name2"), None, 7).unwrap();
        assert_eq!(
            cat.volume_meta("v").unwrap(),
            (Some("Name2".to_string()), Some("new desc".to_string()))
        );
        // Explicit clear of the name (empty) falls back to the label; description untouched.
        cat.set_volume_meta("v", Some(""), None, 8).unwrap();
        assert_eq!(cat.volume_meta("v").unwrap().0, None);
        assert_eq!(
            cat.effective_labels().unwrap().get("v").cloned(),
            Some("Detected".to_string())
        );
    }

    #[test]
    fn update_archive_hash_changes_hash_and_size() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.zip", "OLD"), 100)
            .unwrap();
        let id = cat.loose_file_id("vol-1", "a.zip").unwrap().unwrap();
        cat.update_archive_hash(id, "NEW", 999, 200).unwrap();
        let rec = cat.get_file(id).unwrap().unwrap();
        assert_eq!(rec.content_hash, "NEW");
        assert_eq!(rec.size_bytes, 999);
    }
}

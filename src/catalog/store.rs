use crate::catalog::Catalog;
use crate::catalog::models::*;
use rusqlite::params;

impl Catalog {
    pub fn upsert_volume(&self, v: &Volume) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO volumes(volume_id, label, identified_by, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id) DO UPDATE SET label=excluded.label,
                 identified_by=excluded.identified_by, last_seen_at=excluded.last_seen_at",
            params![v.volume_id, v.label, v.identified_by, v.first_seen_at, v.last_seen_at],
        )?;
        Ok(())
    }

    /// (size_bytes, modified_time-or-0) for a loose file, if catalogued.
    pub fn get_file_meta(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<(i64, i64)>> {
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
            params![f.volume_id, f.relative_path, f.filename, f.extension, f.size_bytes,
                f.content_hash, f.created_time, f.modified_time, f.accessed_time,
                f.category.as_str(), f.container_chain, now],
        )?;
        Ok(())
    }

    /// Refresh last_seen/status for an unchanged file without re-hashing. Returns true if a row matched.
    pub fn touch_seen(&self, volume_id: &str, relative_path: &str, now: i64) -> anyhow::Result<bool> {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            rusqlite::params![volume_id, relative_path, now],
        )?;
        Ok(n > 0)
    }

    /// Flag active files (loose or archived) on this volume not touched by the current scan as missing.
    pub fn mark_missing_scanned(&self, volume_id: &str, scan_started_at: i64, _now: i64) -> anyhow::Result<usize> {
        let n = self.conn.execute(
            "UPDATE files SET status='missing'
             WHERE volume_id=?1 AND status='active' AND last_seen_at < ?2",
            params![volume_id, scan_started_at],
        )?;
        Ok(n)
    }

    /// Insert/update one archive entry (a file inside an archive). Identity is
    /// (volume_id, archive_rel_path, container_chain) via idx_files_archived_identity.
    pub fn upsert_archive_entry(&self, volume_id: &str, archive_rel_path: &str,
        e: &crate::archive::ArchiveEntry, now: i64) -> anyhow::Result<()>
    {
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
            params![volume_id, archive_rel_path, e.filename, e.extension, e.size_bytes,
                e.content_hash, Category::from_extension(&e.extension).as_str(), e.container_chain, now],
        )?;
        Ok(())
    }

    /// Refresh last_seen/status for every archive entry under one archive file (unchanged-archive skip).
    pub fn touch_archive_entries(&self, volume_id: &str, archive_rel_path: &str, now: i64)
        -> anyhow::Result<usize>
    {
        let n = self.conn.execute(
            "UPDATE files SET last_seen_at=?3, status='active'
             WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NOT NULL AND status='active'",
            params![volume_id, archive_rel_path, now],
        )?;
        Ok(n)
    }

    pub fn log_scan_error(&self, volume_id: Option<&str>, path: &str, reason: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO scan_errors(volume_id, path, reason, occurred_at) VALUES (?1,?2,?3,?4)",
            params![volume_id, path, reason, now],
        )?;
        Ok(())
    }

    pub fn duplicate_group_count(&self) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT count(*) FROM (SELECT content_hash FROM files
                 WHERE status IN ('active','missing') GROUP BY content_hash HAVING count(*) > 1)",
            [], |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn volume_stats(&self) -> anyhow::Result<Vec<(String, String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.volume_id, v.label,
                    count(f.id) FILTER (WHERE f.status='active'),
                    IFNULL(sum(f.size_bytes) FILTER (WHERE f.status='active'),0)
             FROM volumes v LEFT JOIN files f ON f.volume_id=v.volume_id
             GROUP BY v.volume_id, v.label ORDER BY v.label",
        )?;
        let rows = stmt.query_map([], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?,
        )))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Search by free text plus optional filters. Empty query returns all (filtered).
    pub fn search(&self, query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>)
        -> anyhow::Result<Vec<FileRecord>>
    {
        let mut sql = String::from(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at FROM files WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            sql.push_str(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)");
            // FTS prefix match on each token
            let match_expr = q.split_whitespace().map(|t| format!("{t}*")).collect::<Vec<_>>().join(" ");
            args.push(Box::new(match_expr));
        }
        if let Some(c) = category { sql.push_str(" AND category = ?"); args.push(Box::new(c.to_string())); }
        if let Some(v) = volume { sql.push_str(" AND volume_id = ?"); args.push(Box::new(v.to_string())); }
        if let Some(s) = status { sql.push_str(" AND status = ?"); args.push(Box::new(s.to_string())); }
        sql.push_str(" ORDER BY relative_path LIMIT 1000");

        let mut stmt = self.conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), Self::map_file_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn map_file_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get(0)?,
            volume_id: r.get(1)?,
            relative_path: r.get(2)?,
            filename: r.get(3)?,
            extension: r.get(4)?,
            size_bytes: r.get(5)?,
            content_hash: r.get(6)?,
            created_time: r.get(7)?,
            modified_time: r.get(8)?,
            accessed_time: r.get(9)?,
            category: Category::from_db(&r.get::<_, String>(10)?),
            container_chain: r.get(11)?,
            status: FileStatus::from_db(&r.get::<_, String>(12)?),
            first_seen_at: r.get(13)?,
            last_seen_at: r.get(14)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;
    use crate::catalog::models::*;
    use crate::archive::ArchiveEntry;

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
            volume_id: "vol-1".into(), label: "Test HDD".into(),
            identified_by: "marker".into(), first_seen_at: 100, last_seen_at: 100,
        }).unwrap();
        (tmp, cat)
    }

    #[test]
    fn upsert_is_idempotent_and_search_finds_it() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "docs/thesis.txt", "hashA"), 250).unwrap(); // same path again
        let hits = cat.search("thesis", None, None, None).unwrap();
        assert_eq!(hits.len(), 1); // one row, not two
        assert_eq!(hits[0].relative_path, "docs/thesis.txt");
    }

    #[test]
    fn duplicate_groups_counted_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "same"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "c.txt", "unique"), 200).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1);
    }

    #[test]
    fn mark_missing_flags_files_not_seen_this_scan() {
        let (_t, cat) = open_tmp();
        // seen in an earlier scan at t=200
        cat.upsert_file(&mk_file("vol-1", "gone.txt", "h1"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 200).unwrap();
        // new scan starts at t=300; only kept.txt is re-seen
        cat.upsert_file(&mk_file("vol-1", "kept.txt", "h2"), 300).unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        let missing = cat.search("gone", None, None, Some("missing")).unwrap();
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn volume_stats_counts_active_files() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "h1"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "h2"), 200).unwrap();
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
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 200).unwrap();
        cat.upsert_archive_entry("vol-1", "backups/old.zip", &e, 250).unwrap(); // same identity again
        let hits = cat.search("vacation", None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].container_chain.as_deref(), Some("photos.zip › vacation.jpg"));
        assert_eq!(hits[0].relative_path, "backups/old.zip");
    }

    #[test]
    fn archive_entry_dedupes_against_loose_file_by_hash() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "loose/vacation.jpg", "same"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("vacation.jpg", "same"), 200).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 1); // loose + archived share a hash
    }

    #[test]
    fn missing_sweep_covers_archive_entries() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("gone.jpg", "h1"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 200).unwrap();
        // rescan at 300 re-sees only kept.jpg
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("kept.jpg", "h2"), 300).unwrap();
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 1);
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
    }

    #[test]
    fn touch_archive_entries_refreshes_all_under_archive() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("b.jpg", "h2"), 200).unwrap();
        let touched = cat.touch_archive_entries("vol-1", "old.zip", 300).unwrap();
        assert_eq!(touched, 2);
        // after touch, a later sweep starting at 300 does NOT mark them missing
        let n = cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn touch_does_not_resurrect_missing_archive_entries() {
        let (_t, cat) = open_tmp();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 200).unwrap();
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("gone.jpg", "h2"), 200).unwrap();
        // rescan at 300 re-sees only a.jpg -> gone.jpg swept to missing
        cat.upsert_archive_entry("vol-1", "old.zip", &mk_entry("a.jpg", "h1"), 300).unwrap();
        cat.mark_missing_scanned("vol-1", 300, 300).unwrap();
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
        // a later incremental-skip touch must NOT resurrect gone.jpg
        cat.touch_archive_entries("vol-1", "old.zip", 400).unwrap();
        assert_eq!(cat.search("gone", None, None, Some("missing")).unwrap().len(), 1);
        assert_eq!(cat.search("gone", None, None, Some("active")).unwrap().len(), 0);
    }
}

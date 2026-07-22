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

use crate::catalog::Catalog;
use std::collections::HashMap;

/// Headline and review-list totals. The `*_all` fields ignore the floor by design — a UI filter
/// must never change the number the user is told is reclaimable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateTotals {
    pub groups: i64,
    pub reclaimable_bytes: i64,
    pub groups_all: i64,
    pub reclaimable_all_bytes: i64,
    pub archive_locked_bytes: i64,
}

/// One duplicate group. No member rows are loaded — members are fetched per page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub content_hash: String,
    pub copies: i64,
    pub size_bytes: i64,
    pub reclaimable_bytes: i64,
}

/// Position in the ranked list. A cursor rather than an offset: quarantining re-ranks the list, and
/// an offset would silently skip groups the user believes they already reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCursor {
    pub reclaimable_bytes: i64,
    pub content_hash: String,
}

/// A member of a duplicate group, with the keep flag decided by `KEEP_ORDER`.
#[derive(Debug, Clone)]
pub struct DuplicateMember {
    pub record: crate::catalog::models::FileRecord,
    pub is_suggested_keep: bool,
}

impl Catalog {
    /// Aggregate duplicate figures. Uses a plain `GROUP BY` over `idx_files_hash` (no window
    /// function), so the Overview/Drives path stays cheap on large catalogs.
    pub fn duplicate_totals(&self, min_size: i64) -> anyhow::Result<DuplicateTotals> {
        // (groups, reclaimable) for loose duplicates at or above `floor`.
        let loose = |floor: i64| -> anyhow::Result<(i64, i64)> {
            Ok(self.conn.query_row(
                "SELECT COUNT(*), IFNULL(SUM((c - 1) * s), 0) FROM (
                     SELECT COUNT(*) AS c, MIN(size_bytes) AS s
                     FROM files
                     WHERE status = 'active' AND container_chain IS NULL AND size_bytes >= ?1
                     GROUP BY content_hash HAVING COUNT(*) > 1)",
                rusqlite::params![floor],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        };
        let (groups, reclaimable_bytes) = loose(min_size)?;
        let (groups_all, reclaimable_all_bytes) = loose(0)?;

        // Computed independently over archived rows. NOT the complement of the loose figure: a hash
        // can appear both loose and archived, so these must never be summed into one total.
        let archive_locked_bytes: i64 = self.conn.query_row(
            "SELECT IFNULL(SUM((c - 1) * s), 0) FROM (
                 SELECT COUNT(*) AS c, MIN(size_bytes) AS s
                 FROM files
                 WHERE status = 'active' AND container_chain IS NOT NULL
                 GROUP BY content_hash HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )?;

        Ok(DuplicateTotals {
            groups,
            reclaimable_bytes,
            groups_all,
            reclaimable_all_bytes,
            archive_locked_bytes,
        })
    }

    /// Bytes each volume would free if every non-kept loose copy were quarantined. Floor-free: this
    /// is the honest headline figure, not the filtered review list.
    pub fn reclaimable_by_volume(&self) -> anyhow::Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT volume_id, IFNULL(SUM(size_bytes), 0) FROM dup_loose
             WHERE rn > 1 GROUP BY volume_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// Loose duplicate groups at or above `min_size`, ranked by reclaimable space descending.
    /// Pass the previous page's last group as `after` to get the next page.
    pub fn duplicate_groups_ranked(
        &self,
        min_size: i64,
        limit: usize,
        after: Option<&DuplicateCursor>,
    ) -> anyhow::Result<Vec<DuplicateGroup>> {
        let (ar, ah) = match after {
            Some(c) => (c.reclaimable_bytes, c.content_hash.as_str()),
            // i64::MAX sorts before every real value, so "no cursor" means "from the top".
            None => (i64::MAX, ""),
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT content_hash, copies, size_bytes, reclaimable FROM (
                 SELECT content_hash,
                        COUNT(*)                         AS copies,
                        MIN(size_bytes)                  AS size_bytes,
                        (COUNT(*) - 1) * MIN(size_bytes) AS reclaimable
                 FROM files
                 WHERE status = 'active' AND container_chain IS NULL AND size_bytes >= ?1
                 GROUP BY content_hash HAVING COUNT(*) > 1)
             WHERE reclaimable < ?2 OR (reclaimable = ?2 AND content_hash > ?3)
             ORDER BY reclaimable DESC, content_hash ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(rusqlite::params![min_size, ar, ah, limit as i64], |r| {
            Ok(DuplicateGroup {
                content_hash: r.get(0)?,
                copies: r.get(1)?,
                size_bytes: r.get(2)?,
                reclaimable_bytes: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Members of the given groups, keyed by content hash, each flagged with whether it is the
    /// suggested keep. The window runs only over the page's hashes, not the whole table.
    pub fn duplicate_members_for(
        &self,
        hashes: &[String],
    ) -> anyhow::Result<HashMap<String, Vec<DuplicateMember>>> {
        let mut out: HashMap<String, Vec<DuplicateMember>> = HashMap::new();
        if hashes.is_empty() {
            return Ok(out);
        }
        let holders = vec!["?"; hashes.len()].join(",");
        let sql = format!(
            "SELECT {cols}, ROW_NUMBER() OVER (PARTITION BY content_hash ORDER BY {order}) AS rn
             FROM files
             WHERE status = 'active' AND container_chain IS NULL AND content_hash IN ({holders})
             ORDER BY content_hash, rn",
            cols = crate::catalog::store::FILE_COLUMNS,
            order = KEEP_ORDER,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let args: Vec<&dyn rusqlite::types::ToSql> = hashes
            .iter()
            .map(|h| h as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(args.as_slice(), |r| {
            let record = Catalog::map_file_record(r)?;
            let rn: i64 = r.get("rn")?;
            Ok(DuplicateMember {
                record,
                is_suggested_keep: rn == 1,
            })
        })?;
        for m in rows {
            let m = m?;
            out.entry(m.record.content_hash.clone())
                .or_default()
                .push(m);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;

    /// Two loose copies of A (100 B), three loose copies of B (10 B), one unique file,
    /// plus two archived copies of C (50 B).
    fn seed() -> (tempfile::TempDir, Catalog) {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.conn
            .execute_batch(
                "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
                     VALUES ('v1','V1','marker',1,1),('v2','V2','marker',1,1);
                 INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,
                     content_hash,created_time,modified_time,accessed_time,category,
                     container_chain,status,first_seen_at,last_seen_at) VALUES
                 ('v1','a1','a1','t',100,'A',100,100,NULL,'other',NULL,'active',1,1),
                 ('v2','a2','a2','t',100,'A',200,200,NULL,'other',NULL,'active',1,1),
                 ('v1','b1','b1','t', 10,'B',100,100,NULL,'other',NULL,'active',1,1),
                 ('v1','b2','b2','t', 10,'B',200,200,NULL,'other',NULL,'active',1,1),
                 ('v2','b3','b3','t', 10,'B',300,300,NULL,'other',NULL,'active',1,1),
                 ('v1','u1','u1','t',999,'U',100,100,NULL,'other',NULL,'active',1,1),
                 ('v1','z.zip','c1','t', 50,'C',100,100,NULL,'other','c1','active',1,1),
                 ('v1','z2.zip','c2','t',50,'C',100,100,NULL,'other','c2','active',1,1);",
            )
            .unwrap();
        (t, cat)
    }

    #[test]
    fn totals_are_loose_only_and_the_floor_never_touches_the_headline() {
        let (_t, cat) = seed();
        let tot = cat.duplicate_totals(0).unwrap();
        // loose duplicates: A (2 copies x 100 -> 100 reclaimable), B (3 x 10 -> 20)
        assert_eq!(tot.groups_all, 2);
        assert_eq!(tot.reclaimable_all_bytes, 120);
        assert_eq!(tot.groups, 2);
        assert_eq!(tot.reclaimable_bytes, 120);
        // archived duplicates are reported separately, never folded into the headline
        assert_eq!(
            tot.archive_locked_bytes, 50,
            "one redundant archived copy of C"
        );

        // a floor hides small groups from the review list but leaves the headline intact
        let tot = cat.duplicate_totals(50).unwrap();
        assert_eq!(tot.groups, 1, "only A survives a 50-byte floor");
        assert_eq!(tot.reclaimable_bytes, 100);
        assert_eq!(tot.groups_all, 2, "headline must ignore the floor");
        assert_eq!(
            tot.reclaimable_all_bytes, 120,
            "headline must ignore the floor"
        );
    }

    #[test]
    fn reclaimable_by_volume_attributes_to_the_non_kept_copies() {
        let (_t, cat) = seed();
        let m = cat.reclaimable_by_volume().unwrap();
        // A: keep v1 (older) -> v2 owes 100. B: keep v1/b1 -> v1 owes 10 (b2), v2 owes 10 (b3).
        assert_eq!(m.get("v1").copied().unwrap_or(0), 10);
        assert_eq!(m.get("v2").copied().unwrap_or(0), 110);
    }

    #[test]
    fn groups_are_ranked_by_reclaimable_and_respect_the_floor() {
        let (_t, cat) = seed();
        let g = cat.duplicate_groups_ranked(0, 10, None).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].content_hash, "A", "biggest reclaimable first");
        assert_eq!(g[0].reclaimable_bytes, 100);
        assert_eq!(g[0].copies, 2);
        assert_eq!(g[1].content_hash, "B");
        assert_eq!(g[1].reclaimable_bytes, 20);

        let g = cat.duplicate_groups_ranked(50, 10, None).unwrap();
        assert_eq!(g.len(), 1, "floor removes B");
        assert_eq!(g[0].content_hash, "A");
    }

    #[test]
    fn cursor_paging_is_stable_and_does_not_repeat_or_skip() {
        let (_t, cat) = seed();
        let first = cat.duplicate_groups_ranked(0, 1, None).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].content_hash, "A");

        let cur = super::DuplicateCursor {
            reclaimable_bytes: first[0].reclaimable_bytes,
            content_hash: first[0].content_hash.clone(),
        };
        let second = cat.duplicate_groups_ranked(0, 10, Some(&cur)).unwrap();
        assert_eq!(second.len(), 1, "only B remains after the cursor");
        assert_eq!(second[0].content_hash, "B");

        // paging again from the same cursor after the first group is gone must not resurface A
        cat.conn
            .execute(
                "UPDATE files SET status='quarantined' WHERE content_hash='A'",
                [],
            )
            .unwrap();
        let again = cat.duplicate_groups_ranked(0, 10, Some(&cur)).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].content_hash, "B",
            "mutation must not skip or repeat groups"
        );
    }

    #[test]
    fn members_carry_the_suggested_keep_flag() {
        let (_t, cat) = seed();
        let m = cat
            .duplicate_members_for(&["A".to_string(), "B".to_string()])
            .unwrap();
        let a = &m["A"];
        assert_eq!(a.len(), 2);
        let keep: Vec<_> = a.iter().filter(|x| x.is_suggested_keep).collect();
        assert_eq!(keep.len(), 1, "exactly one keep per group");
        assert_eq!(keep[0].record.relative_path, "a1", "oldest copy is kept");
        assert_eq!(m["B"].iter().filter(|x| x.is_suggested_keep).count(), 1);
    }

    #[test]
    fn members_for_an_empty_hash_list_is_empty_not_an_error() {
        let (_t, cat) = seed();
        assert!(cat.duplicate_members_for(&[]).unwrap().is_empty());
    }
}

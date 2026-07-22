# Honest, Ranked, Bounded Duplicate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make duplicate review usable and honest at 1.8M+ files — reclaimable counts only what quarantine can actually reclaim, groups are ranked by reclaimable space and cursor-paged, and a 1 MiB review floor hides the noise while always stating what it hides.

**Architecture:** One SQL keep-ordering definition (`KEEP_ORDER`) is the single source of truth, used by both the `dup_loose` view (whole-table keep attribution) and the per-page members query. New aggregate/paged catalog methods replace the in-memory `duplicate_groups()` loader, which is deleted along with `duplicate_group_count()` and the old `reclaimable_bytes_by_volume()`.

**Tech Stack:** Rust, rusqlite (bundled SQLite, window functions), axum, plain HTML/CSS/JS.

**Spec:** [docs/superpowers/specs/2026-07-22-duplicate-review-scale-design.md](../specs/2026-07-22-duplicate-review-scale-design.md)

**Two deliberate deviations from the spec's Rust sketch** (the observable behaviour and the JSON are
unchanged — the spec's own HTTP section already requires both):

1. `duplicate_group_members(&self, content_hash)` becomes **`duplicate_members_for(&self, hashes: &[String])`**,
   one batched query per page. The spec's HTTP section mandates "members for the page fetched in ONE
   query"; a per-hash method would be 50 round-trips.
2. `suggested_keep_id` is **not** a field on `DuplicateGroup`. Computing it there needs a correlated
   subquery against `dup_loose`, which re-evaluates a whole-table window function once per group — 50×
   per page over 1.7M rows. It comes from the members query instead (`rn = 1`), which windows only over
   the page's hashes, and is still attached to each group in the JSON exactly as the spec specifies.

## Global Constraints

- **One implementation, no compatibility path.** The end state must contain no `duplicate_groups()`, no `duplicate_group_count()`, no in-memory `reclaimable_bytes_by_volume()`. Breaking the API/CLI shape is expected and fine.
- **Keep-ordering is defined exactly once** as the Rust constant `KEEP_ORDER` and interpolated into every SQL site that needs it. Never re-type the ORDER BY.
- `KEEP_ORDER` = `IFNULL(created_time, 9223372036854775807), IFNULL(modified_time, 9223372036854775807), id` — oldest created, then modified, then id, **NULLs last** (SQLite sorts NULLs first by default; the `IFNULL` is what preserves the existing Rust behaviour).
- **Floor is query-time only.** `min_size` is in **bytes**, default `1_048_576` (1 MiB). The catalog keeps every row; scanning is unchanged.
- **Floor never affects the headline.** `groups_all` / `reclaimable_all_bytes` are always floor-free.
- **Reclaimable is loose-only** (`container_chain IS NULL`). `archive_locked_bytes` is computed **independently** over `container_chain IS NOT NULL` — the two are *not* complements and must never be summed into one "total".
- **Paging is cursor-based**, ordered `reclaimable_bytes DESC, content_hash ASC`. Never offset.
- Reliability constraint unchanged: no change to quarantine/purge/repack semantics.
- Conventional Commits; both trailers:
  `Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Every task ends green: `cargo test --release`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Branch `feat/duplicate-review-scale`. Do not merge, tag or push.

## File structure

| File | Responsibility in this change |
| --- | --- |
| `src/catalog/schema.rs` | Adds the `dup_loose` view, built from `KEEP_ORDER`. |
| `src/catalog/dedup.rs` **(new)** | All duplicate/reclaimable queries + their types. Keeps `store.rs` from growing further; this is one cohesive responsibility. |
| `src/catalog/mod.rs` | Declares the new `dedup` module. |
| `src/catalog/store.rs` | Old duplicate/reclaimable methods **deleted** (Task 5). |
| `src/web.rs` | `api_duplicates` rewritten; `api_drives`/`api_stats` use the new totals. |
| `src/web_ui.rs` | Duplicates page: totals header, floor control, ranked cursor paging, "up next". |
| `src/commands.rs` | `cmd_duplicates` ranked + floored + limited; `cmd_status` honest totals. |

---

### Task 1: `dup_loose` view + `KEEP_ORDER` single definition

**Files:**
- Modify: `src/catalog/schema.rs`
- Create: `src/catalog/dedup.rs`
- Modify: `src/catalog/mod.rs`

**Interfaces:**
- Produces: `pub const KEEP_ORDER: &str` in `dedup.rs`; the `dup_loose` view with columns `id, volume_id, content_hash, size_bytes, rn, copies`. Tasks 2–5 rely on both.

- [ ] **Step 1: Create `src/catalog/dedup.rs` with the single ordering definition**

```rust
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
```

- [ ] **Step 2: Register the module**

In `src/catalog/mod.rs`, add alongside the existing module declarations:

```rust
pub mod dedup;
```

- [ ] **Step 3: Write the failing test for the view**

Append to `src/catalog/schema.rs`'s test module (create `#[cfg(test)] mod tests { use super::*; ... }` if absent):

```rust
#[test]
fn dup_loose_view_exists_and_flags_keep_and_copies() {
    let conn = Connection::open_in_memory().unwrap();
    apply(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
             VALUES ('v','V','marker',1,1);
         INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,content_hash,
             created_time,modified_time,accessed_time,category,container_chain,status,
             first_seen_at,last_seen_at)
         VALUES ('v','new.txt','new.txt','txt',10,'H',200,200,NULL,'other',NULL,'active',1,1),
                ('v','old.txt','old.txt','txt',10,'H',100,100,NULL,'other',NULL,'active',1,1),
                ('v','solo.txt','solo.txt','txt',10,'U',100,100,NULL,'other',NULL,'active',1,1),
                ('v','in.zip','x.txt','txt',10,'H',100,100,NULL,'other','x.txt','active',1,1);",
    )
    .unwrap();

    // the older of the two 'H' rows is the keep (rn=1); archived row is excluded entirely
    let keep: String = conn
        .query_row(
            "SELECT relative_path FROM files WHERE id=(SELECT id FROM dup_loose WHERE content_hash='H' AND rn=1)",
            [], |r| r.get(0)).unwrap();
    assert_eq!(keep, "old.txt");

    let copies: i64 = conn
        .query_row("SELECT DISTINCT copies FROM dup_loose WHERE content_hash='H'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(copies, 2, "archived row must not count toward loose copies");

    let solo: i64 = conn
        .query_row("SELECT copies FROM dup_loose WHERE content_hash='U'", [], |r| r.get(0)).unwrap();
    assert_eq!(solo, 1);
}

#[test]
fn dup_loose_sorts_null_timestamps_last() {
    let conn = Connection::open_in_memory().unwrap();
    apply(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
             VALUES ('v','V','marker',1,1);
         INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,content_hash,
             created_time,modified_time,accessed_time,category,container_chain,status,
             first_seen_at,last_seen_at)
         VALUES ('v','nulls.txt','nulls.txt','txt',10,'H',NULL,NULL,NULL,'other',NULL,'active',1,1),
                ('v','dated.txt','dated.txt','txt',10,'H',500,500,NULL,'other',NULL,'active',1,1);",
    )
    .unwrap();
    let keep: String = conn
        .query_row(
            "SELECT relative_path FROM files WHERE id=(SELECT id FROM dup_loose WHERE content_hash='H' AND rn=1)",
            [], |r| r.get(0)).unwrap();
    assert_eq!(keep, "dated.txt", "a NULL timestamp must never win the keep slot");
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test --lib schema::tests -- --nocapture`
Expected: FAIL — `no such table: dup_loose`.

- [ ] **Step 5: Create the view in `apply()`**

In `src/catalog/schema.rs`, after the existing `execute_batch(...)` call that creates tables/indexes/triggers, add:

```rust
    // The single definition of "loose active duplicate + which copy is kept".
    // Built from dedup::KEEP_ORDER so the rule lives in exactly one place.
    conn.execute_batch(&format!(
        r#"
        DROP VIEW IF EXISTS dup_loose;
        CREATE VIEW dup_loose AS
        SELECT id, volume_id, content_hash, size_bytes,
               ROW_NUMBER() OVER (PARTITION BY content_hash ORDER BY {order}) AS rn,
               COUNT(*)     OVER (PARTITION BY content_hash)                  AS copies
        FROM files
        WHERE status = 'active' AND container_chain IS NULL;
        "#,
        order = crate::catalog::dedup::KEEP_ORDER
    ))?;
```

`DROP VIEW IF EXISTS` before `CREATE` makes the definition self-migrating: an existing database picks up a changed `KEEP_ORDER` on next open, with no version table.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib schema::tests -- --nocapture`
Expected: PASS, both tests.

- [ ] **Step 7: Full suite + gates**

Run: `cargo test --release 2>&1 | grep "test result"` — every line `ok`.
Run: `cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: both exit 0.

- [ ] **Step 8: Commit**

```bash
git add src/catalog/schema.rs src/catalog/dedup.rs src/catalog/mod.rs
git commit -m "feat(catalog): add dup_loose view with a single keep-ordering definition

KEEP_ORDER is the one place the 'which copy survives' rule is written; the
view is generated from it, so the whole-table and per-page queries cannot
drift. NULL timestamps sort last (SQLite would otherwise sort them first
and silently change which copy is kept).

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Totals + per-volume reclaimable (loose-only, honest)

**Files:**
- Modify: `src/catalog/dedup.rs`

**Interfaces:**
- Consumes: `dup_loose`, `KEEP_ORDER`, `DEFAULT_MIN_SIZE` (Task 1).
- Produces:
  ```rust
  pub struct DuplicateTotals { pub groups: i64, pub reclaimable_bytes: i64,
      pub groups_all: i64, pub reclaimable_all_bytes: i64, pub archive_locked_bytes: i64 }
  impl Catalog {
      pub fn duplicate_totals(&self, min_size: i64) -> anyhow::Result<DuplicateTotals>;
      pub fn reclaimable_by_volume(&self) -> anyhow::Result<std::collections::HashMap<String, i64>>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `src/catalog/dedup.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;

    /// Two loose copies of A (100 B), three loose copies of B (10 B), one unique,
    /// plus two archived copies of C (50 B).
    fn seed() -> (tempfile::TempDir, Catalog) {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.conn
            .execute_batch(
                "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
                     VALUES ('v1','V1','marker',1,1),('v2','V2','marker',1,1);
                 INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,content_hash,
                     created_time,modified_time,accessed_time,category,container_chain,status,
                     first_seen_at,last_seen_at)
                 VALUES
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
    fn totals_are_loose_only_and_floor_never_touches_the_headline() {
        let (_t, cat) = seed();
        let tot = cat.duplicate_totals(0).unwrap();
        // loose duplicates: A (2 copies x 100 -> 100 reclaimable), B (3 x 10 -> 20)
        assert_eq!(tot.groups_all, 2);
        assert_eq!(tot.reclaimable_all_bytes, 120);
        assert_eq!(tot.groups, 2);
        assert_eq!(tot.reclaimable_bytes, 120);
        // archived duplicates are reported separately, never folded into the headline
        assert_eq!(tot.archive_locked_bytes, 50, "one redundant archived copy of C");

        // a floor hides small groups from the review list but leaves the headline intact
        let tot = cat.duplicate_totals(50).unwrap();
        assert_eq!(tot.groups, 1, "only A survives a 50-byte floor");
        assert_eq!(tot.reclaimable_bytes, 100);
        assert_eq!(tot.groups_all, 2, "headline must ignore the floor");
        assert_eq!(tot.reclaimable_all_bytes, 120, "headline must ignore the floor");
    }

    #[test]
    fn reclaimable_by_volume_attributes_to_the_non_kept_copies() {
        let (_t, cat) = seed();
        let m = cat.reclaimable_by_volume().unwrap();
        // A: keep v1 (older) -> v2 owes 100. B: keep v1/b1 -> v1 owes 10 (b2), v2 owes 10 (b3).
        assert_eq!(m.get("v1").copied().unwrap_or(0), 10);
        assert_eq!(m.get("v2").copied().unwrap_or(0), 110);
    }
}
```

Add `tempfile` to `[dev-dependencies]` in `Cargo.toml` only if it is not already there (it is used by existing tests, so it should be).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib dedup::tests`
Expected: FAIL — `duplicate_totals` / `reclaimable_by_volume` not found.

- [ ] **Step 3: Implement both queries**

Append to `src/catalog/dedup.rs` (above the test module):

```rust
use crate::catalog::Catalog;

/// Headline and review-list totals. `*_all` fields ignore the floor by design — a UI filter must
/// never change the number the user is told is reclaimable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateTotals {
    pub groups: i64,
    pub reclaimable_bytes: i64,
    pub groups_all: i64,
    pub reclaimable_all_bytes: i64,
    pub archive_locked_bytes: i64,
}

impl Catalog {
    /// Aggregate duplicate figures. Uses a plain GROUP BY over `idx_files_hash` (no window
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
    pub fn reclaimable_by_volume(&self) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT volume_id, IFNULL(SUM(size_bytes), 0) FROM dup_loose
             WHERE copies > 1 AND rn > 1 GROUP BY volume_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib dedup::tests`
Expected: PASS, both tests.

- [ ] **Step 5: Gates + commit**

```bash
cargo test --release 2>&1 | grep "test result"
cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/dedup.rs
git commit -m "feat(catalog): loose-only reclaimable totals as SQL aggregates

Reclaimable now counts only loose files, which is what quarantine can
actually free by renaming; archived duplicates are reported separately and
are deliberately NOT the complement of that figure (a hash can be both).
The floor applies to the review list only and never to the headline.

Replaces an in-memory sum that materialised every duplicate group on every
Drives/Overview load.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Ranked, cursor-paged groups + per-page members

**Files:**
- Modify: `src/catalog/dedup.rs`

**Interfaces:**
- Consumes: `KEEP_ORDER`, `dup_loose`, `DuplicateTotals` (Tasks 1–2).
- Produces:
  ```rust
  pub struct DuplicateGroup { pub content_hash: String, pub copies: i64,
      pub size_bytes: i64, pub reclaimable_bytes: i64 }
  pub struct DuplicateCursor { pub reclaimable_bytes: i64, pub content_hash: String }
  pub struct DuplicateMember { pub record: crate::catalog::models::FileRecord, pub is_suggested_keep: bool }
  impl Catalog {
      pub fn duplicate_groups_ranked(&self, min_size: i64, limit: usize,
          after: Option<&DuplicateCursor>) -> anyhow::Result<Vec<DuplicateGroup>>;
      pub fn duplicate_members_for(&self, hashes: &[String])
          -> anyhow::Result<std::collections::HashMap<String, Vec<DuplicateMember>>>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

Add inside the existing `mod tests` in `src/catalog/dedup.rs`:

```rust
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
            .execute("UPDATE files SET status='quarantined' WHERE content_hash='A'", [])
            .unwrap();
        let again = cat.duplicate_groups_ranked(0, 10, Some(&cur)).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].content_hash, "B", "mutation must not skip or repeat groups");
    }

    #[test]
    fn members_carry_the_suggested_keep_flag() {
        let (_t, cat) = seed();
        let m = cat.duplicate_members_for(&["A".to_string(), "B".to_string()]).unwrap();
        let a = &m["A"];
        assert_eq!(a.len(), 2);
        let keep: Vec<_> = a.iter().filter(|x| x.is_suggested_keep).collect();
        assert_eq!(keep.len(), 1, "exactly one keep per group");
        assert_eq!(keep[0].record.relative_path, "a1", "oldest copy is kept");
        assert_eq!(m["B"].iter().filter(|x| x.is_suggested_keep).count(), 1);
    }

    #[test]
    fn members_for_empty_hash_list_is_empty_not_an_error() {
        let (_t, cat) = seed();
        assert!(cat.duplicate_members_for(&[]).unwrap().is_empty());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib dedup::tests`
Expected: FAIL — `duplicate_groups_ranked` / `duplicate_members_for` not found.

- [ ] **Step 3: Implement the ranked query and the per-page members query**

Append to `src/catalog/dedup.rs` (above the test module):

```rust
/// One duplicate group. No member rows are loaded — members are fetched per page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub content_hash: String,
    pub copies: i64,
    pub size_bytes: i64,
    pub reclaimable_bytes: i64,
}

/// Position in the ranked list. Cursor rather than offset: quarantining re-ranks the list, and an
/// offset would silently skip groups the user believes they reviewed.
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
    /// Loose duplicate groups at or above `min_size`, ranked by reclaimable space descending.
    /// Pass the previous page's last group as `after` to get the next page.
    pub fn duplicate_groups_ranked(
        &self,
        min_size: i64,
        limit: usize,
        after: Option<&DuplicateCursor>,
    ) -> anyhow::Result<Vec<DuplicateGroup>> {
        let (ar, ah) = match after {
            Some(c) => (c.reclaimable_bytes, c.content_hash.clone()),
            // i64::MAX sorts before every real value, so "no cursor" means "from the top".
            None => (i64::MAX, String::new()),
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT content_hash, copies, size_bytes, reclaimable FROM (
                 SELECT content_hash,
                        COUNT(*)                        AS copies,
                        MIN(size_bytes)                 AS size_bytes,
                        (COUNT(*) - 1) * MIN(size_bytes) AS reclaimable
                 FROM files
                 WHERE status = 'active' AND container_chain IS NULL AND size_bytes >= ?1
                 GROUP BY content_hash HAVING COUNT(*) > 1)
             WHERE reclaimable < ?2 OR (reclaimable = ?2 AND content_hash > ?3)
             ORDER BY reclaimable DESC, content_hash ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![min_size, ar, ah, limit as i64],
            |r| {
                Ok(DuplicateGroup {
                    content_hash: r.get(0)?,
                    copies: r.get(1)?,
                    size_bytes: r.get(2)?,
                    reclaimable_bytes: r.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Members of the given groups, keyed by content hash, each flagged with whether it is the
    /// suggested keep. The window runs only over the page's hashes, not the whole table.
    pub fn duplicate_members_for(
        &self,
        hashes: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, Vec<DuplicateMember>>> {
        let mut out: std::collections::HashMap<String, Vec<DuplicateMember>> =
            std::collections::HashMap::new();
        if hashes.is_empty() {
            return Ok(out);
        }
        let holders = vec!["?"; hashes.len()].join(",");
        let sql = format!(
            "SELECT {cols}, ROW_NUMBER() OVER (PARTITION BY content_hash ORDER BY {order}) AS rn
             FROM files
             WHERE status = 'active' AND container_chain IS NULL AND content_hash IN ({holders})
             ORDER BY content_hash, rn",
            cols = super::store::FILE_COLUMNS,
            order = KEEP_ORDER,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let args: Vec<&dyn rusqlite::types::ToSql> =
            hashes.iter().map(|h| h as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(args.as_slice(), |r| {
            let rec = Catalog::map_file_record(r)?;
            let rn: i64 = r.get("rn")?;
            Ok(DuplicateMember { record: rec, is_suggested_keep: rn == 1 })
        })?;
        for m in rows {
            let m = m?;
            out.entry(m.record.content_hash.clone()).or_default().push(m);
        }
        Ok(out)
    }
}
```

**Note on visibility:** `FILE_COLUMNS` ([src/catalog/store.rs:21](../../../src/catalog/store.rs#L21)) and
`Catalog::map_file_record` ([src/catalog/store.rs:735](../../../src/catalog/store.rs#L735)) are both private.
Change `const FILE_COLUMNS` → `pub(crate) const FILE_COLUMNS` and `fn map_file_record` →
`pub(crate) fn map_file_record` so `dedup.rs` reuses them. Do **not** copy the column list — the
mapper reads by column name and the two would drift.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib dedup::tests`
Expected: PASS, all six tests in the module.

- [ ] **Step 5: Gates + commit**

```bash
cargo test --release 2>&1 | grep "test result"
cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/dedup.rs src/catalog/store.rs
git commit -m "feat(catalog): ranked, cursor-paged duplicate groups

Groups are ordered by reclaimable space and paged by cursor rather than
offset, so quarantining a group cannot shift offsets and silently skip
groups the user believes they reviewed. Members for a page are fetched in
one query whose window runs only over that page's hashes.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Rewrite the HTTP API

**Files:**
- Modify: `src/web.rs`

**Interfaces:**
- Consumes: `duplicate_totals`, `duplicate_groups_ranked`, `duplicate_members_for`, `reclaimable_by_volume`, `DEFAULT_MIN_SIZE`.
- Produces: `GET /api/duplicates?min_size=&limit=&after_reclaimable=&after_hash=` returning
  `{ totals, groups: [{ content_hash, copies, size_bytes, reclaimable_bytes, suggested_keep_id, members: [...] }], next }`;
  `DriveDto.reclaimable_bytes` from `reclaimable_by_volume`; `/api/stats` carrying the totals.

- [ ] **Step 1: Write the failing web tests**

Add to the test module in `src/web.rs`. Note `seed_dupes()` ([src/web.rs:1208](../../../src/web.rs#L1208))
creates 10-byte files, so every duplicates request in tests must pass `min_size=0` — the 1 MiB
default would correctly hide the whole fixture.

```rust
    #[tokio::test]
    async fn api_duplicates_is_ranked_bounded_and_reports_totals() {
        let (_t, db, _state) = seed_dupes();
        let v = get_json(&db, "/api/duplicates?min_size=0&limit=1").await;
        assert!(v["totals"]["reclaimable_all_bytes"].as_i64().unwrap() > 0);
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "limit must bound the page");
        assert!(groups[0]["reclaimable_bytes"].as_i64().unwrap() > 0);
        assert!(groups[0]["suggested_keep_id"].as_i64().unwrap() > 0);
        assert!(!groups[0]["members"].as_array().unwrap().is_empty());
        assert!(v["next"]["content_hash"].is_string(), "cursor for the next page");
    }

    #[tokio::test]
    async fn api_duplicates_floor_filters_the_list_but_not_the_headline() {
        let (_t, db, _state) = seed_dupes();
        let all = get_json(&db, "/api/duplicates?min_size=0").await;
        let floored = get_json(&db, "/api/duplicates?min_size=999999999").await;
        assert!(floored["groups"].as_array().unwrap().is_empty(), "floor empties the list");
        assert_eq!(
            floored["totals"]["reclaimable_all_bytes"], all["totals"]["reclaimable_all_bytes"],
            "headline must not move when the floor changes"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --release --lib web::tests::api_duplicates`
Expected: FAIL — the response is still a bare array, so the `totals`/`groups` lookups are null.

- [ ] **Step 3: Replace `api_duplicates` and its DTOs**

In `src/web.rs`, delete the old `GroupDto`, the `suggested_keep` helper, and the body of `api_duplicates`. Add:

```rust
#[derive(Serialize)]
struct TotalsDto {
    groups: i64,
    reclaimable_bytes: i64,
    groups_all: i64,
    reclaimable_all_bytes: i64,
    archive_locked_bytes: i64,
}

#[derive(Serialize)]
struct GroupDto {
    content_hash: String,
    copies: i64,
    size_bytes: i64,
    reclaimable_bytes: i64,
    suggested_keep_id: i64,
    members: Vec<MemberDto>,
}

#[derive(Serialize)]
struct CursorDto { reclaimable_bytes: i64, content_hash: String }

#[derive(Serialize)]
struct DuplicatesDto { totals: TotalsDto, groups: Vec<GroupDto>, next: Option<CursorDto> }

#[derive(Deserialize, Default)]
struct DuplicatesParams {
    min_size: Option<i64>,
    limit: Option<usize>,
    after_reclaimable: Option<i64>,
    after_hash: Option<String>,
}

async fn api_duplicates(
    State(state): State<AppState>,
    Query(p): Query<DuplicatesParams>,
) -> Result<Json<DuplicatesDto>, (axum::http::StatusCode, String)> {
    use crate::catalog::dedup::{DuplicateCursor, DEFAULT_MIN_SIZE};
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let min_size = p.min_size.unwrap_or(DEFAULT_MIN_SIZE);
    let limit = p.limit.unwrap_or(50).clamp(1, 200);
    let after = match (p.after_reclaimable, p.after_hash) {
        (Some(r), Some(h)) => Some(DuplicateCursor { reclaimable_bytes: r, content_hash: h }),
        _ => None,
    };

    let t = cat.duplicate_totals(min_size).map_err(err500)?;
    let groups = cat
        .duplicate_groups_ranked(min_size, limit, after.as_ref())
        .map_err(err500)?;
    let hashes: Vec<String> = groups.iter().map(|g| g.content_hash.clone()).collect();
    let mut members = cat.duplicate_members_for(&hashes).map_err(err500)?;

    let labels: std::collections::HashMap<String, String> = cat
        .volume_stats().map_err(err500)?
        .into_iter().map(|(id, label, _, _)| (id, label)).collect();
    let mounts = state.mounts.snapshot();

    let next = groups.last().map(|g| CursorDto {
        reclaimable_bytes: g.reclaimable_bytes,
        content_hash: g.content_hash.clone(),
    });

    let out_groups = groups
        .into_iter()
        .map(|g| {
            let ms = members.remove(&g.content_hash).unwrap_or_default();
            let suggested_keep_id =
                ms.iter().find(|m| m.is_suggested_keep).map(|m| m.record.id).unwrap_or(0);
            let members = ms
                .into_iter()
                .map(|m| {
                    let f = m.record;
                    MemberDto {
                        id: f.id,
                        location: f.display_location(),
                        filename: f.filename.clone(),
                        volume_label: labels.get(&f.volume_id).cloned().unwrap_or_default(),
                        mounted: mounts.contains_key(&f.volume_id),
                        volume_id: f.volume_id,
                        size_bytes: f.size_bytes,
                        category: f.category.as_str().to_string(),
                        created_time: f.created_time,
                        modified_time: f.modified_time,
                        status: f.status.as_str().to_string(),
                        is_loose: f.container_chain.is_none(),
                    }
                })
                .collect();
            GroupDto {
                content_hash: g.content_hash,
                copies: g.copies,
                size_bytes: g.size_bytes,
                reclaimable_bytes: g.reclaimable_bytes,
                suggested_keep_id,
                members,
            }
        })
        .collect();

    Ok(Json(DuplicatesDto {
        totals: TotalsDto {
            groups: t.groups,
            reclaimable_bytes: t.reclaimable_bytes,
            groups_all: t.groups_all,
            reclaimable_all_bytes: t.reclaimable_all_bytes,
            archive_locked_bytes: t.archive_locked_bytes,
        },
        groups: out_groups,
        next,
    }))
}
```

- [ ] **Step 4: Point `api_drives` and `api_stats` at the new figures**

In `api_drives`, replace `let reclaim = cat.reclaimable_bytes_by_volume()...` with:

```rust
    let reclaim = cat.reclaimable_by_volume().map_err(err500)?;
```

In `api_stats`, replace `let duplicate_groups = cat.duplicate_group_count()...` with:

```rust
    let totals = cat
        .duplicate_totals(crate::catalog::dedup::DEFAULT_MIN_SIZE)
        .map_err(err500)?;
    let duplicate_groups = totals.groups_all;
```

and add `archive_locked_bytes: totals.archive_locked_bytes` plus
`reclaimable_all_bytes: totals.reclaimable_all_bytes` to the stats DTO (adding both fields to the
struct definition), so the Overview can show the honest headline and the archive figure.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --release --lib web::`
Expected: PASS. Fix any other web test that asserted the old array shape by updating it to the new object shape — do not reintroduce the array.

- [ ] **Step 6: Gates + commit**

```bash
cargo test --release 2>&1 | grep "test result"
cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web.rs
git commit -m "feat(review): bounded, ranked /api/duplicates with honest totals

The endpoint previously serialised every group and every member -- 254k
groups over 1.7M rows on a real catalog. It now returns a bounded, ranked
page plus a cursor, and reports loose-only reclaimable alongside the
archive-locked figure. Drives/Overview no longer materialise duplicate
groups in memory.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Delete the old implementation

**Files:**
- Modify: `src/catalog/store.rs`, `src/commands.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–4.
- Produces: a codebase with exactly one duplicate/reclaimable implementation.

**Context:** This task exists to enforce the "one implementation" rule. `cmd_duplicates` currently prints every group (254,226 on the real catalog), so it is rewritten here rather than left calling a deleted method.

- [ ] **Step 1: Rewrite `cmd_duplicates` and `cmd_status`**

In `src/commands.rs`, replace the body of `cmd_duplicates` with:

```rust
pub fn cmd_duplicates() -> anyhow::Result<()> {
    use crate::catalog::dedup::DEFAULT_MIN_SIZE;
    let (_cfg, cat) = open_catalog()?;
    let totals = cat.duplicate_totals(DEFAULT_MIN_SIZE)?;
    let groups = cat.duplicate_groups_ranked(DEFAULT_MIN_SIZE, 20, None)?;
    if groups.is_empty() {
        println!("No duplicate groups at or above {DEFAULT_MIN_SIZE} bytes.");
    }
    let hashes: Vec<String> = groups.iter().map(|g| g.content_hash.clone()).collect();
    let members = cat.duplicate_members_for(&hashes)?;
    for g in &groups {
        println!(
            "{} reclaimable — {} copies x {} bytes  (hash {})",
            g.reclaimable_bytes,
            g.copies,
            g.size_bytes,
            &g.content_hash[..16.min(g.content_hash.len())]
        );
        for m in members.get(&g.content_hash).into_iter().flatten() {
            println!(
                "  {}#{}  {}  [{}]",
                if m.is_suggested_keep { "KEEP " } else { "     " },
                m.record.id,
                m.record.display_location(),
                m.record.volume_id
            );
        }
    }
    println!(
        "\nShowing top {} of {} groups at/above {} bytes. Reclaimable: {} bytes \
         ({} bytes total, floor-free). Archive-locked: {} bytes (needs repack).",
        groups.len(), totals.groups, DEFAULT_MIN_SIZE,
        totals.reclaimable_bytes, totals.reclaimable_all_bytes, totals.archive_locked_bytes
    );
    Ok(())
}
```

In `cmd_status`, replace `let groups = cat.duplicate_group_count()?;` with:

```rust
    let groups = cat.duplicate_totals(0)?.groups_all;
```

- [ ] **Step 2: Delete the old methods**

In `src/catalog/store.rs`, delete these three methods entirely: `duplicate_groups`,
`duplicate_group_count` (line ~211), and `reclaimable_bytes_by_volume` (line ~463). Delete the tests
that exercise only those methods — including `duplicate_group_count_ignores_all_missing_groups`
(line ~848) — since `dedup.rs` now covers that behaviour. Keep every other test.

- [ ] **Step 3: Prove the old implementation is gone**

```bash
grep -rn "duplicate_groups()\|duplicate_group_count()\|reclaimable_bytes_by_volume" src/ || echo "CLEAN: single implementation"
```

Expected: `CLEAN: single implementation`. Any hit means a caller or a stale definition survived — fix it rather than re-adding the method.

- [ ] **Step 4: Verify the build and full suite**

Run: `cargo test --release 2>&1 | grep "test result"`
Expected: every line `ok`.
Run: `cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check`
Expected: both exit 0.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs src/commands.rs
git commit -m "refactor(catalog): delete the in-memory duplicate implementation

Removes duplicate_groups(), duplicate_group_count() and the in-memory
reclaimable_bytes_by_volume(). The SQL layer in catalog::dedup is now the
only implementation. cmd_duplicates gains ranking, the review floor and a
top-20 limit instead of printing every group.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Duplicates page — totals header, floor control, ranked paging

**Files:**
- Modify: `src/web_ui.rs`

**Interfaces:**
- Consumes: the `/api/duplicates` response shape from Task 4.

**Context:** The page keeps its existing compare-and-confirm cards. What changes: it loads a ranked page instead of everything, shows honest totals, and never hides groups silently.

- [ ] **Step 1: Replace the page markup**

In `review_page`'s `main`, replace the header row (currently the `page-h` + `#progress` row) with:

```html
<div class="rv-page">
  <div class="row" style="justify-content:space-between;align-items:baseline;margin-bottom:6px">
    <h1 class="page-h" style="margin:0">Review duplicates</h1><span class="mut" id="progress"></span></div>
  <div class="row" style="justify-content:space-between;align-items:center;flex-wrap:wrap;gap:12px;margin-bottom:18px">
    <div class="mut" id="totals" style="font-size:13px"></div>
    <label class="row" style="font-size:13px;color:var(--mut);gap:8px">Only show files ≥
      <select id="minsize" class="btn" style="padding:6px 10px">
        <option value="0">any size</option>
        <option value="65536">64 KB</option>
        <option value="1048576" selected>1 MB</option>
        <option value="10485760">10 MB</option>
        <option value="104857600">100 MB</option>
      </select></label>
  </div>
  <div id="group"></div>
  <div class="mut" id="upnext" style="font-size:12px;margin-top:14px"></div>
```

Keep the existing `.rvbar` buttons, the `_ToDelete` note and `#msg` exactly as they are.

- [ ] **Step 2: Replace the loading logic**

In `review_page`'s script, replace `load()` and the head of `render()` with:

```js
let groups=[],idx=0,keepId=null,totals=null,next=null,minSize=1048576,exhausted=false;
function fmtGroups(n){ return n.toLocaleString(); }
async function loadPage(reset){
  if(reset){ groups=[]; idx=0; next=null; exhausted=false; }
  const p=new URLSearchParams({min_size:String(minSize),limit:"50"});
  if(next){ p.set("after_reclaimable",String(next.reclaimable_bytes)); p.set("after_hash",next.content_hash); }
  const r=await apiGet("/api/duplicates?"+p.toString());
  totals=r.totals;
  if(!r.groups.length) exhausted=true;
  groups=groups.concat(r.groups);
  next=r.next;
  paintTotals();
}
function paintTotals(){
  if(!totals)return;
  const hiddenGroups=totals.groups_all-totals.groups;
  const hiddenBytes=totals.reclaimable_all_bytes-totals.reclaimable_bytes;
  let s=`<b>${fmtGroups(totals.groups)}</b> groups · <b>${fmtSize(totals.reclaimable_bytes)}</b> reclaimable`;
  if(totals.archive_locked_bytes>0) s+=` · ${fmtSize(totals.archive_locked_bytes)} locked in archives (needs repack)`;
  if(hiddenGroups>0) s+=`<br><span style="color:var(--mut2)">${fmtGroups(hiddenGroups)} smaller groups (${fmtSize(hiddenBytes)}) hidden by this filter — lower it to review them.</span>`;
  $("#totals").innerHTML=s;
}
async function load(){ try{ await loadPage(true); }catch(e){ $("#msg").textContent="Load error: "+e; return; } render(); }
```

Then in `render()`, replace the exhaustion check `if(idx>=groups.length){...}` with:

```js
  if(idx>=groups.length){
    if(next && !exhausted){ loadPage(false).then(render).catch(e=>{$("#msg").textContent="Load error: "+e;}); return; }
    $("#progress").textContent="";
    $("#group").innerHTML='<div class="empty"><h2 style="margin:0 0 6px">All duplicate groups reviewed 🎉</h2><p class="mut">Nothing left to compare at this size filter.</p></div>';
    $("#upnext").textContent="";
    $("#confirm").style.display="none"; $("#skip").style.display="none"; return;
  }
```

and replace the `#progress` line with one that shows position and value:

```js
  $("#progress").textContent=`Group ${idx+1} of ${fmtGroups(totals?totals.groups:groups.length)} · ${g.members.length} copies · ${fmtSize(g.reclaimable_bytes)} reclaimable`;
```

- [ ] **Step 3: Add the "up next" strip and wire the floor control**

Add at the end of `render()`:

```js
  const rest=groups.slice(idx+1,idx+4);
  $("#upnext").innerHTML = rest.length
    ? "Up next: "+rest.map(x=>`${fmtSize(x.reclaimable_bytes)} (${x.copies} copies)`).join(" · ")
    : "";
```

and register the control near the other listeners:

```js
$("#minsize").addEventListener("change",()=>{ minSize=Number($("#minsize").value); load(); });
```

- [ ] **Step 4: Update the Console `duplicates` command**

In `console_page`'s script, replace
`if(cmd==="duplicates"){ printJSON(await apiGet("/api/duplicates")); return; }` with:

```js
    if(cmd==="duplicates"){ printJSON(await apiGet("/api/duplicates?limit=20")); return; }
```

- [ ] **Step 5: Verify the page renders and is self-contained**

```bash
cargo build --release
cargo test --release --lib web:: 2>&1 | grep "test result"
```

Expected: build succeeds; web tests `ok` (they assert the page is self-contained — no `http://`).

Then run the app against the sandbox and look at the page:

```bash
pwsh -File UI/shoot.ps1 -Width 1600 -Height 950
```

Read `UI/Screenshots/shot_duplicates_light.png` and confirm: the totals line shows groups + reclaimable, the size filter is present, and the compare cards still render.

- [ ] **Step 6: Gates + commit**

```bash
cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web_ui.rs
git commit -m "feat(review): ranked duplicates page with honest totals and a size filter

The page loads a ranked page at a time instead of every group, shows
loose-only reclaimable alongside the archive-locked figure, and always
states how many smaller groups the size filter is hiding.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: Verify against the real catalogue

**Files:** none — read-only verification.

**Context:** The sandbox has ~13 files; the numbers in the spec came from the real catalogue (1,788,308 files). This task proves the implementation reproduces them. **Read-only: never write to the user's catalog.**

- [ ] **Step 1: Check the figures against the real database**

```bash
python - <<'PY'
import sqlite3, os
db=os.path.join(os.environ['APPDATA'],'justPrototype','CleanUpStorages','data','catalog.db')
con=sqlite3.connect(f'file:{db}?mode=ro',uri=True)
q=lambda s,*a: con.execute(s,a).fetchone()
loose=lambda f: q("""SELECT COUNT(*), IFNULL(SUM((c-1)*s),0) FROM (
   SELECT COUNT(*) c, MIN(size_bytes) s FROM files
   WHERE status='active' AND container_chain IS NULL AND size_bytes>=?
   GROUP BY content_hash HAVING COUNT(*)>1)""", f)
print("floor 0      groups=%d reclaimable=%.3f TB" % (loose(0)[0], loose(0)[1]/1e12))
print("floor 1 MiB  groups=%d reclaimable=%.3f TB" % (loose(1048576)[0], loose(1048576)[1]/1e12))
print("archive-locked: %.3f TB" % (q("""SELECT IFNULL(SUM((c-1)*s),0) FROM (
   SELECT COUNT(*) c, MIN(size_bytes) s FROM files
   WHERE status='active' AND container_chain IS NOT NULL
   GROUP BY content_hash HAVING COUNT(*)>1)""")[0]/1e12))
con.close()
PY
```

Expected, matching the spec's success criteria: floor 0 → **61,816 groups / 1.060 TB**; floor 1 MiB → **4,433 groups / ~1.054 TB**; archive-locked ≈ **2.2 TB**. Small drift is fine if the catalog has been scanned further since; the *shape* (loose ≈ 1 TB, not 3.28 TB; 1 MiB floor cutting ~57k groups) must hold.

- [ ] **Step 2: Record the outcome**

If the numbers match, note them in the task report. If they do not, stop and report BLOCKED with both the expected and actual figures — a mismatch means the SQL does not implement the spec, and that must be understood before this ships.

---

## Notes for later (not this plan)

- Archive-locked duplicates (~2.2 TB) are only *reported*; reclaiming them needs repack-based flows.
- The ranked query is bounded but unmeasured at 9M rows — worth timing once the catalog grows (relates to the scan-performance epic).

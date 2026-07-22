# Honest, ranked, bounded duplicate review — design

**Status:** approved
**Date:** 2026-07-22
**Closes:** #29 (rank by reclaimable), #33 (reclaimable overstated ~3×), #3 (duplicate noise floor)

> **Corrected after the merge:** earlier drafts of this spec (and the merge commit) cited the
> reclaimable-overstated bug as **#30**. It is **#33**. #30 is "Make duplicate highlighting
> server-backed", a Browse-tab issue this work does *not* address and which stays open.

## Why

Measured against the real catalogue (1,788,308 files, ~4 TB of a 20 TB target), the duplicate review
path does not work at scale and reports a number it cannot deliver:

- **`/api/duplicates` is unbounded.** It serialises every group with every member — 254,226 groups
  over 1,728,866 rows. The Duplicates page is not "badly ranked", it is unusable.
- **`reclaimable_bytes_by_volume()` materialises every duplicate group in memory** to sum bytes, and
  runs on *every* Drives and Overview load. ~1.7M records built per page view.
- **The reported figure is ~3× too high.** It counts files inside archives, which quarantine cannot
  reclaim by a rename. Honest loose-only total: **1.06 TB**, not 3.28 TB.
- **Noise drowns the signal.** 254,226 groups, but 50% of reclaimable space is in **131** groups.
  12,191 empty files form one bogus group; 11,235 × 212 B and several thousand × 4 KB groups follow.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Size floor | **1 MiB default, user-adjustable** | Shows 4,433 of 61,816 groups while keeping 1054.4 GB of 1060.2 GB. Removes 93% of the noise for 0.55% of the value. |
| Floor scope | **Review list only — never the headline** | Dragging a filter must not change your "reclaimable" number, or the figure feels arbitrary — the exact credibility problem #33 fixes. |
| Floor mechanism | **Query-time filter, never catalog-level** | Every row stays scanned and stored. The floor changes what is *shown*, nothing else. |
| Reclaimable | **Loose files only**, archive-locked reported separately | Quarantine reclaims by renaming a real file; an archived duplicate needs a repack. Both numbers shown, always. |
| Paging | **Cursor, not offset** | Quarantining re-ranks the list; offsets would silently skip groups. |
| Keep-selection | **One SQL definition, used everywhere** | Per-group *and* per-volume attribution both need it. Two implementations would drift. |
| Compatibility | **None kept.** Old code deleted, API shape changed freely | Explicit instruction: one implementation, no legacy paths. |

## Architecture

Everything derives from **one SQL view** so the definition of "duplicate", "keep" and "reclaimable"
exists exactly once.

```sql
CREATE VIEW IF NOT EXISTS dup_loose AS
SELECT id, volume_id, content_hash, size_bytes,
       ROW_NUMBER() OVER (PARTITION BY content_hash
         ORDER BY IFNULL(created_time,  9223372036854775807),
                  IFNULL(modified_time, 9223372036854775807),
                  id)                              AS rn,
       COUNT(*)   OVER (PARTITION BY content_hash) AS copies
FROM files
WHERE status = 'active' AND container_chain IS NULL;
```

- a **duplicate** is a row with `copies > 1`
- the **keep** is `rn = 1` — oldest `created_time`, then `modified_time`, then `id`, with NULLs last
  (`IFNULL(..., i64::MAX)` reproduces the existing Rust ordering; SQLite would otherwise sort NULLs
  first)
- **reclaimable** = the `size_bytes` of every row with `rn > 1`

Because identical content implies identical size, a group's reclaimable is exactly
`(copies - 1) × size_bytes`.

### Catalog API (replaces the old one entirely)

```rust
pub struct DuplicateGroup {            // one row per group — no member rows loaded
    pub content_hash: String,
    pub copies: i64,
    pub size_bytes: i64,
    pub reclaimable_bytes: i64,        // (copies-1) * size_bytes
    pub suggested_keep_id: i64,        // from the view's rn = 1
}

pub struct DuplicateTotals {
    pub groups: i64,                   // above the floor
    pub reclaimable_bytes: i64,        // above the floor
    pub groups_all: i64,               // floor-free — the honest headline
    pub reclaimable_all_bytes: i64,    // floor-free — the honest headline
    pub archive_locked_bytes: i64,     // see definition below — duplicated bytes inside archives
}

/// Cursor for stable paging over a list that re-ranks as you quarantine.
pub struct DuplicateCursor { pub reclaimable_bytes: i64, pub content_hash: String }

impl Catalog {
    fn duplicate_totals(&self, min_size: i64) -> Result<DuplicateTotals>;
    fn duplicate_groups_ranked(&self, min_size: i64, limit: usize,
                               after: Option<&DuplicateCursor>) -> Result<Vec<DuplicateGroup>>;
    fn duplicate_group_members(&self, content_hash: &str) -> Result<Vec<FileRecord>>;
    fn reclaimable_by_volume(&self) -> Result<HashMap<String, i64>>;  // loose-only, floor-free
}
```

Ordering is `reclaimable_bytes DESC, content_hash ASC`; the cursor filter is
`(reclaimable < ?) OR (reclaimable = ? AND content_hash > ?)`, which is stable under mutation.

**Deleted, not deprecated:** `duplicate_groups()` (the `Vec<Vec<FileRecord>>` loader) and the
in-memory `reclaimable_bytes_by_volume()`. `duplicate_counts()` (per-hash counts annotating search
results) is unrelated and stays. `duplicate_group_count()` is replaced by `DuplicateTotals.groups_all`.

Totals avoid the window function — a plain `GROUP BY content_hash HAVING COUNT(*) > 1` over
`idx_files_hash` — so the common Overview/Drives path stays cheap. The view is used where keep
attribution is genuinely required.

**Two definitions that must not be guessed at:**

- **`min_size` is in bytes; the default is `1_048_576` (1 MiB).** The UI renders sizes with the
  existing 1024-based `fmtSize`, which already labels those units "KB"/"MB", so the control reads
  "1 MB" — consistent with the rest of the app.
- **`archive_locked_bytes`** is computed independently, *not* as "all-rows total minus loose total":
  group rows where `container_chain IS NOT NULL` by `content_hash`, keep groups with `COUNT(*) > 1`,
  and sum `(copies - 1) × size_bytes`. A hash can appear both loose and archived, so the two figures
  are not complements and must never be presented as though they sum to a single "total".
  Measured: **1.23 TB**. An earlier draft of this spec said 2.2 TB, derived as "3.28 TB total minus
  1.06 TB loose" — i.e. by treating them as complements, the exact mistake this paragraph forbids.

### HTTP

`GET /api/duplicates?min_size=&limit=&after_reclaimable=&after_hash=` returns:

```json
{ "totals": { … }, "groups": [ { …group…, "members": [ …MemberDto… ] } ],
  "next": { "reclaimable_bytes": 0, "content_hash": "" } }
```

Members for the page's groups are fetched in **one** query (`WHERE content_hash IN (…)`), so a page
costs two queries regardless of size. Default `limit` 50.

`GET /api/drives` and `/api/stats` report the floor-free honest totals plus `archive_locked_bytes`.

### UI (Duplicates page)

Keeps the existing compare-and-confirm cards — that flow is good — and gains:

- a header: **"4,433 groups · 1.05 TB reclaimable · 1.23 TB locked in archives"**
- a **size-floor control** (default 1 MiB) that always states what it is hiding:
  *"57,383 groups (5.8 GB) hidden by the 1 MB filter"* — one click to lower it. Never silent.
- groups arrive **ranked by reclaimable**, walked via cursor paging; the next page loads as the user
  works through the current one
- an "up next" strip showing the following groups with their reclaimable size

### CLI

`cmd_duplicates` currently prints every group (254,226 of them). It gains the same ranking, the same
default floor, and a `--limit` (default 20), printing reclaimable per group and the honest totals.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **Paging skips groups** as quarantining re-ranks the list — the user believes they reviewed everything | Cursor paging (never offset), plus a test that pages while mutating the underlying data |
| **Keep-selection changes** which copy survives (NULL ordering differs between Rust and SQLite) | The view pins the order with `IFNULL(..., i64::MAX)`; a test asserts the chosen id for NULL timestamps and for exact ties |
| **5.8 GB hidden** by the default floor (3.7 GB of it real 100 KB–1 MB photos/docs) | The hidden count and bytes are always displayed with a one-click override; the floor never touches the headline |
| **1.23 TB archive-locked duplicates become invisible** and forgotten | Reported as its own figure everywhere the headline appears |
| Window function cost at 9M rows | Totals use a plain aggregate over `idx_files_hash`; measure the ranked query before shipping |

## Non-goals

- No change to quarantine, purge or repack semantics. The floor is presentational.
- No change to what is scanned or stored — the catalogue keeps every row, including empty files.
- Not fixing archive-locked duplicates (repack-based reclaim) — reported only.
- No backward compatibility, no deprecation shims, no second code path.

## Success criteria

1. `/api/duplicates` is bounded: a page is two queries and a small payload, independent of catalogue size.
2. Drives/Overview no longer materialise duplicate groups in memory; reclaimable comes from a SQL aggregate.
3. Reclaimable reports **1.06 TB** (loose-only) on the real catalogue, with 1.23 TB shown separately as archive-locked.
4. Default review list shows **4,433** groups ranked biggest-first, and states that 57,383 groups (5.8 GB) are hidden.
5. Keep-selection is defined once, in `dup_loose`, and is covered by tests for NULL timestamps and ties.
6. Paging is stable while groups are quarantined — no group silently skipped.
7. `duplicate_groups()` and the in-memory reclaimable are **gone from the codebase**.

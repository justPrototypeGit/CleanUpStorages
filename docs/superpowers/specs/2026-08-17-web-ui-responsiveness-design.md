# Web UI responsiveness — design

**Status:** approved
**Date:** 2026-08-17
**Closes:** #28 (server-side paging for Browse), plus the wider "every page is slow" report
**Epic:** #27

## Why

Measured against the real catalogue (3.94 GB, 3,123,556 rows, 5.62 TB across two drives):

| what | measured |
| --- | --- |
| `browse` start → listening | **80 s** |
| `GET /api/drives` | **15,956 ms** (804 bytes) |
| `GET /api/duplicates?limit=50` | **14,542 ms** |
| `GET /api/stats` | **10,022 ms** (401 bytes) |
| `GET /api/detected-drives` | 4,344 ms |
| `GET /api/volumes` | 4,046 ms (292 bytes) |
| `GET /api/search` | 3,810 ms (308 KB) |
| `GET /api/tree-duplicates` | 3,507 ms (2.6 MB) |
| every HTML page | **79–96 ms** |

The HTML shell is already fast. **Every complaint about "slow pages" is an API call**, and the
smallest responses are the slowest: 10 seconds to produce 401 bytes is not a payload problem.

## What the measurements actually say

Three distinct causes, and they need three different fixes. Paging alone would fix almost none of it.

**1. Aggregates scan the whole table.** `volume_stats` uses `idx_files_volume`, which carries only
`volume_id`, so SQLite fetches `status` and `size_bytes` from the row for all 3.1 M rows. The dedup
queries use `idx_files_status` and then fetch `content_hash`, `size_bytes` and `container_chain` per
row. By contrast `status counts`, whose index happens to cover it, runs in 186 ms — an 18x
difference from covering alone.

**2. Some results are recomputed constantly and change rarely.** Per-volume file counts and byte
totals change only when a scan, quarantine or purge runs, yet every page load recomputes them from
3.1 M rows.

**3. Some responses are simply too big.** `/api/search` ships up to 3,000 rows and the client builds
the entire tree; `/api/tree-duplicates` ships 2.6 MB in one go.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Dedup queries | **One covering index** `(status, container_chain, content_hash, size_bytes)` | Measured: grouping 2,779 → **51 ms**, member lookup 3,039 → **2 ms**. Costs +511 MB and 15 s to build |
| Per-volume totals | **Materialise, don't index** | An index still leaves a 3.1 M-row GROUP BY (7.6 → 2.9 s). Stored totals make it a two-row read, and the update points already exist |
| Integrity check | **Off the startup path** | 80 s before the port opens, on every launch, to verify something that has not changed since last time |
| Big lists | **Page them, newest/biggest first** | Browse and the folder view both ship everything; the user asked for batches loaded on demand |

### The index, and its cost

+511 MB on a 3.94 GB catalogue. Two reasons that is acceptable here where it would not have been
before: #32 was closed because storage is not the constraint at this corpus size, and the scanning
this project was built for is essentially done — so the write-path cost that #26 worked to reduce is
now paid once per rescan rather than continuously.

**It is still a real cost and must be measured, not assumed**: the insert path gets one more index to
maintain, so a rescan of a full drive is timed before and after.

### Materialised volume totals

`volumes` gains `active_files` and `active_bytes`, recomputed at exactly the points that already
recompute `directory_trees`: after a completed scan, after quarantine, after purge. Same trigger
points, same best-effort treatment, same "derived data, safe to drop and rebuild" contract.

Stale totals are a display problem, never a safety one — nothing destructive reads them.

### Integrity check

`PRAGMA integrity_check` over 3.94 GB is the 80 s. It runs before the server binds, on every start.

It moves to a background task that runs after the port opens, with the result surfaced on the
Console page. The CLI keeps its synchronous check before any destructive verb, because there the
delay is worth it and the user is already waiting.

**A corrupt catalogue must still be loud.** The check still runs, still reports, and still blocks
destructive operations — it just stops blocking the page you wanted to read.

### Paging

`/api/search` gains `limit`/`offset` with a total count, and the client requests more as the user
scrolls or expands a folder rather than materialising the whole tree.

`/api/tree-duplicates` gains the same. It is already sorted actionable-first and by reclaimable
bytes, so the first page is the part worth looking at — and the measured concentration says the top
20 groups carry 63% of all reclaimable space.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| The index slows scanning | Timed before and after on a real rescan; recorded either way. It is one index on a path that already maintains five |
| Materialised totals drift from reality | Recomputed at the same points as `directory_trees`; a rescan always corrects them. Never read by anything destructive |
| Moving the integrity check hides corruption | It still runs, still reports, still gates destructive verbs. Only the *blocking* moves |
| Paging changes what the user sees first | Ordering is unchanged — the existing sort already puts the biggest wins first |

## Non-goals

- No schema change to `files`, no change to what is catalogued, no change to quarantine or purge.
- No change to the folder-collapse algorithm or its numbers.

## Success criteria

1. Every measured endpoint above is re-measured on the same catalogue and recorded.
2. `/api/duplicates` and `/api/drives` drop below one second.
3. `browse` listens in under two seconds.
4. Rescan time is measured with the new index and the result recorded, whatever it shows.
5. Existing tests pass unmodified; the duplicate figures (1,201 actionable groups, 2.39 TB) are
   unchanged.

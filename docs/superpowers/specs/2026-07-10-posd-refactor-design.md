# PoSD complexity-reduction refactor — design

## Goal

Reduce complexity in the existing (working, fully-tested) codebase by eliminating the
change-amplification and information-leakage hot spots surfaced by a *A Philosophy of Software
Design* review. **Every change is behavior-preserving** and guarded by the existing 118-test
suite; the overriding reliability constraint ("nothing lost or corrupted") means no functional
behavior may change except the one deliberate, tiny consistency fix called out in item 6.

This is a refactor, not a feature. The measure of success is: fewer places to edit when a schema
or policy changes, and each unit's interface hiding more of its implementation — not lines added.

## Findings and fixes

### 1. `files` column list — information leakage (highest severity)
The 16-column `SELECT id, volume_id, … FROM files` list is duplicated across **6 query sites** in
`src/catalog/store.rs` (search_filtered, get_file, duplicate_groups, active_copies,
archive_entries, quarantined_rows), and `map_file_record` reads the result back by **positional
index** `r.get(0)…r.get(15)`. Adding or reordering a column requires editing 7 coupled locations,
and any drift between a SELECT's column order and the mapper's indices is a silent runtime panic.

**Fix:** introduce `const FILE_COLUMNS: &str` (the shared column list) used by every SELECT, and
convert `map_file_record` to **named-column access** (`r.get("id")`, `r.get("volume_id")`, …).
Named access removes the positional coupling entirely — the mapper no longer depends on SELECT
column *order*, only on names, which the schema already fixes in one place. One design decision
(the file columns) now lives in one place.

### 2. CSRF gate duplicated 6× (`src/web.rs`)
The identical four-line token-check-and-`tracing::warn!` block (same comment) opens all six
mutating handlers (`api_quarantine`, `api_repack`, `api_forget_drive`, `api_purge_all`, `api_scan`,
`api_pick_folder`).

**Fix:** a `fn check_csrf(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode,
String)>` that performs the check, logs the warn, and returns `Err((FORBIDDEN, …))` on mismatch.
Each handler becomes `check_csrf(&headers, &state)?;` as its first line. The security invariant
(check first, before any catalog/filesystem/dialog access) is preserved and now lives in one
audited place.

### 3. Post-mutation snapshot + `now` duplicated 4× (`src/web.rs`)
The `if let Ok(cfg) = Config::default_paths() { let _ = snapshot(...) }` block appears identically
in the four mutating handlers that write, and the `SystemTime → secs` `now` computation appears 4×.

**Fix:** `fn now_secs() -> Result<i64, (StatusCode, String)>` (or infallible, see plan) and
`fn snapshot_after_mutation(state: &AppState, now: i64)` (best-effort, never fails the request).
Handlers call these instead of inlining.

### 4. CLI command prologue duplicated 9× (`src/commands.rs`)
Every command hand-writes `Config::default_paths()? → Catalog::open(&cfg.catalog_path)?`; five
repeat the identical `backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(),
cfg.snapshot_retention, now)?` line; two repeat the integrity-check-then-bail block.

**Fix:** a helper returning the opened `(Config, Catalog)` (e.g. `fn open_catalog() ->
anyhow::Result<(Config, Catalog)>`), an `fn open_catalog_checked()` variant that also runs the
integrity check (used by `scan`/`browse`), and a `fn snapshot(cfg, now)` wrapper. `now_secs()`
already exists here and stays. Commands shrink to their actual logic.

### 5. `active_file_id` misnamed (`src/catalog/store.rs`)
It filters `container_chain IS NULL` (i.e. *loose* files, any status) — not *active* files, despite
the name. Safe today only because the loose-identity partial unique index guarantees one loose row
per path.

**Fix:** rename to `loose_file_id` and update its callers. Pure clarity; no behavior change.

### 6. `duplicate_group_count` vs `duplicate_groups` can diverge (`src/catalog/store.rs`)
`duplicate_group_count` counts groups over `status IN ('active','missing')`, while
`duplicate_groups` (what the review UI steps through) is `status='active'`. So the Overview /
`status` CLI count can be larger than the number of groups the review page actually shows.

**Fix:** align `duplicate_group_count` to `status='active'` so "N duplicate groups" means the same
thing everywhere. **This is the one deliberate behavior change:** a group whose only remaining
copies are `missing` will no longer be counted. That is the correct meaning — such a group is not
reviewable and reclaims nothing. The affected test(s) get updated to assert the aligned semantics.

### Explicitly out of scope
- **Splitting `web.rs` wholesale.** It is large (~1370 lines incl. a ~640-line test module) but
  *cohesive* (it is the HTTP layer). A blanket split is code-motion churn with little complexity
  reduction and real readability/merge cost — PoSD does not treat size alone as complexity. The
  DRY helpers above already de-bloat it.
- **One targeted extraction IS in scope:** the pure image/zip-preview helpers `thumbnail_jpeg` and
  `read_zip_entry` (no HTTP concern, independently testable) move to a small new
  `src/image_preview.rs` module; `api_preview` stays in `web.rs` and calls them. This is a genuine
  cohesion win, not mere motion.
- **Moving DTOs out of `web.rs`.** Rejected: DTOs and their `From` impls are the handlers'
  interface; co-locating them aids locality. Moving them adds cross-file jumps for no complexity
  reduction.
- **Any change to on-disk formats, the catalog schema, the quarantine/purge/repack engines, or the
  reliability guarantees.**

## Testing

Behavior preservation is verified by the existing suite (110 lib + 8 integration + doctests),
which already covers every touched surface: search/get/duplicates/quarantine/purge/repack queries
(item 1), all six mutating endpoints incl. the CSRF-reject path (items 2–3), every CLI command
(item 4), and the preview thumbnail (extraction). Each refactor task runs the relevant focused
tests green before and after; the full suite runs green before each commit. Item 6 updates the one
count test to the aligned semantics. New helpers get a direct unit test only where they encapsulate
non-trivial logic (`check_csrf` success + reject; `loose_file_id` rename keeps its existing test).

## Sequencing

Items are independent and land as separate small tasks (store column const → CSRF helper →
snapshot/now helpers → CLI prologue helper → rename → count alignment → preview extraction), each
its own reviewed, committed unit, so any single one can be reverted without disturbing the others.

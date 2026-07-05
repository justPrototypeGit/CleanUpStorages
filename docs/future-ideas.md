# Future ideas / next-version backlog

Ideas captured for later versions of CleanUpStorages. These are **not** part of the current approved design
(`docs/superpowers/specs/2026-07-04-cleanupstorages-design.md`) and are intentionally deferred. When an idea is
picked up, it graduates into its own design spec via the normal brainstorming → spec → plan flow.

## Semantic analysis of items

- **Local person recognition** in photos — group/label images by the people in them, running **locally**
  (on-device, no cloud upload) to keep personal data private. Would let the register answer questions like
  "show me all photos of person X across every drive."
- More broadly: semantic/content-based understanding of items (not just filename/hash), e.g. content tagging,
  visual similarity, topic classification for documents — feeding richer search and organization.

_(Raised 2026-07-04. Privacy is a hard requirement: any such analysis must run locally.)_

## Follow-ups logged from the Phase 1a code review (2026-07-04)

Not blockers — Phase 1a shipped without these — but worth doing:

- **Encapsulate the catalog connection.** `catalog::Catalog.conn` is currently `pub`, and the scanner drives transactions via raw `execute_batch("BEGIN"/"COMMIT; BEGIN"/"COMMIT")` strings through it. This leaks SQL/transaction lifecycle out of the store module (a PoSD information leak). Add `Catalog::with_transaction(|…| …)` (or `begin`/`commit_batch`) and demote `conn` to non-`pub`, keeping all SQL inside the catalog module.
- **Phase 2 must not blindly trust Phase 1a hashes for destructive actions.** Two incremental-scan tradeoffs are safe in 1a (nothing acts on hashes yet) but matter once dedup/purge move or delete files: (1) the size + second-granularity mtime skip can miss a same-size edit made within one second, leaving `content_hash` stale; (2) all zero-byte files share the empty-input BLAKE3 digest, so they collapse into one large "duplicate group." Before Phase 2 quarantines/deletes on hash equality, re-hash candidates (or require a full/`--force` scan) and special-case empty files.
- **Directory-level unreadable subtree** (`scanner`): a permission-denied *directory* logs a walk error (good) but any previously-catalogued files beneath it can still be swept to `missing`, since they aren't individually re-seen. The per-file errored-but-present case was fixed in 1a; the whole-subtree case is a harder follow-up (e.g. suppress the missing-sweep for paths under a directory that failed to enumerate).

## Follow-ups logged from the Phase 1b (archives) code review (2026-07-04)

Not blockers — 1b shipped with the two catalog/zip-bomb correctness fixes applied — but worth doing:

- **Peak nested-archive buffering is `depth × entry_max_bytes`.** Only nested archives are buffered (leaves stream), and each is bounded by `entry_max_bytes` (2 GiB), but a deeply-nested chain holds each ancestor's decompressed bytes alive simultaneously — worst case ~`max_depth(8) × 2 GiB`. Bounded (not OOM) and needs a crafted multi-GB nested zip, so it's not reachable by ordinary personal data. Fix when convenient: thread a cumulative "bytes currently buffered" budget through `scan_level`, or lower the default `entry_max_bytes`.
- **`container_chain` is not full-text searchable.** `files_fts` indexes only `filename` and `relative_path`, so an archived file is found by its leaf filename but not by an intermediate nested-archive name in its chain (e.g. searching `photos` won't match `photos.zip › vacation.jpg`). Spec §9 wants archived content searchable; add `container_chain` to the FTS columns + sync triggers in a later pass.
- **Errored archive entry has no touch-protection.** Loose files that error mid-scan are protected from the missing-sweep via `touch_seen` (Phase 1a fix), but an archive whose descent fails after its mtime changed will have its entries swept to `missing` (non-destructive, self-heals on the next successful descent). Analogous to the directory-subtree limitation above.
- **Encapsulation smells (carried):** `scan_volume` builds a second `Config` via `default_paths()` rather than receiving limits from the caller; `upsert_file`/`upsert_archive_entry` share an INSERT…ON CONFLICT skeleton; `descend_archive` has 8 positional params. Fold into the same "store/scanner encapsulation" cleanup as the Phase-1a `pub conn` follow-up.

## Follow-ups logged from the Phase 1c (web browse) code review (2026-07-04)

Shipped clean (all four review findings fixed: read-only per-request catalog open, FTS-token quoting, page error handling, quote-safe escaping). Remaining enhancements:

- **Surface size/date filters in the browse UI.** `SearchFilters` and `/api/search` already accept `min_size`/`max_size`/`modified_after`/`modified_before`, but the page only exposes query + volume + category + status controls. Add size and date-range inputs to the header when wanted. (Date filtering uses `modified_time`, which is NULL for archive entries, so a date filter currently excludes archived content — decide whether that's desired or whether archive entries should inherit the containing archive's mtime.)
- **When the review GUI arrives (Phase 2)**, it will add action endpoints (confirm-duplicate, quarantine) to this same server; those are the first *write* endpoints and will need care (CSRF is minimal on localhost, but the read-only-open split means write handlers must use the read-write `Catalog::open` deliberately). **[Done in 2b: CSRF token guard + read-write open + reversible-only quarantine.]**

## Follow-ups logged from the Phase 2b (review GUI) code review (2026-07-05)

Shipped clean — no Critical issues; the two Important items (partial-failure hiding committed moves; per-member disk enumeration) and the location-formatter divergence were fixed before merge. Remaining minors:

- **Bound the thumbnail decoder.** `thumbnail_jpeg` decodes at full resolution before downscaling with no `image::Limits`. A pathological on-disk image (decompression bomb / huge dimensions) could spike memory during preview. Malformed input already returns `Err`→404 (no panic), and the threat model is the user's own files on localhost, but decoding via `image::ImageReader::…limits(…)` would make "bounded" true end-to-end.
- **Preview doesn't filter `status='active'`.** A quarantined/purged record can still be previewed if its bytes are still readable at `relative_path` (returns 404 once moved). Harmless and arguably useful (preview before confirming removal); add an `active` gate only if strict consistency is wanted.
- **`esc()` and `fmtSize()` are duplicated** across the two inline `<script>` blocks (`INDEX_HTML`/`REVIEW_HTML`). Inherent to two self-contained pages; a shared inlined snippet would stop them drifting.
- **Carried (unchanged by 2b):** extract `const FILE_COLUMNS` for the 16-column file SELECTs; `active_file_id` status filter/rename; purge bookkeeping transaction; rescan-before-quarantine survivor UX; surface "the safe-to-remove copy lives on an unconnected drive"; size/date UI filters on browse; `container_chain` in FTS.

## Follow-ups logged from the Phase 2a (quarantine/purge) code review (2026-07-04)

Shipped clean — the final review's two data-safety findings were fixed before merge (disk-aware survivor check so a stale catalog can't cause last-copy loss; catalog-aware `quarantine_dest` so a purged row's path can't orphan a re-quarantined file). Remaining cleanups:

- **Extract `const FILE_COLUMNS`.** The 16-column `files` SELECT is now duplicated ~6× across `store.rs` (search_filtered, get_file, duplicate_groups, quarantined_rows, active_copies) and `map_file_record` reads by positional `r.get(15)`. Adding a column requires editing every SELECT + the mapper index in lockstep — a runtime-panic trap. Hoist the column list to one const and keep the mapper index-aligned (ideally switch to named-column access).
- **`active_file_id` naming/filter.** It filters `container_chain IS NULL` but not `status='active'` despite the name; safe today only because the loose-identity partial unique index guarantees one loose row per path. Add the status filter or rename to `loose_file_id`.
- **Purge catalog bookkeeping isn't transactional.** `purge_volume`'s `mark_purged` loop + `log_action` are separate statements; wrap them in one transaction so a mid-loop failure can't leave rows marked without an audit entry (not data-loss — the physical delete already completed cleanly before this point).
- **Post-rename transient DB error still aborts a quarantine batch** via `?` (leaves the moved file in `_ToDelete`, recoverable, row reconciled to missing on rescan). Consider catching into the same non-fatal skip path so one flaky write doesn't halt a large batch.
- **UX:** `cmd_purge`'s missing-marker error lacks the "scan the drive first" hint that `cmd_quarantine` gives; `cmd_duplicates` prints a status column that is always "active".
- **Rescan-before-quarantine guidance.** The disk-aware survivor check protects same-drive copies, but a cross-drive survivor on an *unmounted* drive is trusted without verification (it's a genuinely separate physical copy). The review GUI (2b) should surface "the copy that makes this safe to remove lives on «Other Drive» (not currently connected)" so the user decides with full information.


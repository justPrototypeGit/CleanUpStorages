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

## Follow-ups logged from the Phase 2c (archive repack, Case 4) code review (2026-07-06)

Shipped clean — the final review found **no Critical issues** (no data-loss/corruption/last-copy/token-less path); the Important button-gate bug and the diverged `quarantine_dest` were fixed before merge. Remaining minors:

- **Post-swap catalog writes aren't transactional** (`repack_entry` step 10: `mark_quarantined` + `update_archive_hash` + `log_action`). A failure after the on-disk swap leaves the catalog partially updated; it self-heals on the next scan (documented in a code comment now). Wrapping the three in one transaction would make the bookkeeping atomic.
- **`update_archive_hash` doesn't update `modified_time`** — harmless (guarantees the archive re-hashes next scan) but leaves the row's mtime momentarily inconsistent with disk.
- **`available_space` returning `None` skips the pre-flight space check** (now warns). On a near-full drive the failure would then surface at the swap's rename (which rolls back safely) rather than at the guard.
- **`verify_rebuilt` only hash-checks catalogued entries** — un-catalogued entries are preserved byte-for-byte by `raw_copy_file` but not re-hashed (documented in a comment now).
- **Cross-drive scratch location for near-full drives (spec §11)** — 2c builds the temp on the same drive with a pre-flight check; a configurable scratch dir on another disk (with a cross-device build+verify+swap) is deferred.
- **`available_space`/`total_capacity` (sysinfo) duplicated** between `repack.rs` and `volume.rs`; the 16-column file SELECT is now in ~7 methods — fold into shared helpers with the carried `FILE_COLUMNS` item.

## Follow-ups logged from the web-scanning code review (2026-07-07)

Shipped clean — the final review found **no Critical issues** and confirmed a web scan running alongside a quarantine/repack **cannot corrupt or lose data** (WAL + the scanner's 200-file batched commits + the 5s busy-timeout → the worst case is a failed, retryable action). The two cheap improvements (worker integrity-check parity; a page note about the marker file) landed before merge. Remaining fast-follows:

- **Friendlier "scan in progress" error (Important, fast-follow).** While a scan holds the write lock (hashing a big file mid-batch), a concurrent review-page quarantine/repack can surface a raw "database is locked" 500. Either (a) at the top of `api_quarantine`/`api_repack`, if `state.scan_queue.status().running.is_some()`, return `409` with "a scan is in progress — try again when it finishes"; or (b) map `SQLITE_BUSY` to that friendly message; or (c) disable the review-page action buttons while a scan runs (also the deferred "disable actions during scan" item). A deterministic test needs a controllable running state (small test-only seam on `ScanQueue`).
- **Bound / de-duplicate the pending scan queue.** `enqueue` has no cap and no dedup, so a token-holding local script could enqueue unbounded jobs, or the same drive twice (a harmless redundant incremental pass). Low severity (local single-user) — add a small cap and skip enqueuing a path already running/pending.
- **Carried, still open:** concurrent scans; cancel a running/queued scan; per-file live path + ETA; the `esc()`/`$` JS helpers are duplicated across the three self-contained pages (accepted — no frontend build system).

## Follow-ups logged from the Phase 2a (quarantine/purge) code review (2026-07-04)

Shipped clean — the final review's two data-safety findings were fixed before merge (disk-aware survivor check so a stale catalog can't cause last-copy loss; catalog-aware `quarantine_dest` so a purged row's path can't orphan a re-quarantined file).

**Resolved since:** `FILE_COLUMNS` const + named-column mapper and the `active_file_id` → `loose_file_id` rename landed in the PoSD refactor (2026-07-10); purge catalog bookkeeping is now transactional (2026-07-12). Remaining cleanups:

- **Post-rename transient DB error still aborts a quarantine batch** via `?` (leaves the moved file in `_ToDelete`, recoverable, row reconciled to missing on rescan). Consider catching into the same non-fatal skip path so one flaky write doesn't halt a large batch.
- **UX:** `cmd_purge`'s missing-marker error lacks the "scan the drive first" hint that `cmd_quarantine` gives; `cmd_duplicates` prints a status column that is always "active".
- **Rescan-before-quarantine guidance.** The disk-aware survivor check protects same-drive copies, but a cross-drive survivor on an *unmounted* drive is trusted without verification (it's a genuinely separate physical copy). The review GUI (2b) should surface "the copy that makes this safe to remove lives on «Other Drive» (not currently connected)" so the user decides with full information.


## Follow-ups logged from the observability code review (2026-07-07)

Shipped clean — the final review found no Critical/Important issues (verified additive: no behavior/test change, no sensitive data off-machine, exhaustive command match, no hot-loop logging). Applied before merge: plain logs when stderr is redirected; a comment on the command-span nesting caveat. Remaining minor:

- **CaptureWriter test duplication.** The `CaptureWriter`/`CaptureW` test `MakeWriter` is duplicated between the web and scanner test modules (accepted — no shared test-support module yet). *(The CSRF-reject-helper item here was resolved: `check_csrf` was extracted in the PoSD refactor, 2026-07-10.)*
- **Deferred (from the spec, unchanged):** log files / rotation / retention; a log-viewer panel in the web UI; metrics/telemetry export (Prometheus/OpenTelemetry); redaction modes.


## Follow-ups logged from the UI-integration code review (2026-07-10)

Shipped clean — the final whole-branch review found no Critical/Important issues (verified: self-contained across all six pages, CSRF-first on all six mutating handlers, `open_readonly`/`open` split correct, `forget_volume` never touches disk, `purge_all` delegates only to `purge_volume`, XSS-safe via `esc()`/`textContent`, DTO/JS field names all match). One earlier DTO-contract bug (activity feed read wrong field names) was caught and fixed in-branch (`d453511`).

**Resolved since:** the CSRF-reject block was extracted into `check_csrf` and `duplicate_group_count` was aligned to active-only (both PoSD refactor, 2026-07-10); `forget_volume` now logs its audit action inside the delete transaction (2026-07-12). Remaining minor / optional:

- **`api_drives` re-enumerates disks per volume.** It calls `mounts::disk_capacity()` inside the per-volume loop, and each call does `Disks::new_with_refreshed_list()` — O(volumes × all-disks). Harmless for a handful of drives; if drive count grows, add a `disk_capacity` variant taking a pre-refreshed `Disks` list and refresh once per request.
- **Console flag parser requires flag-after-positional** (`scan D:\ --force` works, `scan --force D:\` doesn't). Fails safe (no request, just a usage hint). Improve the tokenizer if console ergonomics matter.


## Follow-ups logged from the Browse-tree code review (2026-07-12)

Shipped clean — the final whole-branch review found no Critical/Important issues (XSS: every
DB-derived interpolation `esc()`-ed or provably numeric; `duplicate_counts` injection-safe + within
the bundled-SQLite variable limit at the 3000 cap; reads stay `open_readonly`; self-contained; no
SHARED_JS collisions; `copies` never 0/1). Optional Minor follow-ups:

- **`◆N` can over-promise on a filtered/capped view.** `copies` is a *global* active-copy count, but
  the tree only shows the search/filter result set (capped at 3000). A file can show `◆2` while only
  one copy is visible, and clicking then highlights just itself. Consider a tooltip nuance ("N copies
  catalogued; not all shown") or counting visible-vs-global separately.
- **Duplicate marker on non-active files.** With a `missing`/`quarantined` status filter, a shown file
  whose hash has >1 *active* copy elsewhere still renders as `.leaf.dup`. Defensible ("this content is
  duplicated"), just a subtle semantic.
- **`countDups` recomputation.** Re-walks each subtree at every render level (~O(n·depth)); harmless at
  the 3000-node cap. If the cap rises, memoize the per-node dup count during `buildTree`.
- **Sibling name collisions in `buildTree`.** A loose file and an archive/folder sharing a name at the
  same level produce two sibling nodes (or the archive reuses a plain-folder node without the 🗜 flag).
  No crash, no file mis-attribution — purely cosmetic.
- **Lazy per-folder loading (from the spec, deferred):** the tree is built from the capped result set;
  a very large catalog would want server-side lazy expansion.


## Follow-ups logged from the drives-fix-and-rename review (2026-07-12)

Shipped clean — the final whole-branch review found no Critical/Important (connectivity change is
detection-only and marker-gated at two layers: `resolve_live` includes a remembered path only when
its marker still equals the volume_id, AND the quarantine/purge/repack engines each re-check the
marker before touching disk; schema additive/idempotent; CSRF-first; self-contained + XSS-safe).
The "rename bumps last_seen_at" minor was fixed in-branch (62c75dc). Remaining optional minors:

- **`rename` audit entry logs raw request params, not applied values.** A partial update logs
  `"display_name": null` (reads as "cleared" when it was left untouched), and untrimmed whitespace is
  logged though the stored value is trimmed. Audit cosmetics only — log the cleaned/applied values and
  skip fields passed as `None`.
- **No-op / unknown-volume rename silently "succeeds."** `cus rename <mount>` with no flags writes a
  no-change `rename` row; `POST /api/rename-drive` against an unknown volume_id updates 0 rows and
  returns `{name: <id>}` as if OK. Harmless; could 404/short-circuit.
- **`api_drives` is N+1** (per-volume `volume_meta`/`volume_last_seen`/`volume_has_scan_errors` +
  `snapshot()`'s readonly open). Fine at realistic drive counts; batch if it ever matters.
- **No automated test for the concurrent readonly-inside-write-handler pattern** (`snapshot()` opens a
  readonly connection while a handler holds a write `Catalog`). WAL + 5s busy_timeout make it safe;
  tests use `MountResolver::Fixed` which never opens the second connection. The sandbox walkthrough in
  the testing guide is the only guard.

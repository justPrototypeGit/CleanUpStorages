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


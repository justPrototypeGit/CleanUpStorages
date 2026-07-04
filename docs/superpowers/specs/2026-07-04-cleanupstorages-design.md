# CleanUpStorages — Design Spec

**Date:** 2026-07-04
**Status:** Approved design (pre-implementation)

## 1. Problem & goal

The user has thousands of GB of important, irreplaceable data (mixed personal + academic) spread across
multiple external HDDs. The data is messy: duplicated, duplicated-and-renamed, disorganized, and much of it
compressed into zip archives (including nested zips) to save space. Most HDDs are near-full.

The user wants to:

1. **Catalog** — build a persistent, searchable "register" of everything they own across every drive, so they
   can find data without hunting through folders or even knowing in advance what exists.
2. **Deduplicate** — find duplicates and remove them, keeping the original (with all its metadata), after
   per-item human confirmation via a visual review tool ("Tinder for duplicates" + WinMerge-style compare).
3. **Reorganize** — later, arrange the cleaned data into a well-organized structure, always with confirmation.

**Overriding constraint: reliability. Nothing may ever be lost or corrupted.** Every design decision below is
subordinate to this. The tool never performs an irreversible destructive action on its own.

## 2. Tech stack

- **Language:** Rust — compiled to a single static binary per platform (Windows `.exe`, macOS arm64). No
  interpreter/runtime to break at runtime. Memory-safe, and `Result`-based error handling forces every I/O and
  hash operation to explicitly handle its failure path (no unhandled exceptions crashing mid-scan, no data
  races when hashing is parallelized).
- **Catalog:** embedded SQLite (single `catalog.db` file), stored **on the user's computer, not on the HDDs**,
  so the register never consumes scarce drive space.
- **Review GUI:** embedded local web server (`axum`) serving plain HTML/CSS/JS. Binds to `127.0.0.1` only — no
  network exposure, no separate frontend build system, no deploy. Opened in the user's normal browser.
- **Hashing:** BLAKE3 — fast, SIMD/parallel, cryptographically strong (collisions between different content are
  not a practical concern), streamed in chunks so huge files never load fully into memory.
- **Target platforms:** Windows (primary dev machine) and Apple MacBook Air (macOS arm64).

## 3. Architecture

One Rust project, three logical components sharing the SQLite catalog as the single source of truth:

1. **Scanner (CLI)** — crawls a mounted drive, hashes files, extracts metadata, writes to the catalog.
   Incremental, resumable, and safe to re-run/interrupt.
2. **Catalog (SQLite)** — permanent record of every file ever seen across every drive and session, plus the
   audit trail. Duplicate groups are derived (files sharing a `content_hash`), not a separately-maintained table.
3. **Review server (embedded web UI)** — started on demand; the user reviews and confirms duplicate decisions
   visually. Nothing destructive happens without explicit confirmation.

Illustrative CLI shape:

```
cleanupstorages scan <path>          # crawl + hash + update catalog (incremental)
cleanupstorages scan <path> --force  # re-hash everything, ignore the skip fast-path
cleanupstorages review               # start local server, open browser review UI
cleanupstorages status               # stats: files, duplicate groups, recoverable space per volume
cleanupstorages purge <volume>       # deliberately empty a volume's quarantine to reclaim space
```

## 4. Volume identity (cross-session, drive-letter-independent)

On first sight of a drive, the scanner writes a small hidden marker file to its root
(e.g. `.cleanupstorages_id`) containing a generated UUID. Every future scan reads that marker to recognize the
same physical drive regardless of the letter/mount point the OS assigns (E: today, F: tomorrow). Fully
cross-platform; avoids inconsistent OS volume-serial APIs.

**Read-only / write-protected / failing drives (can't write the marker):** the tool never silently fails or
mis-identifies. When it cannot write the marker, it **pauses and asks the user per drive**:

- **Proceed read-only** — identify the drive by a computed **fingerprint** (OS volume serial + total filesystem
  size + label) instead of a marker. The catalog records that this volume is *marker-less, identified by
  fingerprint* (slightly less robust than a marker across reformats, but never blocks a scan). The drive is
  never written to.
- **Skip this drive** — leave it uncatalogued for now.

For unattended/batch scanning, a CLI flag can pre-answer this (e.g. `--readonly-fallback=fingerprint|skip`) so
a run over many read-only drives isn't interrupted. Absent the flag, the default is to ask interactively.

## 5. Catalog data model

- **`volumes`** — `volume_id` (UUID from marker), `label`, `first_seen_at`, `last_seen_at`.
- **`files`** — `id`, `volume_id`, `relative_path`, `filename`, `extension`, `size_bytes`, `content_hash`,
  `created_time`, `modified_time`, `accessed_time`, `category` (photo/document/academic/video/other, derived
  from extension), `first_seen_at`, `last_seen_at`, `status` (see §7). For archive entries: a `container_chain`
  (ordered list of archives to open to reach the entry) instead of a plain path.
- **`scan_errors`** — append-only log of anything unreadable/skipped (permission denied, bad sectors, encrypted
  archive, max-depth exceeded, zip-bomb ratio exceeded) so a single bad file never aborts a scan and nothing is
  silently forgotten.
- **`actions_log`** — append-only audit trail of every consequential action (scan run, quarantine move,
  confirmation, repack, purge): old path/status → new path/status, timestamp, and context. The authoritative
  "what did the tool do, and when" record.

## 5a. Catalog durability (the register is the crown jewel)

The catalog is the only record of what lives on drives that aren't currently plugged in, so its own integrity is
treated as critically as the user's files:

- **WAL mode:** SQLite runs in write-ahead-logging mode for crash-resilient writes — an interrupted write
  (crash, power loss, laptop sleep) cannot leave the database structurally corrupted.
- **Auto-backup snapshots:** the tool saves a timestamped copy of the catalog **after each scan** and
  **before every bulk/destructive operation** (quarantine batches, Case-4 repacks, purges), into a local
  `catalog.backups/` directory (on the computer, not the HDDs). It keeps the last **N** snapshots (configurable)
  and prunes older ones. If the live DB is ever corrupted or a bad operation slips through, the user can roll
  back to the latest good snapshot.
- **Integrity check:** the tool runs a quick `PRAGMA integrity_check` on startup and after bulk operations; a
  failure surfaces immediately with guidance to restore from the latest snapshot.

## 6. Scanning behavior

- **Incremental (default):** for each file, compare cheap metadata (size + modified time) against the catalog
  for that volume+path. If unchanged, skip re-hashing. New/changed files are hashed and upserted.
- **Force (`--force`):** skip the fast-path entirely and re-hash every file, still upserting into the same rows
  (no duplicate entries). For use after filesystem repair or if a skip is ever suspected of missing a change.
- **Resumable:** writes commit in small batches, not one giant transaction. An interrupted scan (drive
  unplugged, crash, sleep) loses at most the last few seconds; re-running resumes where it left off.
- **Non-fatal errors:** unreadable files are logged to `scan_errors` and the scan continues.

## 7. File status lifecycle (nothing is ever deleted from the catalog)

Rows are never removed; only `status` changes, preserving each file's origin (volume + original path) forever.

- **`active`** — found on the most recent scan of its volume.
- **`missing`** — previously catalogued, not found on the volume's last scan (deleted/moved outside the tool, or
  gone). We still know where it was and where it came from.
- **`quarantined`** — the tool moved it to `_ToDelete/…` after user confirmation; the quarantine path is
  recorded.
- **`purged`** — a later scan finds the quarantine slot empty (user emptied it), confirming it is truly gone.

Every transition is also written to `actions_log`, so a file's full lifecycle is always reconstructable.

## 8. Duplicate detection

Duplicates are files sharing the same `content_hash`, computed on demand — loose files, archive entries at any
nesting depth, and whole archives all participate in the same hash-matching space. Detection is exact
(byte-for-byte via BLAKE3) in the first phase.

**Near-duplicates** (e.g. a re-compressed photo, a re-saved document) are a **later phase**, and are **never**
auto-actioned — each near-duplicate candidate must be confirmed one-by-one by the user in the review GUI before
anything happens.

## 9. Archives (zip), including nested

The scanner descends into archives at **any nesting depth**. Each entry at every level is catalogued with its
full `container_chain` (e.g. `outer.zip › photos_2019.zip › vacation.jpg`) and content-hashed via streaming
(without extracting the whole tree to disk), so archived content participates in duplicate matching exactly like
loose files.

**Recursion safety limits (all recorded to `scan_errors` when hit, never silently skipped):**

- **Max depth** (configurable, default ~8): deeper nesting logs `max archive depth exceeded` and stops
  descending that branch.
- **Zip-bomb protection:** an entry whose decompressed size exceeds a ratio/absolute cap is logged and skipped,
  not expanded.
- **Unreadable/encrypted archive:** logged (e.g. `password-protected`) and skipped; scan continues.

## 10. Duplicate action policy (soft-delete only; never irreversible)

When the user confirms a duplicate in the review GUI, the loser is **moved to a same-drive `_ToDelete`
quarantine folder** (a rename — instant, no extra space), never hard-deleted. The tool never calls the final
irreversible delete; the user empties quarantine manually (`purge`) when fully comfortable. Every confirmation
and move is logged.

The following cases define what the tool proposes and records. **Actual destructive/quarantine actions only ever
touch whole top-level files or the explicit, verified repack in Case 4 — never blind edits of archive internals.**

- **Case 1 — identical whole archives:** two `.zip` with the same content hash → ordinary duplicate files,
  quarantined normally after confirmation.
- **Case 2 — a loose file is fully contained in an archive:** the loose copy can be quarantined; the archived
  copy stays. Tool recommends and records.
- **Case 3 — a file exists only inside archives, or an archive is fully redundant with loose files:** tool
  reports/advises only, takes no destructive action (e.g. "every file in `old_backup.zip` also exists as loose
  active files; consider deleting the whole archive"). User decides; the decision is logged.
- **Case 4 — identical item inside two *different* archives** (neither whole archive is redundant; the user
  wants to slim one down): a crash-safe, verified **repack**, never an in-place edit. Steps, all recorded:
  1. **Pre-check:** confirm an identical copy survives elsewhere and is `active`. Refuse to remove the copy if no
     verified surviving copy exists — a last copy is never removed.
  2. **Quarantine the removed item's content:** extract just that entry to `_ToDelete/…` (same drive,
     recoverable), so even the removed entry survives as a loose file.
  3. **Build a new archive as a temp file** (`*.rebuilding.tmp`), streaming every *other* entry with its
     metadata preserved. The original is untouched.
  4. **Verify:** re-open the temp archive and re-hash every retained entry against the catalog. If any entry
     fails to verify, or the process is interrupted, the temp is discarded and the original is left completely
     untouched — nothing lost.
  5. **Atomic swap, original preserved:** only after verification passes, move the original archive to
     quarantine (`_ToDelete/…/<name>.original.zip`, recoverable), then move the verified temp into place.
  6. **Record:** `actions_log` captures source archive, removed entry's `container_chain`, the surviving copy it
     was verified against, both quarantine locations, and a before/after entry count. Catalog: removed entry →
     `quarantined`; a new row represents the rebuilt archive (new whole-file hash) with a link back to the
     original.

  Case 4 is opt-in per case in the review GUI, never automatic. Net effect: two independent safety nets (the
  extracted item *and* the untouched original archive both in quarantine), verification before any swap, and a
  full audit record.

**Nested-zip note:** the tool will never automatically repack a *nested* archive to remove an inner entry; such
cases are reported for the user to handle deliberately.

## 11. Storage management (near-full drives)

- **Quarantining is a same-drive rename → ~zero extra space.** Flagging/quarantining duplicates never risks
  filling a drive. But it also doesn't *free* space until quarantine is emptied.
- **Space is reclaimed only on deliberate `purge`, per drive**, when the user is confident. `status` reports
  per-volume "recoverable space: X GB in `_ToDelete`" so the user knows exactly what they'll get back before
  committing. The tool never auto-empties quarantine.
- **Pre-flight free-space checks** guard the only space-hungry operation (Case-4 repack). If the drive is too
  full, the tool won't touch it — it either (a) uses a **configurable scratch location** on a drive with space
  (MacBook internal disk or another HDD) to build/verify the temp archive then swaps it back, or (b) instructs
  the user to purge some quarantine first. It never drives a disk to 100% full.
- **Catalog lives on the computer, not the HDDs**, so the register never eats scarce drive space.
- **Suggested flow for full drives:** scan → review/quarantine (free) → check recoverable space → purge that
  drive to reclaim → then there's headroom for repacks/reorganization. Reclaim early, work incrementally, one
  drive at a time.

## 12. Register / search interface

The catalog is only useful if the user can find things in it (goal #1: "find data without knowing what I
have"). Search is exposed **two ways**, sharing the same catalog:

- **Web UI search/browse screen** (in the same local web app as review): free-text match on
  filename/path/keyword, with filters for drive/volume, `category`, date range, size, and `status`. Results
  show the file's location — drive label + path, or full `container_chain` for archived items — **even for
  drives not currently plugged in** (the catalog persists), and clearly flag `missing`/`quarantined` items.
- **CLI search** (`cleanupstorages search <query> [--category …] [--volume …] [--status …]`): prints matching
  files and their locations to the terminal for quick lookups without a browser.

Both read the same catalog and present the same location info (including archive nesting and offline-drive
results). Backed by a SQLite full-text index over filename/path for responsive search.

## 13. Review GUI

Local web page served at `127.0.0.1:PORT`. Duplicate groups are queued for review. Per group: a WinMerge-style
side-by-side compare (image thumbnails/preview for photos, metadata diff for everything, full `container_chain`
for archived items) and a Tinder-style decision (keep-which / quarantine-which / skip). The proposed
"original to keep" defaults to the earliest creation time / most complete metadata, always user-overridable.
Nothing is destructive until confirmed; every confirmation writes to `actions_log`.

## 14. Phasing

1. **Phase 1 — Catalog + exact dedup detection + search:** scanner (incl. recursive archives), catalog,
   duplicate report, and the register search (CLI `search` + web browse screen). Useful on its own — the full
   searchable inventory is available immediately, before any dedup review exists.
2. **Phase 2 — Review GUI + soft-delete/quarantine + Case 1–4 workflows.**
3. **Phase 3 — Reorganization** into a well-organized structure, always with confirmation. Target structure is
   deliberately **not** decided yet; the catalog captures enough metadata (category, dates, source volume,
   origin) to decide the scheme when we get there.

## 15. Configuration & defaults

All tunables live in a config file (with CLI overrides). Sensible defaults, all adjustable:

| Setting | Default | Notes |
| --- | --- | --- |
| Catalog location | `catalog.db` in the app's data dir on the computer | Never on the HDDs. |
| Snapshot retention (`N`) | 10 | Number of `catalog.backups/` snapshots kept before pruning. |
| Max archive nesting depth | 8 | Deeper branches logged to `scan_errors`, not descended. |
| Zip-bomb ratio cap | e.g. 100× decompressed:compressed, + absolute size cap | Exceeding → logged + skipped. |
| Review server bind | `127.0.0.1`, auto-selected free port | Localhost only, no network exposure. |
| Read-only marker fallback | ask interactively | Overridable via `--readonly-fallback=fingerprint|skip`. |
| Scratch location (Case-4 repack) | none until set; else pre-flight space check on the drive | Set to a drive with free space for repacks on near-full disks. |
| Category mapping | extension → photo/document/academic/video/other | Default mapping, user-editable. |

## 16. Explicitly out of scope (for now)

- Near-duplicate (non-exact) detection — deferred to a later phase, always human-confirmed.
- Automatic repacking of *nested* archives to remove inner entries — reported only.
- Deciding the Phase-3 target folder taxonomy.

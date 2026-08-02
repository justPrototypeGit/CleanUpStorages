# Scan-error visibility as a completeness audit — design

**Status:** approved
**Date:** 2026-07-31
**Closes:** #6 (surface scan-error details in the UI)
**Epic:** #2 (scan control & visibility)

## Why

A scan continues past per-file failures: an unwalkable directory, a file whose metadata or bytes
cannot be read (permission denied, locked by another process, I/O error). Each one is written to
`scan_errors` and the scan moves on. **The file is simply not catalogued.**

Today a drive shows a "had scan errors" pill and nothing more — no list, no reason, no count. With
~16 TB about to be catalogued under a constraint that nothing may ever be lost, a silently
uncatalogued file is a hole in the catalogue's completeness guarantee, and there is currently no way
to find one.

So this is not a log viewer. The question it answers is **"can I trust this catalogue as complete,
and if not, what is missing?"**

## What is already true (and therefore constrains the design)

Investigated before designing:

- **`scan_errors` is append-only and never pruned.** Only `forget` deletes rows
  (`catalog/store.rs:574`). `volume_has_scan_errors` therefore returns true if anything *ever*
  failed, so the pill latches on permanently even after the cause is fixed.
- **Every scan appends a new row for the same failing path.** A path that fails on twenty scans
  leaves twenty rows. The table grows without bound.
- **Errors are not linked to a scan run.** There is no `scan_run_id`. There is an accidental handle —
  `occurred_at` is passed the scan's `now`, which equals `scan_runs.started_at` — but that is a
  coincidence of the call site, not a designed relationship.
- **The failure modes are not equivalent.** On a `read:` error the scanner calls `touch_seen`
  (`scanner.rs:211`), so a *previously catalogued* file keeps its row with a now-stale hash rather
  than being swept to `missing`. A file that has never been catalogued has no row at all. Unreadable
  directories are excluded from the sweep. The genuine hole is **new files that error**.
- **There is no index on `scan_errors.volume_id`.**

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Primary job | **Completeness audit**, not a log | The question worth answering is whether the catalogue can be trusted, not what scrolled past |
| What it claims | **Split "absent" from "unverified"** | An uncatalogued file and a stale-hash file are different risks; one bucket would overstate one and hide the other |
| Resolution | **Per-path self-heal** | Each path resolves independently, so stop/resume needs no special casing and the panel always shows current truth |
| Classification | **Store `kind` at record time**, from `io::ErrorKind` | Windows `io::Error` messages are localized by the OS — the dev machine is Italian — so matching message text would misclassify on the very machine this is built for |
| Surfaces | **Web panel + CLI summary** | Long scans are run from the CLI; the answer must be visible without opening a browser |
| History | **Not kept** | Self-heal discards resolved errors. An audit trail is a different feature with a different cost |

## Architecture

### Data model

Two columns added with the existing idempotent helper (`schema.rs:196`, the same mechanism that
added `original_path`):

```
phase TEXT   -- walk | metadata | read | archive_open | archive_entry
kind  TEXT   -- permission | locked | not_found | io | other
```

`kind` is derived from `std::io::ErrorKind` plus the raw OS error code **at the moment the error is
recorded** — never parsed from `reason`. `reason` is unchanged and remains the human-readable detail.

Two corrections to existing behaviour that this design requires regardless:

- **`UNIQUE(volume_id, path)`**, with the insert becoming an upsert, so a repeatedly-failing path
  holds one row instead of one per scan.
- **An index on `volume_id`**, which does not exist today.

Rows written before this change have `kind IS NULL` and render as *"recorded before classification"*.
There is no backfill: `ErrorKind` cannot be recovered from a localized message, and inventing a
classification would be worse than admitting the gap. Those rows clear themselves on the next scan.

### Self-heal: two rules, because directories are not files

**File errors** (`metadata`, `read`, `archive_open`) clear in one set-based statement at the end of a
scan:

```sql
DELETE FROM scan_errors
 WHERE volume_id=?1 AND phase IN ('metadata','read','archive_open')
   AND path IN (SELECT relative_path FROM files
                WHERE volume_id=?1 AND last_seen_at >= ?2)
```

These three phases all record a plain file path, so they join to `files.relative_path` directly.
`archive_open` qualifies because it records the archive file's own path (`scanner.rs:401`).

This deliberately reuses the mechanism behind the missing-file sweep, and inherits its key property:
**a stopped scan clears only the paths it actually re-reached**, because paths the walk never visited
never had `last_seen_at` bumped. Stop and resume therefore need no special handling here — the same
timestamp that makes the sweep safe makes this safe.

**`walk` and `archive_entry` errors** cannot use that rule, for the same underlying reason: neither
records a path that exists in `files`. A walk error records a *directory*; an archive-entry error
records a composite `archive.zip › inner/path` (`scanner.rs:428`) that is deliberately not a
`relative_path`. Both therefore clear only at the end of a **completed** (not stopped) scan that did
not re-record them — only a completed scan proves the location was visited and is now readable. A
stopped scan never clears either.

### What the audit computes — three buckets, not two

A `LEFT JOIN` from `scan_errors` to `files` on `(volume_id, path)` splits the file-path rows, and
`phase='walk'` is reported separately:

- **absent** — a file with no active `files` row. Invisible to search and to deduplication. This is
  the real completeness hole. Archive-entry errors always land here: the entry was never catalogued,
  even though its containing archive was.
- **unverified** — a `files` row exists from an earlier scan, but this scan could not re-read the
  file, so its stored hash may no longer match its contents. This matters because a stale hash can
  pair the wrong files during duplicate review.
- **unreadable directories** — a directory the walk could not open. Reported as its own bucket and
  **never counted as one missing file**, because the number of files beneath an unopenable directory
  is unknown. Counting it as 1 would be the most misleading number this feature could print: a single
  denied `System Volume Information` and a denied folder holding 40,000 photos would look identical.
  The UI says "contents unknown" rather than inventing a count.

### Surfaces

`GET /api/volumes/:id/errors?bucket&kind&limit&offset` — read-only, so no CSRF token, consistent with
the other read endpoints. Returns both buckets plus totals, paged.

**Drives page**: a per-volume panel grouped by `kind`, paged. The "had scan errors" pill stops
latching and becomes real counts.

**CLI**: a completeness line per volume in `status`, and a summary printed at the end of `scan`:

```
Completeness: 12 files NOT catalogued, 35 unverified, 2 unreadable directories (contents unknown).
```

When all three counts are zero the line reads `Completeness: complete.` — a positive statement is
what makes the absence of a warning trustworthy rather than merely quiet.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **Self-heal clears an error for a path the scan never reached** — the serious one, same class as the sweep bug | The delete is keyed on `last_seen_at >= scan_started_at`; a dedicated regression test stops a scan mid-tree and asserts unreached paths keep their errors |
| Classification breaks on a non-English OS | `kind` comes from `io::ErrorKind`, never message text; a test constructs `io::Error` values from `ErrorKind` directly and asserts classification without touching any string |
| Walk and archive-entry errors never clear, because neither path exists in `files` | Explicit second rule: cleared by a *completed* scan that does not re-record them |
| An unreadable directory is counted as one missing file, understating a hole of unknown size | Reported as its own bucket labelled "contents unknown"; never folded into the file count |
| The error list is unbounded on a badly failing drive | Paged endpoint, grouped counts shown before any list |
| Existing unclassified rows look like a bug | Rendered explicitly as "recorded before classification"; they self-clear on the next scan |

## Non-goals

- **No retry / targeted re-scan** of failed paths. That needs a scanner entry point that does not
  exist, and is its own issue if wanted.
- **No error history or audit trail.** Self-heal discards resolved errors by design.
- No change to what the scanner treats as an error, or to how it recovers from one.
- No change to hashing, quarantine, purge or repack.

## Success criteria

1. After a scan, both the CLI and the Drives page report, per volume, how many files are **absent**
   from the catalogue, how many are **unverified**, and how many directories were **unreadable** —
   and say so positively when there are none.
2. Fixing a cause and re-scanning clears those errors without any manual step.
3. A **stopped** scan clears errors only for the paths it actually re-reached; a regression test
   proves it.
4. Errors are grouped by cause correctly on a non-English Windows locale.
5. A path that fails on many consecutive scans holds exactly one row.
6. Existing scanner and catalogue tests pass unmodified.

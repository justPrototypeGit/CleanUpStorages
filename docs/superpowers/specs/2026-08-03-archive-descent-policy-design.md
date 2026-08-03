# Archive descent policy and settings hardening — design

**Status:** approved
**Date:** 2026-08-03
**Closes:** the residual findings from the #41/#42 pre-merge review (F-A, F-B, the byte-units trap)
and the descent-scope question deferred from that branch.
**Epic:** #21 (scan performance — 20 TB must be practical)

## Why

The catalogue is about to be wiped and rebuilt by a scan of ~20 TB taking five days or more.
Everything here must be settled **before** that scan, because each item either changes *what gets
catalogued* or can silently mis-catalogue during the run — and both cost a second five-day pass to
correct.

Three things came out of the #41/#42 review that were deliberately not fixed there.

**Detection by content is broader than intended.** #42 replaced extension-based archive detection
with zip magic bytes, which was right for its purpose — it stopped macOS AppleDouble sidecars being
probed as archives, and it found zips renamed to other extensions. But *every* zip-format file now
descends: `.docx`, `.xlsx`, `.pptx`, `.jar`, `.apk`, `.epub`, `.odt`. Each becomes a folder of parts
in the catalogue. On a mixed personal-and-academic 20 TB corpus that is plausibly millions of extra
rows, and a Duplicates page full of `[Content_Types].xml` clusters. Reversing it later means a full
re-scan.

**A settings file can silently cripple a scan.** Validation lives only in the `POST /api/settings`
handler. `load_settings` accepts any well-typed value, so a hand-edited `settings.json` — the route
the README itself recommends — can set a 0-byte entry ceiling. Confirmed live during the review:
`largest entry 0 bytes` produced `2 errors, 2 newly missing` with nothing wrong on disk.

**The limits UI invites a units mistake.** The fields take raw bytes. Typing `64` meaning gigabytes
sets a **64-byte** ceiling, which validation accepts because the only lower bound is `>= 1`.

**And one residual data path.** F-A: when top-level detection cannot open a file, it falls back to
the *extension* test. An archive not named `.zip` whose content also changed therefore loses its
entries, and cannot self-heal — only `--force` recovers it.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Descent scope | **Deny-list of container formats**, configurable | A renamed zip and a `.docx` are indistinguishable by magic alone, so the rule must be explicit rather than implied by a policy name |
| Unrecognised zip-format extensions | **Not descended, and reported for the user's decision** | A five-day unattended scan cannot prompt. Conservative by default, but never a silent guess |
| Settings validation | **Also at load: warn and fall back per field** | A preferences file must not be able to convert present files to `missing` |
| Byte fields in the UI | **Show the human value beside the raw one** | Makes a units mistake visible before saving rather than after a five-day scan |
| F-A fallback | **`has_archive_entries` from the catalogue** | The catalogue already knows the file is an archive; the filename does not |

## Architecture

### The descent rule

Evaluated in this order, for a top-level file that has just been hashed:

| condition | outcome |
| --- | --- |
| no zip magic bytes | leaf |
| extension on the deny-list | leaf — a known container, not reported |
| extension `zip`, or on the allow-list | **descend** |
| otherwise (zip magic, unfamiliar extension) | leaf, **and recorded for the user's decision** |

The deny-list is checked **first**, so it always wins. That ordering is deliberate: if a user ever
adds `zip` to the deny-list they mean it, and a rule that silently ignored them would be worse than
one that obeys a choice they can see and undo.

Default deny-list: `docx xlsx pptx docm xlsm pptm jar apk war ear epub odt ods odp nupkg vsix ipa`.
Both lists live in `settings.json` and are editable from the Scan page, so the deny-list can be
extended without a release.

The AppleDouble fix from #42 is unaffected: `._Video.zip` has no zip magic, so it never reaches the
extension rules at all.

**Nested entries inside an archive** are not seekable, so the tail-EOCD check cannot run on them —
they keep the head-magic test introduced in #42. The extension rules above then apply to the entry's
own name exactly as they do at the top level, so a `.docx` stored inside a backup zip is catalogued
as one entry rather than exploded into its parts. An unfamiliar zip-format extension found *inside*
an archive is recorded in the same pending table, with the containing volume's id.

The one asymmetry: a **prefixed** zip nested inside another archive is not detected, because the tail
scan needs seeking. That was already true before this design and is unchanged by it.

### Reporting unrecognised formats

Zip-based formats keep appearing (`.kra`, `.sketch`, `.3mf`, `.usdz`). A fixed deny-list silently
rots; a report does not. So an unfamiliar zip-format extension is recorded rather than guessed at:

```sql
CREATE TABLE pending_archive_formats (
    extension    TEXT NOT NULL,
    volume_id    TEXT NOT NULL,
    count        INTEGER NOT NULL,
    total_bytes  INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    PRIMARY KEY (extension, volume_id)
);
```

Upserted during the scan, surfaced on the Scan page:

```
Unrecognised zip-format files found
  .bak    12 files   4.2 GB   [Descend into these]  [Treat as documents]
  .kra     3 files   0.1 GB   [Descend into these]  [Treat as documents]
```

**Descend** adds the extension to the allow-list; **Treat as documents** adds it to the deny-list.
Either action removes every row for that extension, across all volumes. Newly-allowed extensions are
picked up by a re-scan of those paths; #6's completeness audit already reports what changed.

Rows are stored **per volume** so a drive that is not currently connected still shows what it holds,
but the Scan page **aggregates across volumes** — the decision is about a file format, not about one
drive. `forget <mount>` deletes that volume's rows alongside its files and scan errors, so a dropped
drive does not leave phantom formats behind.

### Settings validation at load

`load_settings` gains the same range checks the `POST` handler applies, with one deliberate
difference: **it never rejects the whole file.** Each invalid field logs a warning naming the field
and the reason, and falls back to the compiled-in default for that field alone. A malformed
preferences file must not be able to stop a five-day scan — the existing best-effort contract from
#41 stands.

Shared rules, so the two paths cannot drift:

- `archive_buffer_max_bytes`, `archive_total_buffer_bytes`: `>= 1`, and `buffer <= total`
- `archive_entry_max_bytes`: `>= 1` when present; unlimited is `null`, never `0`
- `max_archive_depth`, `archive_ratio_cap`: `>= 1`

The memory-ceiling check (25% of RAM) stays in the POST path only: it depends on the machine the UI
is running on, and a catalogue carried to a smaller machine should not have its stored settings
silently rewritten.

### Byte fields in the UI

Each byte-valued input renders the human equivalent beside it, updating as you type:

```
Largest file in an archive (bytes)
[ 68719476736                    ]  = 64.0 GB
```

A mistyped `64` reads `= 64 B` before the user saves. This is a display aid, not a parser — the field
still submits raw bytes, so nothing about the API contract changes.

### F-A: the detection-failure fallback

When `File::open` fails at the detection step, fall back to `has_archive_entries` from the catalogue
rather than to the extension test. The catalogue already records that this path has archive entries,
which is exactly the question being asked, and it is right for renamed zips and `.docx` alike.

Where no catalogue row exists (a new file that cannot be opened), the extension test remains the last
resort. The scan error introduced in #41 continues to be logged either way.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **The deny-list makes the scan silently skip content the user wanted** | Nothing is skipped: `.docx` and friends are still catalogued as ordinary files, just not descended into. Only descent changes |
| An unfamiliar zip format is silently not descended | It is recorded and reported. Silence is the specific thing this design rejects |
| The pending-formats table grows unbounded on a messy drive | Keyed on `(extension, volume_id)`, so it is bounded by the number of distinct extensions, not files |
| Load-time validation rejects a whole settings file and stops a scan | Per-field fallback with a warning; the file is never rejected as a whole. Tested |
| The two validation paths drift apart | One shared set of rules, called from both |
| Users read the human hint as an input format | It is rendered outside the input as static text; the field still takes raw bytes |

## Non-goals

- No change to hashing, quarantine, purge or repack.
- No re-scan orchestration when an extension is approved — a normal re-scan picks it up, and #6's
  completeness audit reports the difference.
- No content-based container detection (reading entry names to spot `[Content_Types].xml`). It would
  catch a `.docx` renamed to `.bak`, but costs a central-directory read per zip-format file on 20 TB.
  The report covers the same ground at no scan cost.
- No change to the memory-ceiling rule or to where it applies.

## Success criteria

1. `.docx`, `.xlsx`, `.jar` and `.epub` are catalogued as ordinary files and **not** descended into.
2. A zip renamed to an unfamiliar extension is **not** descended into, and **is** reported on the
   Scan page with its count and size.
3. Approving a reported extension descends into those files on the next scan; dismissing it stops the
   report and never descends.
4. `._Video.zip` is still catalogued as an ordinary file and produces no archive error.
5. A `settings.json` containing `archive_entry_max_bytes: 0` produces a warning and the default
   ceiling — not `2 newly missing`.
6. A malformed settings file still never stops a scan; per-field fallback is used.
7. The UI shows `= 64.0 GB` beside `68719476736`, and `= 64 B` beside `64`.
8. A detection open failure on a catalogued archive no longer loses its entries, whatever the file is
   named.
9. Existing archive, scanner and settings tests pass unmodified.

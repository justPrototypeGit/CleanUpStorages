# Archive limits and detection — design

**Status:** approved
**Date:** 2026-08-02
**Closes:** #41 (archive limits are not configurable, and the ratio cap rejects legitimate files),
#42 (7 archives fail to open: AppleDouble sidecars and corrupt nested zips)
**Epic:** #21 (scan performance — 20 TB must be practical)

## Why

The catalogue is about to be wiped and rebuilt by a scan of ~20 TB taking five days or more.
Whatever the scanner accepts from archives must therefore be right **before** that scan: getting it
wrong means either silently omitting files or paying another multi-day pass to correct it.

Querying the live catalogue after #6 made scan errors visible showed the current rules are wrong in
both directions.

**Every ratio rejection was a false positive.** All 12 entries refused as `zip bomb: ratio N exceeds
cap 200` were ordinary compressible data:

| ratio | what it actually is |
| --- | --- |
| 815 (×2) | `design_1_wrapper.bit` — a Vivado FPGA bitstream; long runs of zeros |
| 735 | MRI scan recovery data |
| 337 (×2) | Final Cut Pro audio peak data inside a `.fcpbundle` |
| 215–243 | Python venv `site-packages`; a zedboard PWM project |

**Four more were refused on size**, one declaring 34 GB — and those were *streamed* leaf files, which
cost constant memory, so refusing them bought nothing.

**Four "unreadable archives" were never archives.** `._Video.zip` and friends are macOS AppleDouble
sidecars, matched only because `is_archive_name` tests the file extension.

## What the guards actually do today

Reading the code before designing, because it changes the shape of the fix:

- `archive_entry_max_bytes` does **two unrelated jobs**. For a nested archive
  (`archive.rs:190`) it bounds a `Vec` held in RAM — a genuine memory limit. For a leaf file
  (`archive.rs:256`) it is passed to `hash_capped`, which streams in 64 KiB chunks at constant
  memory — so there the cap protects nothing and merely refuses to catalogue large files.
- The declared-size and ratio checks (`archive.rs:162-178`) run **before** the archive/leaf split,
  using sizes the zip merely claims. For leaves both are re-checked properly downstream.
- `hash_capped` already defends against a zip that **under-declares** its sizes, by counting actual
  bytes. This, not the ratio cap, is what makes lying headers safe.
- `Config` has all four fields and `ArchiveLimits::from_config` reads them, but
  `Config::default_paths()` hardcodes the values in **both** construction paths
  (`config.rs:25-28`, `39-42`). Nothing can supply a different value.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Limit shape | **Split by what each protects** | One number cannot serve as both a RAM bound for buffered archives and a size ceiling for streamed files |
| Leaf ceiling | **64 GB default, configurable, `null` = unlimited** | Clears everything in the catalogue (largest rejection declared 34 GB) while still bounding worst-case hashing time |
| Ratio cap | **10 000** | Far above real data (max observed 815), far below a real bomb (~1 000 000:1) |
| Archive detection | **Magic bytes, not extension** | Fixes the AppleDouble false positives *and* finds renamed zips, which are missed entirely today |
| Metadata files | **Catalogue them; never skip** | `._*`, `.DS_Store`, `Thumbs.db` become ordinary small files. Nothing is excluded, so the never-lose-anything rule keeps no exceptions |
| Configuration | **`settings.json`, editable from the web UI** | Persistent, so a resumed scan uses the same limits as the run it continues; the UI carries the explanations JSON cannot |
| Bad settings file | **Warn and use defaults — never fatal** | A malformed settings file must not be able to stop a five-day scan |

## Architecture

### The limits, split by what they protect

```
MEMORY — real RAM, keep tight
  archive_buffer_max_bytes    2 GB   one nested archive buffered at a time
  archive_total_buffer_bytes  2 GB   all live buffers across one descent
  max_archive_depth           8      recursion

CATALOGUE — what we are willing to record
  archive_entry_max_bytes     64 GB  largest leaf file (null = unlimited)

TIME — runaway decompression
  archive_ratio_cap           10000  declared uncompressed / compressed
```

Today's `archive_entry_max_bytes` becomes two fields: the nested-archive path keeps a genuine memory
bound as `archive_buffer_max_bytes`, and the leaf path gets its own ceiling.

**The ratio cap gains a clearer purpose.** With a generous leaf ceiling it is the guard against a
genuine bomb streaming for a long time before its size cap trips. It bounds *time*, not memory, and
that is how it should be documented — the current comment calls it a bomb guard without saying which
resource it protects, which is why 200 looked defensible.

### Detection by content

`is_archive_name` is replaced by a magic-byte test for the zip signatures `PK\x03\x04` (local file
header), `PK\x05\x06` (empty archive) and `PK\x07\x08` (spanned).

Entries **inside** an archive are not seekable, so the first bytes cannot be peeked and rewound. The
reader is reconstructed instead:

```rust
let mut peek = [0u8; 4];
let n = read_up_to(&mut entry, &mut peek)?;
let reader = std::io::Cursor::new(&peek[..n]).chain(entry);
```

so the hash still sees the whole stream. Files on disk are seekable, so they need no such trick.

A file shorter than 4 bytes cannot be a zip and is treated as a leaf without error.

### The incremental-skip path must not open files

`is_archive_name` has two callers in the scanner, and they have opposite constraints:

- **`scanner.rs:327`**, after hashing. The file has just been opened and read in full, so peeking
  its first bytes costs nothing.
- **`scanner.rs:245`**, the incremental skip. This path deliberately **never opens the file** — that
  is what makes a resumed scan fast-forward 225,285 files in 25 s rather than an hour. Detecting by
  magic bytes here would force an open per candidate on every rescan and destroy that.

It is also a correctness problem, not only a performance one. Once renamed zips are detected, they
have archive entries; if the skip path fails to recognise them it will not call
`touch_archive_entries`, their entries keep an old `last_seen_at`, and the missing-file sweep marks
present files as `missing`. That is the exact failure class this project cannot tolerate.

**The scanner therefore stops guessing and asks the catalogue.** Whether a file is an archive is
already known — it has rows with `container_chain IS NOT NULL` — and `get_file_meta` already reads
that same row on the skip path. It gains one more column:

```rust
// (size_bytes, modified_time, has_archive_entries)
pub fn get_file_meta(&self, volume_id: &str, relative_path: &str)
    -> anyhow::Result<Option<(i64, i64, bool)>>
```

computed with an `EXISTS` sub-select in the query the skip path already runs, so there is no extra
round trip and no extra statement. The skip path then calls `touch_archive_entries` based on a
recorded fact rather than on a filename, which is strictly more correct than today.

### Settings

`settings.json`, beside `catalog.db` in the data directory, auto-created with defaults on first run.

Loading is **best-effort by design**: a missing file is normal, and a corrupt or partially-invalid
file logs a warning and falls back to defaults for the fields it cannot read. It is never an error.
Losing a settings value is a preference; failing to open the catalogue is a stopped scan.

Unknown fields are ignored, so a settings file written by a newer build does not break an older one.

### UI

- `GET /api/settings` — effective values.
- `POST /api/settings` — **CSRF-guarded**, like every other write endpoint.

A collapsible **Archive limits** section on the existing Scan page, rather than a seventh page. It
states plainly that changes apply to the **next** scan, not a running one.

Validation refuses what would defeat the limits' purpose:

- `archive_total_buffer_bytes` and `archive_buffer_max_bytes` are refused above **25% of total system
  memory**, read via `sysinfo` (already a dependency). A quarter leaves room for the OS file cache,
  which the scan depends on, and for the web server running in the same process during `browse`. If
  total memory cannot be determined, the value is accepted with a warning rather than blocked — an
  undeterminable machine should not be an unusable one.
- `archive_buffer_max_bytes` may not exceed `archive_total_buffer_bytes` — a per-archive bound above
  the whole descent's budget is meaningless.
- `max_archive_depth` must be at least 1.
- `archive_ratio_cap` must be at least 1.
- Rejections explain which limit was violated and why, and the stored file is left unchanged.

### CLI

No new verb. `scan` prints its effective archive limits as it starts, so the values in force are
visible before committing days of work — where the information actually matters.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| A raised leaf ceiling lets one pathological entry hash for hours | The ratio cap bounds it, and 64 GB bounds the honest worst case to roughly 9–30 minutes at measured throughput |
| Magic-byte detection misses an archive the extension check caught | Zip files always begin with a `PK` signature; a file whose extension says `.zip` but whose bytes do not is, by definition, not a zip — that is the bug being fixed. A test covers a renamed zip and a mislabelled non-zip |
| Peek-and-chain corrupts the hash of an archive entry | A test hashes the same content through both the chained and unchained paths and asserts the digests match |
| **The skip path stops recognising an archive, so the sweep marks its entries `missing`** — the serious one | The skip path reads `has_archive_entries` from the catalogue instead of guessing from the filename; a regression test catalogues a renamed zip, re-scans without changes, and asserts its entries stay `active` |
| Magic-byte detection slows the fast-forward by opening files | It is only applied where the file is already open and read; the skip path never opens anything. A rescan of an unchanged tree must stay in the tens of seconds |
| A user sets a buffer budget that exhausts RAM | Validated against real system memory before being stored |
| A corrupt settings file stops a scan | Loading is best-effort: warn, use defaults, continue. Tested |
| Metadata files add noise to duplicate review | Accepted: they are small and rank last by reclaimable bytes. Filtering is #27's problem, not this spec's |

## Non-goals

- No change to hashing, quarantine, purge or repack.
- No re-scan orchestration: #6's self-heal already clears these errors once the entries read
  successfully, so a normal re-scan is sufficient.
- No settings beyond the archive limits. The file is introduced for these; other options join it when
  they exist.
- No fix for the 3 genuinely corrupt nested zips in #42 — no code change can recover damaged bytes.
  They stay reported, which is correct.

## Success criteria

1. The 12 previously-rejected entries (ratios 215–815) are catalogued.
2. The 4 entries rejected on size, including the 34 GB one, are catalogued.
3. `._Video.zip` and its siblings are catalogued as ordinary files and produce no archive error.
4. A zip renamed to a non-`.zip` extension is detected and descended into.
5. All limits are readable and editable from the web UI, CSRF-guarded, with invalid values refused
   and explained.
6. A corrupt `settings.json` produces a warning and default limits, not a failure.
7. `scan` prints the effective limits at start.
8. A renamed zip that has been catalogued survives an unchanged re-scan with its entries still
   `active` — the skip path recognises it from the catalogue, not from its name.
9. A re-scan of an unchanged tree stays in the tens of seconds; magic-byte detection does not open
   files on the skip path.
10. Existing archive tests pass unmodified.

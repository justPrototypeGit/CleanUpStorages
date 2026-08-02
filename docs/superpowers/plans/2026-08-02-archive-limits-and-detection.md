# Archive Limits and Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the scanner silently omitting legitimate files from archives, and let the user set the limits from the web UI.

**Architecture:** The single `archive_entry_max_bytes` is split into the two unrelated things it currently conflates — a RAM bound for buffered nested archives, and a size ceiling for streamed leaf files. Archive detection moves from the file extension to the zip magic bytes, with the incremental-skip path reading the fact from the catalogue rather than opening files. Limits move into a `settings.json` that the Scan page can edit.

**Tech Stack:** Rust, rusqlite/SQLite, `zip` crate, axum 0.7, plain HTML/CSS/JS (no build step), `serde_json` (already a dependency), `sysinfo` (already a dependency).

## Global Constraints

- **Nothing may ever be lost or corrupted.** ~20 TB of irreplaceable data. This plan exists because the scanner was losing files.
- **No new crates.** `serde`, `serde_json`, `sysinfo`, `zip`, `clap` are already present and sufficient.
- **A bad `settings.json` must never be fatal.** Missing, corrupt, or partially-invalid: warn, use defaults for what cannot be read, continue. Losing a preference is acceptable; failing to open the catalogue is a stopped five-day scan.
- **The incremental-skip path must never open a file.** That is what makes a resumed scan fast-forward 225,285 files in ~25 s instead of an hour.
- **Write endpoints are CSRF-guarded** (`check_csrf`, `src/web.rs:290`); `GET` endpoints are not. The server binds `127.0.0.1` only — do not change binding, auth or CORS.
- The web UI is plain HTML/CSS/JS in a Rust string: **no CDN, no runtime font fetch, no build step.** A test asserts each page contains no `http://` or `https://`.
- Every interpolated value in the UI goes through `esc()` — paths and reasons are untrusted filesystem text.
- Gates for every task: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Commit trailers on every commit:
  ```
  Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
  Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
  ```

## Known pre-existing issue

`scanner::tests::run_scan_logs_volume_resolution` is flaky under parallel execution (#39) and predates this branch. If it fails, re-run. Do not fix it here.

## Test-design warning that applies to several tasks

The existing helper `make_zip` (`src/archive.rs`, in `mod tests`) writes entries with
`CompressionMethod::Stored` — **no compression, so every entry it produces has a ratio of 1.** A
ratio test built on it cannot fail no matter what the cap is. Task 1 adds `make_zip_deflated` for
this reason; use it wherever compression ratio matters.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/archive.rs` (modify) | Split `ArchiveLimits`; magic-byte detection; peek-and-chain for non-seekable entries |
| `src/config.rs` (modify) | `Settings` struct, best-effort load/save of `settings.json`, defaults |
| `src/catalog/store.rs` (modify) | `get_file_meta` also reports whether the file has archive entries |
| `src/scanner.rs` (modify) | Detect top-level archives by content; skip path uses the catalogue fact |
| `src/web.rs` (modify) | `GET`/`POST /api/settings` with validation |
| `src/web_ui.rs` (modify) | "Archive limits" section on the Scan page |
| `src/commands.rs` (modify) | `scan` prints effective limits |

---

### Task 1: Split the limits and fix the defaults

**Files:**
- Modify: `src/archive.rs` (the `ArchiveLimits` struct ~line 10, `from_config` ~line 21, the entry checks ~lines 162-178, the nested branch ~line 190, the leaf branch ~line 256, and `mod tests`)
- Modify: `src/config.rs:8-13, 25-28, 39-42`

**Interfaces:**
- Produces:
  ```rust
  pub struct ArchiveLimits {
      pub max_depth: usize,
      pub buffer_max_bytes: u64,          // was entry_max_bytes, nested-archive path only
      pub entry_max_bytes: Option<u64>,   // leaf files; None = unlimited
      pub ratio_cap: u64,
      pub total_buffer_bytes: u64,
  }
  ```
- `Config` fields become: `max_archive_depth: usize`, `archive_buffer_max_bytes: u64`, `archive_entry_max_bytes: Option<u64>`, `archive_ratio_cap: u64`, `archive_total_buffer_bytes: u64`.
- Defaults: depth 8, buffer 2 GB (`2147483648`), entry `Some(68719476736)` (64 GB), ratio `10000`, total buffer 2 GB.

**Why this task exists:** `archive_entry_max_bytes` currently does two unrelated jobs. On the nested path (`archive.rs:190`) it bounds a `Vec` held in RAM — a real memory limit. On the leaf path (`archive.rs:256`) it is handed to `hash_capped`, which streams 64 KiB at a time in constant memory — so there it protects nothing and merely refuses to catalogue large files. That is why a 34 GB entry was rejected for no benefit.

- [ ] **Step 1: Add a deflating zip helper to `mod tests`**

`make_zip` uses `Stored`, so it cannot produce a ratio above 1. Add beside it:

```rust
    // Deflated, so entries have a real compression ratio. `make_zip` stores uncompressed, which
    // pins every ratio at 1 and makes ratio-cap tests silently unable to fail.
    fn make_zip_deflated(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, bytes) in files {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }
```

- [ ] **Step 2: Write the failing tests**

```rust
    #[test]
    fn a_highly_compressible_file_is_catalogued_not_rejected() {
        // 400 KB of zeros deflates to a few hundred bytes -- a ratio in the high hundreds, which
        // is what a Vivado bitstream or an MRI export actually looks like. Under the old cap of
        // 200 every one of these was silently dropped from the catalogue.
        let zip = make_zip_deflated(&[("bitstream.bit", &vec![0u8; 400 * 1024])]);
        let res = scan_archive(Cursor::new(zip), &limits());
        assert_eq!(res.errors, Vec::<(String, String)>::new(), "no entry should be refused");
        assert_eq!(res.entries.len(), 1);
        assert_eq!(res.entries[0].filename, "bitstream.bit");
        assert_eq!(res.entries[0].size_bytes, 400 * 1024);
    }

    #[test]
    fn an_absurd_ratio_is_still_refused() {
        // The cap still has a job: with a generous leaf ceiling it is what stops a real bomb
        // streaming for a long time. A tiny cap proves the check is reachable at all.
        let zip = make_zip_deflated(&[("bomb.bin", &vec![0u8; 400 * 1024])]);
        let tight = ArchiveLimits { ratio_cap: 2, ..limits() };
        let res = scan_archive(Cursor::new(zip), &tight);
        assert!(res.entries.is_empty(), "the entry must not be catalogued");
        assert_eq!(res.errors.len(), 1);
        assert!(res.errors[0].1.contains("ratio"), "got {:?}", res.errors[0].1);
    }

    #[test]
    fn a_leaf_file_larger_than_the_buffer_bound_is_still_catalogued() {
        // The leaf path streams in constant memory, so the nested-archive buffer bound must not
        // apply to it. This is the 34 GB rejection, in miniature.
        let zip = make_zip(&[("big.mov", &vec![7u8; 64 * 1024])]);
        let small_buffer = ArchiveLimits {
            buffer_max_bytes: 1024,       // far smaller than the entry
            total_buffer_bytes: 1024,
            entry_max_bytes: None,        // unlimited leaf ceiling
            ..limits()
        };
        let res = scan_archive(Cursor::new(zip), &small_buffer);
        assert_eq!(res.errors, Vec::<(String, String)>::new());
        assert_eq!(res.entries.len(), 1);
        assert_eq!(res.entries[0].size_bytes, 64 * 1024);
    }

    #[test]
    fn a_leaf_ceiling_when_set_is_enforced() {
        let zip = make_zip(&[("big.mov", &vec![7u8; 64 * 1024])]);
        let capped = ArchiveLimits { entry_max_bytes: Some(1024), ..limits() };
        let res = scan_archive(Cursor::new(zip), &capped);
        assert!(res.entries.is_empty());
        assert_eq!(res.errors.len(), 1);
    }
```

Update the existing `limits()` helper to the new shape, and `limits_from_config` to the new defaults:

```rust
    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_depth: 8,
            buffer_max_bytes: 2 * 1024 * 1024 * 1024,
            entry_max_bytes: Some(64 * 1024 * 1024 * 1024),
            ratio_cap: 10_000,
            total_buffer_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
```

```rust
    #[test]
    fn limits_from_config() {
        let cfg = Config::default_paths().unwrap();
        let l = ArchiveLimits::from_config(&cfg);
        assert_eq!(l.max_depth, 8);
        assert_eq!(l.buffer_max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(l.entry_max_bytes, Some(64 * 1024 * 1024 * 1024));
        assert_eq!(l.ratio_cap, 10_000);
        assert_eq!(l.total_buffer_bytes, 2 * 1024 * 1024 * 1024);
    }
```

- [ ] **Step 3: Run them and watch them fail**

Run: `cargo test --lib archive::`
Expected: compile errors — `buffer_max_bytes` does not exist, `entry_max_bytes` is not an `Option`.

- [ ] **Step 4: Implement**

In `src/archive.rs`, the struct and `from_config`:

```rust
/// Tunable limits for archive descent, grouped by what each one actually protects.
#[derive(Debug, Clone)]
pub struct ArchiveLimits {
    /// Recursion bound.
    pub max_depth: usize,
    /// MEMORY: the most one nested archive may hold in RAM. Nested archives must be buffered so
    /// they can be both hashed and re-opened with `Seek` to recurse, so this is a real bound.
    pub buffer_max_bytes: u64,
    /// MEMORY: bytes of nested-archive buffer live at once across a whole descent.
    pub total_buffer_bytes: u64,
    /// CATALOGUE: the largest leaf file we will record. `None` is unlimited, and safe: leaves are
    /// stream-hashed in 64 KiB chunks, so their size costs no memory.
    pub entry_max_bytes: Option<u64>,
    /// TIME: declared uncompressed/compressed. With a generous leaf ceiling this is what stops a
    /// genuine bomb streaming for a long time before its size cap trips.
    pub ratio_cap: u64,
}

impl ArchiveLimits {
    pub fn from_config(cfg: &Config) -> ArchiveLimits {
        ArchiveLimits {
            max_depth: cfg.max_archive_depth,
            buffer_max_bytes: cfg.archive_buffer_max_bytes,
            total_buffer_bytes: cfg.archive_total_buffer_bytes,
            entry_max_bytes: cfg.archive_entry_max_bytes,
            ratio_cap: cfg.archive_ratio_cap,
        }
    }
}
```

Replace the pre-split declared-size check (~line 162). The declared size must no longer refuse leaf
files; only the ratio check stays common to both branches:

```rust
        // Ratio is checked for both branches: it is the cheap pre-filter that stops us buffering
        // or streaming something absurd. Declared sizes can lie, which is why `read_capped` and
        // `hash_capped` re-check the real byte counts downstream.
        if uncompressed / compressed > limits.ratio_cap {
            result.errors.push((
                chain,
                format!(
                    "zip bomb: ratio {} exceeds cap {}",
                    uncompressed / compressed,
                    limits.ratio_cap
                ),
            ));
            continue;
        }
```

In the nested branch (~line 190) use the new name:

```rust
            let cap = limits.buffer_max_bytes.min(*budget);
```

and in the same branch's error message, replace `limits.entry_max_bytes` with
`limits.buffer_max_bytes`.

In the leaf branch (~line 256), apply the optional ceiling:

```rust
            // `u64::MAX` when unlimited: `hash_capped` still counts real bytes, so a lying header
            // cannot escape -- there is simply no ceiling to trip.
            let cap = limits.entry_max_bytes.unwrap_or(u64::MAX);
            match hash_capped(&mut entry, cap) {
```

In `src/config.rs`, both construction paths (lines 25-28 and 39-42) and the struct:

```rust
    pub max_archive_depth: usize,
    pub archive_buffer_max_bytes: u64,
    pub archive_total_buffer_bytes: u64,
    /// None = unlimited. Leaf files stream in constant memory, so a ceiling here bounds time, not
    /// memory.
    pub archive_entry_max_bytes: Option<u64>,
    pub archive_ratio_cap: u64,
```

```rust
            max_archive_depth: 8,
            archive_buffer_max_bytes: 2 * 1024 * 1024 * 1024,
            archive_total_buffer_bytes: 2 * 1024 * 1024 * 1024,
            archive_entry_max_bytes: Some(64 * 1024 * 1024 * 1024),
            archive_ratio_cap: 10_000,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the ratio test discriminates**

Set `ratio_cap: 200` in the `limits()` helper, run
`cargo test --lib a_highly_compressible_file_is_catalogued_not_rejected`.
Expected: FAIL — the 400 KB of zeros is refused, reproducing the original bug. Restore `10_000` and
confirm green. Report both outputs.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/archive.rs src/config.rs
git commit -m "fix(archive): split the limits by what each protects, and raise the ratio cap

archive_entry_max_bytes did two unrelated jobs: a real RAM bound for
buffered nested archives, and a pointless ceiling on leaf files that
stream in constant memory. A 34 GB entry was refused for no benefit.

Every ratio rejection in the live catalogue was a false positive --
FPGA bitstreams, MRI exports, Final Cut peak data -- against a cap of
200. The cap is now 10000, far above real data and far below a real
bomb.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Detect archives by content, inside archives

**Files:**
- Modify: `src/archive.rs` (`is_archive_name` ~line 31, the branch at ~line 187, `mod tests`)

**Interfaces:**
- Consumes: `ArchiveLimits` from Task 1.
- Produces:
  ```rust
  /// True if the first bytes are a zip signature.
  pub fn looks_like_zip(prefix: &[u8]) -> bool;
  /// Read up to 4 bytes without consuming them from the caller's perspective.
  fn peek4<R: Read>(r: &mut R) -> std::io::Result<(Vec<u8>, bool)>;  // (bytes, is_zip)
  ```
- `is_archive_name` is **deleted**; nothing may keep using it.

**Why:** `._Video.zip` is a macOS AppleDouble sidecar, not a zip. It matched only because
`is_archive_name` tests the extension. The same change finds zips that were renamed, which are
missed entirely today.

**The constraint:** entries inside a zip are **not** seekable, so the first bytes cannot be peeked
and rewound. Read them, then chain them back in front of the rest so the hash still sees the whole
stream.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn zip_signatures_are_recognised_by_content_not_name() {
        assert!(looks_like_zip(b"PK\x03\x04rest"));
        assert!(looks_like_zip(b"PK\x05\x06"));       // empty archive
        assert!(looks_like_zip(b"PK\x07\x08"));       // spanned
        assert!(!looks_like_zip(b"\x00\x05\x16\x07")); // AppleDouble magic
        assert!(!looks_like_zip(b"PK"));               // too short to be a signature
        assert!(!looks_like_zip(b""));
    }

    #[test]
    fn an_applesdouble_sidecar_named_zip_is_treated_as_a_leaf() {
        // ._Video.zip is macOS metadata about Video.zip. Probing it as an archive is what produced
        // "invalid Zip archive: Could not find EOCD" against a file that was never a zip.
        let sidecar = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        ";
        let zip = make_zip(&[("._Video.zip", sidecar)]);
        let res = scan_archive(Cursor::new(zip), &limits());
        assert_eq!(res.errors, Vec::<(String, String)>::new(), "it is not an archive, so no archive error");
        assert_eq!(res.entries.len(), 1, "it is catalogued as an ordinary file");
        assert_eq!(res.entries[0].filename, "._Video.zip");
        assert_eq!(res.entries[0].size_bytes, sidecar.len() as i64);
    }

    #[test]
    fn a_zip_renamed_to_another_extension_is_still_descended_into() {
        // Missed entirely today: the extension check says no, so its contents were never catalogued.
        let inner = make_zip(&[("inner.txt", b"hello")]);
        let outer = make_zip(&[("backup.bak", &inner)]);
        let res = scan_archive(Cursor::new(outer), &limits());
        let names: Vec<&str> = res.entries.iter().map(|e| e.filename.as_str()).collect();
        assert!(names.contains(&"inner.txt"), "expected to descend into the renamed zip, got {names:?}");
    }

    #[test]
    fn peeking_does_not_change_an_entrys_hash() {
        // The peeked bytes must be chained back, or every entry that survives detection hashes
        // four bytes short -- silently wrong content hashes, which is unrecoverable at dedup time.
        let body = b"the quick brown fox jumps over the lazy dog";
        let zip = make_zip(&[("plain.txt", body)]);
        let res = scan_archive(Cursor::new(zip), &limits());
        let expected = {
            let mut r: &[u8] = body;
            hashing::hash_reader(&mut r).unwrap()
        };
        assert_eq!(res.entries.len(), 1);
        assert_eq!(res.entries[0].content_hash, expected, "hash must cover the whole entry");
        assert_eq!(res.entries[0].size_bytes, body.len() as i64);
    }
```

Delete the now-meaningless `detects_zip_names` test.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib archive::`
Expected: FAIL to compile — `looks_like_zip` does not exist.

- [ ] **Step 3: Implement**

Replace `is_archive_name` in `src/archive.rs`:

```rust
/// True if these leading bytes carry a zip signature.
///
/// By content, not by extension: `._Video.zip` is a macOS AppleDouble sidecar that merely borrows
/// the name, and a zip renamed to `.bak` is still a zip. The extension lies in both directions.
pub fn looks_like_zip(prefix: &[u8]) -> bool {
    matches!(
        prefix,
        [b'P', b'K', 0x03, 0x04, ..] | [b'P', b'K', 0x05, 0x06, ..] | [b'P', b'K', 0x07, 0x08, ..]
    )
}

/// Read up to 4 leading bytes from a non-seekable stream, reporting whether they look like a zip.
/// The bytes are returned so the caller can chain them back -- dropping them would silently
/// truncate the content hash.
fn peek4<R: Read>(r: &mut R) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    let head = buf[..filled].to_vec();
    let is_zip = looks_like_zip(&head);
    Ok((head, is_zip))
}
```

Note the loop: a single `read` may return fewer than 4 bytes from a decompressing reader without
being at EOF, so a naive `read` would misclassify a real zip as a leaf.

At the branch (~line 187), peek first and chain the bytes back:

```rust
        let (head, is_zip) = match peek4(&mut entry) {
            Ok(v) => v,
            Err(e) => {
                result.errors.push((chain, format!("read error: {e}")));
                continue;
            }
        };
        // Chain the peeked bytes back in front, so both branches below see the entire entry.
        let mut entry = std::io::Cursor::new(head).chain(entry);

        if is_zip {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the chaining test discriminates**

Temporarily drop the chain — use `entry` directly instead of `Cursor::new(head).chain(entry)` — and
run `cargo test --lib peeking_does_not_change_an_entrys_hash`.
Expected: FAIL on both the hash and the size, four bytes short. Restore and confirm green. Report
both outputs; a silently truncated hash is unrecoverable at dedup time, so this test is the one that
matters most in this task.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/archive.rs
git commit -m "fix(archive): detect archives by magic bytes, not by extension

macOS AppleDouble sidecars (._Video.zip) are not zips and stop being
probed as archives; zips renamed to another extension are now found,
which the extension check missed entirely.

Entries inside a zip are not seekable, so the peeked bytes are chained
back in front of the stream -- dropping them would truncate every
content hash by four bytes.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: The scanner — top-level detection, and a skip path that never opens files

**Files:**
- Modify: `src/catalog/store.rs:45-60` (`get_file_meta`)
- Modify: `src/scanner.rs:240-251` (the skip path) and `:327` (the descend decision)
- Test: `src/scanner.rs` `mod tests`, `src/catalog/store.rs` `mod tests`

**Interfaces:**
- Consumes: `looks_like_zip` from Task 2.
- Produces: `get_file_meta` returns `Option<(i64, i64, bool)>` — `(size_bytes, modified_time, has_archive_entries)`.

**This is the reliability-critical task. Read this before writing code.**

`is_archive_name` had two callers with opposite constraints:

- **`scanner.rs:327`**, after hashing. The file is already open and fully read, so reading its first
  bytes is cheap and correct.
- **`scanner.rs:245`**, the incremental skip. This path **must never open the file** — not opening is
  precisely what lets a resumed scan fast-forward 225,285 files in ~25 s instead of an hour.

It is also a correctness problem. Once renamed zips are detected (Task 2), they have archive entries.
If the skip path fails to recognise such a file it will not call `touch_archive_entries`, those
entries keep an old `last_seen_at`, and `mark_missing_scanned` **marks present files as `missing`.**
That is the failure class this project cannot tolerate.

So the skip path stops guessing from the filename and reads the recorded fact. `get_file_meta`
already selects that exact row, so it gains one `EXISTS` sub-select — no extra query, no extra
round trip.

- [ ] **Step 1: Write the failing tests**

In `src/catalog/store.rs` `mod tests`:

```rust
    #[test]
    fn get_file_meta_reports_whether_the_file_has_archive_entries() {
        let (_t, cat) = open();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        })
        .unwrap();
        let mk = |rel: &str, chain: Option<&str>| crate::catalog::models::NewFile {
            volume_id: "v".into(),
            relative_path: rel.into(),
            filename: rel.rsplit('/').next().unwrap().into(),
            extension: "zip".into(),
            size_bytes: 10,
            content_hash: "H".into(),
            created_time: Some(1),
            modified_time: Some(5),
            accessed_time: Some(1),
            category: crate::category::Category::Other,
            container_chain: chain.map(|c| c.to_string()),
        };
        cat.upsert_file(&mk("plain.bin", None), 1).unwrap();
        cat.upsert_file(&mk("bundle.bak", None), 1).unwrap();
        cat.upsert_file(&mk("bundle.bak", Some("inner.txt")), 1).unwrap();

        let (_, _, plain_has) = cat.get_file_meta("v", "plain.bin").unwrap().unwrap();
        let (_, _, bundle_has) = cat.get_file_meta("v", "bundle.bak").unwrap().unwrap();
        assert!(!plain_has, "a loose file has no archive entries");
        assert!(bundle_has, "an archive's own row must report that it has entries");
        assert!(cat.get_file_meta("v", "absent.bin").unwrap().is_none());
    }
```

In `src/scanner.rs` `mod tests`:

```rust
    #[test]
    fn a_renamed_zip_keeps_its_entries_active_across_an_unchanged_rescan() {
        // THE regression for this task. Once archives are detected by content, a renamed zip has
        // entries. If the skip path does not recognise it, those entries keep an old last_seen_at
        // and the sweep marks present files missing -- silent data loss in the catalogue.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();

        // A real zip, deliberately NOT named .zip.
        let inner = {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut zw = zip::ZipWriter::new(&mut buf);
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                zw.start_file("inside.txt", opts).unwrap();
                std::io::Write::write_all(&mut zw, b"payload").unwrap();
                zw.finish().unwrap();
            }
            buf.into_inner()
        };
        std::fs::write(root.join("archive.bak"), &inner).unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();

        let entries_active = || -> i64 {
            cat.conn
                .query_row(
                    "SELECT count(*) FROM files WHERE container_chain IS NOT NULL AND status='active'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(-1)
        };
        assert_eq!(entries_active(), 1, "the renamed zip's entry is catalogued");

        // Second scan, nothing changed on disk: the skip path runs, then the sweep.
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let stop2 = crate::scan_control::StopFlag::new();
        let s = scan_volume_with_progress(&cat, &root, &ident(), false, 300, None, &m2, &stop2).unwrap();

        assert_eq!(s.marked_missing, 0, "nothing on disk changed, so nothing may be marked missing");
        assert_eq!(
            entries_active(), 1,
            "the archive entry must still be active after an unchanged rescan"
        );
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib get_file_meta_reports_whether`
Expected: FAIL to compile — `get_file_meta` returns a 2-tuple.

- [ ] **Step 3: Implement `get_file_meta`**

In `src/catalog/store.rs`:

```rust
    /// `(size_bytes, modified_time, has_archive_entries)` for a loose file, or None if unknown.
    ///
    /// The third field exists so the incremental-skip path never has to open a file to learn
    /// whether it is an archive. Guessing from the filename would be wrong for a renamed zip, and
    /// a skip path that fails to touch an archive's entries lets the sweep mark present files
    /// missing.
    pub fn get_file_meta(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<Option<(i64, i64, bool)>> {
        let row = self.conn.query_row(
            "SELECT size_bytes, IFNULL(modified_time,0),
                    EXISTS(SELECT 1 FROM files e
                            WHERE e.volume_id=?1 AND e.relative_path=?2
                              AND e.container_chain IS NOT NULL)
               FROM files
              WHERE volume_id=?1 AND relative_path=?2 AND container_chain IS NULL",
            params![volume_id, relative_path],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)? != 0)),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
```

- [ ] **Step 4: Implement the scanner changes**

At `src/scanner.rs:240`, destructure the third field and use it:

```rust
            match cat.get_file_meta(&identity.volume_id, &rel)? {
                Some((old_size, old_mtime, has_archive_entries))
                    if old_size == size && old_mtime == mtime.unwrap_or(0) =>
                {
                    cat.touch_seen(&identity.volume_id, &rel, now)?;
                    // From the catalogue, not from the filename: a renamed zip has entries too,
                    // and missing them here would let the sweep mark present files missing.
                    if has_archive_entries {
                        cat.touch_archive_entries(&identity.volume_id, &rel, now)?;
                    }
                    true
                }
                _ => false,
            }
```

At `src/scanner.rs:327`, decide by content. The file is already open at this point in the flow, so
reading four bytes is cheap:

```rust
        // By content, not by name. Cheap here because the file has just been hashed, so it is warm
        // in the OS cache -- unlike the skip path above, which must never open anything.
        let is_archive = {
            use std::io::Read;
            std::fs::File::open(path)
                .and_then(|mut f| {
                    let mut head = [0u8; 4];
                    let mut filled = 0;
                    while filled < 4 {
                        match f.read(&mut head[filled..])? {
                            0 => break,
                            n => filled += n,
                        }
                    }
                    Ok(archive::looks_like_zip(&head[..filled]))
                })
                .unwrap_or(false)
        };
        if is_archive {
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the regression test discriminates — mandatory**

Replace `if has_archive_entries` with `if rel.ends_with(".zip")`, reproducing the old filename-based
behaviour, and run
`cargo test --lib a_renamed_zip_keeps_its_entries_active_across_an_unchanged_rescan`.
Expected: FAIL — the entry is no longer active after the second scan, and/or `marked_missing` is
non-zero. Restore and confirm green. **Report both outputs.** A test that cannot catch this is
worthless, because this is the data-loss path.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/store.rs src/scanner.rs
git commit -m "fix(scanner): detect archives by content, and keep the skip path file-free

The incremental skip must never open a file -- that is what makes a
resumed scan fast-forward in seconds. It now reads whether a file has
archive entries from the catalogue, via an EXISTS in the query it
already runs, instead of guessing from the filename.

Without this, a renamed zip's entries would not be touched on a rescan
and the missing-file sweep would mark present files missing.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `settings.json`, loaded best-effort

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` `mod tests`

**Interfaces:**
- Consumes: the `Config` fields from Task 1.
- Produces:
  ```rust
  #[derive(serde::Serialize, serde::Deserialize, Default)]
  pub struct Settings {
      pub max_archive_depth: Option<usize>,
      pub archive_buffer_max_bytes: Option<u64>,
      pub archive_total_buffer_bytes: Option<u64>,
      /// Outer None = "not set, use the default"; inner None = "explicitly unlimited".
      pub archive_entry_max_bytes: Option<Option<u64>>,
      pub archive_ratio_cap: Option<u64>,
  }
  impl Config { pub fn settings_path(&self) -> PathBuf; }
  pub fn load_settings(path: &Path) -> Settings;   // never fails
  pub fn save_settings(path: &Path, s: &Settings) -> anyhow::Result<()>;
  ```

**Why every field is `Option`:** an absent field means "use the default", so a settings file written
by an older build, or one the user has trimmed, still works. Unknown fields are ignored by serde's
default behaviour, so a file from a newer build does not break an older one either.

**Loading never fails.** Missing file, unparseable JSON, wrong types — warn and return defaults. A
five-day scan must not be stoppable by a malformed preferences file.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_missing_settings_file_yields_defaults_without_error() {
        let t = tempfile::tempdir().unwrap();
        let s = load_settings(&t.path().join("settings.json"));
        assert!(s.archive_ratio_cap.is_none(), "absent means 'use the default'");
    }

    #[test]
    fn a_corrupt_settings_file_yields_defaults_rather_than_failing() {
        // A malformed preferences file must never be able to stop a five-day scan.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, b"{ this is not json at all ").unwrap();
        let s = load_settings(&p);
        assert!(s.archive_ratio_cap.is_none());
    }

    #[test]
    fn settings_round_trip_and_partial_files_are_honoured() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        // Only one field set; everything else must stay "use the default".
        std::fs::write(&p, br#"{"archive_ratio_cap": 5000}"#).unwrap();
        let s = load_settings(&p);
        assert_eq!(s.archive_ratio_cap, Some(5000));
        assert!(s.max_archive_depth.is_none());

        let written = Settings { archive_ratio_cap: Some(1234), ..Default::default() };
        save_settings(&p, &written).unwrap();
        assert_eq!(load_settings(&p).archive_ratio_cap, Some(1234));
    }

    #[test]
    fn an_explicitly_unlimited_leaf_ceiling_survives_a_round_trip() {
        // Some(None) means "the user chose unlimited" and must not collapse into "unset" -- serde
        // maps a JSON null onto the OUTER Option unless double_option intervenes, so this test is
        // what proves that attribute is present and working.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        save_settings(&p, &Settings { archive_entry_max_bytes: Some(None), ..Default::default() }).unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, Some(None));

        // And an unset field must NOT come back as "explicitly unlimited".
        save_settings(&p, &Settings::default()).unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, None);
    }

    #[test]
    fn settings_override_the_defaults_in_config() {
        let t = tempfile::tempdir().unwrap();
        std::env::set_var("CLEANUPSTORAGES_DATA_DIR", t.path());
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_ratio_cap": 777, "max_archive_depth": 3}"#).unwrap();
        let cfg = Config::default_paths().unwrap();
        std::env::remove_var("CLEANUPSTORAGES_DATA_DIR");
        assert_eq!(cfg.archive_ratio_cap, 777);
        assert_eq!(cfg.max_archive_depth, 3);
        assert_eq!(cfg.archive_buffer_max_bytes, 2 * 1024 * 1024 * 1024, "unset fields keep the default");
    }
```

**Note on the last test:** it mutates a process-global environment variable, and the existing
`defaults_are_sane` test also calls `Config::default_paths()`. Serialise them on a mutex declared in
the test module so they cannot race:

```rust
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
```

and take `let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());` at the top of both.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib config::`
Expected: FAIL to compile — `Settings`, `load_settings`, `save_settings` do not exist.

- [ ] **Step 3: Implement**

```rust
/// User-set overrides, read from `settings.json`. Every field is optional: absent means "use the
/// default", so a file written by an older build, or trimmed by hand, still works.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_archive_depth: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_buffer_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_total_buffer_bytes: Option<u64>,
    /// Outer `None` = not set. Inner `None` = the user explicitly chose unlimited.
    ///
    /// `deserialize_with` is REQUIRED here and is not decoration: serde maps a JSON `null` onto the
    /// OUTER `Option`, so a plain `Option<Option<u64>>` collapses "explicitly unlimited" into
    /// "unset" and the two become indistinguishable. `double_option` is only invoked when the key
    /// is present, so absent stays `None` while `null` becomes `Some(None)`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub archive_entry_max_bytes: Option<Option<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_ratio_cap: Option<u64>,
}

/// Distinguishes an absent key from an explicit `null`. See `archive_entry_max_bytes`.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}
```

`skip_serializing_if` on every field matters for the same reason: without it an unset field is
written as `null`, and reading that file back would turn "unset" into "explicitly unlimited".

```rust

/// Read settings, falling back to defaults for anything unreadable.
///
/// **Never fails.** A missing file is normal; a corrupt one is a warning. Losing a preference is
/// acceptable, stopping a five-day scan is not.
pub fn load_settings(path: &Path) -> Settings {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            tracing::warn!("could not read {}: {e}; using default limits", path.display());
            return Settings::default();
        }
    };
    match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("{} is not valid settings JSON: {e}; using default limits", path.display());
            Settings::default()
        }
    }
}

pub fn save_settings(path: &Path, s: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(s)?)?;
    Ok(())
}
```

Add to `impl Config`:

```rust
    /// Where user settings live: beside the catalog, never on a scanned drive.
    pub fn settings_path(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(|p| p.join("settings.json"))
            .unwrap_or_else(|| PathBuf::from("settings.json"))
    }
```

In **both** branches of `default_paths()`, apply the settings after building the defaults. Factor the
shared part rather than duplicating it:

```rust
        let s = load_settings(&data_dir.join("settings.json"));
        Ok(Config {
            catalog_path: data_dir.join("catalog.db"),
            snapshot_retention: 10,
            max_archive_depth: s.max_archive_depth.unwrap_or(8),
            archive_buffer_max_bytes: s.archive_buffer_max_bytes.unwrap_or(2 * 1024 * 1024 * 1024),
            archive_total_buffer_bytes: s.archive_total_buffer_bytes.unwrap_or(2 * 1024 * 1024 * 1024),
            archive_entry_max_bytes: s
                .archive_entry_max_bytes
                .unwrap_or(Some(64 * 1024 * 1024 * 1024)),
            archive_ratio_cap: s.archive_ratio_cap.unwrap_or(10_000),
        })
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the corrupt-file test discriminates**

Change `load_settings` to `serde_json::from_str(&raw).expect("bad settings")`, run
`cargo test --lib a_corrupt_settings_file_yields_defaults_rather_than_failing`.
Expected: FAIL with a panic. Restore and confirm green. Report both outputs — this test is the one
standing between a typo and an unusable tool.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/config.rs
git commit -m "feat(config): archive limits are read from settings.json

Every field is optional, so a partial or older file still works, and
loading never fails: a missing file is normal and a corrupt one is a
warning. A malformed preferences file must not be able to stop a
five-day scan.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Settings over HTTP

**Files:**
- Modify: `src/web.rs` (route table ~line 56, handlers, `mod tests`)

**Interfaces:**
- Consumes: `Settings`, `load_settings`, `save_settings`, `Config::settings_path` from Task 4.
- Produces: `GET /api/settings` → the effective limits as JSON; `POST /api/settings` → validated write.

**Constraints:** `POST` is a write endpoint and **must** call `check_csrf` as its first act, exactly
like `api_quarantine` (`src/web.rs:824`) and `api_repack` (`:897`). `GET` takes no token, like every
other read route. Do not change binding, auth or CORS.

**Validation** — reject rather than obey, and leave the stored file untouched on rejection:

- `archive_buffer_max_bytes` and `archive_total_buffer_bytes` above **25% of total system memory**
  (`sysinfo`, already a dependency). A quarter leaves room for the OS file cache the scan depends on
  and for the web server in the same process. If total memory cannot be determined, accept with a
  warning — an undeterminable machine should not be an unusable one.
- `archive_buffer_max_bytes` above `archive_total_buffer_bytes` — a per-archive bound larger than the
  whole descent's budget is meaningless.
- `max_archive_depth` below 1, or `archive_ratio_cap` below 1.

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn settings_endpoint_returns_the_effective_limits() {
        let (_t, db, _state) = seed_dupes();
        let v = get_json(&db, "/api/settings").await;
        assert_eq!(v["archive_ratio_cap"], 10000);
        assert_eq!(v["max_archive_depth"], 8);
    }

    #[tokio::test]
    async fn posting_settings_without_a_csrf_token_is_refused() {
        // Every write endpoint in this file is guarded; a settings write is no exception.
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"archive_ratio_cap":5000}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_oversized_buffer_budget_is_refused_with_a_reason() {
        let (_t, _db, state) = seed_dupes();
        let token = state.csrf_token.clone();
        let app = build_router_with(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/settings")
                    .header("content-type", "application/json")
                    .header("x-cleanup-token", token)
                    .body(Body::from(
                        r#"{"archive_total_buffer_bytes": 1000000000000000}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(res.into_body(), 100_000).await.unwrap();
        let msg = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(msg.to_lowercase().contains("memory"), "the refusal must say why: {msg}");
    }
```

**The CSRF header in this codebase is `x-cleanup-token`** (`check_csrf`, `src/web.rs:290`), compared
against `state.csrf_token`. It is not `x-csrf-token`; using the wrong name makes the "refused"
test pass for the wrong reason and the "accepted" test fail confusingly.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib settings_endpoint`
Expected: FAIL — 404, the routes do not exist.

- [ ] **Step 3: Implement**

```rust
#[derive(serde::Serialize)]
struct SettingsDto {
    max_archive_depth: usize,
    archive_buffer_max_bytes: u64,
    archive_total_buffer_bytes: u64,
    archive_entry_max_bytes: Option<u64>,
    archive_ratio_cap: u64,
}

/// The effective configuration, re-read from disk. Shared by both handlers so a write responds
/// with what is actually in force rather than echoing the request back.
fn effective_settings() -> Result<SettingsDto, (StatusCode, String)> {
    let cfg = crate::config::Config::default_paths().map_err(err500)?;
    Ok(SettingsDto {
        max_archive_depth: cfg.max_archive_depth,
        archive_buffer_max_bytes: cfg.archive_buffer_max_bytes,
        archive_total_buffer_bytes: cfg.archive_total_buffer_bytes,
        archive_entry_max_bytes: cfg.archive_entry_max_bytes,
        archive_ratio_cap: cfg.archive_ratio_cap,
    })
}

async fn api_settings_get() -> Result<Json<SettingsDto>, (StatusCode, String)> {
    Ok(Json(effective_settings()?))
}

/// A quarter of RAM: the scan leans on the OS file cache, and `browse` runs the web server in this
/// same process. Buffering more than this trades a working scan for a bigger buffer.
fn memory_ceiling() -> Option<u64> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    match sys.total_memory() {
        0 => None,
        total => Some(total / 4),
    }
}

fn validate(s: &crate::config::Settings) -> Result<(), String> {
    if let Some(d) = s.max_archive_depth {
        if d < 1 {
            return Err("max_archive_depth must be at least 1".into());
        }
    }
    if let Some(r) = s.archive_ratio_cap {
        if r < 1 {
            return Err("archive_ratio_cap must be at least 1".into());
        }
    }
    if let (Some(per), Some(total)) = (s.archive_buffer_max_bytes, s.archive_total_buffer_bytes) {
        if per > total {
            return Err(
                "archive_buffer_max_bytes cannot exceed archive_total_buffer_bytes: a per-archive \
                 bound larger than the whole descent's budget has no effect"
                    .into(),
            );
        }
    }
    if let Some(ceiling) = memory_ceiling() {
        for (name, v) in [
            ("archive_buffer_max_bytes", s.archive_buffer_max_bytes),
            ("archive_total_buffer_bytes", s.archive_total_buffer_bytes),
        ] {
            if let Some(v) = v {
                if v > ceiling {
                    return Err(format!(
                        "{name} of {v} bytes exceeds a quarter of system memory ({ceiling} bytes); \
                         buffering that much would starve the file cache the scan depends on"
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn api_settings_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<crate::config::Settings>,
) -> Result<Json<SettingsDto>, (StatusCode, String)> {
    check_csrf(&headers, &state)?;
    validate(&body).map_err(|m| (StatusCode::BAD_REQUEST, m))?;
    let cfg = crate::config::Config::default_paths().map_err(err500)?;
    crate::config::save_settings(&cfg.settings_path(), &body).map_err(err500)?;
    Ok(Json(effective_settings()?))
}
```

Register beside the other routes:

```rust
        .route("/api/settings", get(api_settings_get).post(api_settings_post))
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the CSRF test discriminates**

Comment out the `check_csrf(&headers, &state)?;` line, run
`cargo test --lib posting_settings_without_a_csrf_token_is_refused`.
Expected: FAIL — 200 where 403 was expected. Restore and confirm green. Report both outputs.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web.rs
git commit -m "feat(web): read and write the archive limits over HTTP

POST is CSRF-guarded like every other write endpoint, and validation
refuses what would defeat the limits' purpose -- buffer budgets above a
quarter of system memory, a per-archive bound above the descent budget,
a depth or ratio below 1.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: The Scan page section

**Files:**
- Modify: `src/web_ui.rs` (`scan_page`, ~line 1066)
- Test: `src/web.rs` `mod tests` (the existing `scan_page_is_self_contained_and_wired`)

**Interfaces:**
- Consumes: `GET`/`POST /api/settings` from Task 5.

**Constraints:** plain HTML/CSS/JS in a Rust string; no CDN, no runtime font fetch, no build step —
a test asserts the page contains no `http://` or `https://`. Every interpolated value goes through
`esc()`. Match the existing idiom: the `$()` helper, and the CSRF token sent the way the page's other
POSTs already send it (read `scan_page`'s existing fetch calls and copy that exactly).

The section must say plainly that changes apply to the **next** scan, not a running one.

- [ ] **Step 1: Write the failing test**

Extend the existing `scan_page_is_self_contained_and_wired` in `src/web.rs`:

```rust
        // The limits are only reachable from here; a page without the section leaves the user
        // editing JSON by hand, which is what this feature exists to avoid.
        assert!(body.contains("/api/settings"));
        assert!(body.contains("archive_ratio_cap"));
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test --lib scan_page_is_self_contained_and_wired`
Expected: FAIL — the markup is absent.

- [ ] **Step 3: Implement**

Add a collapsible section to `scan_page`'s markup, after the existing content:

```html
<details class="card" id="limits" style="margin-top:16px">
  <summary style="cursor:pointer">Archive limits</summary>
  <div class="mut" style="margin:8px 0 12px">
    Applies to the <b>next</b> scan — a run already in progress keeps the limits it started with.
  </div>
  <div id="limitsbody" class="mut">loading…</div>
  <div id="limitsmsg" class="mut" style="min-height:1.3em;margin-top:8px"></div>
</details>
```

and the script, beside the page's other handlers:

```js
const LIMITS=[
  ["archive_ratio_cap","Ratio cap","Refuses an entry whose declared uncompressed/compressed ratio is higher. Guards time, not memory: real files reach the hundreds, a zip bomb reaches the millions."],
  ["archive_entry_max_bytes","Largest file in an archive (bytes)","Leave empty for unlimited. Files inside archives are streamed, so this bounds how long one entry may take, not how much memory it uses."],
  ["archive_buffer_max_bytes","Nested archive buffer (bytes)","Real memory: a zip inside a zip is held in RAM so it can be hashed and re-opened."],
  ["archive_total_buffer_bytes","Total buffer budget (bytes)","Real memory: the ceiling on all nested-archive buffers alive at once."],
  ["max_archive_depth","Maximum nesting depth",""],
];
async function loadLimits(){
  const d=await (await fetch("/api/settings")).json();
  $("#limitsbody").innerHTML=LIMITS.map(([k,label,help])=>`
    <div style="margin-bottom:10px">
      <label for="lim_${k}">${esc(label)}</label>
      <input id="lim_${k}" name="${esc(k)}" value="${d[k]===null||d[k]===undefined?'':esc(String(d[k]))}" style="width:100%">
      ${help?`<div class="mut" style="font-size:12px">${esc(help)}</div>`:''}
    </div>`).join('')
    + `<button class="btn" id="savelimits">Save</button>`;
}
document.addEventListener('toggle', e=>{
  if(e.target.id==='limits' && e.target.open && !e.target.dataset.loaded){
    e.target.dataset.loaded='1';
    loadLimits().catch(()=>{ $("#limitsbody").textContent='Could not load the limits.'; });
  }
}, true);
document.addEventListener('click', async e=>{
  if(e.target.id!=='savelimits') return;
  const body={};
  for(const [k] of LIMITS){
    const raw=$("#lim_"+k).value.trim();
    // Empty means "unlimited" for the leaf ceiling and "leave unset" for everything else.
    if(raw==='') { if(k==='archive_entry_max_bytes') body[k]=null; continue; }
    const n=Number(raw);
    if(!Number.isFinite(n)||n<0){ $("#limitsmsg").textContent=`${k} must be a number.`; return; }
    body[k]=n;
  }
  // apiPost (defined in the shared shell, web_ui.rs:370) sends the x-cleanup-token header and
  // THROWS on a non-2xx, returning parsed JSON otherwise -- so the refusal arrives as an
  // exception, not as a falsy `ok`.
  try{
    await apiPost("/api/settings", body);
    $("#limitsmsg").textContent="Saved — applies to the next scan.";
  }catch(err){
    $("#limitsmsg").textContent="Refused: "+err.message;
  }
});
```

`apiPost` already exists in the shared page shell (`src/web_ui.rs:370`) and is in scope on every
page — do not define another one.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the page test discriminates**

`assert!(body.contains("/api/settings"))` is a bare substring check and would pass if that string
appeared anywhere else on the page. Delete the `<details id="limits">` block and the script that
fetches it, run `cargo test --lib scan_page_is_self_contained_and_wired`, and confirm it FAILS.
Restore and confirm green. Report both outputs.

- [ ] **Step 6: Check it by eye**

Run `cargo run -- browse` **with a throwaway data directory so the real catalogue is untouched**:

```bash
CLEANUPSTORAGES_DATA_DIR=/tmp/cus-ui-check cargo run -- browse
```

Open the Scan page, expand **Archive limits**, confirm the values load on expand, save a changed
ratio cap, reload and confirm it persisted. Try a total buffer of `999999999999999` and confirm it
is refused with a message naming memory. Report what you actually observed; if no browser is
available, say so plainly rather than claiming you checked.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web_ui.rs src/web.rs
git commit -m "feat(review): edit the archive limits from the Scan page

Each field carries the explanation JSON cannot, including which limits
bound memory and which bound time.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: `scan` prints its limits, and the docs say so

**Files:**
- Modify: `src/commands.rs` (`cmd_scan`), `README.md`
- Test: `src/archive.rs` `mod tests`

**Interfaces:**
- Consumes: `ArchiveLimits` from Task 1.
- Produces: `impl ArchiveLimits { pub fn summary_line(&self) -> String }`

**Why:** the limits decide what a five-day scan will and will not catalogue. They should be visible
at the moment you commit to it, without opening a browser.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_limits_summary_names_every_value_including_unlimited() {
        let l = limits();
        let s = l.summary_line();
        assert!(s.contains("10000"), "the ratio cap must be visible: {s}");
        assert!(s.contains("depth 8"), "got {s}");

        let unlimited = ArchiveLimits { entry_max_bytes: None, ..limits() };
        let u = unlimited.summary_line();
        assert!(
            u.contains("unlimited"),
            "an unlimited ceiling must say so rather than printing a huge number: {u}"
        );
    }
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test --lib the_limits_summary_names_every_value`
Expected: FAIL to compile — no `summary_line`.

- [ ] **Step 3: Implement**

```rust
impl ArchiveLimits {
    /// One line for the CLI, printed before a scan starts: these values decide what will and will
    /// not be catalogued, and a multi-day scan is a bad time to discover them.
    pub fn summary_line(&self) -> String {
        let gb = |b: u64| format!("{:.0} GB", b as f64 / 1_073_741_824.0);
        let entry = match self.entry_max_bytes {
            Some(b) => gb(b),
            None => "unlimited".to_string(),
        };
        format!(
            "Archive limits: ratio cap {}, largest entry {}, nested buffer {} (total {}), depth {}",
            self.ratio_cap,
            entry,
            gb(self.buffer_max_bytes),
            gb(self.total_buffer_bytes),
            self.max_depth
        )
    }
}
```

In `src/commands.rs`, in `cmd_scan`, immediately before the scan begins (after the catalogue is open,
before the counting pass):

```rust
    println!(
        "{}",
        crate::archive::ArchiveLimits::from_config(&cfg).summary_line()
    );
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Update the README**

Under the scan section, add:

````markdown
### Archive limits

Scanning descends into `.zip` archives and catalogues what is inside. Five limits govern that, and
`scan` prints them as it starts:

```
Archive limits: ratio cap 10000, largest entry 64 GB, nested buffer 2 GB (total 2 GB), depth 8
```

- **Ratio cap** refuses an entry whose declared uncompressed/compressed ratio is higher. It bounds
  *time*, not memory — genuine files reach the hundreds (an FPGA bitstream measured 815), while a zip
  bomb reaches the millions.
- **Largest entry** is the biggest file inside an archive that will be catalogued, or unlimited.
  These are streamed, so this bounds how long one entry may take rather than memory use.
- **Nested buffer** and **total buffer** are real memory: a zip inside a zip must be held in RAM to
  be hashed and re-opened.
- **Depth** bounds how many archives deep the scan will go.

Edit them on the **Scan** page of `cleanupstorages browse`, or in `settings.json` beside the
catalogue. Changes apply to the next scan. If that file is missing or invalid the defaults are used
and a warning is logged — a bad settings file never stops a scan.

Archives are recognised by their content, not their extension, so a zip renamed to something else is
still catalogued, and a file that merely ends in `.zip` (such as a macOS `._name.zip` sidecar) is
not mistaken for one.
````

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/archive.rs src/commands.rs README.md
git commit -m "feat(cli): print the archive limits when a scan starts

They decide what a five-day scan will and will not catalogue, so they
belong where you commit to it.

Co-Authored-By: justprototypelabs <217975680+justprototypelabs@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final review

Check the branch against the spec's ten success criteria:

1. Entries with ratios 215–815 are catalogued (Task 1).
2. Entries rejected on size, including the 34 GB one, are catalogued (Task 1).
3. `._Video.zip` and siblings are catalogued as ordinary files with no archive error (Task 2).
4. A zip renamed to another extension is detected and descended into (Tasks 2, 3).
5. All limits are readable and editable from the web UI, CSRF-guarded, invalid values refused and
   explained (Tasks 5, 6).
6. A corrupt `settings.json` yields a warning and defaults, not a failure (Task 4).
7. `scan` prints the effective limits at start (Task 7).
8. A renamed zip survives an unchanged re-scan with its entries still `active` (Task 3).
9. A re-scan of an unchanged tree stays in the tens of seconds — magic-byte detection never runs on
   the skip path (Task 3).
10. Existing archive tests pass unmodified.

Pay particular attention to criterion 8. It is the one that can lose data, and the only place the
whole branch touches the missing-file sweep.

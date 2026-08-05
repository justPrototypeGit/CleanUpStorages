# Archive Descent Policy and Settings Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the scanner exploding every `.docx`/`.jar` into its parts, report unfamiliar zip formats instead of guessing, and close the three residual ways a setting or a transient error can mis-catalogue a five-day scan.

**Architecture:** One shared set of range rules, applied both at settings load (warn + per-field fallback) and at the HTTP boundary. A descent decision function driven by a deny-list and an allow-list in `settings.json`, with unfamiliar zip-format extensions recorded in a new `pending_archive_formats` table and surfaced for the user to approve or dismiss. The detection-failure fallback keys on the catalogue rather than the filename.

**Tech Stack:** Rust, rusqlite/SQLite, `zip` crate, axum 0.7, plain HTML/CSS/JS (no build step), `serde_json` and `sysinfo` (already dependencies).

## Global Constraints

- **Nothing may ever be lost or corrupted.** ~20 TB of irreplaceable data. Every item in this plan exists because something could silently mis-catalogue it.
- **A settings file must never stop a scan.** Loading stays best-effort: warn, fall back **per field**, continue. Never reject the file as a whole.
- **The incremental-skip path must never open a file.** That is what makes a resumed scan fast-forward in seconds.
- **Write endpoints are CSRF-guarded** via `check_csrf` (`src/web.rs`), header **`x-cleanup-token`** (not `x-csrf-token`), compared against `state.csrf_token`. `GET` routes take no token.
- The web UI is plain HTML/CSS/JS in a Rust string: **no CDN, no runtime font fetch, no build step.** A test asserts each page contains no `http://` or `https://`. Every interpolation goes through `esc()`.
- `apiPost` exists in the shared shell (`src/web_ui.rs`), sends the CSRF header, **throws on non-2xx**, and returns parsed JSON — rejections arrive as exceptions, so use `try`/`catch`. There is also an `apiGet`. Do not define either again.
- **No new crates.**
- Gates for every task: `cargo test`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`.
- Commit trailers on every commit:
  ```
  Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
  Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
  ```
- **Never open or run against the real catalogue** at `%APPDATA%\justPrototype\CleanUpStorages\`. Use `tempfile` dirs or `CLEANUPSTORAGES_DATA_DIR`. Known issue #44: `cargo test` writes snapshots there unless the env var is set.

## Known pre-existing issue

`scanner::tests::run_scan_logs_volume_resolution` is flaky under parallel execution (#39). If it fails, re-run. Do not fix it here.

## What already exists (do not re-derive)

- `archive::looks_like_zip(&[u8]) -> bool`, `archive::peek4<R: Read>(&mut R) -> io::Result<(Vec<u8>, bool)>`, `archive::tail_has_eocd_signature<R: Read + Seek>(&mut R) -> io::Result<bool>`
- `scanner::open_for_archive_detection(&Path) -> io::Result<File>`, with a `#[cfg(test)]` hook `FORCE_DETECTION_OPEN_ERROR` (thread-local `Cell<bool>`) that makes it return `PermissionDenied`
- `pub type FileMeta = (i64, i64, bool, Option<i64>)` — `(size_bytes, modified_time, has_archive_entries, revive_floor)`; `Catalog::get_file_meta(volume_id, rel) -> anyhow::Result<Option<FileMeta>>`
- `Catalog::log_scan_error(volume_id, path, reason, phase, kind, now)`, `scan_errors::classify_io(&io::Error) -> &'static str`
- `config::Settings` — every field `Option`, `archive_entry_max_bytes: Option<Option<u64>>` with `double_option` + `skip_serializing_if` (both load-bearing; absent ≠ explicit null). `load_settings(&Path) -> Settings` (never fails), `save_settings`, `Config::settings_path()`
- `web::validate(&Settings, before: &Config) -> Result<(), String>` — validates the MERGED settings
- `Catalog::forget_volume` deletes from `files`, `scan_errors`, `volumes` (`src/catalog/store.rs:606-612`)

## File Structure

| File | Responsibility |
| --- | --- |
| `src/config.rs` (modify) | Shared range rules; deny/allow-list fields; load-time per-field fallback |
| `src/archive.rs` (modify) | The descent decision function (pure, unit-testable) |
| `src/catalog/schema.rs` (modify) | `pending_archive_formats` table |
| `src/catalog/pending_formats.rs` (**create**) | Record, list, resolve, and delete-with-volume — mirrors how `scan_errors.rs` owns its table |
| `src/scanner.rs` (modify) | Apply the decision; record unfamiliar formats; F-A fallback |
| `src/web.rs` (modify) | Pending-formats endpoints; reuse the shared rules |
| `src/web_ui.rs` (modify) | Pending-formats panel; byte-unit hints |
| `README.md` (modify) | Document the policy and the report |

---

### Task 1: One set of range rules, applied at load as well

**Files:**
- Modify: `src/config.rs` (`load_settings`), `src/web.rs` (`validate`)
- Test: `src/config.rs` `mod tests`

**Interfaces:**
- Produces: `pub fn check_ranges(s: &Settings) -> Vec<(&'static str, String)>` in `config.rs` — returns `(field_name, reason)` for every out-of-range field, empty when all are fine.
- `load_settings` keeps its signature and its never-fails contract.

**Why:** validation lives only in the HTTP handler today, so a hand-edited `settings.json` bypasses it. Confirmed live during the #41/#42 pre-merge review: `archive_entry_max_bytes: 0` produced `2 errors, 2 newly missing` with nothing wrong on disk. The rules must apply wherever settings enter the process.

**The contract that must not break:** loading is best-effort. An invalid field logs a warning naming the field and reason, and falls back to the compiled-in default **for that field alone**. The file is never rejected as a whole — a malformed preferences file must not be able to stop a five-day scan.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_zero_entry_ceiling_in_the_file_is_refused_and_falls_back() {
        // Confirmed live during the #41/#42 review: a 0-byte ceiling marks present files `missing`.
        // A preferences file must not be able to do that.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_entry_max_bytes": 0, "archive_ratio_cap": 5000}"#).unwrap();
        let s = load_settings(&p);
        assert_eq!(s.archive_entry_max_bytes, None, "the bad field falls back to the default");
        assert_eq!(s.archive_ratio_cap, Some(5000), "a VALID field beside it must survive");
    }

    #[test]
    fn a_zero_buffer_in_the_file_is_refused_and_falls_back() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_buffer_max_bytes": 0, "max_archive_depth": 3}"#).unwrap();
        let s = load_settings(&p);
        assert_eq!(s.archive_buffer_max_bytes, None);
        assert_eq!(s.max_archive_depth, Some(3));
    }

    #[test]
    fn an_explicit_null_ceiling_is_still_unlimited_not_out_of_range() {
        // `null` means "the user chose unlimited" and must survive the range check untouched.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_entry_max_bytes": null}"#).unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, Some(None));
    }

    #[test]
    fn a_buffer_larger_than_the_total_budget_is_refused_at_load() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(
            &p,
            br#"{"archive_buffer_max_bytes": 4000, "archive_total_buffer_bytes": 1000}"#,
        )
        .unwrap();
        let s = load_settings(&p);
        // The pair is inconsistent, so the per-archive bound is the one dropped: the total budget
        // is the harder ceiling and keeping it is the safer of the two.
        assert_eq!(s.archive_buffer_max_bytes, None);
        assert_eq!(s.archive_total_buffer_bytes, Some(1000));
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --lib config::`
Expected: FAIL — `load_settings` currently returns the values as written, so `archive_entry_max_bytes` is `Some(Some(0))`, not `None`.

- [ ] **Step 3: Implement the shared rules**

In `src/config.rs`:

```rust
/// Range rules for the archive limits, in ONE place.
///
/// Applied both when `settings.json` is read and at the HTTP boundary. The two used to differ, so a
/// hand-edited file could set a 0-byte entry ceiling that the UI would have refused -- and a 0-byte
/// ceiling converts present files to `missing`.
///
/// Returns `(field, reason)` for each out-of-range field; empty when everything is fine.
pub fn check_ranges(s: &Settings) -> Vec<(&'static str, String)> {
    let mut bad = Vec::new();
    if matches!(s.max_archive_depth, Some(0)) {
        bad.push(("max_archive_depth", "must be at least 1".to_string()));
    }
    if matches!(s.archive_ratio_cap, Some(0)) {
        bad.push(("archive_ratio_cap", "must be at least 1".to_string()));
    }
    if matches!(s.archive_buffer_max_bytes, Some(0)) {
        bad.push(("archive_buffer_max_bytes", "must be at least 1".to_string()));
    }
    if matches!(s.archive_total_buffer_bytes, Some(0)) {
        bad.push(("archive_total_buffer_bytes", "must be at least 1".to_string()));
    }
    // Some(Some(0)) is a zero ceiling; Some(None) is "explicitly unlimited" and is fine.
    if matches!(s.archive_entry_max_bytes, Some(Some(0))) {
        bad.push((
            "archive_entry_max_bytes",
            "must be at least 1; unlimited is null, not 0".to_string(),
        ));
    }
    if let (Some(per), Some(total)) = (s.archive_buffer_max_bytes, s.archive_total_buffer_bytes) {
        if per > total {
            bad.push((
                "archive_buffer_max_bytes",
                format!("{per} exceeds archive_total_buffer_bytes ({total}); a per-archive bound \
                         larger than the whole descent's budget has no effect"),
            ));
        }
    }
    bad
}

/// Clear every field `check_ranges` rejected, so it falls back to the compiled-in default.
fn drop_out_of_range(s: &mut Settings, where_: &Path) {
    for (field, reason) in check_ranges(s) {
        tracing::warn!("{}: {field} {reason}; using the default", where_.display());
        match field {
            "max_archive_depth" => s.max_archive_depth = None,
            "archive_ratio_cap" => s.archive_ratio_cap = None,
            "archive_buffer_max_bytes" => s.archive_buffer_max_bytes = None,
            "archive_total_buffer_bytes" => s.archive_total_buffer_bytes = None,
            "archive_entry_max_bytes" => s.archive_entry_max_bytes = None,
            _ => {}
        }
    }
}
```

Call it at the end of `load_settings`, on the successfully-parsed value, before returning:

```rust
    let mut s: Settings = match serde_json::from_value(v) { /* existing arms */ };
    // Per field, never the whole file: a bad preference is a warning, a stopped five-day scan is not.
    drop_out_of_range(&mut s, path);
    s
```

In `src/web.rs`, replace the hand-written range checks inside `validate` with a call to
`crate::config::check_ranges`, keeping the memory-ceiling check where it is (it depends on the
machine the UI runs on, so it stays out of the load path):

```rust
    if let Some((field, reason)) = crate::config::check_ranges(&merged).into_iter().next() {
        return Err(format!("{field} {reason}"));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS. The existing `web::` settings tests must pass unmodified — the rules are the same, only their home changed.

- [ ] **Step 5: Prove the load-time check discriminates — mandatory**

Comment out the `drop_out_of_range(&mut s, path);` call, run
`cargo test --lib a_zero_entry_ceiling_in_the_file_is_refused_and_falls_back`.
Expected: FAIL, showing `Some(Some(0))` where `None` was expected — reproducing the confirmed bug.
Restore and confirm green. Report both outputs.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/config.rs src/web.rs
git commit -m "fix(config): validate the archive limits when settings.json is read

Validation lived only in the HTTP handler, so a hand-edited settings file
bypassed it -- and a 0-byte entry ceiling converts present files to
missing, confirmed live during the #41/#42 review.

One set of rules now serves both paths. Loading stays best-effort: an
invalid field warns and falls back on its own, and the file is never
rejected as a whole.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: The descent decision

**Files:**
- Modify: `src/config.rs` (two new `Settings` fields + `Config` fields), `src/archive.rs` (the decision function)
- Test: `src/archive.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from Task 1 beyond `Settings` existing.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Descent { Descend, Leaf, Unrecognised }

  /// `extension` is lowercase, without a dot ("" when the name has none).
  pub fn descent_for(extension: &str, deny: &[String], allow: &[String]) -> Descent;
  ```
- `Config` gains `archive_deny_extensions: Vec<String>` and `archive_allow_extensions: Vec<String>`; `Settings` gains `Option<Vec<String>>` for each.
- Default deny-list: `docx xlsx pptx docm xlsm pptm jar apk war ear epub odt ods odp nupkg vsix ipa`. Default allow-list: empty.

**The rule, in order — the ordering is the design:**

| condition | result |
| --- | --- |
| extension on the deny-list | `Leaf` |
| extension is `zip`, or on the allow-list | `Descend` |
| otherwise | `Unrecognised` |

The deny-list is checked **first** so it always wins, including if a user denies `zip` itself: a rule
that silently overrode an explicit choice would be worse than one that obeys a choice they can see
and undo.

`Unrecognised` means "zip magic, unfamiliar extension" — a renamed zip or a zip-based format nobody
has classified yet. The caller treats it as a leaf **and records it** (Task 3). A five-day unattended
scan cannot prompt, so it does the conservative thing and reports afterwards.

Note this function never sees magic bytes: the caller has already established the file *is* zip
format. This is purely the naming policy, which is what makes it a pure function worth unit-testing.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_descent_rule_separates_archives_from_document_containers() {
        let deny: Vec<String> = ["docx", "jar", "epub"].iter().map(|s| s.to_string()).collect();
        let allow: Vec<String> = vec![];
        assert_eq!(descent_for("zip", &deny, &allow), Descent::Descend);
        assert_eq!(descent_for("docx", &deny, &allow), Descent::Leaf, "a document container");
        assert_eq!(descent_for("jar", &deny, &allow), Descent::Leaf);
        // The renamed-zip case from #42: unfamiliar, so reported rather than guessed at.
        assert_eq!(descent_for("bak", &deny, &allow), Descent::Unrecognised);
        assert_eq!(descent_for("", &deny, &allow), Descent::Unrecognised, "no extension");
    }

    #[test]
    fn an_approved_extension_is_descended_into() {
        let deny: Vec<String> = vec!["docx".into()];
        let allow: Vec<String> = vec!["bak".into()];
        assert_eq!(descent_for("bak", &deny, &allow), Descent::Descend);
    }

    #[test]
    fn the_deny_list_wins_over_everything() {
        // Deliberate: if a user denies `zip`, or denies something they also allowed, the visible
        // choice must win. Silently overriding them would be worse than obeying an undoable choice.
        let deny: Vec<String> = vec!["zip".into(), "bak".into()];
        let allow: Vec<String> = vec!["bak".into()];
        assert_eq!(descent_for("zip", &deny, &allow), Descent::Leaf);
        assert_eq!(descent_for("bak", &deny, &allow), Descent::Leaf);
    }

    #[test]
    fn extension_matching_ignores_case() {
        let deny: Vec<String> = vec!["docx".into()];
        assert_eq!(descent_for("DOCX", &deny, &[]), Descent::Leaf);
        assert_eq!(descent_for("ZIP", &[], &[]), Descent::Descend);
    }

    #[test]
    fn the_default_deny_list_covers_the_formats_that_prompted_this() {
        let cfg = Config::default_paths().unwrap();
        for e in ["docx", "xlsx", "pptx", "jar", "apk", "epub", "odt"] {
            assert!(
                cfg.archive_deny_extensions.iter().any(|d| d == e),
                "{e} must be denied by default"
            );
        }
        assert!(cfg.archive_allow_extensions.is_empty(), "nothing is approved until the user says so");
    }
```

**Note:** `the_default_deny_list_covers_the_formats_that_prompted_this` calls
`Config::default_paths()`, which reads the ambient environment. Take the `ENV_GUARD` and a
`ScopedDataDir` exactly as the existing `config.rs` tests do — the #41/#42 review found this exact
test shape reading the user's real settings file.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib archive::`
Expected: FAIL to compile — `Descent` and `descent_for` do not exist.

- [ ] **Step 3: Implement**

In `src/archive.rs`:

```rust
/// What to do with a file that is already known to be zip format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Descent {
    /// Catalogue what is inside.
    Descend,
    /// A known container -- catalogue the file itself and leave it whole.
    Leaf,
    /// Zip format with an unfamiliar extension. Treated as a leaf, and reported so the user can
    /// decide, rather than guessed at: a five-day unattended scan cannot ask.
    Unrecognised,
}

/// The naming policy for a file already established to be zip format.
///
/// A renamed zip and a `.docx` are indistinguishable by magic bytes alone, so the difference has to
/// be an explicit rule rather than something implied by a policy name.
///
/// The deny-list is checked FIRST and always wins -- including over `zip` itself. Silently
/// overriding an explicit choice would be worse than obeying one the user can see and undo.
pub fn descent_for(extension: &str, deny: &[String], allow: &[String]) -> Descent {
    let ext = extension.to_ascii_lowercase();
    let has = |list: &[String]| list.iter().any(|e| e.eq_ignore_ascii_case(&ext));
    if has(deny) {
        return Descent::Leaf;
    }
    if ext == "zip" || has(allow) {
        return Descent::Descend;
    }
    Descent::Unrecognised
}
```

In `src/config.rs`, add to `Settings`:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_deny_extensions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_allow_extensions: Option<Vec<String>>,
```

and to `Config`, with defaults applied in the shared constructor:

```rust
    pub archive_deny_extensions: Vec<String>,
    pub archive_allow_extensions: Vec<String>,
```

```rust
/// Zip-format files that are documents or packages, not archives worth exploding into parts.
/// Extending this needs no release -- it is editable in settings.json and on the Scan page.
const DEFAULT_DENY: &[&str] = &[
    "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "jar", "apk", "war", "ear", "epub", "odt",
    "ods", "odp", "nupkg", "vsix", "ipa",
];
```

```rust
            archive_deny_extensions: s
                .archive_deny_extensions
                .unwrap_or_else(|| DEFAULT_DENY.iter().map(|s| s.to_string()).collect()),
            archive_allow_extensions: s.archive_allow_extensions.unwrap_or_default(),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the ordering test discriminates**

Move the deny-list check *below* the `zip`/allow check in `descent_for`, run
`cargo test --lib the_deny_list_wins_over_everything`.
Expected: FAIL on the `zip` case. Restore and confirm green. Report both outputs.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/archive.rs src/config.rs
git commit -m "feat(archive): descent policy separates archives from document containers

A renamed zip and a .docx are indistinguishable by magic bytes, so the
difference is now an explicit rule: a deny-list of container formats
checked first, then .zip and anything the user has approved, and
everything else reported rather than guessed at.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Pass the limits into the scanner instead of reading the environment

**Files:**
- Modify: `src/scanner.rs:146-157` (`scan_volume_with_progress`), `src/commands.rs`, `src/scan_queue.rs`, and every test that calls it
- Test: existing scanner tests must pass unmodified apart from the added argument

**Interfaces:**
- Produces: `scan_volume_with_progress(cat, root, identity, force, now, progress, metrics, stop, limits: &ArchiveLimits)`.

**Why this task exists — and why it is not optional:** line 157 currently reads

```rust
let limits = ArchiveLimits::from_config(&Config::default_paths()?);
```

so the core scan function reaches into the ambient environment. Two consequences:

- **Every scanner test that touches archives reads the user's real `settings.json`** unless
  `CLEANUPSTORAGES_DATA_DIR` happens to be set. The #41/#42 review found exactly this shape in
  `archive::tests::limits_from_config` and treated it as a finding: once the user saves any limit
  from the UI, tests start failing and the real data directory gets created.
- Tasks 4 and 5 need to drive scans with specific deny/allow lists. With the limits built internally
  there is no way to do that except by writing a settings file and mutating a process-global env var
  in every test.

Passing them in fixes both, and makes the scan function honest: its behaviour becomes a function of
its arguments.

- [ ] **Step 1: Change the signature**

Add `limits: &ArchiveLimits` as the final parameter, delete the `from_config` line, and use the
argument. Update the callers:

- `src/commands.rs` (`cmd_scan`) — it already builds `ArchiveLimits::from_config(&cfg)` for the
  summary line, so pass that same value rather than building it twice.
- `src/scan_queue.rs` — build it from the config it already opens.
- `src/scanner.rs` `mod tests` — add a helper beside `setup()`/`ident()`:

```rust
    /// Limits for tests: the compiled-in defaults, with NO ambient environment read.
    fn test_limits() -> crate::archive::ArchiveLimits {
        crate::archive::ArchiveLimits {
            max_depth: 8,
            buffer_max_bytes: 2 * 1024 * 1024 * 1024,
            total_buffer_bytes: 2 * 1024 * 1024 * 1024,
            entry_max_bytes: Some(64 * 1024 * 1024 * 1024),
            ratio_cap: 10_000,
            deny_extensions: Vec::new(),
            allow_extensions: Vec::new(),
        }
    }
```

(The two list fields are added in Task 2; if that task has not landed yet, omit them here and add
them when it does.)

- [ ] **Step 2: Run the tests**

Run: `cargo test`
Expected: PASS once every call site is updated. No behaviour changes in this task — it is a pure
plumbing change, so any test that changes result is a mistake.

- [ ] **Step 3: Prove no scanner test reads the environment any more**

Run: `grep -n "default_paths" src/scanner.rs`
Expected: no match inside `scan_volume_with_progress`. Report the output.

Then, as a live check, point `CLEANUPSTORAGES_DATA_DIR` at a temp directory containing
`{"archive_ratio_cap": 3}` and run the scanner tests. They must all still pass — before this change,
a settings file could alter their behaviour.

- [ ] **Step 4: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/scanner.rs src/commands.rs src/scan_queue.rs
git commit -m "refactor(scanner): take the archive limits as an argument

scan_volume_with_progress read Config::default_paths() internally, so its
behaviour depended on the ambient environment and every archive test read
the user's real settings.json. The #41/#42 review flagged the same shape
elsewhere and treated it as a defect.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Record unfamiliar formats, and apply the decision

**Files:**
- Create: `src/catalog/pending_formats.rs`
- Modify: `src/catalog/mod.rs`, `src/catalog/schema.rs`, `src/catalog/store.rs` (the `forget_volume` deletes), `src/scanner.rs`
- Test: the new file, plus `src/scanner.rs` `mod tests`

**Interfaces:**
- Consumes: `Descent`/`descent_for` from Task 2; `Config::archive_deny_extensions` / `archive_allow_extensions`.
- Produces:
  ```rust
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct PendingFormat { pub extension: String, pub count: i64, pub total_bytes: i64, pub first_seen_at: i64 }

  impl Catalog {
      pub fn record_pending_format(&self, volume_id: &str, extension: &str, size_bytes: i64, now: i64) -> anyhow::Result<()>;
      /// Aggregated across volumes, biggest first.
      pub fn pending_formats(&self) -> anyhow::Result<Vec<PendingFormat>>;
      pub fn clear_pending_format(&self, extension: &str) -> anyhow::Result<usize>;
  }
  ```

**Schema** (in `schema.rs`, beside the other `CREATE TABLE IF NOT EXISTS`):

```sql
CREATE TABLE IF NOT EXISTS pending_archive_formats (
    extension     TEXT NOT NULL,
    volume_id     TEXT NOT NULL,
    count         INTEGER NOT NULL,
    total_bytes   INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    PRIMARY KEY (extension, volume_id)
);
```

Per volume, so an unplugged drive still shows what it holds; the UI aggregates because the decision
is about a file format, not a drive.

**`forget_volume` must delete these rows too** — beside the existing `scan_errors` delete
(`store.rs:~608`), or a dropped drive leaves phantom formats behind.

**Counting semantics:** a scan re-counts from scratch for the volume it is scanning. Clear that
volume's rows at the start of the scan and re-record as it goes, so counts never double on a rescan.
Do this in the same place the scan already prepares per-volume state, and **only for a completed
scan's volume** — never clear another volume's rows.

- [ ] **Step 1: Write the failing tests**

In `src/catalog/pending_formats.rs` `mod tests`:

```rust
    #[test]
    fn recording_the_same_extension_accumulates_per_volume_and_aggregates_across() {
        let t = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&t.path().join("c.db")).unwrap();
        cat.record_pending_format("v1", "bak", 100, 10).unwrap();
        cat.record_pending_format("v1", "bak", 200, 20).unwrap();
        cat.record_pending_format("v2", "bak", 400, 30).unwrap();
        cat.record_pending_format("v1", "kra", 50, 40).unwrap();

        let rows = cat.pending_formats().unwrap();
        let bak = rows.iter().find(|r| r.extension == "bak").unwrap();
        assert_eq!(bak.count, 3, "aggregated across both volumes");
        assert_eq!(bak.total_bytes, 700);
        assert_eq!(bak.first_seen_at, 10, "earliest sighting wins");
        assert_eq!(rows[0].extension, "bak", "biggest first");

        assert_eq!(cat.clear_pending_format("bak").unwrap(), 2, "both volumes' rows go");
        let rows = cat.pending_formats().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].extension, "kra");
    }
```

In `src/scanner.rs` `mod tests`:

```rust
    #[test]
    fn a_document_container_is_catalogued_whole_and_a_renamed_zip_is_reported() {
        // The whole point of this branch: a .docx must not explode into its parts, and a zip with
        // an unfamiliar extension must be reported rather than silently descended or ignored.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = {
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
        std::fs::write(root.join("thesis.docx"), &zip_bytes).unwrap();
        std::fs::write(root.join("backup.bak"), &zip_bytes).unwrap();
        std::fs::write(root.join("real.zip"), &zip_bytes).unwrap();

        let m = crate::scan_metrics::ScanMetrics::new();
        let stop = crate::scan_control::StopFlag::new();
        scan_volume_with_progress(&cat, &root, &ident(), false, 100, None, &m, &stop).unwrap();

        let entries: Vec<String> = cat
            .conn
            .prepare("SELECT relative_path FROM files WHERE container_chain IS NOT NULL ORDER BY relative_path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(entries, vec!["real.zip".to_string()], "only the .zip is descended into");

        let pending = cat.pending_formats().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].extension, "bak");
        assert_eq!(pending[0].count, 1);

        // All three are still catalogued as ordinary files -- nothing is skipped.
        let loose: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM files WHERE container_chain IS NULL AND status='active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(loose, 3);
    }

    #[test]
    fn a_rescan_does_not_double_count_pending_formats() {
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = {
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
        std::fs::write(root.join("backup.bak"), &zip_bytes).unwrap();
        let stop = crate::scan_control::StopFlag::new();
        for now in [100, 200] {
            let m = crate::scan_metrics::ScanMetrics::new();
            scan_volume_with_progress(&cat, &root, &ident(), true, now, None, &m, &stop).unwrap();
        }
        let pending = cat.pending_formats().unwrap();
        assert_eq!(pending[0].count, 1, "a rescan re-counts, it does not accumulate");
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib pending_formats`
Expected: FAIL to compile — the module does not exist.

- [ ] **Step 3: Implement the table and module**

Add the `CREATE TABLE` above to `schema.rs`'s batch. Create `src/catalog/pending_formats.rs`:

```rust
//! Zip-format files whose extension nobody has classified yet.
//!
//! The scanner will not guess: a five-day unattended run cannot ask, so an unfamiliar zip-format
//! extension is left whole and recorded here for the user to approve or dismiss.

use crate::catalog::Catalog;
use rusqlite::params;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingFormat {
    pub extension: String,
    pub count: i64,
    pub total_bytes: i64,
    pub first_seen_at: i64,
}

impl Catalog {
    pub fn record_pending_format(
        &self,
        volume_id: &str,
        extension: &str,
        size_bytes: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO pending_archive_formats(extension, volume_id, count, total_bytes, first_seen_at)
             VALUES (?1,?2,1,?3,?4)
             ON CONFLICT(extension, volume_id) DO UPDATE SET
                 count = count + 1,
                 total_bytes = total_bytes + excluded.total_bytes,
                 first_seen_at = MIN(first_seen_at, excluded.first_seen_at)",
            params![extension.to_ascii_lowercase(), volume_id, size_bytes, now],
        )?;
        Ok(())
    }

    /// Aggregated across volumes -- the decision is about a file format, not one drive.
    pub fn pending_formats(&self) -> anyhow::Result<Vec<PendingFormat>> {
        let mut stmt = self.conn.prepare(
            "SELECT extension, SUM(count), SUM(total_bytes), MIN(first_seen_at)
               FROM pending_archive_formats GROUP BY extension
              ORDER BY SUM(total_bytes) DESC, extension",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingFormat {
                extension: r.get(0)?,
                count: r.get(1)?,
                total_bytes: r.get(2)?,
                first_seen_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn clear_pending_format(&self, extension: &str) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM pending_archive_formats WHERE extension=?1",
            params![extension.to_ascii_lowercase()],
        )?)
    }

    /// Drop one volume's rows, so a rescan re-counts instead of accumulating.
    pub fn clear_pending_formats_for_volume(&self, volume_id: &str) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM pending_archive_formats WHERE volume_id=?1",
            params![volume_id],
        )?)
    }
}
```

Add `pub mod pending_formats;` to `src/catalog/mod.rs`, and add to `forget_volume` beside the
`scan_errors` delete:

```rust
        self.conn.execute(
            "DELETE FROM pending_archive_formats WHERE volume_id=?1",
            params![volume_id],
        )?;
```

- [ ] **Step 4: Apply the decision in the scanner**

Replace the bare `if is_archive {` with the policy. Where `is_archive` is computed, keep it as the
"is this zip format" answer, then decide:

```rust
        if is_archive {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            match archive::descent_for(&ext, &limits.deny_extensions, &limits.allow_extensions) {
                archive::Descent::Descend => {
                    let _t = metrics.timer(crate::scan_metrics::Phase::Archive);
                    descend_archive(/* existing arguments */)?;
                }
                archive::Descent::Leaf => {}
                archive::Descent::Unrecognised => {
                    // Left whole AND recorded: the user decides, the scanner does not guess.
                    cat.record_pending_format(&identity.volume_id, &ext, size, now)?;
                }
            }
        }
```

Carry the two lists on `ArchiveLimits` (they come from `Config` alongside the numeric limits, and
`ArchiveLimits::from_config` already exists):

```rust
    pub deny_extensions: Vec<String>,
    pub allow_extensions: Vec<String>,
```

Clear the volume's pending rows once at the start of the scan, next to where the scan prepares its
per-volume state:

```rust
    // Re-count from scratch for this volume, so a rescan does not accumulate.
    cat.clear_pending_formats_for_volume(&identity.volume_id)?;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Prove the policy test discriminates — mandatory**

Change the `Descent::Leaf` arm to fall through to `Descend`, run
`cargo test --lib a_document_container_is_catalogued_whole_and_a_renamed_zip_is_reported`.
Expected: FAIL — `thesis.docx` appears among the descended entries. Restore and confirm green.
Report both outputs; this test is the reason the branch exists.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/catalog/pending_formats.rs src/catalog/mod.rs src/catalog/schema.rs src/catalog/store.rs src/scanner.rs
git commit -m "feat(scanner): apply the descent policy and report unfamiliar zip formats

.docx, .jar and friends are catalogued whole instead of exploding into
their parts. A zip with an unfamiliar extension is left whole and
recorded, so the user decides rather than the scanner guessing -- a
five-day unattended scan cannot ask.

Counts are re-derived per volume on each scan, and forget() drops them
with the volume.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: F-A — the detection-failure fallback keys on the catalogue

**Files:**
- Modify: `src/scanner.rs` (the detection error arm, ~line 383-404)
- Test: `src/scanner.rs` `mod tests`

**Interfaces:**
- Consumes: `Catalog::get_file_meta -> Option<FileMeta>` where `FileMeta = (i64, i64, bool, Option<i64>)` and field 2 (0-indexed) is `has_archive_entries`.

**Why:** when `open_for_archive_detection` fails, the current fallback is the *extension* test. An
archive **not named `.zip`** — a renamed zip, or one the user has approved — whose content also
changed therefore loses its entries, and cannot self-heal: the archive's own row stays `active`, so
the revive floor is `None` and a later clean rescan does nothing. Only `--force` recovers it.
Confirmed during the #41/#42 pre-merge review.

The catalogue already knows whether this path has archive entries. That is exactly the question
being asked, and unlike the filename it is right for renamed zips and `.docx` alike.

**A test hook already exists:** `FORCE_DETECTION_OPEN_ERROR` (a `#[cfg(test)]` thread-local `Cell<bool>`
in `scanner.rs`) makes `open_for_archive_detection` return `PermissionDenied`. Use it; do not invent
another mechanism. Reset it at the end of the test so it cannot leak into other tests on the same
thread.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_detection_failure_on_a_catalogued_archive_keeps_its_entries() {
        // The archive is named .bak, so the old extension-based fallback said "not an archive",
        // descend never ran, and the sweep took its entries -- with no way to self-heal.
        let (tmp, cat) = setup();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = {
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
        std::fs::write(root.join("backup.bak"), &zip_bytes).unwrap();

        // Approve .bak so the first scan descends and the entry exists. Task 3 made the limits an
        // argument, so this needs no environment or settings file.
        let limits = crate::archive::ArchiveLimits {
            allow_extensions: vec!["bak".to_string()],
            ..test_limits()
        };
        let stop = crate::scan_control::StopFlag::new();
        let m = crate::scan_metrics::ScanMetrics::new();
        let ident = ident();
        scan_volume_with_progress(&cat, &root, &ident, false, 100, None, &m, &stop, &limits)
            .unwrap();
        let active = || -> i64 {
            cat.conn
                .query_row(
                    "SELECT count(*) FROM files WHERE container_chain IS NOT NULL AND status='active'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(active(), 1, "the entry is catalogued");

        // Now force the detection open to fail, with the file's content changed so the skip path
        // does not short-circuit.
        std::fs::write(root.join("backup.bak"), [&zip_bytes[..], b"x"].concat()).unwrap();
        FORCE_DETECTION_OPEN_ERROR.with(|f| f.set(true));
        let m2 = crate::scan_metrics::ScanMetrics::new();
        let r = scan_volume_with_progress(&cat, &root, &ident, false, 300, None, &m2, &stop, &limits);
        // Reset before unwrapping, so a failure cannot leave the hook set for other tests on this
        // thread.
        FORCE_DETECTION_OPEN_ERROR.with(|f| f.set(false));
        r.unwrap();

        assert_eq!(active(), 1, "a detection failure must not cost the archive its entries");
    }
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test --lib a_detection_failure_on_a_catalogued_archive_keeps_its_entries`
Expected: FAIL — the entry count drops to 0, because `.bak` is not `.zip` so the extension fallback
returns false.

- [ ] **Step 3: Implement**

In the detection error arm, replace `ext_looks_like_zip` as the fallback value:

```rust
                    // Fall back on what the catalogue already knows, not on the filename: it is
                    // right for a renamed zip and for a .docx alike. Only when there is no row at
                    // all (a new file we could not open) does the extension remain the last resort.
                    match cat.get_file_meta(&identity.volume_id, &rel)? {
                        Some((_, _, has_archive_entries, _)) => has_archive_entries,
                        None => ext_looks_like_zip,
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove it discriminates**

Revert the fallback to `ext_looks_like_zip`, run the new test, confirm it FAILS, restore, confirm
green. Report both outputs.

- [ ] **Step 6: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/scanner.rs
git commit -m "fix(scanner): detection-failure fallback keys on the catalogue, not the filename

An archive not named .zip whose content also changed lost its entries when
the detection open failed, and could not self-heal -- only --force
recovered it. The catalogue already records that the path has archive
entries, which is exactly the question being asked.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Pending formats over HTTP

**Files:**
- Modify: `src/web.rs` (routes, handlers, `mod tests`)

**Interfaces:**
- Consumes: `Catalog::pending_formats`, `clear_pending_format`; `config::{load_settings, save_settings, Settings}`; `Config::settings_path`.
- Produces: `GET /api/pending-formats`; `POST /api/pending-formats/resolve` with body `{"extension": "bak", "action": "descend" | "document"}`.

**Constraints:** `POST` calls `check_csrf` first (header `x-cleanup-token`). `GET` takes no token.
The resolve handler **merges** into the stored settings — it must not overwrite the other fields, the
mistake caught during #41/#42.

`descend` appends the extension to `archive_allow_extensions`; `document` appends it to
`archive_deny_extensions`. Either way the pending rows for that extension are cleared. Appending must
be idempotent — resolving twice must not produce duplicates.

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn pending_formats_are_listed_and_resolving_updates_the_lists() {
        let (_t, db, state) = seed_dupes();
        {
            let cat = Catalog::open(&db).unwrap();
            cat.record_pending_format("vol-1", "bak", 4096, 10).unwrap();
        }
        let v = get_json(&db, "/api/pending-formats").await;
        assert_eq!(v[0]["extension"], "bak");
        assert_eq!(v[0]["count"], 1);

        let token = state.csrf_token.clone();
        let app = build_router_with(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pending-formats/resolve")
                    .header("content-type", "application/json")
                    .header("x-cleanup-token", token)
                    .body(Body::from(r#"{"extension":"bak","action":"descend"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let v = get_json(&db, "/api/pending-formats").await;
        assert!(v.as_array().unwrap().is_empty(), "resolved formats stop being reported");
    }

    #[tokio::test]
    async fn resolving_a_format_without_a_csrf_token_is_refused() {
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pending-formats/resolve")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"extension":"bak","action":"descend"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }
```

Both tests reach `Config::default_paths()` through the resolve handler, so wrap them in the existing
`ScopedDataDir` guard — otherwise they write into the user's real data directory.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --lib pending_formats_are_listed`
Expected: FAIL — 404, the routes do not exist.

- [ ] **Step 3: Implement**

```rust
#[derive(serde::Deserialize)]
struct ResolveFormat {
    extension: String,
    /// "descend" -> allow-list; "document" -> deny-list.
    action: String,
}

async fn api_pending_formats(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::catalog::pending_formats::PendingFormat>>, (StatusCode, String)> {
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    Ok(Json(cat.pending_formats().map_err(err500)?))
}

async fn api_resolve_format(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResolveFormat>,
) -> Result<Json<Vec<crate::catalog::pending_formats::PendingFormat>>, (StatusCode, String)> {
    check_csrf(&headers, &state)?;
    let ext = body.extension.to_ascii_lowercase();
    if ext.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "extension is required".into()));
    }
    let cfg = crate::config::Config::default_paths().map_err(err500)?;
    let path = cfg.settings_path();
    // Merge, never overwrite: the other settings must survive.
    let mut s = crate::config::load_settings(&path);
    let list = match body.action.as_str() {
        "descend" => &mut s.archive_allow_extensions,
        "document" => &mut s.archive_deny_extensions,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown action {other:?}; expected \"descend\" or \"document\""),
            ))
        }
    };
    let mut v = list.take().unwrap_or_else(|| match body.action.as_str() {
        "document" => cfg.archive_deny_extensions.clone(),
        _ => cfg.archive_allow_extensions.clone(),
    });
    if !v.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
        v.push(ext.clone());
    }
    *list = Some(v);
    crate::config::save_settings(&path, &s).map_err(err500)?;

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    cat.clear_pending_format(&ext).map_err(err500)?;
    Ok(Json(cat.pending_formats().map_err(err500)?))
}
```

Register beside the other routes:

```rust
        .route("/api/pending-formats", get(api_pending_formats))
        .route("/api/pending-formats/resolve", post(api_resolve_format))
```

**Note the `take()`/default dance:** when the stored settings have no explicit list, the *effective*
list is the compiled-in default, so it must be seeded from `cfg` before appending — otherwise
resolving one format silently discards the whole default deny-list.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the CSRF test discriminates**

Comment out `check_csrf(&headers, &state)?;`, run
`cargo test --lib resolving_a_format_without_a_csrf_token_is_refused`, confirm it FAILS (200 vs 403),
restore, confirm green. Report both outputs.

- [ ] **Step 6: Add a test for the seeding trap**

```rust
    #[tokio::test]
    async fn resolving_a_format_keeps_the_default_deny_list() {
        // The trap: with no stored list, the EFFECTIVE deny-list is the compiled-in default. If the
        // handler appends to an empty vec instead of seeding from that default, one click silently
        // drops .docx/.jar back onto the descend path -- invisible until a later scan explodes
        // every Office document into its parts.
        let _guard = ScopedDataDir::new();
        let (_t, db, state) = seed_dupes();
        {
            let cat = Catalog::open(&db).unwrap();
            cat.record_pending_format("vol-1", "kra", 10, 10).unwrap();
        }
        let token = state.csrf_token.clone();
        let app = build_router_with(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pending-formats/resolve")
                    .header("content-type", "application/json")
                    .header("x-cleanup-token", token)
                    .body(Body::from(r#"{"extension":"kra","action":"document"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let cfg = crate::config::Config::default_paths().unwrap();
        let stored = crate::config::load_settings(&cfg.settings_path());
        let deny = stored.archive_deny_extensions.expect("the list was written");
        assert!(deny.iter().any(|e| e == "kra"), "the resolved format was added");
        assert!(
            deny.iter().any(|e| e == "docx"),
            "the compiled-in defaults must survive: got {deny:?}"
        );
        assert!(deny.iter().any(|e| e == "jar"), "got {deny:?}");
    }
```

**Adapt the `ScopedDataDir` construction to whatever the existing guard in `src/web.rs` actually
looks like** — it was added during #41/#42 and is already used by the settings tests.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web.rs
git commit -m "feat(web): list and resolve unfamiliar zip formats

Resolving merges into the stored settings and seeds from the effective
default list, so approving one format cannot silently discard the rest of
the deny-list.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: The Scan page — pending formats, and unit hints

**Files:**
- Modify: `src/web_ui.rs` (`scan_page`)
- Test: `src/web.rs` `mod tests` (`scan_page_is_self_contained_and_wired`)

**Interfaces:**
- Consumes: `GET /api/pending-formats`, `POST /api/pending-formats/resolve` from Task 5.

**Two things:**

1. **A pending-formats panel** above the Archive limits section, hidden when the list is empty:

```
Unrecognised zip-format files found
  .bak    12 files   4.2 GB   [Descend into these]  [Treat as documents]
```

2. **Unit hints beside every byte field.** The fields take raw bytes, and typing `64` meaning
gigabytes sets a 64-byte ceiling that validation accepts. Render the human equivalent next to the
input, updating on `input`:

```
Largest file in an archive (bytes)
[ 68719476736                    ]  = 64.0 GB
```

so a mistyped `64` reads `= 64 B` before saving. This is a display aid — the field still submits raw
bytes and the API contract is unchanged.

**Constraints:** every interpolation through `esc()`; use the existing `apiGet`/`apiPost` (the latter
throws on non-2xx, so use `try`/`catch` and surface `err.message`); no CDN, no build step; fetch the
pending list when the page loads its other data, not on a timer.

- [ ] **Step 1: Write the failing test**

Extend `scan_page_is_self_contained_and_wired`:

```rust
        assert!(body.contains("/api/pending-formats"));
        assert!(body.contains("humanBytes"), "byte fields carry a unit hint");
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test --lib scan_page_is_self_contained_and_wired`
Expected: FAIL — neither string is present.

- [ ] **Step 3: Implement**

Markup, above the `#limits` section:

```html
<div class="card" id="pendingfmt" style="margin-top:16px;display:none">
  <b>Unrecognised zip-format files found</b>
  <div class="mut" style="margin:6px 0 10px">
    These are zip files with an extension the scanner does not recognise. They were catalogued whole
    and <b>not</b> opened. Approve one to catalogue what is inside on the next scan.
  </div>
  <div id="pendingfmtbody"></div>
  <div class="mut" id="pendingfmtmsg" style="min-height:1.3em;margin-top:8px"></div>
</div>
```

Script:

```js
function humanBytes(n){
  const v=Number(n);
  if(!Number.isFinite(v)) return '';
  const u=['B','KB','MB','GB','TB'];
  let i=0, x=v;
  while(x>=1024 && i<u.length-1){ x/=1024; i++; }
  return (i===0? x : x.toFixed(1))+' '+u[i];
}
async function loadPendingFormats(){
  const rows=await apiGet("/api/pending-formats");
  const box=$("#pendingfmt");
  if(!rows.length){ box.style.display='none'; return; }
  box.style.display='';
  $("#pendingfmtbody").innerHTML=rows.map(r=>`<div class="erow">
      <span class="tag">.${esc(r.extension)}</span>
      <span>${esc(String(r.count))} files</span>
      <span class="mut">${esc(humanBytes(r.total_bytes))}</span>
      <button class="btn pfmt" data-ext="${esc(r.extension)}" data-action="descend">Descend into these</button>
      <button class="btn pfmt" data-ext="${esc(r.extension)}" data-action="document">Treat as documents</button>
    </div>`).join('');
}
document.addEventListener('click', async e=>{
  const b=e.target.closest('button.pfmt');
  if(!b) return;
  try{
    await apiPost("/api/pending-formats/resolve", {extension:b.dataset.ext, action:b.dataset.action});
    $("#pendingfmtmsg").textContent =
      b.dataset.action==='descend'
        ? `.${b.dataset.ext} will be opened on the next scan.`
        : `.${b.dataset.ext} will be catalogued whole.`;
    await loadPendingFormats();
  }catch(err){ $("#pendingfmtmsg").textContent="Failed: "+err.message; }
});
```

For the unit hints, render a `<span class="mut" id="hint_${esc(k)}">` beside each byte input inside
`loadLimits`, set it from `humanBytes` on render, and update it on `input`. The depth and ratio
fields are not byte values — do not give them a hint.

Call `loadPendingFormats()` where the page already loads its data, and again after a successful
limits save.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Prove the page test discriminates**

Delete the `#pendingfmt` block and its script, run
`cargo test --lib scan_page_is_self_contained_and_wired`, confirm it FAILS, restore, confirm green.
Report both outputs.

- [ ] **Step 6: Check it by eye — with a throwaway data directory**

```bash
CLEANUPSTORAGES_DATA_DIR=/tmp/cus-ui cargo run -- browse --no-open
```

Seed a pending format first (scan a folder containing a zip named `.bak`), then confirm: the panel
appears, the counts are right, both buttons work, the panel disappears once resolved, and a byte
field typed as `64` shows `= 64 B`. **If you cannot run a browser, say so plainly** rather than
claiming you checked — a previous task in this project verified an equivalent panel over HTTP only,
and the gap was only closed later by headless Edge.

- [ ] **Step 7: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add src/web_ui.rs src/web.rs
git commit -m "feat(review): report unfamiliar zip formats and show byte units

The limits fields take raw bytes, so typing 64 meaning gigabytes set a
64-byte ceiling that validation accepted. The hint makes the mistake
visible before saving.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: Documentation

**Files:**
- Modify: `README.md`

**Why:** the README currently says archives are recognised by content and lists the consequence
(`.docx`, `.jar` included). That is about to stop being true, and a README describing behaviour the
binary no longer has is a real defect — this project has already shipped that mistake once.

- [ ] **Step 1: Update the archive section**

Replace the paragraph beginning "Archives are recognised by **their content, not their extension**"
with:

````markdown
Archives are recognised by **their content, not their extension**, so a zip renamed to something else
is not missed, and a file that merely ends in `.zip` — such as a macOS `._name.zip` sidecar — is not
mistaken for one.

Being zip format is not enough to be opened, though. `.docx`, `.xlsx`, `.jar`, `.epub` and similar
are zip files, but they are documents and packages rather than archives, so they are catalogued
**whole** rather than exploded into their internal parts. That list lives in `settings.json` as
`archive_deny_extensions` and is editable on the Scan page.

A zip file with an extension in neither list — say a backup renamed to `.bak` — is catalogued whole
and **reported**, not guessed at:

```
Unrecognised zip-format files found
  .bak    12 files   4.2 GB   [Descend into these]  [Treat as documents]
```

Approving it opens those files on the next scan; dismissing it treats them as documents. A long
unattended scan can never ask, so it takes the conservative option and tells you afterwards.
````

- [ ] **Step 2: Verify every claim against the binary**

Run a scan against a throwaway data directory over a folder containing a `.zip`, a `.docx` (any zip
renamed) and a `.bak` zip. Confirm: only the `.zip` is descended into, all three are catalogued, and
the `.bak` is reported. Paste the real output into your report. **Do not copy the example from the
plan** — this project has already shipped a README whose example did not match the binary.

- [ ] **Step 3: Gates and commit**

```bash
cargo test && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
git add README.md
git commit -m "docs: describe the archive descent policy

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final review

Check the branch against the spec's nine success criteria, and pay particular attention to:

1. **Can a present file be recorded as `missing`, or a phantom as present?** Tasks 3 and 4 both touch
   the path that decides whether `descend_archive` runs, and that decision governs whether an
   archive's entries are touched — which is what the missing-file sweep keys on. This area has
   produced four wrong fixes across two branches. Trace: a `.docx` that was previously descended into
   under the old behaviour and is now a leaf (what happens to its existing entries on the first scan
   after this change?), a detection failure, and a stopped scan.
2. **The first scan after this branch is a behaviour change on an existing catalogue.** Entries
   belonging to `.docx`/`.jar` files catalogued by the old code will no longer be re-touched, so the
   sweep will mark them `missing`. Decide whether that is correct (they genuinely are no longer
   catalogued) or whether they should be deleted outright, and make sure the answer is deliberate and
   documented rather than incidental.
3. The seeding trap in Task 5 — resolving one format must not discard the default deny-list.

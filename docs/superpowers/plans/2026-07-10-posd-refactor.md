# PoSD Complexity-Reduction Refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the change-amplification and information-leakage hot spots from the PoSD review, behavior-preserving, so a schema or policy change touches one place instead of many.

**Architecture:** Seven independent refactor tasks — a shared `FILE_COLUMNS` const + named-column mapper (store), a `check_csrf` helper and `now_secs`/`snapshot_after_mutation` helpers (web), a CLI-prologue helper (commands), a `loose_file_id` rename, a `duplicate_group_count` semantics alignment, and extraction of the pure image-preview helpers into their own module. Each lands as its own commit and can be reverted alone.

**Tech Stack:** Rust, rusqlite (SQLite), axum 0.7. Existing 118-test suite (110 lib + 8 integration + doctests) is the behavior-preservation guard.

## Global Constraints

- **Behavior-preserving.** The existing test suite must stay green with NO test weakened or deleted — the sole exception is the one count test updated in Task 6 to the deliberately-aligned semantics. If a refactor makes a test fail, the refactor is wrong (not the test), except that one.
- **Reliability unchanged.** No change to schema, on-disk formats, engines, or the "nothing lost or corrupted" guarantees.
- **CSRF invariant preserved.** The token check must remain the FIRST thing every mutating handler does, before any catalog/filesystem/dialog access; the `check_csrf` helper must log a `tracing::warn!` and return `403 "missing or bad token"` on mismatch — identical observable behavior to today.
- **Conventional Commits**, scope from CLAUDE.md (`catalog`, `web`, `cli`, plus `refactor` type). Every commit message body ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- **Windows/PowerShell dev env.** `cargo build`, `cargo test`.
- Run the focused tests for the touched surface while iterating; run the full `cargo test -p cleanupstorages` once before each commit.

## File structure

- `src/catalog/store.rs` — MODIFY (Tasks 1, 5, 6): add `FILE_COLUMNS`, named mapper, rename, count SQL.
- `src/web.rs` — MODIFY (Tasks 2, 3, 7): `check_csrf`, `now_secs`/`snapshot_after_mutation`, remove preview helpers.
- `src/image_preview.rs` — CREATE (Task 7): pure `thumbnail_jpeg` + `read_zip_entry`.
- `src/lib.rs` — MODIFY (Task 7): `pub mod image_preview;`.
- `src/commands.rs` — MODIFY (Task 4): `open_catalog`/`open_catalog_checked`/`snapshot` helpers.
- `src/repack.rs`, `src/purge.rs`, `src/quarantine.rs` — MODIFY (Task 5): rename call sites (mostly tests).

---

### Task 1: `FILE_COLUMNS` const + named-column mapper

**Files:**
- Modify: `src/catalog/store.rs`

**Interfaces:**
- Produces: `const FILE_COLUMNS: &str` — the shared `files` column list, used by every full-row SELECT.
- Changes: `map_file_record` reads by column NAME, not position, so it no longer depends on SELECT column order.

**Context:** There are 6 full-row SELECTs (lines ~186, 239, 255, 295, 330, 351) all listing the same 16 columns, and `map_file_record` (lines ~413–432) reads `r.get(0)…r.get(15)`. This task makes the column list live once and removes the positional coupling.

- [ ] **Step 1: Confirm the baseline is green**

Run: `cargo test -p cleanupstorages --lib catalog`
Expected: PASS (this is a refactor; tests are the guard, they must pass before and after).

- [ ] **Step 2: Add the const** near the top of the `impl Catalog` block or module (in `src/catalog/store.rs`, above `search_filtered`):

```rust
/// The full `files` column list, in one place. Every full-row SELECT uses this; the mapper
/// (`map_file_record`) reads results by column NAME, so this list and the mapper cannot drift.
const FILE_COLUMNS: &str =
    "id, volume_id, relative_path, filename, extension, size_bytes, content_hash, \
     created_time, modified_time, accessed_time, category, container_chain, \
     status, first_seen_at, last_seen_at, original_path";
```

- [ ] **Step 3: Convert `map_file_record` to named access**

Replace the body of `map_file_record` (`src/catalog/store.rs:413-432`) with:

```rust
    fn map_file_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: r.get("id")?,
            volume_id: r.get("volume_id")?,
            relative_path: r.get("relative_path")?,
            filename: r.get("filename")?,
            extension: r.get("extension")?,
            size_bytes: r.get("size_bytes")?,
            content_hash: r.get("content_hash")?,
            created_time: r.get("created_time")?,
            modified_time: r.get("modified_time")?,
            accessed_time: r.get("accessed_time")?,
            category: Category::from_db(&r.get::<_, String>("category")?),
            container_chain: r.get("container_chain")?,
            status: FileStatus::from_db(&r.get::<_, String>("status")?),
            first_seen_at: r.get("first_seen_at")?,
            last_seen_at: r.get("last_seen_at")?,
            original_path: r.get("original_path")?,
        })
    }
```

(`rusqlite::Row::get` accepts a `&str` column name via its `RowIndex` impl — no new dependency.)

- [ ] **Step 4: Replace each of the 6 SELECT column lists with the const.**

For the string-built query in `search_filtered` (`src/catalog/store.rs:185-189`):

```rust
        let mut sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE 1=1");
```

For the other 5 (`get_file` ~239, `duplicate_groups` ~255, `active_copies`/now-`loose` variants, `archive_entries` ~330, `quarantined_rows` ~351): each is a `self.conn.prepare("SELECT <cols> FROM files WHERE …")`. Rewrite each `prepare` argument using `format!`, e.g.:

```rust
        let mut stmt = self.conn.prepare(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE id=?1"))?;
```

Preserve each query's exact WHERE/ORDER/params — ONLY the column list changes. Do this for all 6 sites (search_filtered, get_file, duplicate_groups, active_copies, archive_entries, quarantined_rows). After this, `grep -c "id, volume_id, relative_path, filename, extension, size_bytes, content_hash" src/catalog/store.rs` returns 1 (only the const).

- [ ] **Step 5: Run the full suite**

Run: `cargo test -p cleanupstorages`
Expected: PASS, unchanged counts. Named access + the const are behavior-identical; any failure means a SELECT's WHERE/params changed by mistake — fix that.

- [ ] **Step 6: Commit**

```bash
git add src/catalog/store.rs
git commit -m "refactor(catalog): single FILE_COLUMNS const + named-column mapper"
```

---

### Task 5: Rename `active_file_id` → `loose_file_id`

**Files:**
- Modify: `src/catalog/store.rs` (definition + its 2 inline tests), `src/repack.rs` (1 real caller + tests), `src/quarantine.rs` (tests), `src/purge.rs` (tests), `src/web.rs` (tests)

**Interfaces:**
- Renames: `Catalog::active_file_id(volume_id, relative_path)` → `Catalog::loose_file_id(volume_id, relative_path)`. Same signature, same body, accurate name (it returns the loose row at a path regardless of status).

**Context:** The method filters `container_chain IS NULL` (loose files, any status), not "active" — the name misleads. Pure rename; no behavior change. 19 call sites (1 real in `repack.rs:130`, the rest tests).

- [ ] **Step 1: Rename the definition + doc comment** in `src/catalog/store.rs:223`. Update the doc comment to describe it accurately ("Id of the loose file at this path, if catalogued, regardless of status."). Keep the body unchanged.

```rust
    /// Id of the loose file (container_chain IS NULL) at this path, if catalogued, regardless of
    /// status. Exactly one such row can exist per (volume, path) — the loose-identity partial
    /// unique index guarantees it.
    pub fn loose_file_id(&self, volume_id: &str, relative_path: &str) -> anyhow::Result<Option<i64>> {
```

- [ ] **Step 2: Update all call sites.** Replace every `active_file_id(` with `loose_file_id(` across `src/catalog/store.rs`, `src/repack.rs`, `src/quarantine.rs`, `src/purge.rs`, `src/web.rs`. Verify none remain:

Run: `grep -rn "active_file_id" src/`
Expected: no matches.

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p cleanupstorages`
Expected: PASS (pure rename).

- [ ] **Step 4: Commit**

```bash
git add src/catalog/store.rs src/repack.rs src/quarantine.rs src/purge.rs src/web.rs
git commit -m "refactor(catalog): rename active_file_id -> loose_file_id (accurate name)"
```

---

### Task 6: Align `duplicate_group_count` to active-only

**Files:**
- Modify: `src/catalog/store.rs` (the method + its 2 inline tests if affected)

**Interfaces:**
- Changes: `duplicate_group_count` counts groups over `status='active'` only, matching `duplicate_groups` (what the review UI steps). Deliberate, tiny behavior change — a group whose only remaining copies are `missing` is no longer counted (it is not reviewable and reclaims nothing).

**Context:** Current SQL (`src/catalog/store.rs:146-152`) uses `status IN ('active','missing')`, so the Overview / `status` count can exceed the number of groups review actually shows. This is the ONE intended behavior change in this plan.

- [ ] **Step 1: Change the SQL.** In `duplicate_group_count` (`src/catalog/store.rs:147-150`), change the inner filter from `status IN ('active','missing')` to `status='active'`:

```rust
    pub fn duplicate_group_count(&self) -> anyhow::Result<i64> {
        let n = self.conn.query_row(
            "SELECT count(*) FROM (SELECT content_hash FROM files
                 WHERE status='active' GROUP BY content_hash HAVING count(*) > 1)",
            [], |r| r.get(0),
        )?;
        Ok(n)
    }
```

- [ ] **Step 2: Check the two inline tests** (`src/catalog/store.rs:507`, `:555`). Read them: both currently assert `== 1`. If their fixtures use only `active` rows, they still pass unchanged. If either seeds a `missing` row that was previously counted, update the assertion to the aligned value and add a one-line comment. Add a NEW focused test proving the alignment if one doesn't already exist:

```rust
    #[test]
    fn duplicate_group_count_ignores_all_missing_groups() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        // Two files sharing a hash, both marked missing -> not a reviewable group.
        let mut f = crate::catalog::models::NewFile {
            volume_id: "v".into(), relative_path: "a".into(), filename: "a".into(),
            extension: "".into(), size_bytes: 1, content_hash: "dup".into(),
            created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Other, container_chain: None };
        cat.upsert_file(&f, 1).unwrap();
        f.relative_path = "b".into(); f.filename = "b".into();
        cat.upsert_file(&f, 1).unwrap();
        // Both rows have last_seen_at=1; a scan starting at 300 sweeps anything not seen this pass
        // (last_seen_at < 300) to missing. Signature: mark_missing_scanned(volume_id, scan_started_at, now).
        cat.mark_missing_scanned("v", 300, 300).unwrap();
        assert_eq!(cat.duplicate_group_count().unwrap(), 0); // active-only: no reviewable groups
    }
```

This uses the module's real temp-catalog helper `open_tmp()` and the real `mark_missing_scanned(volume_id, scan_started_at, now)` signature (confirmed in `src/catalog/store.rs`). The assertion (count == 0 for an all-missing group) is the point.

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p cleanupstorages`
Expected: PASS (with the count test asserting the aligned semantics).

- [ ] **Step 4: Commit**

```bash
git add src/catalog/store.rs
git commit -m "refactor(catalog): count only active (reviewable) duplicate groups"
```

---

### Task 2: `check_csrf` helper

**Files:**
- Modify: `src/web.rs`

**Interfaces:**
- Produces: `fn check_csrf(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, String)>` — returns `Ok(())` when `x-cleanup-token` matches `state.csrf_token`; else logs `tracing::warn!("rejected request: missing or bad CSRF token")` and returns `Err((StatusCode::FORBIDDEN, "missing or bad token".into()))`.

**Context:** The identical 4-line check opens 6 handlers (`api_quarantine`, `api_repack`, `api_forget_drive`, `api_purge_all`, `api_scan`, `api_pick_folder`).

- [ ] **Step 1: Add the helper** near `err500` (`src/web.rs:201`):

```rust
/// CSRF gate for mutating endpoints: require the per-run token (a cross-site page can't read it).
/// Call this FIRST in every mutating handler, before any catalog/filesystem/dialog access.
fn check_csrf(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, String)> {
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok())
        == Some(state.csrf_token.as_str());
    if !ok {
        tracing::warn!("rejected request: missing or bad CSRF token");
        return Err((StatusCode::FORBIDDEN, "missing or bad token".into()));
    }
    Ok(())
}
```

- [ ] **Step 2: Replace the inline block in all 6 handlers** with a single first line `check_csrf(&headers, &state)?;`. Each handler already binds `headers: HeaderMap` and `State(state)`. Remove the old `let ok = …; if !ok { … }` block and its comment. Verify only the helper's warn string remains:

Run: `grep -c "missing or bad CSRF token" src/web.rs`
Expected: 1.

- [ ] **Step 3: Run the CSRF + web tests**

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS — including `csrf_rejection_is_logged` (asserts the WARN + "token") and every `*_requires_csrf_token` / `*_requires_token` test. Observable behavior is unchanged.

- [ ] **Step 4: Commit**

```bash
git add src/web.rs
git commit -m "refactor(web): extract check_csrf helper (was duplicated across 6 handlers)"
```

---

### Task 3: `now_secs` + `snapshot_after_mutation` helpers (web)

**Files:**
- Modify: `src/web.rs`

**Interfaces:**
- Produces: `fn now_secs() -> Result<i64, (StatusCode, String)>` — current UNIX seconds, mapping a clock error to a 500 via `err500`.
- Produces: `fn snapshot_after_mutation(state: &AppState, now: i64)` — best-effort catalog snapshot (opens `Config::default_paths()`, calls `backup::snapshot`, ignores all errors; never fails the request).

**Context:** The `SystemTime → secs` computation appears 4× and the `if let Ok(cfg) = Config::default_paths() { let _ = snapshot(...) }` block appears 4× in the four writing handlers (`api_quarantine`, `api_repack`, `api_forget_drive`, `api_purge_all`).

- [ ] **Step 1: Add both helpers** near `check_csrf`:

```rust
/// Current time as UNIX seconds; a clock error becomes a 500 (matches existing handler behavior).
fn now_secs() -> Result<i64, (StatusCode, String)> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64)
}

/// Best-effort catalog snapshot after a mutation. Never fails the request — a snapshot error is
/// swallowed, exactly as the inlined blocks did.
fn snapshot_after_mutation(state: &AppState, now: i64) {
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
}
```

- [ ] **Step 2: Replace the inlined code** in the four handlers. Each `let now = std::time::SystemTime::now()…as i64;` becomes `let now = now_secs()?;`. Each best-effort snapshot block becomes `snapshot_after_mutation(&state, now);`. Preserve placement (forget/purge snapshot BEFORE the mutation; quarantine/repack AFTER — keep each call where the inlined block was). Verify:

Run: `grep -c "duration_since(std::time::UNIX_EPOCH)" src/web.rs`
Expected: 1 (only inside `now_secs`).

- [ ] **Step 3: Run the web tests**

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS (behavior identical).

- [ ] **Step 4: Commit**

```bash
git add src/web.rs
git commit -m "refactor(web): extract now_secs + snapshot_after_mutation helpers"
```

---

### Task 7: Extract the image-preview helpers into `src/image_preview.rs`

**Files:**
- Create: `src/image_preview.rs`
- Modify: `src/web.rs` (remove the two fns; call the new module), `src/lib.rs` (`pub mod image_preview;`)

**Interfaces:**
- Produces: `image_preview::thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>>` and `image_preview::read_zip_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>>` — the two pure helpers, moved verbatim, now `pub(crate)`.

**Context:** `thumbnail_jpeg` (`src/web.rs:351`) and `read_zip_entry` (`src/web.rs:360`) have no HTTP concern; they're pure byte→byte functions used by `api_preview`. Moving them is a genuine cohesion win. `PREVIEW_MAX_DIM` and the `api_preview` handler stay in `web.rs`.

- [ ] **Step 1: Create `src/image_preview.rs`** with the two functions moved verbatim (make them `pub(crate)`), plus their imports:

```rust
//! Pure image/zip helpers for photo previews: decode+downscale to a JPEG thumbnail, and read one
//! entry's bytes from a zip. No HTTP concern — kept out of `web.rs` so each stays focused.

use std::path::Path;

/// Decode any supported image, downscale to fit `max_dim` on the longest side, re-encode as JPEG.
pub(crate) fn thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
    let thumb = img.thumbnail(max_dim, max_dim); // preserves aspect ratio, never upsizes past bounds
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg)?;
    Ok(out.into_inner())
}

/// Read one top-level entry's bytes from a zip archive.
pub(crate) fn read_zip_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}
```

Also MOVE the existing unit test `thumbnail_downscales_and_encodes_jpeg` (currently in `web.rs` tests, ~line 840) into an inline `#[cfg(test)] mod tests` in `image_preview.rs`, adjusting the call to the local `thumbnail_jpeg`. (Check the test's exact body in `web.rs` and reproduce it; it builds an image in-memory and asserts the output decodes.)

- [ ] **Step 2: Register the module** in `src/lib.rs`: add `pub mod image_preview;` with the other `pub mod` lines.

- [ ] **Step 3: Update `web.rs`.** Delete `thumbnail_jpeg` and `read_zip_entry` (and their now-unused imports, if any became unused — e.g. the `zip`/`image` uses may still be needed elsewhere in web.rs; only remove imports the compiler flags). Update `api_preview`'s two call sites to `crate::image_preview::read_zip_entry(...)` and `crate::image_preview::thumbnail_jpeg(...)`. Remove the moved test from `web.rs`.

- [ ] **Step 4: Build + run the full suite**

Run: `cargo build` then `cargo test -p cleanupstorages`
Expected: PASS, no unused-import/dead-code warnings. The preview endpoint tests (`api_preview_*`) and the moved thumbnail test are green.

- [ ] **Step 5: Commit**

```bash
git add src/image_preview.rs src/lib.rs src/web.rs
git commit -m "refactor(web): extract pure image/zip preview helpers into image_preview module"
```

---

### Task 4: CLI prologue helpers (`src/commands.rs`)

**Files:**
- Modify: `src/commands.rs`

**Interfaces:**
- Produces: `fn open_catalog() -> anyhow::Result<(Config, Catalog)>` — `Config::default_paths()` + `Catalog::open(&cfg.catalog_path)`.
- Produces: `fn open_catalog_checked() -> anyhow::Result<(Config, Catalog)>` — as above plus the integrity check that `cmd_scan`/`cmd_browse` run (bail with the "restore the latest snapshot from …" message on failure).
- Produces: `fn snapshot(cfg: &Config, now: i64) -> anyhow::Result<std::path::PathBuf>` — wraps `backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)`.

**Context:** All 9 commands open config+catalog by hand; 5 repeat the identical snapshot line; 2 repeat the integrity-check-then-bail block. `now_secs()` already exists here and is reused.

- [ ] **Step 1: Add the three helpers** (in `src/commands.rs`, after `now_secs`):

```rust
/// Open the config and catalog — the prologue every command shares.
fn open_catalog() -> anyhow::Result<(Config, Catalog)> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    Ok((cfg, cat))
}

/// Like `open_catalog`, plus the integrity guard used before scanning/serving: refuse to act on a
/// catalog that fails its check and point at the snapshots.
fn open_catalog_checked() -> anyhow::Result<(Config, Catalog)> {
    let (cfg, cat) = open_catalog()?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }
    Ok((cfg, cat))
}

/// Timestamped catalog snapshot (the CLI's audit/rollback point).
fn snapshot(cfg: &Config, now: i64) -> anyhow::Result<std::path::PathBuf> {
    backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)
}
```

- [ ] **Step 2: Rewrite each command's prologue.** In every `cmd_*`, replace the `let cfg = Config::default_paths()?; let cat = Catalog::open(&cfg.catalog_path)?;` pair with `let (cfg, cat) = open_catalog()?;`. In `cmd_scan` and `cmd_browse`, replace the pair AND the following integrity-check block with `let (cfg, cat) = open_catalog_checked()?;`. Replace each `backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?` with `snapshot(&cfg, now)?`. Keep every `println!`, control-flow, and the pre/post ordering of snapshots exactly as-is. Where a command doesn't use `cfg` after opening (only `cat`), bind `let (_cfg, cat) = …;` or keep `cfg` if the snapshot needs it — let the compiler's unused-variable check guide you (no `#[allow]`).

- [ ] **Step 3: Build (watch for unused `cfg`/`mut`) and run**

Run: `cargo build` then `cargo test -p cleanupstorages`
Expected: PASS, no warnings. All CLI integration tests (`forget_cli` and the others) stay green.

- [ ] **Step 4: Commit**

```bash
git add src/commands.rs
git commit -m "refactor(cli): extract open_catalog / open_catalog_checked / snapshot prologue helpers"
```

---

## Self-review notes

- **Spec coverage:** item 1 → Task 1; item 2 → Task 2; item 3 → Task 3; item 4 → Task 4; item 5 → Task 5; item 6 → Task 6; targeted preview extraction → Task 7. All spec items mapped.
- **Ordering:** store tasks first (1 → 5 → 6) so the catalog layer settles before the web/CLI tasks that call it; web tasks next (2 → 3 → 7); CLI last (4). Each task is independent and separately revertible; none depends on another's new symbol except that all assume Task 1's rename-free baseline (Task 5's rename is isolated to one method name).
- **Behavior preservation:** every task's gate is the pre-existing suite staying green, except Task 6 which intentionally changes one count and updates its test to match — called out explicitly.
- **Type/name consistency:** `check_csrf(&headers, &state)`, `now_secs() -> Result<i64,(StatusCode,String)>`, `snapshot_after_mutation(&state, now)`, `open_catalog()/open_catalog_checked() -> (Config, Catalog)`, `snapshot(&cfg, now)`, `loose_file_id`, `FILE_COLUMNS`, `image_preview::{thumbnail_jpeg, read_zip_entry}` — used consistently across the tasks that reference them.
- **No placeholders:** every code step shows the actual code; the two spots needing the implementer to confirm a real symbol (the missing-sweep method in Task 6; which imports become unused in Task 7) are called out with how to resolve.

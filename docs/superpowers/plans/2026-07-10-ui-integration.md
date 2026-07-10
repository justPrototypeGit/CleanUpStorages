# UI Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Google Stitch mockups in `StitchExport/` into the app's real UI — six pages (Overview, Browse, Duplicates, Drives, Scan, Console) sharing one self-contained macOS-glass design system, fully wired to the live API — plus the small backend additions two of those pages need.

**Architecture:** Backend gains six thin additions (activity feed reader + scan logging, per-volume reclaimable bytes, disk capacity + `/api/drives`, `forget_volume`, `purge_all`) each with CLI/web parity where the rest of the tool has it. Frontend gains a shared shell module (`src/web_ui.rs`) holding one design-system `<style>`, an inline SVG icon set, a sidebar/toolbar shell renderer, and shared JS helpers; all six page handlers render through it. No CDN, no external fonts/icons, no build step — consistent with the existing `src/web.rs` security posture.

**Tech Stack:** Rust, axum 0.7, rusqlite (SQLite), sysinfo (already a dep — used for disk capacity), tokio. Plain HTML/CSS/JS embedded as Rust string consts. Tests: inline `#[cfg(test)]` unit + axum `oneshot` handler tests, plus CLI integration tests via `CARGO_BIN_EXE_cleanupstorages`.

## Global Constraints

- **Self-contained pages only.** No external network requests: no CDN scripts, no Google Fonts, no icon web-fonts. Every page must pass the existing assertion `!body.contains("http://") && !body.contains("https://")`. Fonts are system stacks; icons are inline SVG.
- **Reliability dominates.** No new destructive action beyond what exists. `forget_volume` deletes only catalog rows, never files on disk. `purge_all` is only the existing `purge_volume` looped over mounted volumes.
- **CSRF on every mutating endpoint.** The `x-cleanup-token` header must equal `state.csrf_token`, checked FIRST before any catalog access, returning `403` + a `tracing::warn!` on mismatch — identical to the existing `api_quarantine`/`api_repack`/`api_scan` pattern.
- **Reads use `Catalog::open_readonly`; writes use `Catalog::open`.** Never open read-write in a read-only handler.
- **XSS-safe rendering.** Client JS inserts DB-derived text only via the existing `esc()` helper or `textContent`, never raw `innerHTML` of server data.
- **Conventional Commits**, scopes from CLAUDE.md: `catalog`, `scanner`, `cli`, `review`, `storage`, plus `web` for the UI. Every commit message body ends with the `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer.
- **Windows/PowerShell dev environment.** Build with `cargo build --release`; run tests with `cargo test`.

## File structure

- `src/catalog/store.rs` — MODIFY: add `recent_actions`, `reclaimable_bytes_by_volume`, `forget_volume`, `volume_last_seen`, `volume_has_scan_errors`.
- `src/scanner.rs` — MODIFY: log a `"scan"` action from `run_scan`.
- `src/mounts.rs` — MODIFY: add `disk_capacity(path) -> Option<(u64,u64)>`.
- `src/purge.rs` — MODIFY: add `purge_all` wrapper returning a structured per-volume result.
- `src/web.rs` — MODIFY: add DTOs + handlers (`/api/activity`, `/api/drives`, `/api/forget-drive`, `/api/purge-all`), add routes (`/browse`, `/drives`, `/console`), move Browse off `/` to Overview, render every page through `web_ui`.
- `src/web_ui.rs` — CREATE: shared `STYLE` const, inline SVG icon map, `shell()` renderer, `SHARED_JS` (esc/fmtSize/fmtDate), and the six page bodies.
- `src/commands.rs` — MODIFY: add `cmd_forget`, extend `cmd_purge` for `--all`.
- `src/main.rs` — MODIFY: add `Forget` subcommand, add `--all` flag to `Purge`.
- `src/lib.rs` — MODIFY: `pub mod web_ui;`.
- `tests/` — existing integration tests continue; CLI parity covered by new inline/integration tests.
- `docs/TESTING-GUIDE.md`, `CLAUDE.md` — MODIFY: document new pages/commands.

---

### Task 1: Activity feed reader + scan-completion logging

**Files:**
- Modify: `src/catalog/store.rs` (add `recent_actions` near `log_action` ~line 346)
- Modify: `src/scanner.rs` (`run_scan`, ~line 200)

**Interfaces:**
- Produces: `Catalog::recent_actions(&self, limit: usize) -> anyhow::Result<Vec<(String, String, i64)>>` returning `(action, details_json, occurred_at)` newest-first.
- Produces: `run_scan` now appends one `actions_log` row with action `"scan"` and details `{"volume_id","label","hashed","skipped","errors","marked_missing","archive_entries"}` on a successful (non-skipped) scan.

- [ ] **Step 1: Write the failing test** (append inside the existing `#[cfg(test)] mod tests` in `src/catalog/store.rs`)

```rust
    #[test]
    fn recent_actions_returns_newest_first() {
        let (_t, cat) = open_tmp(); // existing test helper in this module
        cat.log_action("quarantine", "{\"file_id\":1}", 100).unwrap();
        cat.log_action("purge", "{\"volume_id\":\"v\"}", 200).unwrap();
        let rows = cat.recent_actions(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "purge");      // newest first
        assert_eq!(rows[0].2, 200);
        assert_eq!(rows[1].0, "quarantine");
        // limit is respected
        assert_eq!(cat.recent_actions(1).unwrap().len(), 1);
    }
```

If `open_tmp()` is not the helper name in this module, use the same construction the neighbouring `log_action_appends` test uses to get a `Catalog`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cleanupstorages recent_actions_returns_newest_first`
Expected: FAIL — `no method named recent_actions`.

- [ ] **Step 3: Implement `recent_actions`** (in `src/catalog/store.rs`, right after `log_action`)

```rust
    /// The most recent `limit` audit entries, newest first: (action, details_json, occurred_at).
    pub fn recent_actions(&self, limit: usize) -> anyhow::Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT action, IFNULL(details,''), occurred_at FROM actions_log
             ORDER BY occurred_at DESC, id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?,
        )))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p cleanupstorages recent_actions_returns_newest_first`
Expected: PASS.

- [ ] **Step 5: Log scan completion from `run_scan`** (in `src/scanner.rs`, replace the tail of `run_scan` after `scan_volume_with_progress` returns)

```rust
    let summary = scan_volume_with_progress(cat, mount_root, &identity, force, now, progress)?;
    // Audit trail: one row per completed scan so the Overview "recent activity" feed can show it.
    let _ = cat.log_action("scan", &serde_json::json!({
        "volume_id": identity.volume_id, "label": identity.label,
        "hashed": summary.hashed, "skipped": summary.skipped, "errors": summary.errors,
        "marked_missing": summary.marked_missing, "archive_entries": summary.archive_entries,
    }).to_string(), now);
    Ok(Some((identity, summary)))
```

- [ ] **Step 6: Write a test that a scan logs an action** (append to `src/scanner.rs`'s test module; mirror how existing scanner tests build a catalog + marked drive. If scanner tests already have a helper that runs a scan against a temp drive, reuse it.)

```rust
    #[test]
    fn run_scan_logs_a_scan_action() {
        // Arrange: a temp drive with one file and a catalog (reuse this module's scan test setup).
        let (_tmp, root, cat) = setup_marked_drive_with_one_file(); // use the module's existing helper
        let n = crate::scanner::run_scan(&cat, &root, false,
            crate::volume::ReadonlyMode::Fingerprint, 1234, None).unwrap();
        assert!(n.is_some());
        let acts = cat.recent_actions(10).unwrap();
        assert!(acts.iter().any(|(a, d, t)| a == "scan" && *t == 1234 && d.contains("\"hashed\"")));
    }
```

If no such helper exists, construct the drive inline the way the nearest existing `run_scan`/`scan_volume` test does (temp dir + `std::fs::write` a file + `Catalog::open`).

- [ ] **Step 7: Run scanner tests**

Run: `cargo test -p cleanupstorages --lib scanner`
Expected: PASS (new test green, existing scanner tests still green).

- [ ] **Step 8: Commit**

```bash
git add src/catalog/store.rs src/scanner.rs
git commit -m "feat(catalog): read recent actions; log scan completions to audit trail"
```

---

### Task 2: `/api/activity` endpoint

**Files:**
- Modify: `src/web.rs` (DTO + handler + route)

**Interfaces:**
- Consumes: `Catalog::recent_actions` (Task 1).
- Produces: `GET /api/activity` → `Vec<ActivityDto { kind: String, summary: String, occurred_at: i64 }>`, summary formatted server-side per action kind.

- [ ] **Step 1: Write the failing test** (in `src/web.rs` test module; `seed_dupes()` already exists and seeds a catalog + Fixed mounts)

```rust
    #[tokio::test]
    async fn api_activity_returns_formatted_rows() {
        let (_t, db, state) = seed_dupes();
        { // write an action to read back
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.log_action("purge",
                "{\"volume_id\":\"vol-1\",\"files_purged\":3,\"bytes_reclaimed\":2048}", 500).unwrap();
        }
        let v = get_json_state(state, "/api/activity").await;
        let arr = v.as_array().unwrap();
        assert!(!arr.is_empty());
        assert_eq!(arr[0]["kind"], "purge");
        assert!(arr[0]["summary"].as_str().unwrap().contains("Purged"));
        assert_eq!(arr[0]["occurred_at"], 500);
    }
```

Check `seed_dupes`'s return shape in this test module; if it returns `(_t, state)` without the db path, open the catalog via `state.catalog_path.clone()` instead.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cleanupstorages api_activity_returns_formatted_rows`
Expected: FAIL — route not found (404) / handler missing.

- [ ] **Step 3: Add the DTO, summary formatter, handler** (in `src/web.rs`, near the other DTOs ~line 414)

```rust
#[derive(Serialize)]
struct ActivityDto { kind: String, summary: String, occurred_at: i64 }

/// Human summary for one audit row. `details` is the JSON stored by the engine; parse best-effort
/// and fall back to the raw action name so a schema change can never break the feed.
fn activity_summary(action: &str, details: &str) -> String {
    let d: serde_json::Value = serde_json::from_str(details).unwrap_or(serde_json::Value::Null);
    let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| d.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    match action {
        "scan" => format!("Scanned {} — {} hashed, {} unchanged", s("label"), n("hashed"), n("skipped")),
        "quarantine" => format!("Quarantined {}", if s("filename").is_empty() { s("relative_path") } else { s("filename") }),
        "quarantine_skip" => "Skipped a file to protect the last copy".to_string(),
        "quarantine_error" => "A file could not be quarantined".to_string(),
        "repack" => format!("Repacked an archive (removed {})", s("removed_entry")),
        "purge" => format!("Purged {} file(s), reclaimed {} MiB", n("files_purged"), n("bytes_reclaimed") / (1024 * 1024)),
        "forget" => format!("Removed drive '{}' from the catalog", s("label")),
        other => other.to_string(),
    }
}

async fn api_activity(State(state): State<AppState>)
    -> Result<Json<Vec<ActivityDto>>, (StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let rows = cat.recent_actions(30).map_err(err500)?;
    Ok(Json(rows.into_iter().map(|(action, details, occurred_at)| ActivityDto {
        summary: activity_summary(&action, &details), kind: action, occurred_at,
    }).collect()))
}
```

- [ ] **Step 4: Register the route** (in `build_router_with`, with the other `/api/*` GET routes)

```rust
        .route("/api/activity", get(api_activity))
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p cleanupstorages api_activity_returns_formatted_rows`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): add /api/activity feed with server-side summaries"
```

---

### Task 3: Per-volume reclaimable bytes

**Files:**
- Modify: `src/catalog/store.rs`

**Interfaces:**
- Produces: `Catalog::reclaimable_bytes_by_volume(&self) -> anyhow::Result<std::collections::HashMap<String, i64>>` — for each volume, the total `size_bytes` of active files that are duplicate-group members but NOT the group's suggested-keep (earliest-created, fallback earliest-modified, fallback smallest id), i.e. the bytes the review flow would suggest quarantining.

- [ ] **Step 1: Write the failing test** (in `src/catalog/store.rs` tests)

```rust
    #[test]
    fn reclaimable_bytes_by_volume_excludes_the_keep() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let mut f = crate::catalog::models::NewFile {
            volume_id: "v".into(), relative_path: "a.bin".into(), filename: "a.bin".into(),
            extension: "bin".into(), size_bytes: 100, content_hash: "dup".into(),
            created_time: Some(10), modified_time: Some(10), accessed_time: None,
            category: crate::category::Category::Other, container_chain: None };
        cat.upsert_file(&f, 1).unwrap();                     // keep (created 10)
        f.relative_path = "b.bin".into(); f.filename = "b.bin".into();
        f.created_time = Some(20);                            // newer duplicate -> reclaimable
        cat.upsert_file(&f, 1).unwrap();
        f.relative_path = "u.bin".into(); f.filename = "u.bin".into();
        f.content_hash = "uniq".into(); f.size_bytes = 999;   // unique -> not counted
        cat.upsert_file(&f, 1).unwrap();
        let map = cat.reclaimable_bytes_by_volume().unwrap();
        assert_eq!(map.get("v").copied().unwrap_or(0), 100); // only the non-keep duplicate
    }
```

Use the same temp-catalog helper the other store tests use (`open_tmp` here is a placeholder for whatever this module already provides).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cleanupstorages reclaimable_bytes_by_volume_excludes_the_keep`
Expected: FAIL — method missing.

- [ ] **Step 3: Implement** (in `src/catalog/store.rs`). Reuse `duplicate_groups()` (already defined ~line 233) and the same keep rule the web layer uses, so results can't drift from the review UI.

```rust
    /// Bytes-per-volume of active duplicate members that are NOT their group's suggested keep.
    /// Suggested keep = earliest created_time, then earliest modified_time, then smallest id.
    pub fn reclaimable_bytes_by_volume(&self) -> anyhow::Result<std::collections::HashMap<String, i64>> {
        let mut out: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for group in self.duplicate_groups()? {
            let keep = group.iter().min_by_key(|f| (
                f.created_time.unwrap_or(i64::MAX), f.modified_time.unwrap_or(i64::MAX), f.id,
            )).map(|f| f.id).unwrap_or(0);
            for f in &group {
                if f.id != keep {
                    *out.entry(f.volume_id.clone()).or_default() += f.size_bytes;
                }
            }
        }
        Ok(out)
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p cleanupstorages reclaimable_bytes_by_volume_excludes_the_keep`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/catalog/store.rs
git commit -m "feat(catalog): compute per-volume reclaimable (non-keep duplicate) bytes"
```

---

### Task 4: Disk capacity helper + `/api/drives` endpoint

**Files:**
- Modify: `src/mounts.rs` (capacity helper)
- Modify: `src/catalog/store.rs` (`volume_last_seen`, `volume_has_scan_errors`)
- Modify: `src/web.rs` (DTO + handler + route)

**Interfaces:**
- Produces: `mounts::disk_capacity(path: &std::path::Path) -> Option<(u64, u64)>` → `(total_bytes, available_bytes)`, `None` if undeterminable. Mirrors the existing `repack::available_space` sysinfo pattern.
- Produces: `Catalog::volume_last_seen(&self, volume_id: &str) -> anyhow::Result<Option<i64>>`.
- Produces: `Catalog::volume_has_scan_errors(&self, volume_id: &str) -> anyhow::Result<bool>`.
- Produces: `GET /api/drives` → `Vec<DriveDto>` (see below), one row per catalogued volume, sorted by label.

- [ ] **Step 1: Write the failing capacity test** (in `src/mounts.rs` tests)

```rust
    #[test]
    fn disk_capacity_of_temp_dir_is_some_and_sane() {
        let tmp = tempfile::tempdir().unwrap();
        // The temp dir lives on a real mounted filesystem, so capacity should resolve.
        let cap = disk_capacity(tmp.path());
        if let Some((total, avail)) = cap {
            assert!(total > 0);
            assert!(avail <= total);
        } // On some CI filesystems this can be None; the None branch is acceptable.
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cleanupstorages disk_capacity_of_temp_dir`
Expected: FAIL — `disk_capacity` not found.

- [ ] **Step 3: Implement `disk_capacity`** (in `src/mounts.rs`)

```rust
/// (total, available) bytes on the filesystem holding `path`, by longest-matching mount point.
/// None if it can't be determined. Same longest-prefix approach as `repack::available_space`.
pub fn disk_capacity(path: &std::path::Path) -> Option<(u64, u64)> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point();
        if path.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(b, _, _)| len > b).unwrap_or(true) {
                best = Some((len, d.total_space(), d.available_space()));
            }
        }
    }
    best.map(|(_, t, a)| (t, a))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p cleanupstorages disk_capacity_of_temp_dir`
Expected: PASS.

- [ ] **Step 5: Add the two small catalog readers** (in `src/catalog/store.rs`)

```rust
    /// The volume's last_seen_at (updated on every scan), if the volume exists.
    pub fn volume_last_seen(&self, volume_id: &str) -> anyhow::Result<Option<i64>> {
        let row = self.conn.query_row(
            "SELECT last_seen_at FROM volumes WHERE volume_id=?1",
            params![volume_id], |r| r.get::<_, i64>(0));
        match row {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// True if this volume has any recorded scan error.
    pub fn volume_has_scan_errors(&self, volume_id: &str) -> anyhow::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM scan_errors WHERE volume_id=?1",
            params![volume_id], |r| r.get(0))?;
        Ok(n > 0)
    }
```

- [ ] **Step 6: Add a store test for both** (in `src/catalog/store.rs` tests)

```rust
    #[test]
    fn volume_last_seen_and_scan_errors() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "V".into(), identified_by: "marker".into(),
            first_seen_at: 5, last_seen_at: 42 }).unwrap();
        assert_eq!(cat.volume_last_seen("v").unwrap(), Some(42));
        assert_eq!(cat.volume_last_seen("nope").unwrap(), None);
        assert_eq!(cat.volume_has_scan_errors("v").unwrap(), false);
        cat.log_scan_error(Some("v"), "some/path", "permission denied", 9).unwrap();
        assert_eq!(cat.volume_has_scan_errors("v").unwrap(), true);
    }
```

- [ ] **Step 7: Add `DriveDto` + `api_drives` handler** (in `src/web.rs`). It joins catalog volume stats with live mounts + capacity.

```rust
#[derive(Serialize)]
struct DriveDto {
    volume_id: String,
    label: String,
    mount_path: Option<String>,   // None if not currently connected
    connected: bool,
    active_files: i64,
    active_bytes: i64,
    total_bytes: Option<u64>,     // None if unmounted or undeterminable
    free_bytes: Option<u64>,
    reclaimable_bytes: i64,
    last_seen_at: Option<i64>,
    has_errors: bool,
}

async fn api_drives(State(state): State<AppState>)
    -> Result<Json<Vec<DriveDto>>, (StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let reclaim = cat.reclaimable_bytes_by_volume().map_err(err500)?;
    let mounts = state.mounts.snapshot();
    let mut out = Vec::new();
    for (volume_id, label, active_files, active_bytes) in cat.volume_stats().map_err(err500)? {
        let mount_path = mounts.get(&volume_id).cloned();
        let (total_bytes, free_bytes) = match &mount_path {
            Some(p) => match crate::mounts::disk_capacity(p) { Some((t, f)) => (Some(t), Some(f)), None => (None, None) },
            None => (None, None),
        };
        out.push(DriveDto {
            connected: mount_path.is_some(),
            mount_path: mount_path.map(|p| p.display().to_string()),
            reclaimable_bytes: reclaim.get(&volume_id).copied().unwrap_or(0),
            last_seen_at: cat.volume_last_seen(&volume_id).map_err(err500)?,
            has_errors: cat.volume_has_scan_errors(&volume_id).map_err(err500)?,
            volume_id, label, active_files, active_bytes, total_bytes, free_bytes,
        });
    }
    Ok(Json(out))
}
```

- [ ] **Step 8: Register the route** (in `build_router_with`)

```rust
        .route("/api/drives", get(api_drives))
```

- [ ] **Step 9: Write the endpoint test** (in `src/web.rs` tests)

```rust
    #[tokio::test]
    async fn api_drives_lists_catalogued_volume_with_reclaimable() {
        let (_t, _db, state) = seed_dupes(); // seeds vol-1 "Photos HDD" with a duplicate group
        let v = get_json_state(state, "/api/drives").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], "Photos HDD");
        assert_eq!(arr[0]["connected"], true); // Fixed mount is present
        assert!(arr[0]["reclaimable_bytes"].as_i64().unwrap() >= 0);
    }
```

- [ ] **Step 10: Run all new tests**

Run: `cargo test -p cleanupstorages disk_capacity_of_temp_dir volume_last_seen_and_scan_errors api_drives_lists_catalogued_volume_with_reclaimable`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add src/mounts.rs src/catalog/store.rs src/web.rs
git commit -m "feat(web): add /api/drives with real disk capacity, last-scan, reclaimable"
```

---

### Task 5: Forget-drive — engine + CLI + web

**Files:**
- Modify: `src/catalog/store.rs` (`forget_volume`)
- Modify: `src/commands.rs` (`cmd_forget`)
- Modify: `src/main.rs` (`Forget` subcommand + dispatch + span name)
- Modify: `src/web.rs` (DTO + handler + route)

**Interfaces:**
- Produces: `Catalog::forget_volume(&self, volume_id: &str, now: i64) -> anyhow::Result<usize>` — deletes the volume's `files` rows and its `volumes` row inside one transaction, logs a `"forget"` action, returns the number of file rows removed. Files on disk are never touched.
- Produces: `commands::cmd_forget(mount: &Path) -> anyhow::Result<()>`.
- Produces: `POST /api/forget-drive {volume_id: String}` → `ForgetResultDto { removed_files: usize }`, CSRF-gated.

- [ ] **Step 1: Write the failing store test** (in `src/catalog/store.rs` tests)

```rust
    #[test]
    fn forget_volume_deletes_rows_and_fts_but_returns_count() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "Gone".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let f = crate::catalog::models::NewFile {
            volume_id: "v".into(), relative_path: "a.txt".into(), filename: "a.txt".into(),
            extension: "txt".into(), size_bytes: 1, content_hash: "h".into(),
            created_time: None, modified_time: None, accessed_time: None,
            category: crate::category::Category::Other, container_chain: None };
        cat.upsert_file(&f, 1).unwrap();
        let removed = cat.forget_volume("v", 500).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(cat.volume_last_seen("v").unwrap(), None);          // volume row gone
        assert!(cat.search("a", None, None, None).unwrap().is_empty()); // FTS row gone
        assert!(cat.recent_actions(5).unwrap().iter().any(|(a, _, _)| a == "forget"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cleanupstorages forget_volume_deletes_rows`
Expected: FAIL — method missing.

- [ ] **Step 3: Implement `forget_volume`** (in `src/catalog/store.rs`). Delete files first (FTS `files_ad` trigger fires per row), then the volume row, in a transaction.

```rust
    /// Remove ALL catalog knowledge of a volume: its file rows (FTS cleaned up by triggers) and
    /// its `volumes` row. Never touches files on disk — a later rescan fully rebuilds the volume.
    /// Returns the number of file rows removed, and logs a `forget` audit action.
    pub fn forget_volume(&self, volume_id: &str, now: i64) -> anyhow::Result<usize> {
        let label: String = self.conn.query_row(
            "SELECT label FROM volumes WHERE volume_id=?1", params![volume_id],
            |r| r.get(0)).unwrap_or_else(|_| volume_id.to_string());
        self.conn.execute_batch("BEGIN")?;
        let removed = self.conn.execute("DELETE FROM files WHERE volume_id=?1", params![volume_id])?;
        self.conn.execute("DELETE FROM scan_errors WHERE volume_id=?1", params![volume_id])?;
        self.conn.execute("DELETE FROM volumes WHERE volume_id=?1", params![volume_id])?;
        self.conn.execute_batch("COMMIT")?;
        self.log_action("forget", &serde_json::json!({
            "volume_id": volume_id, "label": label, "removed_files": removed }).to_string(), now)?;
        Ok(removed)
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p cleanupstorages forget_volume_deletes_rows`
Expected: PASS.

- [ ] **Step 5: Add `cmd_forget`** (in `src/commands.rs`, after `cmd_purge`). Snapshot before, resolve marker to a volume id, forget, report.

```rust
pub fn cmd_forget(mount: &Path) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}; nothing to forget", mount.display()))?;
    let now = now_secs();
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot (pre-forget): {}", snap.display());
    let removed = cat.forget_volume(&vid, now)?;
    println!("Forgot volume {vid}: removed {removed} catalog entries. Files on disk are untouched; rescan to re-add.");
    Ok(())
}
```

- [ ] **Step 6: Add the `Forget` subcommand** (in `src/main.rs`: enum variant, span-name arm, dispatch arm)

```rust
    /// Remove a drive's catalog entries (files on disk untouched; rescan to re-add).
    Forget {
        /// Current mount path of the drive to forget.
        mount: std::path::PathBuf,
    },
```
Add `Command::Forget { .. } => "forget",` to the `name` match, and:
```rust
        Command::Forget { mount } => commands::cmd_forget(&mount),
```

- [ ] **Step 7: Add the web DTO + handler** (in `src/web.rs`). Follow the exact CSRF-first shape of `api_repack`.

```rust
#[derive(Deserialize)]
struct ForgetReq { volume_id: String }

#[derive(Serialize)]
struct ForgetResultDto { removed_files: usize }

async fn api_forget_drive(State(state): State<AppState>, headers: HeaderMap, body: Json<ForgetReq>)
    -> Result<Json<ForgetResultDto>, (StatusCode, String)>
{
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok {
        tracing::warn!("rejected request: missing or bad CSRF token");
        return Err((StatusCode::FORBIDDEN, "missing or bad token".into()));
    }
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    let removed = cat.forget_volume(&body.volume_id, now).map_err(err500)?;
    Ok(Json(ForgetResultDto { removed_files: removed }))
}
```
Register: `.route("/api/forget-drive", post(api_forget_drive))`.

- [ ] **Step 8: Web test — CSRF gate + success** (in `src/web.rs` tests; mirror the quarantine tests using `post_json`)

```rust
    #[tokio::test]
    async fn forget_drive_requires_token_then_removes() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state.clone(), "/api/forget-drive", None,
            serde_json::json!({"volume_id":"vol-1"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, json) = post_json(state, "/api/forget-drive", Some("T"),
            serde_json::json!({"volume_id":"vol-1"})).await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["removed_files"].as_i64().unwrap() >= 1);
    }
```
(`Some("T")` is the token `seed_dupes` assigns — confirm the literal it uses and match it.)

- [ ] **Step 9: CLI integration test** (create `tests/forget_cli.rs`, following the existing CLI integration test pattern using `CARGO_BIN_EXE_cleanupstorages` + `CLEANUPSTORAGES_DATA_DIR`)

```rust
use std::process::Command;

#[test]
fn forget_removes_a_scanned_drive() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let drive = tmp.path().join("DriveX");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("f.txt"), b"hello").unwrap();
    let exe = env!("CARGO_BIN_EXE_cleanupstorages");

    let run = |args: &[&str]| {
        Command::new(exe).args(args)
            .env("CLEANUPSTORAGES_DATA_DIR", &data)
            .output().unwrap()
    };
    // scan (fingerprint fallback so no marker write is needed on odd filesystems)
    let out = run(&["scan", drive.to_str().unwrap(), "--readonly-fallback", "fingerprint"]);
    assert!(out.status.success(), "scan failed: {}", String::from_utf8_lossy(&out.stderr));
    // forget
    let out = run(&["forget", drive.to_str().unwrap()]);
    assert!(out.status.success(), "forget failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("removed"));
    // status now shows no volumes
    let out = run(&["status"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("DriveX"), "volume should be gone from status: {s}");
}
```

- [ ] **Step 10: Run the lot**

Run: `cargo test -p cleanupstorages forget`
Expected: PASS (store, web, and CLI tests).

- [ ] **Step 11: Commit**

```bash
git add src/catalog/store.rs src/commands.rs src/main.rs src/web.rs tests/forget_cli.rs
git commit -m "feat: forget-drive (catalog-only) via CLI and web, files on disk untouched"
```

---

### Task 6: Purge-all — engine + CLI + web

**Files:**
- Modify: `src/purge.rs` (`purge_all`)
- Modify: `src/commands.rs` (`cmd_purge` gains an `all` path)
- Modify: `src/main.rs` (`--all` flag on `Purge`)
- Modify: `src/web.rs` (DTO + handler + route)

**Interfaces:**
- Produces: `purge::purge_all(cat, &mounts, now) -> anyhow::Result<PurgeAllOutcome>` where `mounts: &std::collections::HashMap<String, std::path::PathBuf>` and
  ```rust
  pub struct PurgeAllOutcome {
      pub purged: Vec<(String, usize, i64)>,   // (volume_id, files_purged, bytes_reclaimed)
      pub skipped_unmounted: Vec<String>,       // volume_ids with reclaimable space but not connected
      pub errors: Vec<String>,                  // "<volume_id>: <error>"
  }
  ```
  It purges every mounted volume that has reclaimable quarantine; unmounted volumes with reclaimable space are reported, not purged.
- Produces: `commands::cmd_purge(mount: Option<&Path>, all: bool)` — signature changes; when `all`, iterate live mounts.
- Produces: `POST /api/purge-all` → `PurgeAllResultDto`, CSRF-gated.

- [ ] **Step 1: Write the failing engine test** (in `src/purge.rs` tests; reuse the module's existing quarantine+marker setup helpers)

```rust
    #[test]
    fn purge_all_purges_mounted_and_reports_unmounted() {
        // Reuse this module's helper that builds a marked drive with one quarantined file.
        let (_tmp, root, vid, cat) = setup_quarantined_drive(); // existing-style helper
        let mut mounts = std::collections::HashMap::new();
        mounts.insert(vid.clone(), root.clone());
        let out = purge_all(&cat, &mounts, 1000).unwrap();
        assert_eq!(out.purged.len(), 1);
        assert_eq!(out.purged[0].0, vid);
        assert!(out.skipped_unmounted.is_empty());
        // Second run: nothing left to reclaim.
        let out2 = purge_all(&cat, &mounts, 1001).unwrap();
        assert!(out2.purged.is_empty());
    }
```

If the module lacks a reusable helper, build the quarantined drive inline the way the existing `purge_volume` test does (create marker, quarantine a file via `quarantine::quarantine_files`, then call `purge_all`).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cleanupstorages purge_all_purges_mounted_and_reports_unmounted`
Expected: FAIL — `purge_all` not found.

- [ ] **Step 3: Implement `purge_all`** (in `src/purge.rs`)

```rust
#[derive(Debug, Default)]
pub struct PurgeAllOutcome {
    pub purged: Vec<(String, usize, i64)>,
    pub skipped_unmounted: Vec<String>,
    pub errors: Vec<String>,
}

/// Purge every volume that has reclaimable quarantine. Mounted volumes are purged via
/// `purge_volume`; volumes with reclaimable space that aren't currently mounted are reported in
/// `skipped_unmounted` (you can't delete files on a disk that isn't connected).
pub fn purge_all(
    cat: &Catalog,
    mounts: &std::collections::HashMap<String, std::path::PathBuf>,
    now: i64,
) -> anyhow::Result<PurgeAllOutcome> {
    let mut out = PurgeAllOutcome::default();
    for (volume_id, _label, _files, _bytes) in cat.volume_stats()? {
        let reclaimable = cat.recoverable_bytes(&volume_id)?;
        if reclaimable == 0 { continue; }
        match mounts.get(&volume_id) {
            Some(root) => match purge_volume(cat, root, &volume_id, now) {
                Ok(o) => out.purged.push((volume_id, o.files_purged, o.bytes_reclaimed)),
                Err(e) => out.errors.push(format!("{volume_id}: {e}")),
            },
            None => out.skipped_unmounted.push(volume_id),
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p cleanupstorages purge_all_purges_mounted_and_reports_unmounted`
Expected: PASS.

- [ ] **Step 5: Update `cmd_purge` for `--all`** (in `src/commands.rs`). Change the signature and branch.

```rust
pub fn cmd_purge(mount: Option<&Path>, all: bool) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let now = now_secs();
    // snapshot BEFORE the irreversible delete
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot (pre-purge): {}", snap.display());
    if all {
        let mounts = crate::mounts::live_mounts();
        let out = purge::purge_all(&cat, &mounts, now)?;
        let total: i64 = out.purged.iter().map(|(_, _, b)| *b).sum();
        println!("Purged {} volume(s), reclaimed {} MiB total.", out.purged.len(), total / (1024*1024));
        for v in &out.skipped_unmounted { println!("  skipped (not connected): {v}"); }
        for e in &out.errors { println!("  error: {e}"); }
        return Ok(());
    }
    let mount = mount.ok_or_else(|| anyhow::anyhow!("a mount path is required unless --all is given"))?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}", mount.display()))?;
    let out = purge::purge_volume(&cat, mount, &vid, now)?;
    println!("Purged {} file(s), reclaimed {} MiB.", out.files_purged, out.bytes_reclaimed / (1024*1024));
    Ok(())
}
```

- [ ] **Step 6: Update `main.rs` `Purge`** — make `mount` optional and add `--all`:

```rust
    /// Permanently delete a drive's _ToDelete quarantine and reclaim space.
    Purge {
        /// Current mount path of the drive to purge (omit when using --all).
        mount: Option<std::path::PathBuf>,
        /// Purge every currently-connected drive that has quarantined files.
        #[arg(long)]
        all: bool,
    },
```
Update the dispatch arm:
```rust
        Command::Purge { mount, all } => commands::cmd_purge(mount.as_deref(), all),
```

- [ ] **Step 7: Add the web DTO + handler** (in `src/web.rs`)

```rust
#[derive(Serialize)]
struct PurgeAllResultDto {
    purged_volumes: usize,
    files_purged: usize,
    bytes_reclaimed: i64,
    skipped_unmounted: Vec<String>,
    errors: Vec<String>,
}

async fn api_purge_all(State(state): State<AppState>, headers: HeaderMap)
    -> Result<Json<PurgeAllResultDto>, (StatusCode, String)>
{
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok {
        tracing::warn!("rejected request: missing or bad CSRF token");
        return Err((StatusCode::FORBIDDEN, "missing or bad token".into()));
    }
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    let out = crate::purge::purge_all(&cat, &state.mounts.snapshot(), now).map_err(err500)?;
    Ok(Json(PurgeAllResultDto {
        purged_volumes: out.purged.len(),
        files_purged: out.purged.iter().map(|(_, f, _)| f).sum(),
        bytes_reclaimed: out.purged.iter().map(|(_, _, b)| b).sum(),
        skipped_unmounted: out.skipped_unmounted,
        errors: out.errors,
    }))
}
```
Register: `.route("/api/purge-all", post(api_purge_all))`.

- [ ] **Step 8: Web test** (in `src/web.rs` tests)

```rust
    #[tokio::test]
    async fn purge_all_requires_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/purge-all", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
```

- [ ] **Step 9: Fix other `cmd_purge` callers** — none exist outside `main.rs` (verify with `grep -rn "cmd_purge" src`). Confirm the build.

Run: `cargo build`
Expected: compiles.

- [ ] **Step 10: Run tests**

Run: `cargo test -p cleanupstorages purge`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add src/purge.rs src/commands.rs src/main.rs src/web.rs
git commit -m "feat: purge --all across mounted volumes via CLI and web"
```

---

### Task 7: Shared UI shell + Overview page

**Files:**
- Create: `src/web_ui.rs`
- Modify: `src/lib.rs` (`pub mod web_ui;`)
- Modify: `src/web.rs` (routes: `/` → Overview, `/browse` → Browse; render Overview via `web_ui`)

**Interfaces:**
- Produces: `web_ui::STYLE: &str` — the shared design-system CSS (light+dark via CSS custom properties + `prefers-color-scheme`), covering: layout (sidebar 260px + toolbar + main), `.card`, `.btn`/`.btn-primary`/`.btn-danger`, `.pill`/status pills, `.mono`, table, `.bar`, console. Uses the tokens from `StitchExport/.../precision_file_architect/DESIGN.md` (primary `#0071e3`/`#0A84FF`; neutrals `#ffffff`/`#f5f5f7`/`#1d1d1f`; status amber/red/green/gray).
- Produces: `web_ui::shell(active: &str, csrf: &str, title: &str, main_html: &str, page_script: &str) -> String` — returns a full self-contained HTML document: `<head>` with charset/viewport, `<meta name="csrf">`, `<title>`, `<style>STYLE</style>`; `<body>` with the sidebar (nav items Overview/Browse/Duplicates/Drives/Scan/Console using inline SVG icons, the one matching `active` marked `aria-current` + `.active`), a toolbar, `<main>{main_html}</main>`, `<script>{SHARED_JS}</script>`, and `<script>{page_script}</script>`.
- Produces: `web_ui::SHARED_JS: &str` defining `esc`, `fmtSize`, `fmtDate`, and `apiGet`/`apiPost` (the latter attaches the csrf header from the `<meta>`).
- Produces: `web_ui::overview_page(csrf: &str) -> String` (calls `shell("overview", …)`).

**Reference:** `StitchExport/stitch_cleanupstorages_ui/overview/code.html` (hero stat, duplicates card, reclaimable-by-drive bars, recent activity list). Match the layout; drop the fake avatar/profile block and all fake numbers.

- [ ] **Step 1: Create `src/web_ui.rs` with `STYLE`, `SHARED_JS`, `icon()`, `shell()`.**

Write the module. `shell()` must produce exactly one sidebar with six nav entries and inject the active class. Skeleton (fill `STYLE` with the full design-system CSS per DESIGN.md; keep it self-contained — no `http`/`https` anywhere):

```rust
//! Shared, self-contained UI shell: one design-system stylesheet, an inline SVG icon set, a
//! sidebar/toolbar renderer, and shared client helpers. Every page in `web.rs` renders through
//! `shell()` so the six screens stay visually identical and no markup is duplicated.

pub const STYLE: &str = r##"
:root{color-scheme:light dark;--bg:#f5f5f7;--panel:#ffffffcc;--content:#ffffff;--fg:#1d1d1f;
 --mut:#6e6e73;--line:#1d1d1f1a;--accent:#0071e3;--amber:#b45309;--amber-bg:#f59e0b26;
 --red:#c0392b;--red-bg:#e74c3c22;--green:#1a7f37;--green-bg:#2ecc7122;--gray:#6e6e73;}
@media (prefers-color-scheme:dark){:root{--bg:#000;--panel:#1d1d1fcc;--content:#1d1d1f;
 --fg:#f5f5f7;--mut:#98989d;--line:#ffffff1a;--accent:#0a84ff;}}
*{box-sizing:border-box;}
body{margin:0;font:14px/1.45 -apple-system,"Segoe UI",Roboto,sans-serif;background:var(--bg);color:var(--fg);}
.mono{font-family:ui-monospace,"Cascadia Code","SF Mono",Consolas,monospace;font-variant-numeric:tabular-nums;}
aside.side{position:fixed;left:0;top:0;bottom:0;width:260px;background:var(--panel);
 backdrop-filter:blur(20px) saturate(180%);border-right:1px solid var(--line);padding:20px 12px;}
aside.side h1{font-size:18px;margin:0 8px 20px;letter-spacing:-.3px;}
nav a{display:flex;gap:10px;align-items:center;padding:7px 10px;margin:2px 0;border-radius:6px;
 color:var(--mut);text-decoration:none;font-weight:500;}
nav a.active{background:color-mix(in srgb,var(--accent) 12%,transparent);color:var(--accent);}
nav a:hover:not(.active){background:var(--line);}
nav a svg{width:16px;height:16px;flex:none;}
header.top{position:fixed;top:0;left:260px;right:0;height:52px;display:flex;align-items:center;
 padding:0 20px;background:var(--panel);backdrop-filter:blur(20px);border-bottom:1px solid var(--line);}
main{margin-left:260px;padding:76px 24px 40px;max-width:1100px;}
.card{background:var(--content);border:1px solid var(--line);border-radius:14px;padding:20px;margin:0 0 16px;}
.grid{display:grid;grid-template-columns:repeat(12,1fr);gap:16px;}
.btn{font:inherit;padding:8px 14px;border-radius:8px;border:1px solid var(--line);
 background:transparent;color:var(--fg);cursor:pointer;}
.btn-primary{background:var(--accent);border-color:var(--accent);color:#fff;}
.btn-danger{border-color:var(--red);color:var(--red);}
.pill{font-size:11px;padding:2px 8px;border-radius:999px;}
.pill.quarantined{color:var(--amber);background:var(--amber-bg);}
.pill.missing{color:var(--red);background:var(--red-bg);}
.pill.active{color:var(--green);background:var(--green-bg);}
.pill.purged{color:var(--gray);background:var(--line);}
.mut{color:var(--mut);} table{width:100%;border-collapse:collapse;}
th,td{text-align:left;padding:8px;border-bottom:1px solid var(--line);vertical-align:top;}
th{color:var(--mut);font-weight:600;font-size:12px;}
.progressbar{height:6px;background:var(--line);border-radius:999px;overflow:hidden;}
.progressbar>span{display:block;height:100%;background:var(--accent);}
.console-out{font-family:ui-monospace,Consolas,monospace;white-space:pre-wrap;background:var(--content);
 border:1px solid var(--line);border-radius:10px;padding:12px;min-height:300px;max-height:60vh;overflow:auto;}
.console-in{width:100%;font-family:ui-monospace,Consolas,monospace;padding:10px;border-radius:8px;
 border:1px solid var(--line);background:var(--content);color:var(--fg);}
"##;

pub const SHARED_JS: &str = r##"
const $=s=>document.querySelector(s);
const CSRF=(document.querySelector('meta[name="csrf"]')||{}).content||"";
function esc(s){return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));}
function fmtSize(n){if(n==null)return"—";const u=["B","KB","MB","GB","TB"];let i=0,x=Number(n);while(x>=1024&&i<u.length-1){x/=1024;i++;}return (i?x.toFixed(1):x)+" "+u[i];}
function fmtDate(t){return t?new Date(t*1000).toISOString().slice(0,10):"—";}
async function apiGet(u){const r=await fetch(u);if(!r.ok)throw new Error(await r.text());return r.json();}
async function apiPost(u,body){const r=await fetch(u,{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify(body||{})});if(!r.ok)throw new Error(await r.text());return r.json();}
"##;

/// Inline SVG for a nav glyph (stroke-based, currentColor). Unknown keys get a generic dot.
fn icon(name: &str) -> &'static str {
    match name {
        "overview" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/></svg>"#,
        "browse" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 7h5l2 2h11v9a2 2 0 0 1-2 2H3z"/></svg>"#,
        "duplicates" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>"#,
        "drives" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><circle cx="7" cy="7.5" r="1"/><circle cx="7" cy="16.5" r="1"/></svg>"#,
        "scan" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>"#,
        "console" => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/></svg>"#,
        _ => r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/></svg>"#,
    }
}

struct NavItem { key: &'static str, href: &'static str, label: &'static str }
const NAV: &[NavItem] = &[
    NavItem{key:"overview",href:"/",label:"Overview"},
    NavItem{key:"browse",href:"/browse",label:"Browse"},
    NavItem{key:"duplicates",href:"/review",label:"Duplicates"},
    NavItem{key:"drives",href:"/drives",label:"Drives"},
    NavItem{key:"scan",href:"/scan",label:"Scan"},
    NavItem{key:"console",href:"/console",label:"Console"},
];

/// Render a full self-contained page. `active` is a NAV key.
pub fn shell(active: &str, csrf: &str, title: &str, main_html: &str, page_script: &str) -> String {
    let nav = NAV.iter().map(|n| {
        let cls = if n.key == active { "active" } else { "" };
        format!(r#"<a class="{cls}" href="{}">{}<span>{}</span></a>"#, n.href, icon(n.key), n.label)
    }).collect::<String>();
    format!(r##"<!doctype html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf" content="{csrf}"><title>CleanUpStorages — {title}</title>
<style>{style}</style></head><body>
<aside class="side"><h1>CleanUpStorages</h1><nav>{nav}</nav></aside>
<header class="top"><strong>{title}</strong></header>
<main>{main_html}</main>
<script>{shared}</script><script>{page_script}</script>
</body></html>"##,
        csrf = csrf, title = title, style = STYLE, nav = nav, main_html = main_html,
        shared = SHARED_JS, page_script = page_script)
}
```

- [ ] **Step 2: Add the Overview page** (append to `src/web_ui.rs`). Its script fetches `/api/stats`, `/api/drives`, `/api/activity` and renders — all through `esc`/`textContent`.

```rust
pub fn overview_page(csrf: &str) -> String {
    let main = r##"
<section class="card"><div class="mut" style="font-size:11px;text-transform:uppercase;letter-spacing:.08em">System health</div>
  <h2 id="hero" style="margin:6px 0 2px;font-size:26px">…</h2>
  <div class="mut" id="hero-sub"></div></section>
<div class="grid">
  <div class="card" style="grid-column:span 5"><h3 style="margin-top:0">Duplicate groups</h3>
    <div style="font-size:22px" id="dupe-count">…</div>
    <div class="mut" id="dupe-reclaim"></div>
    <a class="btn btn-primary" href="/review" style="display:inline-block;margin-top:12px;text-decoration:none">Review duplicates</a></div>
  <div class="card" style="grid-column:span 7"><h3 style="margin-top:0">Reclaimable space</h3>
    <div id="reclaim-bars"></div></div>
  <div class="card" style="grid-column:span 12"><h3 style="margin-top:0">Recent activity</h3>
    <div id="activity" class="mut">Loading…</div></div>
</div>"##;
    let script = r##"
async function init(){
  const st=await apiGet("/api/stats");
  const totalFiles=st.volumes.reduce((a,v)=>a+v.active_files,0);
  $("#hero").textContent=totalFiles.toLocaleString()+" files catalogued";
  $("#hero-sub").textContent="across "+st.volumes.length+" drive"+(st.volumes.length===1?"":"s")+" · catalog stored on this computer";
  $("#dupe-count").textContent=st.duplicate_groups+" group"+(st.duplicate_groups===1?"":"s");
  const drives=await apiGet("/api/drives");
  const totalReclaim=drives.reduce((a,d)=>a+(d.reclaimable_bytes||0),0);
  $("#dupe-reclaim").textContent="~"+fmtSize(totalReclaim)+" reclaimable";
  const max=Math.max(1,...drives.map(d=>d.reclaimable_bytes||0));
  $("#reclaim-bars").innerHTML=drives.map(d=>`<div style="margin:10px 0">
     <div style="display:flex;justify-content:space-between"><span>${esc(d.label)}</span><span class="mono">${fmtSize(d.reclaimable_bytes)}</span></div>
     <div class="progressbar"><span style="width:${Math.round(100*(d.reclaimable_bytes||0)/max)}%"></span></div></div>`).join("")||'<span class="mut">Nothing to reclaim.</span>';
  const acts=await apiGet("/api/activity");
  $("#activity").innerHTML=acts.length?acts.map(a=>`<div style="padding:6px 0;border-bottom:1px solid var(--line)">
     <span>${esc(a.summary)}</span> <span class="mut mono" style="float:right">${fmtDate(a.occurred_at)}</span></div>`).join(""):"No activity yet.";
}
init().catch(e=>{$("#activity").textContent="Error: "+e;});"##;
    shell("overview", csrf, "Overview", main, script)
}
```

- [ ] **Step 3: Wire routes in `src/web.rs`.** Add `pub mod web_ui;` to `src/lib.rs`. In `build_router_with`, change `/` to Overview and add `/browse` for the existing Browse page:

```rust
        .route("/", get(overview))
        .route("/browse", get(browse))
```
Add handlers (near `index`):
```rust
async fn overview(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::overview_page(&state.csrf_token))
}
async fn browse(State(_s): State<AppState>) -> Html<&'static str> { Html(INDEX_HTML) }
```
Keep the old `index` handler name only if still referenced; otherwise remove it. `INDEX_HTML` stays for now (restyled in Task 10). Update `INDEX_HTML`'s header links `href="/review"`/`href="/scan"` to also include `/` (they already point correctly).

- [ ] **Step 4: Update the moved-page test + add an Overview test** (in `src/web.rs` tests). Change `index_page_has_search_ui_and_calls_api` to fetch `/browse` instead of `/`. Add:

```rust
    #[tokio::test]
    async fn root_is_overview_and_self_contained() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("/api/activity"), "overview loads activity");
        assert!(body.contains("/api/drives"), "overview loads drives");
        assert!(body.contains("Recent activity"));
        assert!(!body.contains("http://") && !body.contains("https://"), "self-contained");
    }
```

- [ ] **Step 5: Build + run web tests**

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS (Overview test green; the relocated browse test green at `/browse`).

- [ ] **Step 6: Commit**

```bash
git add src/web_ui.rs src/lib.rs src/web.rs
git commit -m "feat(web): shared UI shell + Overview dashboard; move Browse to /browse"
```

---

### Task 8: Drives page

**Files:**
- Modify: `src/web_ui.rs` (`drives_page`)
- Modify: `src/web.rs` (route `/drives`)

**Interfaces:**
- Consumes: `/api/drives` (Task 4), `/api/forget-drive` (Task 5), `/api/purge-all` (Task 6), `/api/scan` (existing, for the "Rescan"/"Repair" action).
- Produces: `web_ui::drives_page(csrf: &str) -> String`.

**Reference:** `StitchExport/stitch_cleanupstorages_ui/drives_management/code.html`. Reinterpret its buttons: "Eject" → **Forget** (confirm dialog, calls `/api/forget-drive`); "Repair"/"Update Catalog" → **Rescan** (calls `/api/scan` with the drive's mount path); "Purge All" → **Purge all** (`/api/purge-all`). Drop fake capacity/toast fiction; show real values, and hide the capacity bar when `total_bytes` is null.

- [ ] **Step 1: Add `drives_page`** (in `src/web_ui.rs`). Confirmation before Forget is mandatory (`window.confirm`), and the copy must say files on disk are untouched.

```rust
pub fn drives_page(csrf: &str) -> String {
    let main = r##"
<div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px">
  <div class="mut">Manage catalogued drives. Nothing here deletes files on your drives.</div>
  <button class="btn btn-danger" id="purge-all">Purge all quarantines</button></div>
<div id="drives" class="mut">Loading drives…</div>
<div class="mut" id="msg" style="margin-top:12px;min-height:1.4em"></div>"##;
    let script = r##"
function bar(d){ if(d.total_bytes==null) return "";
  const used=d.total_bytes-d.free_bytes, pct=Math.round(100*used/d.total_bytes);
  return `<div style="margin:10px 0"><div style="display:flex;justify-content:space-between">
    <span class="mono">${fmtSize(used)} of ${fmtSize(d.total_bytes)} used</span><span class="mut">${pct}% full</span></div>
    <div class="progressbar"><span style="width:${pct}%"></span></div></div>`; }
async function load(){
  const drives=await apiGet("/api/drives");
  $("#drives").innerHTML = drives.length? drives.map(d=>`<div class="card" data-vid="${esc(d.volume_id)}" data-path="${esc(d.mount_path||'')}">
    <div style="display:flex;justify-content:space-between;align-items:start">
      <div><h3 style="margin:0">${esc(d.label)}</h3>
        <div class="mut" style="font-size:12px">${d.connected?'<span class="pill active">connected</span>':'<span class="pill purged">offline</span>'}
          · ${d.active_files.toLocaleString()} files · last scan ${fmtDate(d.last_seen_at)}
          ${d.has_errors?' · <span class="pill missing">had scan errors</span>':''}</div></div>
      <div class="mut mono">${fmtSize(d.reclaimable_bytes)} reclaimable</div></div>
    ${bar(d)}
    <div style="display:flex;gap:8px;margin-top:12px">
      <button class="btn rescan" ${d.connected?'':'disabled'}>${d.has_errors?'Repair (rescan)':'Rescan'}</button>
      <button class="btn btn-danger forget">Forget…</button></div></div>`).join("")
    : '<div class="mut">No drives catalogued yet. Scan one from the Scan page.</div>';
  for(const c of document.querySelectorAll("[data-vid]")){
    c.querySelector(".forget").onclick=async()=>{
      const vid=c.dataset.vid, label=c.querySelector("h3").textContent;
      if(!window.confirm(`Forget "${label}"? This removes it from the catalog only — files on the drive are NOT deleted. You can rescan to re-add it.`))return;
      try{ const r=await apiPost("/api/forget-drive",{volume_id:vid}); $("#msg").textContent=`Forgot ${label} (${r.removed_files} entries removed).`; load(); }
      catch(e){ $("#msg").textContent="Error: "+e; }
    };
    c.querySelector(".rescan").onclick=async()=>{
      const path=c.dataset.path; if(!path){ $("#msg").textContent="Drive not connected."; return; }
      try{ await apiPost("/api/scan",{path,force:false}); $("#msg").textContent="Rescan queued for "+path+". Watch progress on the Scan page."; }
      catch(e){ $("#msg").textContent="Error: "+e; }
    };
  }
}
$("#purge-all").onclick=async()=>{
  if(!window.confirm("Permanently delete every drive's _ToDelete quarantine? This is the only real delete and cannot be undone."))return;
  try{ const r=await apiPost("/api/purge-all",{});
    let m=`Purged ${r.purged_volumes} volume(s), reclaimed ${fmtSize(r.bytes_reclaimed)}.`;
    if(r.skipped_unmounted.length)m+=" Skipped (offline): "+r.skipped_unmounted.join(", ")+".";
    if(r.errors.length)m+=" Errors: "+r.errors.join("; ");
    $("#msg").textContent=m; load(); }
  catch(e){ $("#msg").textContent="Error: "+e; }
};
load().catch(e=>{$("#drives").textContent="Error: "+e;});"##;
    shell("drives", csrf, "Drives", main, script)
}
```

- [ ] **Step 2: Add the route + handler** (in `src/web.rs`)

```rust
        .route("/drives", get(drives_page_h))
```
```rust
async fn drives_page_h(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::drives_page(&state.csrf_token))
}
```

- [ ] **Step 3: Write the page test** (in `src/web.rs` tests)

```rust
    #[tokio::test]
    async fn drives_page_is_wired_and_self_contained() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/drives").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"csrf\""));
        assert!(body.contains("/api/drives"));
        assert!(body.contains("/api/forget-drive"));
        assert!(body.contains("/api/purge-all"));
        assert!(!body.contains("http://") && !body.contains("https://"));
    }
```

- [ ] **Step 4: Run**

Run: `cargo test -p cleanupstorages drives_page_is_wired`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): Drives page (capacity, rescan, forget, purge-all)"
```

---

### Task 9: Console page (client-side REPL)

**Files:**
- Modify: `src/web_ui.rs` (`console_page`)
- Modify: `src/web.rs` (route `/console`)

**Interfaces:**
- Consumes: existing endpoints only (`/api/search`, `/api/stats`, `/api/duplicates`, `/api/quarantine`, `/api/repack`, `/api/scan`, `/api/purge-all`, `/api/forget-drive`, `/api/drives`).
- Produces: `web_ui::console_page(csrf: &str) -> String`.

The page parses a typed line into one existing API call; unrecognized commands print a usage hint and make no request. Output is appended as `esc`-escaped scrollback.

- [ ] **Step 1: Add `console_page`** (in `src/web_ui.rs`)

```rust
pub fn console_page(csrf: &str) -> String {
    let main = r##"
<div class="mut" style="margin-bottom:8px">Runs this app's own commands only — the same safe actions as the buttons. Type <span class="mono">help</span>.</div>
<div id="out" class="console-out" aria-live="polite"></div>
<input id="cmd" class="console-in" style="margin-top:10px" placeholder="e.g. status  ·  search thesis  ·  scan D:\ --force" autofocus>"##;
    let script = r##"
const out=$("#out");
function print(s,cls){ const d=document.createElement("div"); if(cls)d.className=cls; d.textContent=s; out.appendChild(d); out.scrollTop=out.scrollHeight; }
function printJSON(o){ print(JSON.stringify(o,null,2)); }
const HELP=`Commands:
  status                         catalog summary
  duplicates                     list duplicate groups
  search <query> [--category c] [--status s]
  scan <path> [--force]          queue a scan
  quarantine <id> [id ...]       quarantine file ids
  repack <id>                    remove an in-archive duplicate
  forget <volumeId>              remove a drive from the catalog
  purge --all                    purge every mounted quarantine
  drives                         list drives
  help, clear`;
// naive shell-ish tokenizer: splits on whitespace, honours "double quotes".
function tokenize(line){ const m=line.match(/"[^"]*"|\S+/g)||[]; return m.map(t=>t.replace(/^"|"$/g,"")); }
function flag(toks,name){ const i=toks.indexOf("--"+name); if(i<0)return null; const v=toks[i+1]; toks.splice(i, v&&!v.startsWith("--")?2:1); return v||true; }
async function exec(line){
  print("$ "+line);
  const toks=tokenize(line); const cmd=(toks.shift()||"").toLowerCase();
  try{
    if(cmd==="help"||cmd===""){ print(HELP); return; }
    if(cmd==="clear"){ out.innerHTML=""; return; }
    if(cmd==="status"){ printJSON(await apiGet("/api/stats")); return; }
    if(cmd==="drives"){ printJSON(await apiGet("/api/drives")); return; }
    if(cmd==="duplicates"){ printJSON(await apiGet("/api/duplicates")); return; }
    if(cmd==="search"){ const cat=flag(toks,"category"), st=flag(toks,"status");
      const p=new URLSearchParams(); if(toks.length)p.set("q",toks.join(" ")); if(cat)p.set("category",cat); if(st)p.set("status",st);
      printJSON(await apiGet("/api/search?"+p.toString())); return; }
    if(cmd==="scan"){ const force=!!flag(toks,"force"); const path=toks.join(" ");
      if(!path){ print("usage: scan <path> [--force]","mut"); return; }
      printJSON(await apiPost("/api/scan",{path,force})); return; }
    if(cmd==="quarantine"){ const ids=toks.map(Number).filter(n=>!isNaN(n));
      if(!ids.length){ print("usage: quarantine <id> [id ...]","mut"); return; }
      printJSON(await apiPost("/api/quarantine",{quarantine_ids:ids})); return; }
    if(cmd==="repack"){ const id=Number(toks[0]); if(isNaN(id)){ print("usage: repack <id>","mut"); return; }
      printJSON(await apiPost("/api/repack",{entry_id:id})); return; }
    if(cmd==="forget"){ if(!toks[0]){ print("usage: forget <volumeId>","mut"); return; }
      printJSON(await apiPost("/api/forget-drive",{volume_id:toks[0]})); return; }
    if(cmd==="purge"){ if(flag(toks,"all")){ printJSON(await apiPost("/api/purge-all",{})); return; }
      print("only 'purge --all' is supported from the console; use the Drives page for a single drive.","mut"); return; }
    print("unknown command: "+cmd+" (try 'help')","mut");
  }catch(e){ print("error: "+e,"mut"); }
}
$("#cmd").addEventListener("keydown",e=>{ if(e.key==="Enter"){ const v=e.target.value; e.target.value=""; if(v.trim())exec(v.trim()); }});
print(HELP);"##;
    shell("console", csrf, "Console", main, script)
}
```

- [ ] **Step 2: Route + handler** (in `src/web.rs`)

```rust
        .route("/console", get(console_page_h))
```
```rust
async fn console_page_h(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::console_page(&state.csrf_token))
}
```

- [ ] **Step 3: Page test** (in `src/web.rs` tests)

```rust
    #[tokio::test]
    async fn console_page_is_self_contained_and_maps_commands() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/console").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"csrf\""));
        assert!(body.contains("/api/stats") && body.contains("/api/scan") && body.contains("/api/purge-all"));
        assert!(!body.contains("http://") && !body.contains("https://"), "self-contained");
    }
```

- [ ] **Step 4: Run**

Run: `cargo test -p cleanupstorages console_page_is_self_contained`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): Console page — client-side REPL over the existing API"
```

---

### Task 10: Restyle Browse onto the shell

**Files:**
- Modify: `src/web_ui.rs` (`browse_page`)
- Modify: `src/web.rs` (`/browse` renders `web_ui::browse_page`; delete `INDEX_HTML`)

**Interfaces:**
- Produces: `web_ui::browse_page(csrf: &str) -> String`, preserving all current behavior: live search (`/api/search` debounced), drive filter from `/api/volumes`, category + status filters, `esc`-safe rows with status pills, location shown with archive `›` notation.

**Reference:** `StitchExport/stitch_cleanupstorages_ui/browse_search/code.html` (search field, three segmented filters, results table with Location/Drive/Type/Size/Status). Drop the fake avatar/details-panel and the fixed sample rows.

- [ ] **Step 1: Add `browse_page`** to `src/web_ui.rs` reproducing the existing `INDEX_HTML` JS behavior (search/filters/render) inside the shell. The search input MUST keep `id="q"` and the results container `id="results"`, and the page MUST fetch `/api/search`, `/api/volumes`, `/api/stats` (the existing tests assert these). Use `apiGet` from `SHARED_JS`. Render each hit row with a status `.pill` when not active; show `h.location` (already includes `›` for archives).

```rust
pub fn browse_page(csrf: &str) -> String {
    let main = r##"
<div style="display:flex;gap:10px;flex-wrap:wrap;align-items:center;margin-bottom:12px">
  <input id="q" type="search" placeholder="Search filename or path…" style="flex:1;min-width:220px;padding:8px 12px;border-radius:999px;border:1px solid var(--line);background:var(--content);color:var(--fg)" autofocus>
  <select id="volume" class="btn"><option value="">All drives</option></select>
  <select id="category" class="btn"><option value="">All types</option>
    <option value="photo">Photo</option><option value="video">Video</option>
    <option value="document">Document</option><option value="academic">Academic</option><option value="other">Other</option></select>
  <select id="status" class="btn"><option value="">Any status</option>
    <option value="active">Active</option><option value="missing">Missing</option>
    <option value="quarantined">Quarantined</option><option value="purged">Purged</option></select>
</div>
<div class="mut" id="count" style="margin-bottom:8px"></div>
<div class="card" style="padding:0"><table><thead><tr>
  <th>Location</th><th>Drive</th><th>Type</th><th style="text-align:right">Size</th><th>Status</th>
</tr></thead><tbody id="results"></tbody></table></div>"##;
    let script = r##"
let timer=null;
async function run(){ try{
  const p=new URLSearchParams(); const q=$("#q").value.trim(); if(q)p.set("q",q);
  for(const k of ["volume","category","status"]){ const v=$("#"+k).value; if(v)p.set(k,v); }
  const hits=await apiGet("/api/search?"+p.toString());
  $("#count").textContent=hits.length+" result"+(hits.length===1?"":"s")+(hits.length>=500?" (showing first 500)":"");
  $("#results").innerHTML=hits.map(h=>{
    const pill=h.status==="active"?"":`<span class="pill ${esc(h.status)}">${esc(h.status)}</span>`;
    return `<tr><td class="mono">${esc(h.location)}</td><td>${esc(h.volume_id)}</td><td>${esc(h.category)}</td>
      <td class="mono" style="text-align:right">${fmtSize(h.size_bytes)}</td><td>${pill}</td></tr>`; }).join("");
}catch(e){ $("#count").textContent="Search error: "+e; } }
function debounced(){ clearTimeout(timer); timer=setTimeout(run,180); }
async function init(){
  const vs=await apiGet("/api/volumes"); const sel=$("#volume");
  for(const v of vs){ const o=document.createElement("option"); o.value=v.volume_id; o.textContent=v.label; sel.appendChild(o); }
  $("#q").addEventListener("input",debounced);
  for(const k of ["volume","category","status"]) $("#"+k).addEventListener("change",run);
  run();
}
init();"##;
    shell("browse", csrf, "Browse", main, script)
}
```

- [ ] **Step 2: Point `/browse` at it and remove `INDEX_HTML`.** In `src/web.rs`, change the `browse` handler to `Html(crate::web_ui::browse_page(&state.csrf_token))` (return type `Html<String>`), and delete the now-unused `INDEX_HTML` const and the old `index` handler if still present.

- [ ] **Step 3: Update the browse test** — `index_page_has_search_ui_and_calls_api` (now hitting `/browse`) still asserts `id="q"`, `id="results"`, `/api/search`, and self-containment; all preserved. Confirm it passes unchanged aside from the URI.

- [ ] **Step 4: Run**

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "refactor(web): restyle Browse onto the shared shell"
```

---

### Task 11: Restyle Review (Duplicates) onto the shell

**Files:**
- Modify: `src/web_ui.rs` (`review_page`)
- Modify: `src/web.rs` (`review` handler renders `web_ui::review_page`; delete `REVIEW_HTML`)

**Interfaces:**
- Produces: `web_ui::review_page(csrf: &str) -> String`, preserving ALL current review behavior: fetch `/api/duplicates`, Tinder-style group cards, suggested-keep highlighted + clickable to change, thumbnails via `/api/preview/:id` for mounted photos, "Remove from archive" button (only on non-keep archived members) posting `/api/repack`, "Keep selected, quarantine the rest" posting `/api/quarantine`, skip, and the "All duplicates reviewed 🎉" end state.

**Reference:** `StitchExport/stitch_cleanupstorages_ui/review_duplicates/code.html` (comparison cards, blue keep ring, offline card state, bottom action bar, reassurance line). This is the signature screen — match it closely.

- [ ] **Step 1: Add `review_page`** to `src/web_ui.rs`. Port the existing `REVIEW_HTML` `<script>` verbatim in behavior (it already uses `esc`/`fmtSize`/`fmtDate` and the CSRF meta — but now those come from `SHARED_JS`, so DELETE the local `esc/fmtSize/fmtDate/CSRF` redefinitions to avoid `const` redeclaration collisions). Keep the class names the CSS expects (`.card.keep`, `.thumb`, `.noimg`). Card markup for each member mirrors the current `card(m)` function; the action bar has the primary/secondary buttons and the reassurance line about `_ToDelete`.

The page MUST still contain the strings `/api/duplicates` and `/api/quarantine` and `name="csrf"` (asserted by the existing `review_page_*` test).

Add the review-specific CSS to `STYLE` (in Task 7) if not already present: `.cards{display:flex;flex-wrap:wrap;gap:12px}` `.card.keep{border-color:var(--accent);box-shadow:0 0 0 2px var(--accent) inset}` `.thumb{width:100%;height:150px;object-fit:contain;border-radius:8px;background:#0003}` `.noimg{...}` — or scope them inside the page's own `main` via a small `<style>` in `main_html`. Prefer adding to shared `STYLE`.

- [ ] **Step 2: Render it** — in `src/web.rs`, `review` becomes `Html(crate::web_ui::review_page(&state.csrf_token))`; delete `REVIEW_HTML`.

- [ ] **Step 3: Run the existing review test** (`review_page_*`), which asserts csrf meta, `/api/duplicates`, `/api/quarantine`, self-contained.

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS.

- [ ] **Step 4: Manual sanity (optional but recommended):** build and open `/review` against the sandbox to confirm thumbnails + keep-highlight still work.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "refactor(web): restyle Review duplicates onto the shared shell"
```

---

### Task 12: Restyle Scan onto the shell

**Files:**
- Modify: `src/web_ui.rs` (`scan_page`)
- Modify: `src/web.rs` (`scan_page` handler renders `web_ui::scan_page`; delete `SCAN_HTML`)

**Interfaces:**
- Produces: `web_ui::scan_page(csrf: &str) -> String`, preserving behavior: detected drives from `/api/detected-drives` (click to fill path), `/api/pick-folder` Browse button, force checkbox, `/api/scan` submit, live status polling `/api/scan/status` with ticking counts + recent scans list.

**Reference:** `StitchExport/stitch_cleanupstorages_ui/scan_a_drive/code.html` (detected-drive cards, path field + Browse, force toggle, live status panel with counts, recent scans). Drop the fake throughput/percentages not in the real status payload.

- [ ] **Step 1: Add `scan_page`** to `src/web_ui.rs`, porting the existing `SCAN_HTML` `<script>` behavior (again removing the local `esc`/`CSRF` redefinitions — use `SHARED_JS`). Must still contain `/api/scan`, `/api/detected-drives`, `/api/pick-folder`, and `name="csrf"` (asserted by `scan_page_is_self_contained_and_wired`). Keep the running/queued/recent DOM ids.

- [ ] **Step 2: Render it** — `scan_page` handler becomes `Html(crate::web_ui::scan_page(&state.csrf_token))`; delete `SCAN_HTML`. (Note: rename the Rust handler if it now collides with `web_ui::scan_page`; e.g. keep the handler `async fn scan_page(...)` calling `crate::web_ui::scan_page(...)` — the paths disambiguate, but if it reads confusingly, rename the handler to `scan_page_h` and update the route.)

- [ ] **Step 3: Run**

Run: `cargo test -p cleanupstorages --lib web`
Expected: PASS (`scan_page_is_self_contained_and_wired` green).

- [ ] **Step 4: Full test sweep**

Run: `cargo test`
Expected: PASS across unit + integration tests.

- [ ] **Step 5: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "refactor(web): restyle Scan onto the shared shell"
```

---

### Task 13: Docs — testing guide + build/run notes

**Files:**
- Modify: `docs/TESTING-GUIDE.md`
- Modify: `CLAUDE.md` (Project status → real build/run commands now that the app is built)

**Interfaces:** none (docs only).

- [ ] **Step 1: Update `docs/TESTING-GUIDE.md`.** In §3 (the web UI), add the new pages: Overview (dashboard landing at `/`), Drives (capacity, Rescan, Forget with its confirm + "files untouched" copy, Purge all), and Console (type `status`, `search report`, `help`). Add a §4 note that `cus purge --all` and `cus forget <path>` exist as CLI equivalents. Append the new features to the checklist table:

```markdown
| Overview dashboard (stats + activity feed) | §3 | |
| Drives page (capacity, rescan, forget, purge-all) | §3 | |
| Console (client-side command REPL) | §3 | |
| Forget a drive (catalog-only; files kept) | §3, §4 | |
| Purge all quarantines at once | §3, §4 | |
```

- [ ] **Step 2: Update `CLAUDE.md`.** Replace the "Design phase complete; no application code exists yet" note in **Project status** with real commands:

```markdown
## Build / test / run

- Build: `cargo build --release` → `target/release/cleanupstorages(.exe)`
- Test: `cargo test`
- Run the web UI: `cleanupstorages browse` (serves 127.0.0.1, opens a browser)
- CLI: `scan`, `search`, `status`, `duplicates`, `quarantine`, `purge` (`--all`), `repack`, `forget`, `browse`
- Safe end-to-end walkthrough: `docs/TESTING-GUIDE.md` (+ `scripts/make-test-sandbox.ps1`)
```

- [ ] **Step 3: Commit**

```bash
git add docs/TESTING-GUIDE.md CLAUDE.md
git commit -m "docs: document new UI pages and forget/purge-all commands"
```

---

## Self-review notes

- **Spec coverage:** Overview (T7), Browse restyle (T10), Duplicates restyle (T11), Drives (T4+T5+T6+T8), Scan restyle (T12), Console (T9); activity feed (T1+T2), disk capacity (T4), forget-drive (T5), purge-all (T6), reclaimable-per-drive (T3), "Repair"=rescan (T8), CLI parity for forget/purge-all (T5/T6), self-contained + light/dark + shared shell + system fonts + inline SVG (T7), docs (T13). All spec items map to a task.
- **Type consistency:** `recent_actions` → `(String,String,i64)` used by `activity_summary`; `reclaimable_bytes_by_volume` → `HashMap<String,i64>` used by `api_drives`/overview; `disk_capacity` → `Option<(u64,u64)>` used by `DriveDto.total_bytes/free_bytes`; `forget_volume(&str,i64)->usize` used by CLI + web; `purge_all(cat,&HashMap,i64)->PurgeAllOutcome` used by CLI + web. `cmd_purge` signature change (now `Option<&Path>, bool`) — only caller is `main.rs`, updated in T6.
- **Ordering:** backend (T1–T6) precedes the pages that consume it (T7–T9); restyles (T10–T12) are independent and last; docs (T13) last.
- **Known caution:** T7/T10/T11/T12 must delete the per-page local `const esc=…`/`CSRF=…` when moving scripts under `SHARED_JS`, or the browser will throw "Identifier already declared". Each restyle task calls this out.

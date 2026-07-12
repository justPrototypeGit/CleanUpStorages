# Drives: fix offline detection + rename/description — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Folder-based drives no longer read as "offline," and each drive can be given a custom name + description shown everywhere.

**Architecture:** Remember each volume's last-scanned path (new column); mount resolution merges the disk-root marker scan with remembered paths whose marker still matches (a pure, testable function). Two more nullable columns hold a user name/description, edited via a new CSRF endpoint + CLI verb and shown on Drives/Browse/Overview.

**Tech Stack:** Rust, rusqlite, axum. Additive schema via the existing `ensure_column`. Existing suite is the guard.

## Global Constraints

- **Reliability:** additive schema only; the change affects connectivity *detection* only, never a destructive path. A remembered path is trusted ONLY if `volume::read_volume_id(path) == Some(volume_id)` (marker still matches), so a moved/renamed folder correctly reads offline and no volume is mis-attributed.
- **Self-contained pages** (no `http(s)://`), **XSS-safe** (`esc()`/`textContent`), **reads use `Catalog::open_readonly`, writes `Catalog::open`**, **CSRF-first** on the new mutating endpoint (`check_csrf(&headers,&state)?` as the first line) — all per the established patterns.
- **Conventional Commits**, scopes `catalog`/`scanner`/`web`/`cli`; body ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- **Windows/PowerShell dev env:** `cargo build`, `cargo test`. Run focused tests while iterating; full `cargo test -p cleanupstorages` once before each commit.

## File structure

- `src/catalog/schema.rs` — MODIFY (T1): three `ensure_column` calls.
- `src/catalog/store.rs` — MODIFY (T1): `set_volume_path`, `volume_paths`, `set_volume_meta`, `volume_meta`, `effective_labels`.
- `src/mounts.rs` — MODIFY (T2): `MountResolver::Live { catalog_path }`, `resolve_live`, `remembered_paths`.
- `src/scanner.rs` — MODIFY (T2): record the scanned path in `run_scan`.
- `src/web.rs` — MODIFY (T2 construction, T3 DTOs/handlers/routes/tests).
- `src/commands.rs`, `src/main.rs` — MODIFY (T3): `cus rename`.
- `src/web_ui.rs` — MODIFY (T4): Drives edit UI; effective names on Browse/Overview.

---

### Task 1: schema columns + volume path/meta store methods

**Files:** Modify `src/catalog/schema.rs`, `src/catalog/store.rs`

**Interfaces produced:**
- `Catalog::set_volume_path(volume_id, path: &str, now) -> anyhow::Result<()>`
- `Catalog::volume_paths() -> anyhow::Result<Vec<(String, String)>>` (volume_id, last_scanned_path; only non-null)
- `Catalog::set_volume_meta(volume_id, display_name: Option<&str>, description: Option<&str>, now) -> anyhow::Result<()>` (empty/whitespace → NULL; logs a `rename` action)
- `Catalog::volume_meta(volume_id) -> anyhow::Result<(Option<String>, Option<String>)>`
- `Catalog::effective_labels() -> anyhow::Result<HashMap<String,String>>` (volume_id → display_name-if-nonempty-else-label)

- [ ] **Step 1: Add the columns.** In `src/catalog/schema.rs`, right after the existing `ensure_column(conn, "files", "original_path", "TEXT")?;` (line ~75):

```rust
    ensure_column(conn, "volumes", "last_scanned_path", "TEXT")?;
    ensure_column(conn, "volumes", "display_name", "TEXT")?;
    ensure_column(conn, "volumes", "description", "TEXT")?;
```

- [ ] **Step 2: Write the failing store tests** (in `src/catalog/store.rs` tests; use the module's real `open_tmp()` + a volume via `upsert_volume`):

```rust
    #[test]
    fn volume_path_and_meta_round_trip() {
        let (_t, cat) = open_tmp();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "v".into(), label: "Detected".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        // path
        cat.set_volume_path("v", "/some/folder", 5).unwrap();
        assert_eq!(cat.volume_paths().unwrap(), vec![("v".to_string(), "/some/folder".to_string())]);
        // meta: set name + description
        cat.set_volume_meta("v", Some("My Photos"), Some("holiday pics"), 6).unwrap();
        assert_eq!(cat.volume_meta("v").unwrap(), (Some("My Photos".to_string()), Some("holiday pics".to_string())));
        assert_eq!(cat.effective_labels().unwrap().get("v").cloned(), Some("My Photos".to_string()));
        // clearing the name (empty) falls back to the detected label
        cat.set_volume_meta("v", Some("  "), None, 7).unwrap();
        assert_eq!(cat.volume_meta("v").unwrap().0, None);
        assert_eq!(cat.effective_labels().unwrap().get("v").cloned(), Some("Detected".to_string()));
    }
```

- [ ] **Step 3: Run — expect FAIL** (`cargo test -p cleanupstorages volume_path_and_meta_round_trip`) → methods missing.

- [ ] **Step 4: Implement the five methods** (in `src/catalog/store.rs`, near `volume_stats`/`log_action`):

```rust
    /// Record the absolute path a volume was last scanned at (so a folder-drive can be recognized
    /// as connected later even though it isn't a disk mount root).
    pub fn set_volume_path(&self, volume_id: &str, path: &str, now: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE volumes SET last_scanned_path=?2, last_seen_at=?3 WHERE volume_id=?1",
            params![volume_id, path, now])?;
        Ok(())
    }

    /// (volume_id, last_scanned_path) for every volume that has a remembered path.
    pub fn volume_paths(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT volume_id, last_scanned_path FROM volumes WHERE last_scanned_path IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Set/clear a volume's user display name + description (empty/whitespace clears to NULL, which
    /// falls back to the detected label). Logs a `rename` audit action.
    pub fn set_volume_meta(&self, volume_id: &str, display_name: Option<&str>,
        description: Option<&str>, now: i64) -> anyhow::Result<()>
    {
        let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty());
        let dn = clean(display_name);
        let desc = clean(description);
        self.conn.execute(
            "UPDATE volumes SET display_name=?2, description=?3, last_seen_at=?4 WHERE volume_id=?1",
            params![volume_id, dn, desc, now])?;
        self.log_action("rename", &serde_json::json!({
            "volume_id": volume_id, "display_name": dn, "description": desc }).to_string(), now)?;
        Ok(())
    }

    /// A volume's (display_name, description); both None if unset or the volume is unknown.
    pub fn volume_meta(&self, volume_id: &str) -> anyhow::Result<(Option<String>, Option<String>)> {
        let row = self.conn.query_row(
            "SELECT display_name, description FROM volumes WHERE volume_id=?1",
            params![volume_id], |r| Ok((r.get(0)?, r.get(1)?)));
        match row {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((None, None)),
            Err(e) => Err(e.into()),
        }
    }

    /// volume_id → the name to show: the user display_name when set, else the detected label.
    pub fn effective_labels(&self) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT volume_id, COALESCE(NULLIF(display_name,''), label) FROM volumes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
    }
```

- [ ] **Step 5: Run — expect PASS.** Then full `cargo test -p cleanupstorages`.

- [ ] **Step 6: Commit**

```bash
git add src/catalog/schema.rs src/catalog/store.rs
git commit -m "feat(catalog): remember volume last-scanned path + user name/description"
```

---

### Task 2: record path on scan + merge remembered paths into mount resolution

**Files:** Modify `src/scanner.rs`, `src/mounts.rs`, `src/web.rs`

**Interfaces:**
- Consumes: `Catalog::set_volume_path`, `Catalog::volume_paths` (T1), `volume::read_volume_id`.
- Produces: `mounts::resolve_live(disk_roots: HashMap<String,PathBuf>, remembered: &[(String,PathBuf)]) -> HashMap<String,PathBuf>` (pure); `MountResolver::Live { catalog_path: PathBuf }` whose `snapshot()` merges the disk-root scan with the catalog's remembered paths, and `resolve()` derives from `snapshot()`.

- [ ] **Step 1: Record the path in `run_scan`** (`src/scanner.rs`). Right after the `cat.upsert_volume(&Volume { … })?;` call, add:

```rust
    // Remember where this volume was scanned so a folder-drive (not a disk root) can be recognized
    // as connected later. Best-effort: a bookkeeping failure must not fail the scan.
    let _ = cat.set_volume_path(&identity.volume_id, &mount_root.display().to_string(), now);
```

- [ ] **Step 2: Write the failing `resolve_live` test** (in `src/mounts.rs` tests):

```rust
    #[test]
    fn resolve_live_adds_matching_remembered_paths_only() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("DriveA");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(".cleanupstorages_id"), "vol-A").unwrap();
        let gone = tmp.path().join("Gone"); // never created / no marker
        let remembered = vec![
            ("vol-A".to_string(), folder.clone()),   // marker present + matches -> included
            ("vol-gone".to_string(), gone),          // no marker -> excluded
        ];
        let map = resolve_live(HashMap::new(), &remembered);
        assert_eq!(map.get("vol-A"), Some(&folder));
        assert_eq!(map.get("vol-gone"), None);
        // a disk-root entry for the same id is not overwritten by a remembered path
        let mut roots = HashMap::new();
        roots.insert("vol-A".to_string(), PathBuf::from("D:\\"));
        let map2 = resolve_live(roots, &remembered);
        assert_eq!(map2.get("vol-A"), Some(&PathBuf::from("D:\\"))); // disk root wins
    }
```

- [ ] **Step 3: Run — expect FAIL** (`resolve_live` missing).

- [ ] **Step 4: Change `MountResolver` + add `resolve_live`/`remembered_paths`** (`src/mounts.rs`). Replace the `enum` and `impl` with:

```rust
#[derive(Clone)]
pub enum MountResolver {
    /// Detect mounts live from the OS + the catalog's remembered volume paths (production).
    Live { catalog_path: PathBuf },
    /// A fixed volume_id → root map (tests).
    Fixed(HashMap<String, PathBuf>),
}

impl MountResolver {
    /// The current mount root for `volume_id`, if connected.
    pub fn resolve(&self, volume_id: &str) -> Option<PathBuf> {
        self.snapshot().get(volume_id).cloned()
    }

    /// Snapshot the current volume_id → mount map once.
    pub fn snapshot(&self) -> HashMap<String, PathBuf> {
        match self {
            MountResolver::Live { catalog_path } =>
                resolve_live(live_mounts(), &remembered_paths(catalog_path)),
            MountResolver::Fixed(m) => m.clone(),
        }
    }
}

/// Merge the disk-root marker scan with remembered volume paths: a remembered path is included only
/// when its marker still equals the volume_id (so a moved/renamed folder is correctly absent), and
/// never overrides a disk-root hit for the same volume.
pub fn resolve_live(disk_roots: HashMap<String, PathBuf>, remembered: &[(String, PathBuf)])
    -> HashMap<String, PathBuf>
{
    let mut map = disk_roots;
    for (vid, path) in remembered {
        if map.contains_key(vid) { continue; }
        if crate::volume::read_volume_id(path).as_deref() == Some(vid.as_str()) {
            map.insert(vid.clone(), path.clone());
        }
    }
    map
}

/// The catalog's remembered (volume_id, last_scanned_path) pairs; empty if the catalog can't be read.
fn remembered_paths(catalog_path: &std::path::Path) -> Vec<(String, PathBuf)> {
    match crate::catalog::Catalog::open_readonly(catalog_path) {
        Ok(cat) => cat.volume_paths().unwrap_or_default().into_iter()
            .map(|(id, p)| (id, PathBuf::from(p))).collect(),
        Err(_) => Vec::new(),
    }
}
```

Keep the existing `scan_mounts`, `live_mounts`, `disk_capacity` functions unchanged.

- [ ] **Step 5: Fix the two construction/matcher sites in `src/web.rs`.**
  - In `AppState::new_live` (~line 28): `mounts: crate::mounts::MountResolver::Live { catalog_path: catalog_path.clone() },`
  - In the `app_state_new_live_has_token_and_live_mounts` test: `assert!(matches!(s.mounts, crate::mounts::MountResolver::Live { .. }));`

- [ ] **Step 6: Run** `cargo test -p cleanupstorages` — the `resolve_live` test passes, and every existing test still compiles/passes (tests use `MountResolver::Fixed`, unaffected). `cargo build` warning-clean.

- [ ] **Step 7: Commit**

```bash
git add src/scanner.rs src/mounts.rs src/web.rs
git commit -m "fix(scanner): recognize folder-drives as connected via remembered scan path"
```

---

### Task 3: rename/description — DTOs, endpoint, CLI, effective names in APIs

**Files:** Modify `src/web.rs`, `src/commands.rs`, `src/main.rs`

**Interfaces:**
- `DriveDto` gains `display_name: Option<String>`, `description: Option<String>`.
- `VolumeDto` gains `display_name: Option<String>` (the effective name).
- `POST /api/rename-drive {volume_id, name?, description?}` → `{name}` (CSRF).
- `cus rename <mount> [--name X] [--description Y]`.
- `/api/search` `volume_label` and `/api/volumes`/`/api/stats` names use the effective label.

- [ ] **Step 1: Add fields to the DTOs** (`src/web.rs`). `DriveDto`: add `display_name: Option<String>` and `description: Option<String>`. `VolumeDto`: change to `struct VolumeDto { volume_id: String, label: String, display_name: Option<String>, active_files: i64, active_bytes: i64 }`.

- [ ] **Step 2: Fill them.** In `api_drives`, before the `out.push`, add `let (display_name, description) = cat.volume_meta(&volume_id).map_err(err500)?;` and include both in the `DriveDto`. In `volume_dtos`:

```rust
fn volume_dtos(cat: &Catalog) -> anyhow::Result<Vec<VolumeDto>> {
    let eff = cat.effective_labels()?;
    Ok(cat.volume_stats()?.into_iter()
        .map(|(volume_id, label, active_files, active_bytes)| {
            let display_name = eff.get(&volume_id).cloned();
            VolumeDto { volume_id, label, display_name, active_files, active_bytes }
        }).collect())
}
```

In `api_search`, change the label map (currently `cat.volume_stats()…map(|(id,label,_,_)| (id,label))`) to `let labels = cat.effective_labels().map_err(err500)?;` so `volume_label` becomes the effective name. (The seed tests seed no display_name, so `volume_label` still equals the detected label — existing assertions hold.)

- [ ] **Step 3: Add the endpoint** (`src/web.rs`), CSRF-first like `api_forget_drive`:

```rust
#[derive(Deserialize)]
struct RenameReq { volume_id: String, name: Option<String>, description: Option<String> }

#[derive(Serialize)]
struct RenameResultDto { name: String }

async fn api_rename_drive(State(state): State<AppState>, headers: HeaderMap, body: Json<RenameReq>)
    -> Result<Json<RenameResultDto>, (StatusCode, String)>
{
    check_csrf(&headers, &state)?;
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = now_secs()?;
    cat.set_volume_meta(&body.volume_id, body.name.as_deref(), body.description.as_deref(), now)
        .map_err(err500)?;
    let name = cat.effective_labels().map_err(err500)?
        .get(&body.volume_id).cloned().unwrap_or_else(|| body.volume_id.clone());
    Ok(Json(RenameResultDto { name }))
}
```

Register: `.route("/api/rename-drive", post(api_rename_drive))`. Also add a `"rename"` arm to `activity_summary` for a nicer feed line: `"rename" => "Renamed a drive".to_string(),`.

- [ ] **Step 4: Endpoint test** (`src/web.rs` tests; `seed_dupes` → vol-1 "Photos HDD", token "T"):

```rust
    #[tokio::test]
    async fn rename_drive_requires_token_then_persists_effective_name() {
        let (_t, db, state) = seed_dupes();
        let (status, _) = post_json(state.clone(), "/api/rename-drive", None,
            serde_json::json!({"volume_id":"vol-1","name":"Trip 2019"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, json) = post_json(state, "/api/rename-drive", Some("T"),
            serde_json::json!({"volume_id":"vol-1","name":"Trip 2019","description":"summer"})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["name"], "Trip 2019");
        // effective name now flows into /api/search and /api/drives
        let v = get_json(&db, "/api/search").await;
        assert_eq!(v.as_array().unwrap()[0]["volume_label"], "Trip 2019");
        let d = get_json(&db, "/api/drives").await;
        assert_eq!(d.as_array().unwrap()[0]["display_name"], "Trip 2019");
        assert_eq!(d.as_array().unwrap()[0]["description"], "summer");
    }
```

(Check the real `post_json` helper signature in this test module and match it; `get_json` builds its own router over `db`.)

- [ ] **Step 5: CLI `rename`** (`src/commands.rs`): add after `cmd_forget`:

```rust
pub fn cmd_rename(mount: &Path, name: Option<&str>, description: Option<&str>) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}; scan the drive first", mount.display()))?;
    let now = now_secs();
    cat.set_volume_meta(&vid, name, description, now)?;
    let _ = snapshot(&cfg, now);
    println!("Updated drive {vid}.");
    Ok(())
}
```

- [ ] **Step 6: Wire the CLI command** (`src/main.rs`): add the enum variant, the span-name arm (`Command::Rename { .. } => "rename",`), and the dispatch arm.

```rust
    /// Set a drive's custom name and/or description (shown in the UI).
    Rename {
        /// Current mount path of the drive.
        mount: std::path::PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
```
```rust
        Command::Rename { mount, name, description } =>
            commands::cmd_rename(&mount, name.as_deref(), description.as_deref()),
```

- [ ] **Step 7: CLI integration test** (`tests/rename_cli.rs`, mirroring `tests/forget_cli.rs`): scan a temp folder-drive (`--readonly-fallback fingerprint`), run `rename <path> --name "My Drive" --description "notes"`, assert success; a second `status` or `/api/drives` isn't available from CLI, so assert the command prints "Updated drive" and exits 0. (Reuse the `forget_cli.rs` scaffolding.)

- [ ] **Step 8: Build + full test.** `cargo build` (exhaustive match arms) then `cargo test -p cleanupstorages`. Expected PASS.

- [ ] **Step 9: Commit**

```bash
git add src/web.rs src/commands.rs src/main.rs tests/rename_cli.rs
git commit -m "feat: rename drives + description (web endpoint, CLI, effective names in APIs)"
```

---

### Task 4: Drives page edit UI + effective names on Browse/Overview

**Files:** Modify `src/web_ui.rs`

**Interfaces:** Consumes `/api/drives` (now with `display_name`/`description`), `/api/rename-drive`, `/api/volumes` (now with `display_name`).

- [ ] **Step 1: Drives card — show effective name + description + an Edit control.** In `drives_page`'s card rendering (read the current markup first), change the title to the effective name and add the description + an edit affordance. The card title currently is `<h3 …><span class="dot" …></span>${esc(d.label)}</h3>`; change `esc(d.label)` → `esc(d.display_name||d.label)`, and after the status/meta line add:

```js
`${d.description?`<div class="mut" style="font-size:12px;margin-top:2px">${esc(d.description)}</div>`:""}`
```

Add an **Edit** button to the card's action row (next to Rescan/Forget):

```js
`<button class="btn edit">Edit…</button>`
```

And in the per-card wiring loop (where `.forget`/`.rescan` handlers are attached), add an edit handler that reveals a small inline form and posts:

```js
c.querySelector(".edit").onclick=()=>{
  const cur=c.querySelector("h3").textContent.trim();
  const name=window.prompt("Drive name (blank = use detected name):", cur);
  if(name===null) return;
  const desc=window.prompt("Short description (optional):", c.dataset.desc||"");
  if(desc===null) return;
  apiPost("/api/rename-drive",{volume_id:c.dataset.vid,name,description:desc})
    .then(()=>{ $("#msg").textContent="Drive updated."; load(); })
    .catch(e=>{ $("#msg").textContent="Error: "+e; });
};
```

Store the description on the card element for the prompt default: add `data-desc="${esc(d.description||'')}"` to the card's opening `<div class="card" …>`. (A `window.prompt` pair keeps this task small and dependency-free; the richer inline form comes with the visual overhaul in sub-project 2.)

- [ ] **Step 2: Effective names on Browse + Overview.**
  - Browse dropdown (`browse_page` `init`): `o.textContent = v.display_name || v.label;`
  - Overview reclaim bars (`overview_page`): the label span `${esc(d.label)}` → `${esc(d.display_name||d.label)}` (the `/api/drives` rows now carry `display_name`).
  - (Browse tree drive nodes already use `hit.volume_label`, which is now the effective name from Task 3 — no change needed.)

- [ ] **Step 3: Page test** (`src/web.rs` tests): extend `drives_page_is_wired_and_self_contained` (or add a line) to assert the page references `/api/rename-drive`:

```rust
        assert!(body.contains("/api/rename-drive"), "drives page can rename");
```

- [ ] **Step 4: Build + test.** `cargo build`; `cargo test -p cleanupstorages --lib web`. Confirm self-contained (no `http(s)://`) and green.

- [ ] **Step 5: Manual smoke (recommended):** build release, scan a sandbox folder-drive, open `/drives` — it now shows **connected**, and Edit sets a name/description that appears here + in Browse's drive filter and tree.

- [ ] **Step 6: Commit**

```bash
git add src/web_ui.rs src/web.rs
git commit -m "feat(web): Drives edit (name/description) UI + effective names on Browse/Overview"
```

---

## Self-review notes

- **Spec coverage:** offline fix — last_scanned_path column + `set_volume_path`/`volume_paths` (T1), record on scan + `resolve_live` merge + `MountResolver::Live { catalog_path }` (T2); rename/description — columns + `set_volume_meta`/`volume_meta`/`effective_labels` (T1), DriveDto/VolumeDto + endpoint + CLI + effective names in APIs (T3), Drives edit UI + Browse/Overview names (T4). All mapped.
- **Type consistency:** `resolve_live(HashMap<String,PathBuf>, &[(String,PathBuf)]) -> HashMap<String,PathBuf>`; `MountResolver::Live { catalog_path: PathBuf }`; `set_volume_meta(&str, Option<&str>, Option<&str>, i64)`; `effective_labels() -> HashMap<String,String>`; DTO fields `display_name: Option<String>`, `description: Option<String>`; endpoint `{volume_id, name?, description?}` → `{name}`.
- **Reliability:** connectivity change is detection-only; remembered path trusted only on marker match; additive schema; CSRF-first on the new endpoint; reads `open_readonly`.
- **No placeholders:** every code step shows the code; the two "read the current markup first" spots (T4) and the `post_json` signature (T3) are called out with how to resolve.

# Phase 2b — Duplicate Review GUI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A visual review screen in the local web app where the user works through duplicate groups one at a time (Tinder-style), sees the copies side by side with photo thumbnails and a metadata diff, and confirms which copy to keep — quarantining the rest through the Phase-2a engine. This is the tool's first **write** endpoint; it is reversible (quarantine only — purge stays CLI-only) and guarded.

**Architecture:** A new `src/mounts.rs` resolves each `volume_id` to its current mount path (by reading the `.cleanupstorages_id` marker at each mounted drive root via `sysinfo`), so the server can locate and preview files and know which drives are connected. The web layer gains `GET /api/duplicates` (groups + per-member metadata + a suggested keep), `GET /api/preview/:id` (an in-memory downscaled JPEG thumbnail for photos on connected drives, loose or one-level-archived), `POST /api/quarantine` (CSRF-guarded; groups the requested ids by volume and calls `quarantine::quarantine_files` per connected volume), and a `/review` page. `AppState` gains a `MountResolver` and a per-run CSRF token; existing browse routes are unchanged.

**Tech Stack:** Rust 1.88, existing deps, plus `image = "0.25"` (decode/resize/encode thumbnails). Reuses `zip`, `axum`, `tokio`, `uuid`, `serde`.

## Global Constraints

- **Reversible-only writes over HTTP.** The only mutating endpoint is `POST /api/quarantine`, which performs the reversible same-drive move via the Phase-2a engine. **Purge (the irreversible delete) is NOT exposed over HTTP** — it stays a deliberate CLI command. No other write endpoint exists.
- **Localhost-only + CSRF guard.** The server still binds `127.0.0.1` only. `POST /api/quarantine` requires a header `x-cleanup-token` equal to a random per-run token embedded in the page; a request without it gets `403`. (A cross-site page can't read the token — it can't read cross-origin responses — so it can't forge the header. This closes the one realistic CSRF vector for a write endpoint.)
- **All Phase-2a safety gates still apply** because the endpoint calls the same `quarantine::quarantine_files`: marker-verified drive, rename-only, disk-aware never-remove-last-copy guard, append-only `actions_log`. The web layer adds no new destructive path.
- **The drive must be connected to act or preview.** Quarantine/preview resolve the volume's current mount; if the drive isn't mounted, the endpoint returns a clear "drive not connected" result (`409` for quarantine, `404` for preview) and does nothing.
- **Read vs write catalog handles:** read endpoints (`/api/duplicates`, `/api/preview`) use `Catalog::open_readonly`; the write endpoint (`/api/quarantine`) uses the read-write `Catalog::open`.
- **The page stays self-contained** (inlined CSS/JS, no external requests) and **XSS-safe** (all catalog-derived strings escaped before `innerHTML`; the same `esc()` used on the browse page). Thumbnails are served from our own `/api/preview/:id`, not external URLs.
- **Preview is best-effort and bounded:** only `category == photo`, only when the drive is mounted, only loose files or archive entries directly inside a top-level `.zip` (a `container_chain` with no ` › ` nesting separator); anything else returns `404` and the UI falls back to metadata only. Decoding failures are non-fatal (`404`), never a panic. Thumbnails are capped at 320 px longest side.
- **Git:** work on branch `feat/phase2b-review-gui` off `main`. Conventional Commits, scope `mounts`/`web`/`cli`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (already merged):** `quarantine::quarantine_files`, `catalog` (`duplicate_groups`, `get_file`, `volume_stats`, `open`/`open_readonly`), `volume::{read_volume_id, QUARANTINE_DIR}`, `config::Config`, `backup::snapshot`, the 1c web server. **Out of scope (later):** Case 4 archive repack (2c), full Case-3 "whole archive redundant" advisory detection, nested-archive previews, video thumbnails, near-duplicate detection.

---

## File Structure

- `Cargo.toml` — add `image = "0.25"`.
- `src/mounts.rs` — **new**: `MountResolver`, live mount detection, marker reading. Registered in `lib.rs`.
- `src/web.rs` — `AppState` gains `mounts` + `csrf_token`; new routes/handlers/DTOs; `/review` page; `serve` builds the live state + token.
- `src/lib.rs` — `pub mod mounts;`.
- `tests/review_flow.rs` — **new** real-TCP e2e: duplicates → quarantine.

---

### Task 1: Mount registry (`src/mounts.rs`)

**Files:**
- Create: `src/mounts.rs`
- Modify: `src/lib.rs` (`pub mod mounts;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `pub enum MountResolver { Live, Fixed(std::collections::HashMap<String, std::path::PathBuf>) }` — `#[derive(Clone)]`.
- `impl MountResolver { pub fn resolve(&self, volume_id: &str) -> Option<PathBuf> }`.
- `pub fn scan_mounts<I: IntoIterator<Item = PathBuf>>(roots: I) -> HashMap<String, PathBuf>` — read the marker at each root; map volume_id → root. (Testable with fake roots.)
- `pub fn live_mounts() -> HashMap<String, PathBuf>` — `scan_mounts` over `sysinfo` disk mount points.

- [ ] **Step 1: Write failing tests**

Create `src/mounts.rs`:

```rust
//! Resolve each catalogued volume to its CURRENT mount path (drive letters/mounts change), by
//! reading the `.cleanupstorages_id` marker at each connected drive root.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone)]
pub enum MountResolver {
    /// Detect mounts live from the OS (production).
    Live,
    /// A fixed volume_id → root map (tests).
    Fixed(HashMap<String, PathBuf>),
}

impl MountResolver {
    /// The current mount root for `volume_id`, if the drive is connected.
    pub fn resolve(&self, volume_id: &str) -> Option<PathBuf> {
        match self {
            MountResolver::Live => live_mounts().get(volume_id).cloned(),
            MountResolver::Fixed(m) => m.get(volume_id).cloned(),
        }
    }
}

/// Build volume_id → root by reading the identity marker at each candidate root.
pub fn scan_mounts<I: IntoIterator<Item = PathBuf>>(roots: I) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for root in roots {
        if let Some(vid) = crate::volume::read_volume_id(&root) {
            map.entry(vid).or_insert(root);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reads_markers_at_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("driveA");
        let b = tmp.path().join("driveB");
        let c = tmp.path().join("driveC_nomark");
        for d in [&a, &b, &c] { std::fs::create_dir_all(d).unwrap(); }
        std::fs::write(a.join(".cleanupstorages_id"), "vol-A").unwrap();
        std::fs::write(b.join(".cleanupstorages_id"), "vol-B").unwrap();
        // c has no marker

        let map = scan_mounts([a.clone(), b.clone(), c.clone()]);
        assert_eq!(map.get("vol-A"), Some(&a));
        assert_eq!(map.get("vol-B"), Some(&b));
        assert_eq!(map.len(), 2); // c skipped

        let r = MountResolver::Fixed(map);
        assert_eq!(r.resolve("vol-A"), Some(a));
        assert_eq!(r.resolve("vol-missing"), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mounts`
Expected: FAIL — module/`live_mounts` not resolved (add `pub mod mounts;` and `live_mounts` in the next step).

- [ ] **Step 3: Add `live_mounts` + register module**

Append to `src/mounts.rs`:

```rust
/// All currently-mounted drives that carry our marker, by volume_id.
pub fn live_mounts() -> HashMap<String, PathBuf> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    scan_mounts(disks.list().iter().map(|d| d.mount_point().to_path_buf()))
}
```

Add `pub mod mounts;` to `src/lib.rs`.

- [ ] **Step 4: Run tests + build**

Run: `cargo test --lib mounts` then `cargo build`
Expected: PASS; builds.

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/phase2b-review-gui   # only if not already on it
git add src/mounts.rs src/lib.rs
git commit -m "feat(mounts): resolve volume_id to current mount via drive markers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: AppState refactor — mounts + CSRF token (existing routes unchanged)

**Files:**
- Modify: `src/web.rs`
- Test: existing `#[cfg(test)]` still passes; add one token test.

**Interfaces:**
- `AppState { pub catalog_path: PathBuf, pub mounts: crate::mounts::MountResolver, pub csrf_token: String }` — `#[derive(Clone)]`.
- `impl AppState { pub fn new_live(catalog_path: PathBuf) -> AppState }` — Live mounts + a random token (uuid v4).
- `pub fn build_router_with(state: AppState) -> Router` — the real builder (all routes).
- `pub fn build_router(catalog_path: PathBuf) -> Router` — convenience: `build_router_with(AppState::new_live(catalog_path))`. **Existing callers/tests keep working unchanged.**

- [ ] **Step 1: Refactor `AppState` + `build_router`**

In `src/web.rs`, replace the `AppState` struct and `build_router`:

```rust
#[derive(Clone)]
pub struct AppState {
    pub catalog_path: PathBuf,
    pub mounts: crate::mounts::MountResolver,
    pub csrf_token: String,
}

impl AppState {
    /// Production state: live mount detection and a fresh random CSRF token.
    pub fn new_live(catalog_path: PathBuf) -> AppState {
        AppState {
            catalog_path,
            mounts: crate::mounts::MountResolver::Live,
            csrf_token: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// Convenience builder used by the CLI and existing tests (live mounts, random token).
pub fn build_router(catalog_path: PathBuf) -> Router {
    build_router_with(AppState::new_live(catalog_path))
}

/// The full router. New review routes are added here in later tasks.
pub fn build_router_with(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/search", get(api_search))
        .route("/api/volumes", get(api_volumes))
        .route("/api/stats", get(api_stats))
        .with_state(state)
}
```

Update `serve` to build state once (so the token is stable for the run):

```rust
pub async fn serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()> {
    let app = build_router_with(AppState::new_live(catalog_path));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("CleanUpStorages web UI at {url}");
    println!("(browse is read-only; the review screen can quarantine. Press Ctrl+C to stop)");
    if open {
        if let Err(e) = open_browser(&url) {
            eprintln!("could not open a browser automatically ({e}); open {url} yourself");
        }
    }
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 2: Add a token test**

Add to `web.rs` `mod tests`:

```rust
    #[test]
    fn app_state_new_live_has_token_and_live_mounts() {
        let s = AppState::new_live(PathBuf::from("x.db"));
        assert!(!s.csrf_token.is_empty());
        assert!(matches!(s.mounts, crate::mounts::MountResolver::Live));
    }
```

- [ ] **Step 3: Run the full suite**

Run: `cargo test` then `cargo build`
Expected: PASS — all existing web tests (which call `build_router(path)`) still compile and pass; new token test passes. `uuid` is already a dependency.

- [ ] **Step 4: Commit**

```bash
git add src/web.rs
git commit -m "refactor(web): AppState carries mount resolver and per-run CSRF token

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `GET /api/duplicates`

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Route `/api/duplicates`. Returns `Json<Vec<GroupDto>>`.
- `GroupDto { hash: String, suggested_keep_id: i64, members: Vec<MemberDto> }`.
- `MemberDto { id, location, filename, volume_id, volume_label, size_bytes, category, created_time: Option<i64>, modified_time: Option<i64>, status, is_loose: bool, mounted: bool }`.
- Suggested keep = the member with the earliest non-null `created_time`; tie/none → earliest non-null `modified_time`; else the smallest `id`. (Keep the original — usually the oldest.)

- [ ] **Step 1: Write failing tests**

Add a helper and tests to `web.rs` `mod tests` (extends the seeded catalog with a duplicate pair on a mounted fake drive):

```rust
    use std::collections::HashMap;

    // Seed a catalog with a duplicate pair of LOOSE files on one volume, plus a fake mounted drive.
    fn seed_dupes() -> (tempfile::TempDir, PathBuf, AppState) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("driveA");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join(".cleanupstorages_id"), "vol-1").unwrap();
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume {
                volume_id: "vol-1".into(), label: "Photos HDD".into(), identified_by: "marker".into(),
                first_seen_at: 1, last_seen_at: 1 }).unwrap();
            let mk = |path: &str, created: i64| crate::catalog::models::NewFile {
                volume_id: "vol-1".into(), relative_path: path.into(),
                filename: path.rsplit('/').next().unwrap().into(), extension: "jpg".into(),
                size_bytes: 10, content_hash: "DUP".into(), created_time: Some(created),
                modified_time: Some(created), accessed_time: None,
                category: crate::category::Category::Photo, container_chain: None };
            cat.upsert_file(&mk("a.jpg", 1000), 100).unwrap();
            cat.upsert_file(&mk("copy/a.jpg", 2000), 100).unwrap();
        }
        let mut mounts = HashMap::new();
        mounts.insert("vol-1".to_string(), drive);
        let state = AppState { catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into() };
        (tmp, db, state)
    }

    async fn get_json_state(state: AppState, uri: &str) -> serde_json::Value {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK, "uri {uri}");
        let bytes = axum::body::to_bytes(res.into_body(), 5_000_000).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn api_duplicates_groups_with_suggested_keep_and_mounted() {
        let (_t, _db, state) = seed_dupes();
        let v = get_json_state(state, "/api/duplicates").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["members"].as_array().unwrap().len(), 2);
        // earliest created_time (1000) is a.jpg -> its id is the suggested keep
        let members = arr[0]["members"].as_array().unwrap();
        let keep = arr[0]["suggested_keep_id"].as_i64().unwrap();
        let a = members.iter().find(|m| m["filename"] == "a.jpg").unwrap();
        assert_eq!(a["id"].as_i64().unwrap(), keep);
        assert_eq!(a["volume_label"], "Photos HDD");
        assert_eq!(a["mounted"], true);
        assert_eq!(a["is_loose"], true);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — route/handler absent.

- [ ] **Step 3: Implement**

In `src/web.rs`, add DTOs + handler and register the route in `build_router_with`:

```rust
#[derive(Serialize)]
struct MemberDto {
    id: i64, location: String, filename: String, volume_id: String, volume_label: String,
    size_bytes: i64, category: String, created_time: Option<i64>, modified_time: Option<i64>,
    status: String, is_loose: bool, mounted: bool,
}

#[derive(Serialize)]
struct GroupDto { hash: String, suggested_keep_id: i64, members: Vec<MemberDto> }

fn display_location(f: &FileRecord) -> String {
    let base = f.original_path.as_deref().unwrap_or(&f.relative_path);
    match &f.container_chain {
        Some(chain) => format!("{base} › {chain}"),
        None => base.to_string(),
    }
}

/// Earliest-created (fallback earliest-modified, fallback smallest id) — keep the original.
fn suggested_keep(members: &[FileRecord]) -> i64 {
    members.iter().min_by_key(|f| (
        f.created_time.unwrap_or(i64::MAX),
        f.modified_time.unwrap_or(i64::MAX),
        f.id,
    )).map(|f| f.id).unwrap_or(0)
}

async fn api_duplicates(State(state): State<AppState>)
    -> Result<Json<Vec<GroupDto>>, (axum::http::StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let labels: std::collections::HashMap<String, String> = cat.volume_stats().map_err(err500)?
        .into_iter().map(|(id, label, _, _)| (id, label)).collect();
    let groups = cat.duplicate_groups().map_err(err500)?;
    let mut out = Vec::new();
    for group in groups {
        // Capture the shared content hash before consuming the group's rows.
        let hash = group.first().map(|f| f.content_hash.clone()).unwrap_or_default();
        let keep = suggested_keep(&group);
        let members = group.into_iter().map(|f| {
            let mounted = state.mounts.resolve(&f.volume_id).is_some();
            MemberDto {
                id: f.id, location: display_location(&f), filename: f.filename.clone(),
                volume_label: labels.get(&f.volume_id).cloned().unwrap_or_default(),
                volume_id: f.volume_id, size_bytes: f.size_bytes,
                category: f.category.as_str().to_string(),
                created_time: f.created_time, modified_time: f.modified_time,
                status: f.status.as_str().to_string(),
                is_loose: f.container_chain.is_none(), mounted,
            }
        }).collect::<Vec<_>>();
        out.push(GroupDto { hash, suggested_keep_id: keep, members });
    }
    Ok(Json(out))
}
```

Register the route in `build_router_with`: `.route("/api/duplicates", get(api_duplicates))`.

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): GET /api/duplicates with suggested keep and mount status

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `GET /api/preview/:id` (photo thumbnails)

**Files:**
- Modify: `Cargo.toml` (add `image = "0.25"`)
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Route `/api/preview/:id`. Returns a JPEG thumbnail (`Content-Type: image/jpeg`) for a photo on a mounted drive; otherwise `404` with a short reason.
- `fn thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>>` — decode any supported image, resize to fit `max_dim`, encode JPEG.
- `fn read_zip_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>>` — read one top-level entry's bytes.
- Bounds: photo category only; loose OR archive entry whose `container_chain` has no ` › ` (top-level entry); drive mounted; else 404.

- [ ] **Step 1: Add dependency**

In `Cargo.toml` `[dependencies]`: `image = "0.25"`.

- [ ] **Step 2: Write failing tests**

Add to `web.rs` `mod tests`:

```rust
    fn tiny_png() -> Vec<u8> {
        // 2x2 red PNG, generated via the image crate.
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn thumbnail_downscales_and_encodes_jpeg() {
        // a 100x40 image thumbnails to <=32px longest side, and the output decodes as JPEG.
        let img = image::RgbImage::from_pixel(100, 40, image::Rgb([0, 128, 255]));
        let mut src = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut src, image::ImageFormat::Png).unwrap();
        let thumb = thumbnail_jpeg(&src.into_inner(), 32).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert!(decoded.width() <= 32 && decoded.height() <= 32);
        assert!(decoded.width() >= 1);
    }

    #[tokio::test]
    async fn preview_returns_jpeg_for_loose_photo_on_mounted_drive() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, db, state) = seed_dupes();
        // write a real image at the loose path on the fake drive
        let drive = match &state.mounts { crate::mounts::MountResolver::Fixed(m) => m["vol-1"].clone(), _ => unreachable!() };
        std::fs::write(drive.join("a.jpg"), tiny_png()).unwrap();
        // find a.jpg's id
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let id = cat.active_file_id("vol-1", "a.jpg").unwrap().unwrap();

        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri(format!("/api/preview/{id}"))
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let ct = res.headers().get("content-type").unwrap().to_str().unwrap().to_string();
        assert_eq!(ct, "image/jpeg");
        let bytes = axum::body::to_bytes(res.into_body(), 5_000_000).await.unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — `thumbnail_jpeg`/route absent.

- [ ] **Step 4: Implement**

In `src/web.rs` add imports and code:

```rust
use std::path::Path;
use axum::extract::Path as AxPath;
use axum::response::Response;
use axum::http::{StatusCode, header};

/// Decode any supported image, downscale to fit `max_dim` on the longest side, re-encode as JPEG.
fn thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
    let thumb = img.thumbnail(max_dim, max_dim); // preserves aspect ratio, never upsizes past bounds
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg)?;
    Ok(out.into_inner())
}

/// Read one top-level entry's bytes from a zip archive.
fn read_zip_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

const PREVIEW_MAX_DIM: u32 = 320;

async fn api_preview(State(state): State<AppState>, AxPath(id): AxPath<i64>) -> Response {
    let not_found = |msg: &str| (StatusCode::NOT_FOUND, msg.to_string()).into_response();

    let cat = match Catalog::open_readonly(&state.catalog_path) {
        Ok(c) => c, Err(e) => return err500(e).into_response(),
    };
    let rec = match cat.get_file(id) {
        Ok(Some(r)) => r, Ok(None) => return not_found("no such file"),
        Err(e) => return err500(e).into_response(),
    };
    if rec.category != crate::category::Category::Photo {
        return not_found("preview only for photos");
    }
    let Some(mount) = state.mounts.resolve(&rec.volume_id) else {
        return not_found("drive not connected");
    };

    let bytes = match &rec.container_chain {
        None => std::fs::read(mount.join(&rec.relative_path)).ok(),
        Some(chain) if !chain.contains(" › ") => read_zip_entry(&mount.join(&rec.relative_path), chain).ok(),
        Some(_) => return not_found("nested-archive preview not supported"),
    };
    let Some(bytes) = bytes else { return not_found("file unavailable") };

    match thumbnail_jpeg(&bytes, PREVIEW_MAX_DIM) {
        Ok(jpeg) => ([(header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response(),
        Err(_) => not_found("not a decodable image"),
    }
}
```

Add `use axum::response::IntoResponse;` to the imports at the top if not already present. Register the route: `.route("/api/preview/:id", get(api_preview))`.

- [ ] **Step 5: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS (thumbnail unit test + preview handler test). `cargo build` may take a while (image crate).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/web.rs
git commit -m "feat(web): GET /api/preview/:id photo thumbnails (loose + top-level archive)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: `POST /api/quarantine` (CSRF-guarded write)

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Route `POST /api/quarantine`. Header `x-cleanup-token` must equal `state.csrf_token` (else `403`). JSON body `{ "quarantine_ids": [i64, ...] }`. Groups ids by volume; for each volume that resolves to a mount, calls `quarantine::quarantine_files(&cat_rw, &mount, &volume_id, &ids, now)`. Volumes not mounted are reported as skipped. Takes a catalog snapshot after. Returns `QuarantineResultDto { quarantined, skipped, unmounted_volumes: Vec<String> }`.
- Uses read-write `Catalog::open`.

- [ ] **Step 1: Write failing tests**

Add to `web.rs` `mod tests`:

```rust
    async fn post_json(state: AppState, uri: &str, token: Option<&str>, body: serde_json::Value)
        -> (axum::http::StatusCode, serde_json::Value)
    {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let mut req = Request::builder().method("POST").uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token { req = req.header("x-cleanup-token", t); }
        let app = build_router_with(state);
        let res = app.oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 5_000_000).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn quarantine_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/quarantine", None,
            serde_json::json!({"quarantine_ids":[1]})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn quarantine_moves_the_chosen_copy() {
        let (_t, db, state) = seed_dupes();
        // put real files on the fake drive so the disk-aware survivor check passes and the move works
        let drive = match &state.mounts { crate::mounts::MountResolver::Fixed(m) => m["vol-1"].clone(), _ => unreachable!() };
        std::fs::create_dir_all(drive.join("copy")).unwrap();
        std::fs::write(drive.join("a.jpg"), b"DUP").unwrap();
        std::fs::write(drive.join("copy/a.jpg"), b"DUP").unwrap();
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let victim = cat.active_file_id("vol-1", "copy/a.jpg").unwrap().unwrap();
        drop(cat);

        let (status, json) = post_json(state, "/api/quarantine", Some("T"),
            serde_json::json!({"quarantine_ids":[victim]})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(json["quarantined"], 1);
        assert!(!drive.join("copy/a.jpg").exists());
        assert!(drive.join("_ToDelete/copy/a.jpg").exists());
        assert!(drive.join("a.jpg").exists()); // survivor stays
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — route absent.

- [ ] **Step 3: Implement**

In `src/web.rs`:

```rust
use axum::routing::post;
use axum::http::HeaderMap;

#[derive(Deserialize)]
struct QuarantineReq { quarantine_ids: Vec<i64> }

#[derive(Serialize, Default)]
struct QuarantineResultDto { quarantined: usize, skipped: usize, unmounted_volumes: Vec<String> }

async fn api_quarantine(State(state): State<AppState>, headers: HeaderMap, body: Json<QuarantineReq>)
    -> Result<Json<QuarantineResultDto>, (StatusCode, String)>
{
    // CSRF: require the per-run token (a cross-site page can't read it).
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;

    // Group requested ids by their volume.
    let mut by_volume: std::collections::HashMap<String, Vec<i64>> = std::collections::HashMap::new();
    for id in &body.quarantine_ids {
        if let Some(rec) = cat.get_file(*id).map_err(err500)? {
            by_volume.entry(rec.volume_id).or_default().push(*id);
        }
    }

    let mut result = QuarantineResultDto::default();
    for (volume_id, ids) in by_volume {
        match state.mounts.resolve(&volume_id) {
            Some(mount) => {
                let out = crate::quarantine::quarantine_files(&cat, &mount, &volume_id, &ids, now)
                    .map_err(err500)?;
                result.quarantined += out.quarantined;
                result.skipped += out.skipped;
            }
            None => {
                result.skipped += ids.len();
                result.unmounted_volumes.push(volume_id);
            }
        }
    }

    // Snapshot after the mutation.
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    Ok(Json(result))
}
```

Register: `.route("/api/quarantine", post(api_quarantine))`.

- [ ] **Step 4: Run tests + full suite**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS (csrf-403 test + move test + all existing).

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): POST /api/quarantine (CSRF-guarded, per-volume) via the 2a engine

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: The review page + nav + end-to-end

**Files:**
- Modify: `src/web.rs` (add `/review` route + page; link it from the browse page)
- Create: `tests/review_flow.rs`
- Test: inline page test + real-TCP e2e

**Interfaces:**
- Route `/review` → `Html<String>` (self-contained page, embeds the CSRF token in a `<meta name="csrf">`).
- The browse page (`/`) gets a link to `/review`.

- [ ] **Step 1: Write the failing page test**

Add to `web.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn review_page_is_self_contained_and_has_token() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/review").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"csrf\""), "token meta present");
        assert!(body.contains("/api/duplicates"), "fetches duplicates");
        assert!(body.contains("/api/quarantine"), "posts to quarantine");
        assert!(!body.contains("http://") && !body.contains("https://"), "self-contained");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — `/review` route absent.

- [ ] **Step 3: Implement the page + route + nav**

Add the handler (returns owned HTML with the token injected):

```rust
async fn review(State(state): State<AppState>) -> Html<String> {
    Html(REVIEW_HTML.replace("{{CSRF}}", &state.csrf_token))
}
```

Add the `REVIEW_HTML` const (self-contained; one group at a time; thumbnails via `/api/preview/:id`; metadata table; Keep/Quarantine-others/Skip). Register `.route("/review", get(review))`, and add a link on the browse page header (in `INDEX_HTML`, inside the `<h1>` line add `<a href="/review" style="font-size:12px">Review duplicates →</a>`).

```rust
const REVIEW_HTML: &str = r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf" content="{{CSRF}}">
<title>CleanUpStorages — Review duplicates</title>
<style>
  :root { color-scheme: light dark; --bg:#111; --fg:#eee; --mut:#999; --line:#333; --accent:#5aa0ff; --danger:#e0705a; }
  @media (prefers-color-scheme: light){ :root{ --bg:#fff; --fg:#111; --mut:#666; --line:#ddd; } }
  *{box-sizing:border-box;} body{margin:0;font:14px/1.4 system-ui,sans-serif;background:var(--bg);color:var(--fg);}
  header{padding:12px 16px;border-bottom:1px solid var(--line);display:flex;gap:12px;align-items:center;}
  header a{color:var(--accent);text-decoration:none;font-size:12px;}
  main{padding:16px;max-width:1000px;margin:0 auto;}
  .progress{color:var(--mut);margin-bottom:12px;}
  .cards{display:flex;flex-wrap:wrap;gap:12px;}
  .card{border:1px solid var(--line);border-radius:10px;padding:10px;width:230px;}
  .card.keep{border-color:var(--accent);box-shadow:0 0 0 1px var(--accent) inset;}
  .thumb{width:100%;height:150px;object-fit:contain;background:#0004;border-radius:6px;}
  .noimg{width:100%;height:150px;display:flex;align-items:center;justify-content:center;color:var(--mut);background:#0002;border-radius:6px;font-size:12px;text-align:center;}
  .loc{word-break:break-all;font-size:12px;margin:6px 0 2px;}
  .kv{color:var(--mut);font-size:12px;} .kv b{color:var(--fg);font-weight:600;}
  .badge{font-size:11px;color:var(--accent);} .arch{color:var(--mut);font-size:11px;}
  .actions{display:flex;gap:8px;margin-top:8px;}
  button{font:inherit;padding:8px 12px;border-radius:8px;border:1px solid var(--line);background:transparent;color:var(--fg);cursor:pointer;}
  button.primary{border-color:var(--accent);color:var(--accent);}
  button.danger{border-color:var(--danger);color:var(--danger);}
  .msg{color:var(--mut);margin-top:10px;min-height:1.4em;}
</style></head>
<body>
<header><strong>Review duplicates</strong><a href="/">← Back to browse</a></header>
<main>
  <div class="progress" id="progress"></div>
  <div id="group"></div>
  <div class="actions">
    <button class="primary" id="confirm">Keep selected, quarantine the rest</button>
    <button id="skip">Skip</button>
  </div>
  <div class="msg" id="msg"></div>
</main>
<script>
const $=s=>document.querySelector(s);
const CSRF=document.querySelector('meta[name="csrf"]').content;
function esc(s){return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));}
function fmtSize(n){const u=["B","KB","MB","GB","TB"];let i=0,x=n;while(x>=1024&&i<u.length-1){x/=1024;i++;}return (i?x.toFixed(1):x)+" "+u[i];}
function fmtDate(t){return t?new Date(t*1000).toISOString().slice(0,10):"—";}
let groups=[],idx=0,keepId=null;
async function load(){
  try{ groups=await (await fetch("/api/duplicates")).json(); }catch(e){ $("#msg").textContent="Load error: "+e; return; }
  idx=0; render();
}
function render(){
  if(idx>=groups.length){ $("#progress").textContent=""; $("#group").innerHTML="<p>All duplicate groups reviewed. 🎉</p>"; $("#confirm").style.display="none"; $("#skip").style.display="none"; return; }
  const g=groups[idx]; keepId=g.suggested_keep_id;
  $("#progress").textContent=`Group ${idx+1} of ${groups.length} · ${g.members.length} copies`;
  $("#group").innerHTML=`<div class="cards">${g.members.map(m=>card(m)).join("")}</div>`;
  for(const el of document.querySelectorAll(".card")) el.addEventListener("click",()=>{ keepId=Number(el.dataset.id); paint(); });
  paint();
}
function card(m){
  const img=(m.category==="photo"&&m.mounted)?`<img class="thumb" loading="lazy" src="/api/preview/${m.id}" onerror="this.replaceWith(Object.assign(document.createElement('div'),{className:'noimg',textContent:'no preview'}))">`:`<div class="noimg">${m.mounted?"no preview":"drive not connected"}</div>`;
  const arch=m.is_loose?"":`<div class="arch">inside archive — can't quarantine individually yet</div>`;
  return `<div class="card" data-id="${m.id}">${img}
    <div class="loc">${esc(m.location)}</div>
    <div class="kv"><b>${esc(m.volume_label||m.volume_id)}</b></div>
    <div class="kv">${fmtSize(m.size_bytes)} · created ${fmtDate(m.created_time)}</div>
    <div class="kv">status: ${esc(m.status)}</div>${arch}
    <div class="badge kept-badge" style="visibility:hidden">✓ keep this</div></div>`;
}
function paint(){
  for(const el of document.querySelectorAll(".card")){
    const on=Number(el.dataset.id)===keepId;
    el.classList.toggle("keep",on);
    el.querySelector(".kept-badge").style.visibility=on?"visible":"hidden";
  }
}
$("#confirm").addEventListener("click",async()=>{
  const g=groups[idx]; if(!g)return;
  const victims=g.members.filter(m=>m.id!==keepId&&m.is_loose).map(m=>m.id);
  if(victims.length===0){ $("#msg").textContent="Nothing to quarantine (the other copies are inside archives)."; return; }
  $("#confirm").disabled=true; $("#msg").textContent="Quarantining…";
  try{
    const res=await fetch("/api/quarantine",{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify({quarantine_ids:victims})});
    const j=await res.json();
    if(!res.ok){ $("#msg").textContent="Error: "+(j&&j!==null?JSON.stringify(j):res.status); }
    else{ let m=`Quarantined ${j.quarantined}, skipped ${j.skipped}.`; if(j.unmounted_volumes&&j.unmounted_volumes.length) m+=" Some drives not connected."; $("#msg").textContent=m; idx++; render(); }
  }catch(e){ $("#msg").textContent="Error: "+e; }
  $("#confirm").disabled=false;
});
$("#skip").addEventListener("click",()=>{ idx++; $("#msg").textContent=""; render(); });
load();
</script>
</body></html>
"##;
```

- [ ] **Step 4: Write the real-TCP e2e test**

Create `tests/review_flow.rs`:

```rust
use std::io::{Read, Write};
use std::net::TcpStream;

fn start(state_db: std::path::PathBuf, drive: std::path::PathBuf) -> std::net::SocketAddr {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let mut mounts = std::collections::HashMap::new();
            mounts.insert("vol-1".to_string(), drive);
            let state = cleanupstorages::web::AppState {
                catalog_path: state_db,
                mounts: cleanupstorages::mounts::MountResolver::Fixed(mounts),
                csrf_token: "TESTTOKEN".to_string(),
            };
            let app = cleanupstorages::web::build_router_with(state);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn req(addr: std::net::SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf
}

#[test]
fn review_duplicates_then_quarantine_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("c.db");
    let drive = tmp.path().join("driveA");
    std::fs::create_dir_all(drive.join("copy")).unwrap();
    std::fs::write(drive.join(".cleanupstorages_id"), "vol-1").unwrap();
    std::fs::write(drive.join("a.jpg"), b"DUP").unwrap();
    std::fs::write(drive.join("copy/a.jpg"), b"DUP").unwrap();
    {
        let cat = cleanupstorages::catalog::Catalog::open(&db).unwrap();
        cat.upsert_volume(&cleanupstorages::catalog::models::Volume {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = cleanupstorages::volume::VolumeIdentity {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into() };
        cleanupstorages::scanner::scan_volume(&cat, &drive, &ident, false, 100).unwrap();
    }
    std::mem::forget(tmp);

    let addr = start(db.clone(), drive.clone());

    // 1) fetch duplicates, grab a victim id (a copy that is NOT the suggested keep)
    let dups = req(addr, "GET /api/duplicates HTTP/1.0\r\nHost: x\r\n\r\n");
    assert!(dups.contains("200 OK"), "dups: {dups}");
    let body = dups.split("\r\n\r\n").nth(1).unwrap_or("");
    let json: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let g = &json[0];
    let keep = g["suggested_keep_id"].as_i64().unwrap();
    let victim = g["members"].as_array().unwrap().iter()
        .find(|m| m["id"].as_i64() != Some(keep)).unwrap()["id"].as_i64().unwrap();

    // 2) POST quarantine with the token
    let payload = format!("{{\"quarantine_ids\":[{victim}]}}");
    let post = format!("POST /api/quarantine HTTP/1.0\r\nHost: x\r\ncontent-type: application/json\r\nx-cleanup-token: TESTTOKEN\r\ncontent-length: {}\r\n\r\n{}", payload.len(), payload);
    let resp = req(addr, &post);
    assert!(resp.contains("200 OK"), "quarantine resp: {resp}");
    assert!(resp.contains("\"quarantined\":1"), "resp: {resp}");

    // 3) exactly one copy remains on disk, the other is in _ToDelete
    let remaining = [drive.join("a.jpg").exists(), drive.join("copy/a.jpg").exists()]
        .iter().filter(|x| **x).count();
    assert_eq!(remaining, 1);
    assert!(drive.join("_ToDelete").exists());
}
```

- [ ] **Step 5: Run tests + release build + manual smoke**

Run: `cargo test --lib web` then `cargo test --test review_flow` then `cargo test` then `cargo build --release`.
Then manual (background, non-blocking): `cargo run -- browse --no-open`, open the URL, click "Review duplicates →", confirm a group renders with thumbnails/metadata and the Keep/Quarantine buttons work; Ctrl+C. Report what you observed.

- [ ] **Step 6: Commit**

```bash
git add src/web.rs tests/review_flow.rs
git commit -m "feat(web): Tinder-style /review page with thumbnails, metadata diff, quarantine

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (§13 review GUI, §10 Cases 1–2):**
- Duplicate groups queued for review, one at a time (Tinder-style) → Task 6 page ✓
- Side-by-side compare with photo thumbnails + metadata → Tasks 4 (preview), 6 (cards) ✓
- Suggested "keep" = earliest created/most complete metadata, user-overridable → Task 3 + Task 6 (click to change keep) ✓
- `container_chain` shown for archived members; archived can't be quarantined individually (flagged) → Tasks 3, 6 ✓
- Keep-which / quarantine-which / skip; confirmation writes to `actions_log` → Task 5 (engine logs) + Task 6 ✓
- Reversible only over HTTP (quarantine, not purge) → global constraint + Task 5 ✓
- Works with offline drives (browse) and requires the drive to act/preview → Tasks 3–5 (mount checks) ✓

**Safety:** the only write endpoint is CSRF-guarded, uses the read-write catalog, and delegates to the Phase-2a engine (marker gate, disk-aware last-copy guard, rename-only, audit log). Purge is not exposed. XSS-safe rendering; self-contained page; localhost bind unchanged.

**Placeholder scan:** no TBD/TODO; every step is runnable as written. (`api_duplicates` captures the group's `content_hash` before consuming its rows.)

**Type consistency:** `AppState` (catalog_path, mounts, csrf_token) consistent across `build_router`/`build_router_with`/`serve`/tests; `MountResolver::{Live,Fixed}` used by handlers and tests; DTOs isolate serde from models; the e2e test constructs `AppState`/`build_router_with`/`MountResolver` with the same field names. `Category::Photo` gate matches `category == photo`.

**Deferred (logged for later):** full Case-3 "whole archive redundant" advisory; nested-archive previews; video thumbnails; surfacing "the survivor lives on an unconnected drive"; size/date UI filters on browse (from 1c).

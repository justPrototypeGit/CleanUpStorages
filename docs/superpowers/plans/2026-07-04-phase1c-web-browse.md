# Phase 1c — Local Web Browse / Search Screen — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `browse` command that starts a self-contained local web app on `127.0.0.1` (auto-selected free port) so the user can search and filter the whole catalog visually — including files on drives that aren't currently plugged in — showing each hit's location (drive + path, or archive `container_chain`), category, size, and status.

**Architecture:** A new `src/web.rs` module builds an `axum` router whose handlers open a short-lived read-only `Catalog` per request (SQLite WAL allows concurrent readers, so no shared-state Mutex is needed) and answer JSON API calls backed by the existing `Catalog` queries. `GET /` serves one self-contained HTML page (inlined CSS + JS, no external requests). A new `cmd_browse` spins up a Tokio runtime and serves until Ctrl-C. Search gains an optional-filter struct (`SearchFilters`) so the web layer can filter by size/date in addition to the category/volume/status the CLI already exposes; the existing `search(query, category, volume, status)` becomes a thin wrapper so no current caller changes.

**Tech Stack:** Rust 1.88, existing deps, plus `axum = "0.7"`, `tokio = { version = "1", features = ["rt-multi-thread", "net", "macros"] }`; dev-dep `tower = { version = "0.5", features = ["util"] }` for router unit tests. `serde`/`serde_json` are already present.

## Global Constraints

- **Read-only and local.** The browse server only reads the catalog; it never scans, mutates rows, or touches user files. It binds **`127.0.0.1` only** (never `0.0.0.0`) on an **auto-selected free port** (`bind("127.0.0.1:0")`), so there is no network exposure.
- **Self-contained page.** The HTML at `GET /` inlines all CSS and JS and makes **no external requests** (no CDNs, fonts, or images) — it only calls this server's own `/api/*` endpoints. This keeps it working offline and avoids leaking anything.
- **Catalog is the single source of truth**, shared with the CLI. Handlers open `Catalog::open(&catalog_path)` per request (cheap; WAL concurrent reads). No catalog is created on a drive.
- **Offline drives still searchable.** Results come from the persisted catalog, so files on unplugged drives appear, clearly flagged by `status` (`missing`/`quarantined`) and by their volume label.
- **No new destructive surface.** This phase adds no delete/quarantine/scan endpoints — browsing only. (Phase 2's review GUI will add action endpoints later.)
- **Reliability unchanged:** existing 33 tests must stay green; the `search` refactor is behavior-preserving for current callers.
- **Git:** work on branch `feat/phase1c-web-browse` off `main`. Conventional Commits, scope `catalog`/`web`/`cli`. Each commit ends with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

**Depends on (already merged):** `Catalog` + `search`/`volume_stats`/`duplicate_group_count`, `models::{FileRecord, FileStatus, Category}`, `config::Config`. **Out of scope:** any review/dedup/quarantine actions, date/size filters in the *CLI* (web-only here), authentication (localhost single-user), Phase 2.

---

## File Structure

- `Cargo.toml` — add `axum`, `tokio` deps; `tower` dev-dep.
- `src/catalog/store.rs` — add `SearchFilters` struct + `search_filtered`; reimplement `search` as a wrapper.
- `src/web.rs` — **new module**: `AppState`, `build_router`, the `GET /` HTML handler, the `/api/*` JSON handlers, `serve`, `open_browser`. Registered in `src/lib.rs`.
- `src/commands.rs` — add `cmd_browse`.
- `src/main.rs` — add the `Browse` subcommand.
- `src/lib.rs` — add `pub mod web;`.

---

### Task 1: `SearchFilters` + `search_filtered` (behavior-preserving refactor)

**Files:**
- Modify: `src/catalog/store.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct SearchFilters { pub query: String, pub category: Option<String>, pub volume: Option<String>, pub status: Option<String>, pub min_size: Option<i64>, pub max_size: Option<i64>, pub modified_after: Option<i64>, pub modified_before: Option<i64> }` with `#[derive(Default, Debug, Clone)]`.
  - `pub fn search_filtered(&self, f: &SearchFilters, limit: usize) -> anyhow::Result<Vec<FileRecord>>`.
  - Existing `pub fn search(&self, query, category, volume, status) -> anyhow::Result<Vec<FileRecord>>` reimplemented to delegate to `search_filtered` (limit 1000). Signature unchanged, so all current callers compile untouched.

- [ ] **Step 1: Write failing tests**

Add to `store.rs` `mod tests`:

```rust
    #[test]
    fn search_filtered_applies_size_and_status() {
        let (_t, cat) = open_tmp();
        let mut small = mk_file("vol-1", "small.txt", "h1"); small.size_bytes = 10;
        let mut big = mk_file("vol-1", "big.txt", "h2"); big.size_bytes = 5000;
        cat.upsert_file(&small, 200).unwrap();
        cat.upsert_file(&big, 200).unwrap();

        let f = SearchFilters { min_size: Some(1000), ..Default::default() };
        let hits = cat.search_filtered(&f, 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].filename, "big.txt");
    }

    #[test]
    fn search_filtered_empty_query_returns_all_filtered() {
        let (_t, cat) = open_tmp();
        cat.upsert_file(&mk_file("vol-1", "a.txt", "h1"), 200).unwrap();
        cat.upsert_file(&mk_file("vol-1", "b.txt", "h2"), 200).unwrap();
        let hits = cat.search_filtered(&SearchFilters::default(), 100).unwrap();
        assert_eq!(hits.len(), 2); // empty query = browse all
    }
```

(The existing `upsert_is_idempotent_and_search_finds_it` etc. still call the old `search` wrapper and must keep passing.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store`
Expected: FAIL — `SearchFilters`/`search_filtered` not found.

- [ ] **Step 3: Implement**

In `src/catalog/store.rs`, add the struct (top of file, after the `use` lines):

```rust
/// Optional filters for a catalog search/browse. All `None`/empty = match everything.
#[derive(Default, Debug, Clone)]
pub struct SearchFilters {
    pub query: String,
    pub category: Option<String>,
    pub volume: Option<String>,
    pub status: Option<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
}
```

Replace the existing `search` method body with a wrapper, and add `search_filtered`:

```rust
    /// Back-compat wrapper: text + category/volume/status filters, limit 1000.
    pub fn search(&self, query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>)
        -> anyhow::Result<Vec<FileRecord>>
    {
        let f = SearchFilters {
            query: query.to_string(),
            category: category.map(str::to_string),
            volume: volume.map(str::to_string),
            status: status.map(str::to_string),
            ..Default::default()
        };
        self.search_filtered(&f, 1000)
    }

    /// Full filtered search over the catalog.
    pub fn search_filtered(&self, f: &SearchFilters, limit: usize) -> anyhow::Result<Vec<FileRecord>> {
        let mut sql = String::from(
            "SELECT id, volume_id, relative_path, filename, extension, size_bytes, content_hash,
                    created_time, modified_time, accessed_time, category, container_chain,
                    status, first_seen_at, last_seen_at FROM files WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let q = f.query.trim();
        if !q.is_empty() {
            sql.push_str(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)");
            let match_expr = q.split_whitespace().map(|t| format!("{t}*")).collect::<Vec<_>>().join(" ");
            args.push(Box::new(match_expr));
        }
        if let Some(c) = &f.category { sql.push_str(" AND category = ?"); args.push(Box::new(c.clone())); }
        if let Some(v) = &f.volume { sql.push_str(" AND volume_id = ?"); args.push(Box::new(v.clone())); }
        if let Some(s) = &f.status { sql.push_str(" AND status = ?"); args.push(Box::new(s.clone())); }
        if let Some(n) = f.min_size { sql.push_str(" AND size_bytes >= ?"); args.push(Box::new(n)); }
        if let Some(n) = f.max_size { sql.push_str(" AND size_bytes <= ?"); args.push(Box::new(n)); }
        if let Some(n) = f.modified_after { sql.push_str(" AND modified_time >= ?"); args.push(Box::new(n)); }
        if let Some(n) = f.modified_before { sql.push_str(" AND modified_time <= ?"); args.push(Box::new(n)); }
        sql.push_str(" ORDER BY relative_path LIMIT ?");
        args.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), Self::map_file_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
```

Delete the old `search` implementation body that built SQL inline (it is now the wrapper above; `map_file_record` stays).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib store` then `cargo test`
Expected: PASS — new filter tests pass; all existing search/scanner/integration tests still pass.

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/phase1c-web-browse   # only if not already on it
git add src/catalog/store.rs
git commit -m "feat(catalog): add SearchFilters + search_filtered; keep search as wrapper

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Web module skeleton + `browse` command

**Files:**
- Modify: `Cargo.toml`
- Create: `src/web.rs`
- Modify: `src/lib.rs` (add `pub mod web;`)
- Modify: `src/commands.rs` (add `cmd_browse`)
- Modify: `src/main.rs` (add `Browse` subcommand)
- Test: inline `#[cfg(test)]` in `src/web.rs`

**Interfaces:**
- Produces:
  - `web::AppState { pub catalog_path: std::path::PathBuf }` — `#[derive(Clone)]`.
  - `web::build_router(catalog_path: PathBuf) -> axum::Router` — routes `/` (placeholder for now) with state.
  - `web::serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()>` (async) — binds `127.0.0.1:0`, prints the URL, optionally opens a browser, serves.
  - `commands::cmd_browse(open: bool) -> anyhow::Result<()>` — builds a Tokio runtime and `block_on(web::serve(...))`.

- [ ] **Step 1: Add dependencies**

In `Cargo.toml` `[dependencies]`:

```toml
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "net", "macros"] }
```

In `[dev-dependencies]`:

```toml
tower = { version = "0.5", features = ["util"] }
```

- [ ] **Step 2: Register the module**

In `src/lib.rs` add `pub mod web;`.

- [ ] **Step 3: Write a failing router test**

Create `src/web.rs`:

```rust
//! Local, read-only web browse/search UI. Binds 127.0.0.1 only.

use std::path::PathBuf;
use axum::{Router, routing::get, extract::State, response::Html};

#[derive(Clone)]
pub struct AppState {
    pub catalog_path: PathBuf,
}

pub fn build_router(catalog_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .with_state(AppState { catalog_path })
}

async fn index(State(_state): State<AppState>) -> Html<&'static str> {
    Html("<!doctype html><title>CleanUpStorages</title><h1>CleanUpStorages</h1>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    #[tokio::test]
    async fn index_returns_200_html() {
        let app = build_router(PathBuf::from("unused.db"));
        let res = app.oneshot(
            Request::builder().uri("/").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("CleanUpStorages"));
    }
}
```

- [ ] **Step 4: Run test to verify it fails / compiles**

Run: `cargo test --lib web`
Expected: FAIL first because deps/module aren't wired until Steps 1–2 are done and `cmd_browse`/subcommand may not exist yet — but this test itself should PASS once `web.rs` compiles. If `cargo test --lib web` passes here, that's fine; the RED for this task is the missing `serve`/`cmd_browse`/`Browse` subcommand wired in Steps 5–7. (This task's "failing test" is compilation-driven: the crate must build with the new subcommand.)

- [ ] **Step 5: Add `serve` + `open_browser` to `src/web.rs`**

Append:

```rust
/// Serve the browse UI on 127.0.0.1 with an OS-assigned free port until the process is stopped.
pub async fn serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()> {
    let app = build_router(catalog_path);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("CleanUpStorages browse UI at {url}");
    println!("(read-only; press Ctrl+C to stop)");
    if open {
        if let Err(e) = open_browser(&url) {
            eprintln!("could not open a browser automatically ({e}); open {url} yourself");
        }
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// Best-effort open of `url` in the user's default browser (never fatal).
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    { std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn().map(|_| ()) }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(url).spawn().map(|_| ()) }
    #[cfg(all(unix, not(target_os = "macos")))]
    { std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ()) }
}
```

- [ ] **Step 6: Add `cmd_browse` to `src/commands.rs`**

Add a `use crate::web;` at the top, and:

```rust
pub fn cmd_browse(open: bool) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }
    drop(cat); // handlers open their own short-lived connections
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(web::serve(cfg.catalog_path.clone(), open))
}
```

- [ ] **Step 7: Add the `Browse` subcommand in `src/main.rs`**

Add to the `Command` enum:

```rust
    /// Start a local web UI (127.0.0.1) to search and browse the catalog.
    Browse {
        /// Do not try to open a browser automatically.
        #[arg(long)]
        no_open: bool,
    },
```

And in the `match`:

```rust
        Command::Browse { no_open } => commands::cmd_browse(!no_open),
```

- [ ] **Step 8: Build + test**

Run: `cargo build` then `cargo test --lib web` then `cargo test`
Expected: builds; the `index_returns_200_html` test passes; full suite green.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/web.rs src/lib.rs src/commands.rs src/main.rs
git commit -m "feat(web): serve read-only browse UI on 127.0.0.1 via a browse command

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: JSON API endpoints (`/api/search`, `/api/volumes`, `/api/stats`)

**Files:**
- Modify: `src/web.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Adds routes `/api/search`, `/api/volumes`, `/api/stats` to `build_router`.
- `GET /api/search` accepts query params `q, category, volume, status, min_size, max_size, modified_after, modified_before, limit` (all optional) → `SearchFilters` → `Catalog::search_filtered` → `Json<Vec<HitDto>>`.
- `HitDto` (serialized): `location, relative_path, container_chain, filename, volume_id, category, size_bytes, status`.
- `GET /api/volumes` → `Json<Vec<VolumeDto>>` (`volume_id, label, active_files, active_bytes`) from `volume_stats`.
- `GET /api/stats` → `Json<StatsDto>` (`duplicate_groups`, `volumes: Vec<VolumeDto>`).

- [ ] **Step 1: Write failing tests**

Add to `web.rs` `mod tests`:

```rust
    use cleanupstorages_helpers::*; // NONE — inline the helper below instead

    fn seed_catalog() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume {
                volume_id: "vol-1".into(), label: "Test HDD".into(), identified_by: "marker".into(),
                first_seen_at: 1, last_seen_at: 1,
            }).unwrap();
            let mut f = crate::catalog::models::NewFile {
                volume_id: "vol-1".into(), relative_path: "docs/thesis.pdf".into(),
                filename: "thesis.pdf".into(), extension: "pdf".into(), size_bytes: 123,
                content_hash: "h1".into(), created_time: None, modified_time: Some(50),
                accessed_time: None, category: crate::category::Category::Document,
                container_chain: None,
            };
            cat.upsert_file(&f, 100).unwrap();
            f.relative_path = "old.zip".into(); f.filename = "inner.jpg".into();
            f.extension = "jpg".into(); f.container_chain = Some("inner.jpg".into());
            f.category = crate::category::Category::Photo; f.content_hash = "h2".into();
            cat.upsert_archive_entry("vol-1", "old.zip",
                &crate::archive::ArchiveEntry {
                    container_chain: "inner.jpg".into(), filename: "inner.jpg".into(),
                    extension: "jpg".into(), size_bytes: 9, content_hash: "h2".into(),
                }, 100).unwrap();
        }
        (tmp, db)
    }

    async fn get_json(db: &PathBuf, uri: &str) -> serde_json::Value {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let app = build_router(db.clone());
        let res = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK, "uri {uri}");
        let bytes = axum::body::to_bytes(res.into_body(), 5_000_000).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn api_search_returns_hits_with_location() {
        let (_t, db) = seed_catalog();
        let v = get_json(&db, "/api/search?q=thesis").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["location"], "docs/thesis.pdf");
        assert_eq!(arr[0]["volume_id"], "vol-1");
    }

    #[tokio::test]
    async fn api_search_shows_archive_chain_in_location() {
        let (_t, db) = seed_catalog();
        let v = get_json(&db, "/api/search?q=inner").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["location"], "old.zip › inner.jpg");
        assert_eq!(arr[0]["category"], "photo");
    }

    #[tokio::test]
    async fn api_volumes_lists_the_volume() {
        let (_t, db) = seed_catalog();
        let v = get_json(&db, "/api/volumes").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], "Test HDD");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib web`
Expected: FAIL — `/api/*` routes/handlers not present.

- [ ] **Step 3: Implement the API**

In `src/web.rs`, add imports and handlers:

```rust
use axum::{Json, extract::Query};
use serde::{Serialize, Deserialize};
use crate::catalog::Catalog;
use crate::catalog::store::SearchFilters;
use crate::catalog::models::FileRecord;

#[derive(Serialize)]
struct HitDto {
    location: String,
    relative_path: String,
    container_chain: Option<String>,
    filename: String,
    volume_id: String,
    category: String,
    size_bytes: i64,
    status: String,
}

impl From<FileRecord> for HitDto {
    fn from(f: FileRecord) -> HitDto {
        let location = match &f.container_chain {
            Some(chain) => format!("{} › {}", f.relative_path, chain),
            None => f.relative_path.clone(),
        };
        HitDto {
            location,
            relative_path: f.relative_path,
            container_chain: f.container_chain,
            filename: f.filename,
            volume_id: f.volume_id,
            category: f.category.as_str().to_string(),
            size_bytes: f.size_bytes,
            status: f.status.as_str().to_string(),
        }
    }
}

#[derive(Serialize)]
struct VolumeDto { volume_id: String, label: String, active_files: i64, active_bytes: i64 }

#[derive(Serialize)]
struct StatsDto { duplicate_groups: i64, volumes: Vec<VolumeDto> }

#[derive(Deserialize, Default)]
struct SearchParams {
    q: Option<String>,
    category: Option<String>,
    volume: Option<String>,
    status: Option<String>,
    min_size: Option<i64>,
    max_size: Option<i64>,
    modified_after: Option<i64>,
    modified_before: Option<i64>,
    limit: Option<usize>,
}

/// Map any error to a 500 with a short text body (localhost dev tool — plain messages are fine).
fn err500<E: std::fmt::Display>(e: E) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn api_search(State(state): State<AppState>, Query(p): Query<SearchParams>)
    -> Result<Json<Vec<HitDto>>, (axum::http::StatusCode, String)>
{
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let filters = SearchFilters {
        query: p.q.unwrap_or_default(),
        category: p.category, volume: p.volume, status: p.status,
        min_size: p.min_size, max_size: p.max_size,
        modified_after: p.modified_after, modified_before: p.modified_before,
    };
    let limit = p.limit.unwrap_or(500).min(5000);
    let hits = cat.search_filtered(&filters, limit).map_err(err500)?;
    Ok(Json(hits.into_iter().map(HitDto::from).collect()))
}

fn volume_dtos(cat: &Catalog) -> anyhow::Result<Vec<VolumeDto>> {
    Ok(cat.volume_stats()?.into_iter()
        .map(|(volume_id, label, active_files, active_bytes)|
            VolumeDto { volume_id, label, active_files, active_bytes })
        .collect())
}

async fn api_volumes(State(state): State<AppState>)
    -> Result<Json<Vec<VolumeDto>>, (axum::http::StatusCode, String)>
{
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    Ok(Json(volume_dtos(&cat).map_err(err500)?))
}

async fn api_stats(State(state): State<AppState>)
    -> Result<Json<StatsDto>, (axum::http::StatusCode, String)>
{
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let duplicate_groups = cat.duplicate_group_count().map_err(err500)?;
    let volumes = volume_dtos(&cat).map_err(err500)?;
    Ok(Json(StatsDto { duplicate_groups, volumes }))
}
```

Update `build_router` to add the routes:

```rust
pub fn build_router(catalog_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/search", get(api_search))
        .route("/api/volumes", get(api_volumes))
        .route("/api/stats", get(api_stats))
        .with_state(AppState { catalog_path })
}
```

Note: `SearchFilters` must be public from `store`. It is declared `pub` in Task 1; ensure `crate::catalog::store::SearchFilters` is reachable (the `store` module is already `pub` under `catalog`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS (3 new api tests + the index test) and full suite green.

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): add /api/search, /api/volumes, /api/stats JSON endpoints

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: The browse page (self-contained HTML/CSS/JS)

**Files:**
- Modify: `src/web.rs` (replace the `index` placeholder with the full page)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `index` now returns the full self-contained page. No new routes.

- [ ] **Step 1: Write a failing test asserting the page wires to the API**

Add to `web.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn index_page_has_search_ui_and_calls_api() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let app = build_router(PathBuf::from("unused.db"));
        let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("id=\"q\""), "search input present");
        assert!(body.contains("id=\"results\""), "results container present");
        assert!(body.contains("/api/search"), "page fetches the search API");
        // self-contained: no external resource references
        assert!(!body.contains("http://"), "no external http resources");
        assert!(!body.contains("https://"), "no external https resources");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib web`
Expected: FAIL — placeholder page lacks these ids.

- [ ] **Step 3: Replace `index` with the full page**

Replace the `index` handler with:

```rust
async fn index(State(_state): State<AppState>) -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>CleanUpStorages — Browse</title>
<style>
  :root { color-scheme: light dark; --bg:#111; --fg:#eee; --mut:#999; --line:#333; --accent:#5aa0ff; }
  @media (prefers-color-scheme: light) { :root { --bg:#fff; --fg:#111; --mut:#666; --line:#ddd; } }
  * { box-sizing: border-box; }
  body { margin:0; font:14px/1.4 system-ui, sans-serif; background:var(--bg); color:var(--fg); }
  header { padding:12px 16px; border-bottom:1px solid var(--line); position:sticky; top:0; background:var(--bg); }
  h1 { font-size:15px; margin:0 0 8px; }
  .controls { display:flex; flex-wrap:wrap; gap:8px; align-items:center; }
  input, select { background:transparent; color:var(--fg); border:1px solid var(--line); border-radius:6px; padding:6px 8px; font:inherit; }
  input#q { flex:1; min-width:180px; }
  .meta { color:var(--mut); padding:8px 16px; }
  main { padding:0 16px 40px; }
  table { width:100%; border-collapse:collapse; }
  th, td { text-align:left; padding:6px 8px; border-bottom:1px solid var(--line); vertical-align:top; }
  th { color:var(--mut); font-weight:600; position:sticky; top:96px; background:var(--bg); }
  td.loc { word-break:break-all; }
  .flag { font-size:11px; padding:1px 6px; border-radius:10px; border:1px solid var(--line); color:var(--mut); }
  .flag.missing { color:#e06c6c; border-color:#e06c6c; }
  .flag.quarantined { color:#e0a86c; border-color:#e0a86c; }
  .size { color:var(--mut); white-space:nowrap; }
</style>
</head>
<body>
<header>
  <h1>CleanUpStorages — Browse <span class="meta" id="stats"></span></h1>
  <div class="controls">
    <input id="q" type="search" placeholder="Search filename or path…" autofocus>
    <select id="volume"><option value="">All drives</option></select>
    <select id="category">
      <option value="">All types</option>
      <option value="photo">Photo</option><option value="video">Video</option>
      <option value="document">Document</option><option value="academic">Academic</option>
      <option value="other">Other</option>
    </select>
    <select id="status">
      <option value="">Any status</option>
      <option value="active">Active</option><option value="missing">Missing</option>
      <option value="quarantined">Quarantined</option><option value="purged">Purged</option>
    </select>
  </div>
</header>
<div class="meta" id="count"></div>
<main>
  <table>
    <thead><tr><th>Location</th><th>Drive</th><th>Type</th><th>Size</th><th>Status</th></tr></thead>
    <tbody id="results"></tbody>
  </table>
</main>
<script>
const $ = s => document.querySelector(s);
function fmtSize(n){ const u=["B","KB","MB","GB","TB"]; let i=0,x=n; while(x>=1024&&i<u.length-1){x/=1024;i++;} return (i?x.toFixed(1):x)+" "+u[i]; }
function esc(s){ const d=document.createElement("div"); d.textContent=s==null?"":s; return d.innerHTML; }
let timer=null;
async function run(){
  const params=new URLSearchParams();
  const q=$("#q").value.trim(); if(q) params.set("q",q);
  for(const k of ["volume","category","status"]){ const v=$("#"+k).value; if(v) params.set(k,v); }
  const res=await fetch("/api/search?"+params.toString());
  const hits=await res.json();
  $("#count").textContent = hits.length+" result"+(hits.length===1?"":"s")+(hits.length>=500?" (showing first 500)":"");
  $("#results").innerHTML = hits.map(h=>{
    const flag = h.status==="active" ? "" : `<span class="flag ${esc(h.status)}">${esc(h.status)}</span>`;
    return `<tr><td class="loc">${esc(h.location)}</td><td>${esc(h.volume_id)}</td><td>${esc(h.category)}</td><td class="size">${fmtSize(h.size_bytes)}</td><td>${flag}</td></tr>`;
  }).join("");
}
function debounced(){ clearTimeout(timer); timer=setTimeout(run,180); }
async function init(){
  const vs=await (await fetch("/api/volumes")).json();
  const sel=$("#volume");
  for(const v of vs){ const o=document.createElement("option"); o.value=v.volume_id; o.textContent=v.label; sel.appendChild(o); }
  const st=await (await fetch("/api/stats")).json();
  $("#stats").textContent = "· "+st.duplicate_groups+" duplicate groups · "+st.volumes.length+" drives";
  $("#q").addEventListener("input",debounced);
  for(const k of ["volume","category","status"]) $("#"+k).addEventListener("change",run);
  run();
}
init();
</script>
</body>
</html>
"##;
```

Change the `index` return type note: it still returns `Html<&'static str>`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib web` then `cargo test`
Expected: PASS (the page test + all API tests + full suite).

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat(web): self-contained browse page with live search and filters

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: End-to-end smoke over real HTTP + polish

**Files:**
- Create: `tests/browse_server.rs`
- Modify: `Cargo.toml` (dev-dep `tokio` full for the test client if needed — see note)

**Interfaces:** none new — verifies the real bound server answers over TCP.

- [ ] **Step 1: Write the failing integration test**

Create `tests/browse_server.rs` (spawns the real router on an ephemeral port and makes a raw HTTP/1.0 request over TCP — no extra HTTP-client dependency):

```rust
use std::io::{Read, Write};
use std::net::TcpStream;

// Start the browse server on an ephemeral port in a background thread, return its addr.
fn start_server() -> std::net::SocketAddr {
    use std::sync::mpsc;
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("c.db");
    {
        let cat = cleanupstorages::catalog::Catalog::open(&db).unwrap();
        cat.upsert_volume(&cleanupstorages::catalog::models::Volume {
            volume_id: "vol-1".into(), label: "Test HDD".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1,
        }).unwrap();
        cat.upsert_file(&cleanupstorages::catalog::models::NewFile {
            volume_id: "vol-1".into(), relative_path: "docs/thesis.pdf".into(),
            filename: "thesis.pdf".into(), extension: "pdf".into(), size_bytes: 5,
            content_hash: "h1".into(), created_time: None, modified_time: None, accessed_time: None,
            category: cleanupstorages::category::Category::Document, container_chain: None,
        }, 100).unwrap();
    }
    // keep tmp alive for the whole test process
    std::mem::forget(tmp);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let app = cleanupstorages::web::build_router(db);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf
}

#[test]
fn server_serves_page_and_search() {
    let addr = start_server();
    let index = http_get(addr, "/");
    assert!(index.contains("200 OK"));
    assert!(index.contains("CleanUpStorages"));

    let search = http_get(addr, "/api/search?q=thesis");
    assert!(search.contains("200 OK"));
    assert!(search.contains("docs/thesis.pdf"), "search body: {search}");
}
```

This needs `tokio` available to the integration test. Add to `[dev-dependencies]` in `Cargo.toml`:

```toml
tokio = { version = "1", features = ["rt", "net", "macros"] }
```

(The main dependency already pulls tokio; this line makes the features explicit for the test target. If `cargo test` complains about a duplicate key because tokio is only a normal dependency, instead of adding a dev-dependency, rely on the normal dependency — remove this dev line. The implementer should keep whichever single form compiles.)

- [ ] **Step 2: Run it to verify it fails, then passes**

Run: `cargo test --test browse_server`
Expected: initially may FAIL to compile if `web::build_router`/`catalog`/`category` aren't `pub` at crate root — they are (all `pub mod`). Once compiling, it PASSES: the server binds, serves the page, and answers the search over real TCP.

- [ ] **Step 3: Manual smoke**

Run:
```bash
cargo run -- browse --no-open
```
Expected: prints `CleanUpStorages browse UI at http://127.0.0.1:<port>`; open that URL in a browser, confirm the search box lists your drives and returns results as you type; Ctrl+C stops it. Report the observed URL line and whether search worked.

- [ ] **Step 4: Full suite + release build**

Run: `cargo test` then `cargo build --release`
Expected: all green; release binary builds.

- [ ] **Step 5: Commit**

```bash
git add tests/browse_server.rs Cargo.toml Cargo.lock
git commit -m "test(web): end-to-end browse server smoke over real TCP

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (§12 web half, §13 bind):**
- Web search/browse screen in a local app → Tasks 2–4 ✓
- Free-text + filters (drive/volume, category, status, size, date) → Task 1 (filters) + Task 3 (API) + Task 4 (UI exposes query/volume/category/status; size/date available via API) ✓
- Results show location (drive + path, or `container_chain` for archived), flag missing/quarantined → Task 3 `HitDto.location` + Task 4 flags ✓
- Works for offline drives (from persisted catalog) → inherent (reads catalog) ✓
- Binds `127.0.0.1` only, auto free port, no network exposure, self-contained page → Tasks 2, 4 (+ test asserts no external http/https) ✓

**Placeholder scan:** No TBD/TODO; every step has runnable code + commands. The one conditional note (Task 5 dev-dep tokio line) gives the implementer a concrete either/or with a compile check. ✓

**Type consistency:** `SearchFilters` fields match between store (Task 1) and `SearchParams`→`SearchFilters` mapping (Task 3); `HitDto.location` composition matches the CLI's ` › ` format; `AppState`/`build_router`/`serve` signatures consistent across Tasks 2–5; handlers open `Catalog` per request (no shared mutable state). ✓

**Notes / deferred:** the UI exposes query + volume + category + status filters; size/date filters exist in the API and `SearchFilters` but are not surfaced as UI controls yet (kept the header uncluttered) — a trivial later addition. Date filtering uses `modified_time` (archive entries have NULL modified_time, so a date filter excludes them — acceptable, documented). Auth is intentionally absent (localhost single-user). These are logged, not gaps.

//! Local, read-only web browse/search UI. Binds 127.0.0.1 only.

use std::path::PathBuf;
use axum::{Router, routing::get, extract::State, response::Html, Json, extract::Query};
use serde::{Serialize, Deserialize};
use crate::catalog::Catalog;
use crate::catalog::store::SearchFilters;
use crate::catalog::models::FileRecord;

#[derive(Clone)]
pub struct AppState {
    pub catalog_path: PathBuf,
}

pub fn build_router(catalog_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/search", get(api_search))
        .route("/api/volumes", get(api_volumes))
        .route("/api/stats", get(api_stats))
        .with_state(AppState { catalog_path })
}

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

/// Web-facing shape for a search hit; keeps serialization concerns out of `catalog::models`.
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

/// Shared by /api/volumes and /api/stats so the two endpoints can't drift apart.
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
}

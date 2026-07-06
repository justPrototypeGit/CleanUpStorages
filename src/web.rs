//! Local, read-only web browse/search UI. Binds 127.0.0.1 only.

use std::path::{Path, PathBuf};
use axum::{Router, routing::{get, post}, extract::State, response::Html, Json, extract::Query};
use axum::http::HeaderMap;
use axum::extract::Path as AxPath;
use axum::response::{IntoResponse, Response};
use axum::http::{StatusCode, header};
use serde::{Serialize, Deserialize};
use crate::catalog::Catalog;
use crate::catalog::store::SearchFilters;
use crate::catalog::models::FileRecord;

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
        .route("/api/duplicates", get(api_duplicates))
        .route("/api/preview/:id", get(api_preview))
        .route("/api/quarantine", post(api_quarantine))
        .route("/api/repack", post(api_repack))
        .route("/review", get(review))
        .with_state(state)
}

async fn index(State(_state): State<AppState>) -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn review(State(state): State<AppState>) -> Html<String> {
    Html(REVIEW_HTML.replace("{{CSRF}}", &state.csrf_token))
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
  <h1>CleanUpStorages — Browse <span class="meta" id="stats"></span> <a href="/review" style="font-size:12px">Review duplicates →</a></h1>
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
function esc(s){ return (s==null?"":String(s)).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c])); }
let timer=null;
async function run(){
  try {
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
  } catch(e) {
    $("#count").textContent = "Search error: "+e;
  }
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
  for(const b of document.querySelectorAll(".repack")) b.addEventListener("click", async (ev)=>{
    ev.stopPropagation();
    const id=Number(b.dataset.id);
    b.disabled=true; $("#msg").textContent="Repacking archive…";
    try{
      const res=await fetch("/api/repack",{method:"POST",headers:{"content-type":"application/json","x-cleanup-token":CSRF},body:JSON.stringify({entry_id:id})});
      if(!res.ok){ $("#msg").textContent="Repack error: "+(await res.text()); b.disabled=false; }
      else{ const j=await res.json(); $("#msg").textContent=`Removed '${j.removed_entry}' from its archive (${j.retained_entries} kept). Original saved in _ToDelete.`; idx++; render(); }
    }catch(e){ $("#msg").textContent="Repack error: "+e; b.disabled=false; }
  });
}
function card(m){
  const img=(m.category==="photo"&&m.mounted)?`<img class="thumb" loading="lazy" src="/api/preview/${m.id}" onerror="this.replaceWith(Object.assign(document.createElement('div'),{className:'noimg',textContent:'no preview'}))">`:`<div class="noimg">${m.mounted?"no preview":"drive not connected"}</div>`;
  const arch = m.is_loose ? "" :
    (m.id===keepId ? `<div class="arch">inside archive</div>`
     : m.mounted ? `<button class="danger repack" data-id="${m.id}">Remove from archive</button>`
                 : `<div class="arch">drive not connected</div>`);
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
    if(!res.ok){ $("#msg").textContent="Error: "+(await res.text()); }
    else{ const j=await res.json(); let m=`Quarantined ${j.quarantined}, skipped ${j.skipped}.`; if(j.unmounted_volumes&&j.unmounted_volumes.length) m+=" Some drives not connected."; if(j.errors&&j.errors.length) m+=" Errors: "+j.errors.join("; "); $("#msg").textContent=m; idx++; render(); }
  }catch(e){ $("#msg").textContent="Error: "+e; }
  $("#confirm").disabled=false;
});
$("#skip").addEventListener("click",()=>{ idx++; $("#msg").textContent=""; render(); });
load();
</script>
</body></html>
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
        let location = f.display_location();
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
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
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
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    Ok(Json(volume_dtos(&cat).map_err(err500)?))
}

async fn api_stats(State(state): State<AppState>)
    -> Result<Json<StatsDto>, (axum::http::StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let duplicate_groups = cat.duplicate_group_count().map_err(err500)?;
    let volumes = volume_dtos(&cat).map_err(err500)?;
    Ok(Json(StatsDto { duplicate_groups, volumes }))
}

#[derive(Serialize)]
struct MemberDto {
    id: i64, location: String, filename: String, volume_id: String, volume_label: String,
    size_bytes: i64, category: String, created_time: Option<i64>, modified_time: Option<i64>,
    status: String, is_loose: bool, mounted: bool,
}

#[derive(Serialize)]
struct GroupDto { hash: String, suggested_keep_id: i64, members: Vec<MemberDto> }

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
    let mounts = state.mounts.snapshot();
    let mut out = Vec::new();
    for group in groups {
        // Capture the shared content hash before consuming the group's rows.
        let hash = group.first().map(|f| f.content_hash.clone()).unwrap_or_default();
        let keep = suggested_keep(&group);
        let members = group.into_iter().map(|f| {
            let mounted = mounts.contains_key(&f.volume_id);
            MemberDto {
                id: f.id, location: f.display_location(), filename: f.filename.clone(),
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

/// Photo thumbnail for a file that is: a photo, mounted, and either loose or a top-level
/// archive entry (no nested-archive chain). Anything else — or a decode failure — is a 404,
/// never a panic.
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

#[derive(Deserialize)]
struct QuarantineReq { quarantine_ids: Vec<i64> }

#[derive(Serialize, Default)]
struct QuarantineResultDto {
    quarantined: usize,
    skipped: usize,
    unmounted_volumes: Vec<String>,
    errors: Vec<String>,
}

/// The web app's first write endpoint. All destructive safety (marker check, disk-aware
/// last-copy guard, rename-only) lives in `quarantine::quarantine_files`; this handler is just
/// the CSRF gate plus grouping requested ids by volume so the engine can be called per-mount.
async fn api_quarantine(State(state): State<AppState>, headers: HeaderMap, body: Json<QuarantineReq>)
    -> Result<Json<QuarantineResultDto>, (StatusCode, String)>
{
    // CSRF: require the per-run token (a cross-site page can't read it). Checked first, before
    // any catalog access, so a bad/missing token does nothing.
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;

    // Group requested ids by their volume; ids that don't resolve to a file are counted skipped.
    let mut by_volume: std::collections::HashMap<String, Vec<i64>> = std::collections::HashMap::new();
    let mut missing = 0usize;
    for id in &body.quarantine_ids {
        match cat.get_file(*id).map_err(err500)? {
            Some(rec) => by_volume.entry(rec.volume_id).or_default().push(*id),
            None => missing += 1,
        }
    }

    let mut result = QuarantineResultDto::default();
    result.skipped += missing;
    let mounts = state.mounts.snapshot();
    for (volume_id, ids) in by_volume {
        if let Some(mount) = mounts.get(&volume_id) {
            match crate::quarantine::quarantine_files(&cat, mount, &volume_id, &ids, now) {
                Ok(out) => {
                    result.quarantined += out.quarantined;
                    result.skipped += out.skipped;
                }
                Err(e) => {
                    result.skipped += ids.len();
                    result.errors.push(format!("{volume_id}: {e}"));
                }
            }
        } else {
            result.skipped += ids.len();
            result.unmounted_volumes.push(volume_id);
        }
    }

    // Snapshot the catalog this request actually mutated (best-effort; a snapshot failure
    // shouldn't fail the request).
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
struct RepackReq { entry_id: i64 }

#[derive(Serialize)]
struct RepackResultDto { removed_entry: String, retained_entries: usize }

/// Remove one entry from its containing archive (Case 4). All destructive safety (marker gate,
/// disk-aware survivor guard, verify-before-swap, two recovery nets) lives in `repack::repack_entry`;
/// this handler is just the CSRF gate plus resolving the entry's volume to a mounted drive.
async fn api_repack(State(state): State<AppState>, headers: HeaderMap, body: Json<RepackReq>)
    -> Result<Json<RepackResultDto>, (StatusCode, String)>
{
    // CSRF: checked first, before any catalog access, so a bad/missing token does nothing.
    let ok = headers.get("x-cleanup-token").and_then(|v| v.to_str().ok()) == Some(state.csrf_token.as_str());
    if !ok { return Err((StatusCode::FORBIDDEN, "missing or bad token".into())); }

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let rec = cat.get_file(body.entry_id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "no such entry".to_string()))?;
    let mount = state.mounts.resolve(&rec.volume_id)
        .ok_or((StatusCode::CONFLICT, "drive not connected".to_string()))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64;
    let out = crate::repack::repack_entry(&cat, &mount, &rec.volume_id, body.entry_id, now)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Snapshot the catalog this request actually mutated (best-effort; a snapshot failure
    // shouldn't fail the request).
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
    Ok(Json(RepackResultDto { removed_entry: out.removed_entry, retained_entries: out.retained_entries }))
}

/// Serve the browse UI on 127.0.0.1 with an OS-assigned free port until the process is stopped.
pub async fn serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()> {
    let app = build_router_with(AppState::new_live(catalog_path));
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

    #[test]
    fn app_state_new_live_has_token_and_live_mounts() {
        let s = AppState::new_live(PathBuf::from("x.db"));
        assert!(!s.csrf_token.is_empty());
        assert!(matches!(s.mounts, crate::mounts::MountResolver::Live));
    }

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
    async fn api_stats_returns_shape() {
        let (_t, db) = seed_catalog();
        let v = get_json(&db, "/api/stats").await;
        assert!(v["duplicate_groups"].is_number());
        assert_eq!(v["volumes"][0]["label"], "Test HDD");
    }

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

    #[tokio::test]
    async fn preview_returns_404_for_undecodable_bytes() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, db, state) = seed_dupes();
        // write garbage bytes (not an image) at the loose path on the fake drive
        let drive = match &state.mounts { crate::mounts::MountResolver::Fixed(m) => m["vol-1"].clone(), _ => unreachable!() };
        std::fs::write(drive.join("a.jpg"), b"this is not an image").unwrap();
        // find a.jpg's id
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let id = cat.active_file_id("vol-1", "a.jpg").unwrap().unwrap();

        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri(format!("/api/preview/{id}"))
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn preview_returns_404_for_non_photo() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, db, state) = seed_dupes();
        // insert a DOCUMENT-category loose file into the catalog
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            let doc = crate::catalog::models::NewFile {
                volume_id: "vol-1".into(), relative_path: "notes.txt".into(),
                filename: "notes.txt".into(), extension: "txt".into(), size_bytes: 100,
                content_hash: "doc_hash".into(), created_time: Some(3000),
                modified_time: Some(3000), accessed_time: None,
                category: crate::category::Category::Document, container_chain: None };
            cat.upsert_file(&doc, 100).unwrap();
        }
        // find notes.txt's id
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let id = cat.active_file_id("vol-1", "notes.txt").unwrap().unwrap();

        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri(format!("/api/preview/{id}"))
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    }

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

    #[tokio::test]
    async fn quarantine_reports_unmounted_volume_without_error() {
        let (_t, db, state) = seed_dupes();
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume { volume_id: "vol-2".into(),
                label: "Offline".into(), identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
            cat.upsert_file(&crate::catalog::models::NewFile {
                volume_id: "vol-2".into(), relative_path: "x.jpg".into(), filename: "x.jpg".into(),
                extension: "jpg".into(), size_bytes: 5, content_hash: "Z".into(), created_time: None,
                modified_time: None, accessed_time: None, category: crate::category::Category::Photo,
                container_chain: None }, 100).unwrap();
        }
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let id = cat.active_file_id("vol-2", "x.jpg").unwrap().unwrap();
        drop(cat);
        let (status, json) = post_json(state, "/api/quarantine", Some("T"),
            serde_json::json!({"quarantine_ids":[id]})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(json["skipped"], 1);
        assert!(json["unmounted_volumes"].as_array().unwrap().iter().any(|v| v=="vol-2"));
    }

    #[tokio::test]
    async fn repack_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/repack", None,
            serde_json::json!({"entry_id": 1})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn repack_removes_entry_over_http() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("driveA");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join(".cleanupstorages_id"), "vol-1").unwrap();
        {
            use std::io::Write;
            let f = std::fs::File::create(drive.join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (n, b) in [("keep.txt", &b"KEEP"[..]), ("dup.txt", &b"SHARED"[..])] {
                zw.start_file(n, opts).unwrap(); zw.write_all(b).unwrap();
            }
            zw.finish().unwrap();
        }
        std::fs::write(drive.join("loose_dup.txt"), b"SHARED").unwrap();
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume { volume_id: "vol-1".into(),
                label: "D".into(), identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
            let ident = crate::volume::VolumeIdentity { volume_id: "vol-1".into(), label: "D".into(),
                identified_by: "marker".into() };
            crate::scanner::scan_volume(&cat, &drive, &ident, false, 100).unwrap();
        }
        let mut mounts = std::collections::HashMap::new();
        mounts.insert("vol-1".to_string(), drive.clone());
        let state = AppState { catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into() };

        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let entry_id = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap().id;
        drop(cat);

        let (status, json) = post_json(state, "/api/repack", Some("T"),
            serde_json::json!({"entry_id": entry_id})).await;
        assert_eq!(status, axum::http::StatusCode::OK, "body {json}");
        assert_eq!(json["removed_entry"], "dup.txt");

        let f = std::fs::File::open(drive.join("bundle.zip")).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        assert!(z.by_name("keep.txt").is_ok());
        assert!(z.by_name("dup.txt").is_err());
    }

    #[tokio::test]
    async fn repack_returns_409_when_drive_not_connected() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let entry_id;
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume { volume_id: "vol-1".into(),
                label: "D".into(), identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
            cat.upsert_archive_entry("vol-1", "bundle.zip",
                &crate::archive::ArchiveEntry { container_chain: "inner.txt".into(),
                    filename: "inner.txt".into(), extension: "txt".into(), size_bytes: 6,
                    content_hash: "H".into() }, 100).unwrap();
            entry_id = cat.archive_entries("vol-1", "bundle.zip").unwrap()
                .into_iter().find(|e| e.container_chain.as_deref() == Some("inner.txt")).unwrap().id;
        }
        // No volumes mounted at all.
        let state = AppState { catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(std::collections::HashMap::new()),
            csrf_token: "T".into() };

        let (status, _) = post_json(state, "/api/repack", Some("T"),
            serde_json::json!({"entry_id": entry_id})).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn repack_returns_400_when_no_survivor() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("driveA");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join(".cleanupstorages_id"), "vol-1").unwrap();
        {
            use std::io::Write;
            let f = std::fs::File::create(drive.join("bundle.zip")).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (n, b) in [("keep.txt", &b"KEEP"[..]), ("dup.txt", &b"SHARED"[..])] {
                zw.start_file(n, opts).unwrap(); zw.write_all(b).unwrap();
            }
            zw.finish().unwrap();
        }
        // NOTE: no loose survivor copy written this time — dup.txt inside the zip is the only copy.
        {
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume { volume_id: "vol-1".into(),
                label: "D".into(), identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
            let ident = crate::volume::VolumeIdentity { volume_id: "vol-1".into(), label: "D".into(),
                identified_by: "marker".into() };
            crate::scanner::scan_volume(&cat, &drive, &ident, false, 100).unwrap();
        }
        let mut mounts = std::collections::HashMap::new();
        mounts.insert("vol-1".to_string(), drive.clone());
        let state = AppState { catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into() };

        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let entry_id = cat.archive_entries("vol-1", "bundle.zip").unwrap()
            .into_iter().find(|e| e.container_chain.as_deref() == Some("dup.txt")).unwrap().id;
        drop(cat);

        let (status, _json) = post_json(state, "/api/repack", Some("T"),
            serde_json::json!({"entry_id": entry_id})).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);

        // Archive untouched: dup.txt is still inside.
        let f = std::fs::File::open(drive.join("bundle.zip")).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        assert!(z.by_name("dup.txt").is_ok());
    }

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

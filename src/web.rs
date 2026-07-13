//! Local, read-only web browse/search UI. Binds 127.0.0.1 only.

use std::path::PathBuf;
use axum::{Router, routing::{get, post}, extract::State, response::Html, Json, extract::Query};
use axum::http::HeaderMap;
use axum::extract::Path as AxPath;
use axum::response::{IntoResponse, Response};
use axum::http::{StatusCode, header};
use serde::{Serialize, Deserialize};
use crate::catalog::Catalog;
use crate::catalog::store::SearchFilters;
use crate::catalog::models::FileRecord;
use tower_http::trace::{TraceLayer, DefaultOnRequest, DefaultOnResponse};
use tower_http::LatencyUnit;

#[derive(Clone)]
pub struct AppState {
    pub catalog_path: PathBuf,
    pub mounts: crate::mounts::MountResolver,
    pub csrf_token: String,
    pub scan_queue: std::sync::Arc<crate::scan_queue::ScanQueue>,
}

impl AppState {
    /// Production state: live mount detection and a fresh random CSRF token.
    pub fn new_live(catalog_path: PathBuf) -> AppState {
        AppState {
            mounts: crate::mounts::MountResolver::Live { catalog_path: catalog_path.clone() },
            csrf_token: uuid::Uuid::new_v4().to_string(),
            scan_queue: crate::scan_queue::ScanQueue::new(catalog_path.clone()),
            catalog_path,
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
        .route("/", get(overview))
        .route("/browse", get(browse))
        .route("/api/search", get(api_search))
        .route("/api/volumes", get(api_volumes))
        .route("/api/stats", get(api_stats))
        .route("/api/activity", get(api_activity))
        .route("/api/drives", get(api_drives))
        .route("/api/detected-drives", get(api_detected_drives))
        .route("/api/duplicates", get(api_duplicates))
        .route("/api/preview/:id", get(api_preview))
        .route("/api/quarantine", post(api_quarantine))
        .route("/api/repack", post(api_repack))
        .route("/api/forget-drive", post(api_forget_drive))
        .route("/api/rename-drive", post(api_rename_drive))
        .route("/api/purge-all", post(api_purge_all))
        .route("/api/scan", post(api_scan))
        .route("/api/scan/status", get(api_scan_status))
        .route("/api/pick-folder", post(api_pick_folder))
        .route("/review", get(review))
        .route("/scan", get(scan_page_h))
        .route("/drives", get(drives_page_h))
        .route("/console", get(console_page_h))
        .route("/assets/:file", get(asset))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<axum::body::Body>| {
                    crate::observability::make_request_span(req)
                })
                .on_request(DefaultOnRequest::new().level(tracing::Level::DEBUG))
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::INFO)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .with_state(state)
}

async fn overview(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::overview_page(&state.csrf_token))
}

async fn browse(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::browse_page(&state.csrf_token))
}

async fn review(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::review_page(&state.csrf_token))
}

async fn scan_page_h(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::scan_page(&state.csrf_token))
}

async fn drives_page_h(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::drives_page(&state.csrf_token))
}

async fn console_page_h(State(state): State<AppState>) -> Html<String> {
    Html(crate::web_ui::console_page(&state.csrf_token))
}

/// Vendored fonts, served same-origin (no external request) and cached hard. Everything is
/// self-hosted so the UI stays 100% offline.
async fn asset(AxPath(file): AxPath<String>) -> Response {
    let bytes: &'static [u8] = match file.as_str() {
        "InterVariable.woff2" => include_bytes!("../assets/InterVariable.woff2"),
        "JetBrainsMono-Regular.woff2" => include_bytes!("../assets/JetBrainsMono-Regular.woff2"),
        "JetBrainsMono-Medium.woff2" => include_bytes!("../assets/JetBrainsMono-Medium.woff2"),
        "MaterialSymbolsOutlined.woff2" => include_bytes!("../assets/MaterialSymbolsOutlined.woff2"),
        _ => return (StatusCode::NOT_FOUND, "no such asset").into_response(),
    };
    (
        [(header::CONTENT_TYPE, "font/woff2"),
         (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
        axum::body::Bytes::from_static(bytes),
    ).into_response()
}

/// Web-facing shape for a search hit; keeps serialization concerns out of `catalog::models`.
#[derive(Serialize)]
struct HitDto {
    location: String,
    relative_path: String,
    container_chain: Option<String>,
    filename: String,
    volume_id: String,
    volume_label: String,
    category: String,
    size_bytes: i64,
    status: String,
    content_hash: String,
    copies: Option<i64>,
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
            volume_label: String::new(), // filled by the handler (needs the catalog's label map)
            category: f.category.as_str().to_string(),
            size_bytes: f.size_bytes,
            status: f.status.as_str().to_string(),
            content_hash: f.content_hash,
            copies: None, // filled by the handler (needs the global duplicate counts)
        }
    }
}

#[derive(Serialize)]
struct DetectedDriveDto {
    mount_path: String,
    volume_id: Option<String>,
    catalogued: bool,
    volume_label: Option<String>,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
}

#[derive(Serialize)]
struct VolumeDto { volume_id: String, label: String, display_name: Option<String>, active_files: i64, active_bytes: i64 }

#[derive(Serialize)]
struct StatsDto { duplicate_groups: i64, volumes: Vec<VolumeDto> }

#[derive(Serialize)]
struct ActivityDto { kind: String, summary: String, occurred_at: i64 }

#[derive(Serialize)]
struct DriveDto {
    volume_id: String,
    label: String,
    display_name: Option<String>,
    description: Option<String>,
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

/// Human summary for one audit row. `details` is the JSON stored by the engine; parse best-effort
/// and fall back to the raw action name so a schema change can never break the feed.
fn activity_summary(action: &str, details: &str) -> String {
    let d: serde_json::Value = serde_json::from_str(details).unwrap_or(serde_json::Value::Null);
    let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| d.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    match action {
        "scan" => format!("Scanned {} — {} hashed, {} unchanged", s("label"), n("hashed"), n("skipped")),
        "quarantine" => {
            let from = s("from");
            let name = from.rsplit('/').next().unwrap_or(&from);
            format!("Quarantined {}", name)
        }
        "quarantine_skip" => "Skipped a file to protect the last copy".to_string(),
        "quarantine_error" => "A file could not be quarantined".to_string(),
        "repack" => format!("Repacked an archive (removed {})", s("removed_entry")),
        "purge" => format!("Purged {} file(s), reclaimed {} MiB", n("files_purged"), n("bytes_reclaimed") / (1024 * 1024)),
        "forget" => format!("Removed drive '{}' from the catalog", s("label")),
        "rename" => "Renamed a drive".to_string(),
        other => other.to_string(),
    }
}

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
    tracing::error!(error = %e, "request failed");
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

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

/// Current time as UNIX seconds; a clock error becomes a 500 (matches existing handler behavior).
fn now_secs() -> Result<i64, (StatusCode, String)> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(err500)?.as_secs() as i64)
}

/// Best-effort catalog snapshot around a mutation (some handlers call this before the mutation as a
/// pre-mutation safety net, others after). Never fails the request — a snapshot error is swallowed.
fn snapshot_best_effort(state: &AppState, now: i64) {
    if let Ok(cfg) = crate::config::Config::default_paths() {
        let _ = crate::catalog::backup::snapshot(&state.catalog_path, &cfg.backups_dir(),
            cfg.snapshot_retention, now);
    }
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
    // Friendly drive names (effective: custom-or-detected) + which results are duplicated
    // (global active-copy count).
    let labels = cat.effective_labels().map_err(err500)?;
    let hashes: Vec<String> = hits.iter().map(|f| f.content_hash.clone()).collect();
    let dupes = cat.duplicate_counts(&hashes).map_err(err500)?;
    let out: Vec<HitDto> = hits.into_iter().map(|f| {
        let mut dto = HitDto::from(f);
        dto.volume_label = labels.get(&dto.volume_id).cloned().unwrap_or_else(|| dto.volume_id.clone());
        dto.copies = dupes.get(&dto.content_hash).copied();
        dto
    }).collect();
    Ok(Json(out))
}

/// Shared by /api/volumes and /api/stats so the two endpoints can't drift apart.
fn volume_dtos(cat: &Catalog) -> anyhow::Result<Vec<VolumeDto>> {
    let eff = cat.effective_labels()?;
    Ok(cat.volume_stats()?.into_iter()
        .map(|(volume_id, label, active_files, active_bytes)| {
            let display_name = eff.get(&volume_id).cloned();
            VolumeDto { volume_id, label, display_name, active_files, active_bytes }
        }).collect())
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

async fn api_activity(State(state): State<AppState>)
    -> Result<Json<Vec<ActivityDto>>, (StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let rows = cat.recent_actions(30).map_err(err500)?;
    Ok(Json(rows.into_iter().map(|(action, details, occurred_at)| ActivityDto {
        summary: activity_summary(&action, &details), kind: action, occurred_at,
    }).collect()))
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
        let (display_name, description) = cat.volume_meta(&volume_id).map_err(err500)?;
        out.push(DriveDto {
            connected: mount_path.is_some(),
            mount_path: mount_path.map(|p| p.display().to_string()),
            reclaimable_bytes: reclaim.get(&volume_id).copied().unwrap_or(0),
            last_seen_at: cat.volume_last_seen(&volume_id).map_err(err500)?,
            has_errors: cat.volume_has_scan_errors(&volume_id).map_err(err500)?,
            volume_id, label, display_name, description, active_files, active_bytes, total_bytes, free_bytes,
        });
    }
    Ok(Json(out))
}

async fn api_detected_drives(State(state): State<AppState>)
    -> Result<Json<Vec<DetectedDriveDto>>, (StatusCode, String)>
{
    let cat = Catalog::open_readonly(&state.catalog_path).map_err(err500)?;
    let labels: std::collections::HashMap<String, String> = cat.volume_stats().map_err(err500)?
        .into_iter().map(|(id, label, _, _)| (id, label)).collect();
    let mut out = Vec::new();
    for (_vid_key, root) in state.mounts.snapshot() {
        let volume_id = crate::volume::read_volume_id(&root);
        let (catalogued, volume_label) = match &volume_id {
            Some(vid) => (labels.contains_key(vid), labels.get(vid).cloned()),
            None => (false, None),
        };
        let (total_bytes, free_bytes) = match crate::mounts::disk_capacity(&root) {
            Some((t, f)) => (Some(t), Some(f)),
            None => (None, None),
        };
        out.push(DetectedDriveDto {
            mount_path: root.display().to_string(), volume_id, catalogued, volume_label,
            total_bytes, free_bytes,
        });
    }
    out.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
    Ok(Json(out))
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
        Some(chain) if !chain.contains(" › ") => crate::image_preview::read_zip_entry(&mount.join(&rec.relative_path), chain).ok(),
        Some(_) => return not_found("nested-archive preview not supported"),
    };
    let Some(bytes) = bytes else { return not_found("file unavailable") };

    match crate::image_preview::thumbnail_jpeg(&bytes, PREVIEW_MAX_DIM) {
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
    check_csrf(&headers, &state)?;

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = now_secs()?;

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
    snapshot_best_effort(&state, now);
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
    check_csrf(&headers, &state)?;

    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let rec = cat.get_file(body.entry_id).map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "no such entry".to_string()))?;
    let mount = state.mounts.resolve(&rec.volume_id)
        .ok_or((StatusCode::CONFLICT, "drive not connected".to_string()))?;
    let now = now_secs()?;
    let out = crate::repack::repack_entry(&cat, &mount, &rec.volume_id, body.entry_id, now)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Snapshot the catalog this request actually mutated (best-effort; a snapshot failure
    // shouldn't fail the request).
    snapshot_best_effort(&state, now);
    Ok(Json(RepackResultDto { removed_entry: out.removed_entry, retained_entries: out.retained_entries }))
}

#[derive(Deserialize)]
struct ForgetReq { volume_id: String }

#[derive(Serialize)]
struct ForgetResultDto { removed_files: usize }

/// Remove a volume's catalog rows entirely (files on disk untouched; a rescan re-adds them).
/// All destructive safety lives in `Catalog::forget_volume` (a same-transaction delete); this
/// handler is just the CSRF gate plus a best-effort pre-mutation snapshot.
async fn api_forget_drive(State(state): State<AppState>, headers: HeaderMap, body: Json<ForgetReq>)
    -> Result<Json<ForgetResultDto>, (StatusCode, String)>
{
    check_csrf(&headers, &state)?;
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = now_secs()?;
    snapshot_best_effort(&state, now);
    let removed = cat.forget_volume(&body.volume_id, now).map_err(err500)?;
    Ok(Json(ForgetResultDto { removed_files: removed }))
}

#[derive(Deserialize)]
struct RenameReq { volume_id: String, name: Option<String>, description: Option<String> }

#[derive(Serialize)]
struct RenameResultDto { name: String }

/// Set a volume's custom display name and/or description. All persistence lives in
/// `Catalog::set_volume_meta`; this handler is just the CSRF gate plus resolving the effective
/// name to return.
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

#[derive(Serialize)]
struct PurgeAllResultDto {
    purged_volumes: usize,
    files_purged: usize,
    bytes_reclaimed: i64,
    skipped_unmounted: Vec<String>,
    errors: Vec<String>,
}

/// Purge every mounted volume that has reclaimable quarantine (Task 6's `purge_all`). All
/// destructive safety lives in `purge::purge_volume` (called per-volume); this handler is just
/// the CSRF gate plus a best-effort pre-mutation snapshot.
async fn api_purge_all(State(state): State<AppState>, headers: HeaderMap)
    -> Result<Json<PurgeAllResultDto>, (StatusCode, String)>
{
    check_csrf(&headers, &state)?;
    let cat = Catalog::open(&state.catalog_path).map_err(err500)?;
    let now = now_secs()?;
    snapshot_best_effort(&state, now);
    let out = crate::purge::purge_all(&cat, &state.mounts.snapshot(), now).map_err(err500)?;
    Ok(Json(PurgeAllResultDto {
        purged_volumes: out.purged.len(),
        files_purged: out.purged.iter().map(|(_, f, _)| f).sum(),
        bytes_reclaimed: out.purged.iter().map(|(_, _, b)| b).sum(),
        skipped_unmounted: out.skipped_unmounted,
        errors: out.errors,
    }))
}

#[derive(Deserialize)]
struct ScanReq { path: String, force: bool }

#[derive(Serialize)]
struct ScanEnqueuedDto { queued_position: usize }

/// Enqueue a background scan of `path`. This handler is just the CSRF gate plus input
/// validation; the actual scan runs one-at-a-time in `ScanQueue`'s worker task so the request
/// returns immediately instead of blocking on a potentially slow drive walk.
async fn api_scan(State(state): State<AppState>, headers: HeaderMap, body: Json<ScanReq>)
    -> Result<Json<ScanEnqueuedDto>, (StatusCode, String)>
{
    check_csrf(&headers, &state)?;

    if body.path.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".into()));
    }
    let path = std::path::PathBuf::from(body.path.trim());
    let pos = state.scan_queue.enqueue(path, body.force);
    Ok(Json(ScanEnqueuedDto { queued_position: pos }))
}

async fn api_scan_status(State(state): State<AppState>) -> Json<crate::scan_queue::StatusSnapshot> {
    Json(state.scan_queue.status())
}

#[derive(Serialize)]
struct PickFolderDto { path: Option<String> }

/// Open the native OS folder-picker dialog and return the chosen path (or `null` on cancel).
/// The dialog call is blocking, so it runs on a `spawn_blocking` thread rather than the async
/// runtime. This handler is just the CSRF gate plus that thread hop.
async fn api_pick_folder(State(state): State<AppState>, headers: HeaderMap)
    -> Result<Json<PickFolderDto>, (StatusCode, String)>
{
    check_csrf(&headers, &state)?;

    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new().set_title("Choose a drive or folder to scan").pick_folder()
    }).await.map_err(err500)?;
    Ok(Json(PickFolderDto { path: picked.map(|p| p.display().to_string()) }))
}

/// Serve the browse UI on 127.0.0.1 with an OS-assigned free port until the process is stopped.
pub async fn serve(catalog_path: PathBuf, open: bool) -> anyhow::Result<()> {
    let state = AppState::new_live(catalog_path);
    tokio::spawn(state.scan_queue.clone().run_worker());
    let app = build_router_with(state);
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
        assert!(matches!(s.mounts, crate::mounts::MountResolver::Live { .. }));
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
    async fn api_search_enriches_label_hash_and_copies() {
        let (_t, db, _state) = seed_dupes();
        let v = get_json(&db, "/api/search").await; // empty query -> all files
        let arr = v.as_array().unwrap();
        assert!(arr.len() >= 2);
        for h in arr {
            assert_eq!(h["volume_label"], "Photos HDD");     // friendly name, not the id
            assert_eq!(h["content_hash"], "DUP");
            assert_eq!(h["copies"], 2);                       // both are duplicated (2 active copies)
        }
    }

    #[tokio::test]
    async fn api_search_copies_null_for_unique_file() {
        let (_t, db) = seed_catalog(); // thesis.pdf (h1) + archived inner.jpg (h2): both unique
        let v = get_json(&db, "/api/search?q=thesis").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["volume_label"], "Test HDD");
        assert!(arr[0]["copies"].is_null());
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
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into(),
            scan_queue: crate::scan_queue::ScanQueue::new(db.clone()) };
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
    async fn api_drives_lists_catalogued_volume_with_reclaimable() {
        let (_t, _db, state) = seed_dupes(); // seeds vol-1 "Photos HDD" with a duplicate group
        let v = get_json_state(state, "/api/drives").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], "Photos HDD");
        assert_eq!(arr[0]["connected"], true); // Fixed mount is present
        assert!(arr[0]["reclaimable_bytes"].as_i64().unwrap() >= 0);
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

    #[tokio::test]
    async fn preview_returns_jpeg_for_loose_photo_on_mounted_drive() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, db, state) = seed_dupes();
        // write a real image at the loose path on the fake drive
        let drive = match &state.mounts { crate::mounts::MountResolver::Fixed(m) => m["vol-1"].clone(), _ => unreachable!() };
        std::fs::write(drive.join("a.jpg"), tiny_png()).unwrap();
        // find a.jpg's id
        let cat = crate::catalog::Catalog::open_readonly(&db).unwrap();
        let id = cat.loose_file_id("vol-1", "a.jpg").unwrap().unwrap();

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
        let id = cat.loose_file_id("vol-1", "a.jpg").unwrap().unwrap();

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
        let id = cat.loose_file_id("vol-1", "notes.txt").unwrap().unwrap();

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

    #[tokio::test]
    async fn purge_all_requires_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/purge-all", None, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
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
        let victim = cat.loose_file_id("vol-1", "copy/a.jpg").unwrap().unwrap();
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
        let id = cat.loose_file_id("vol-2", "x.jpg").unwrap().unwrap();
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
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into(),
            scan_queue: crate::scan_queue::ScanQueue::new(db.clone()) };

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
            csrf_token: "T".into(), scan_queue: crate::scan_queue::ScanQueue::new(db.clone()) };

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
            mounts: crate::mounts::MountResolver::Fixed(mounts), csrf_token: "T".into(),
            scan_queue: crate::scan_queue::ScanQueue::new(db.clone()) };

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
    async fn pick_folder_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/pick-folder", None, serde_json::json!({})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn scan_requires_csrf_token() {
        let (_t, _db, state) = seed_dupes();
        let (status, _) = post_json(state, "/api/scan", None,
            serde_json::json!({"path":"whatever","force":false})).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn scan_enqueues_and_status_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let drive = tmp.path().join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join("a.txt"), b"hi").unwrap();
        { crate::catalog::Catalog::open(&db).unwrap(); }
        let state = AppState {
            catalog_path: db.clone(),
            mounts: crate::mounts::MountResolver::Fixed(std::collections::HashMap::new()),
            csrf_token: "T".into(),
            scan_queue: crate::scan_queue::ScanQueue::new(db.clone()),
        };
        // must run the worker for the enqueued job to progress
        tokio::spawn(state.scan_queue.clone().run_worker());

        let (status, json) = post_json(state.clone(), "/api/scan", Some("T"),
            serde_json::json!({"path": drive.to_string_lossy(), "force": false})).await;
        assert_eq!(status, axum::http::StatusCode::OK, "body {json}");

        // poll status until the scan finishes
        let done = {
            let mut found = false;
            for _ in 0..200 {
                let v = get_json_state(state.clone(), "/api/scan/status").await;
                if v["recent"].as_array().map(|a| !a.is_empty()).unwrap_or(false) { found = true; break; }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            found
        };
        assert!(done, "scan should have completed and appeared in recent");
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
        let res = app.oneshot(Request::builder().uri("/browse").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("id=\"q\""), "search input present");
        assert!(body.contains("id=\"results\""), "results container present");
        assert!(body.contains("/api/search"), "page fetches the search API");
        assert!(body.contains("buildTree") && body.contains("renderTree"), "renders a tree");
        assert!(body.contains("class=\"tree\""), "tree container present");
        // self-contained: no external resource references
        assert!(!body.contains("http://"), "no external http resources");
        assert!(!body.contains("https://"), "no external https resources");
    }

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

    #[tokio::test]
    async fn shell_has_theme_toggle() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-theme=\"dark\"") && body.contains("themebar"), "theme toggle present");
        assert!(body.contains("applyTheme"), "theme JS present");
        assert!(!body.contains("http://") && !body.contains("https://"), "self-contained");
    }

    #[tokio::test]
    async fn detected_drives_flags_catalogued() {
        let (_t, _db, state) = seed_dupes(); // Fixed mount vol-1 -> driveA (marker vol-1), catalogued
        let v = get_json_state(state, "/api/detected-drives").await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["catalogued"], true);
        assert_eq!(arr[0]["volume_label"], "Photos HDD");
    }

    #[tokio::test]
    async fn scan_page_is_self_contained_and_wired() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let (_t, _db, state) = seed_dupes();
        let app = build_router_with(state);
        let res = app.oneshot(Request::builder().uri("/scan").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 2_000_000).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"csrf\""));
        assert!(body.contains("/api/scan"));
        assert!(body.contains("/api/detected-drives"));
        assert!(body.contains("/api/pick-folder"));
        assert!(!body.contains("http://") && !body.contains("https://"));
    }

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
        assert!(body.contains("/api/rename-drive"), "drives page can rename");
        assert!(!body.contains("http://") && !body.contains("https://"));
    }

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

    #[derive(Clone)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf); Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer { self.clone() }
    }

    #[tokio::test]
    async fn request_is_traced_with_method_status_and_id() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        // Serialize with other subscriber-installing tests (tracing's interest cache is global).
        let _tracing_lock = crate::observability::tracing_test_guard();
        let (_t, db) = seed_catalog();
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureWriter(buf.clone()))
            .with_ansi(false) // the custom writer isn't a terminal; disable ANSI so "id=" etc. are contiguous
            .finish();
        let _guard = tracing::subscriber::set_default(sub); // held across the await (current-thread test)

        let app = build_router(db.clone());
        let res = app.oneshot(Request::builder().uri("/api/search?q=thesis").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("GET"), "log: {logged}");
        assert!(logged.contains("200"), "log: {logged}");
        assert!(logged.contains("id="), "request-id field present: {logged}");
    }

    #[tokio::test]
    async fn csrf_rejection_is_logged() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        // Serialize with other subscriber-installing tests (tracing's interest cache is global).
        let _tracing_lock = crate::observability::tracing_test_guard();
        let (_t, _db, state) = seed_dupes();
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(CaptureWriter(buf.clone()))
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(sub);

        let app = build_router_with(state);
        // POST /api/quarantine with NO token -> 403 and a warn line
        let res = app.oneshot(Request::builder().method("POST").uri("/api/quarantine")
            .header("content-type", "application/json")
            .body(Body::from("{\"quarantine_ids\":[1]}")).unwrap()).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("WARN"), "expected a warn line: {logged}");
        assert!(logged.to_lowercase().contains("token"), "reason mentions token: {logged}");
    }

    #[tokio::test]
    async fn api_activity_returns_formatted_rows() {
        let (_t, db, state) = seed_dupes();
        { // write actions to read back (newest-first: purge@500, then quarantine@400)
            let cat = crate::catalog::Catalog::open(&db).unwrap();
            cat.log_action("purge",
                "{\"volume_id\":\"vol-1\",\"files_purged\":3,\"bytes_reclaimed\":2048}", 500).unwrap();
            // Real quarantine payload shape from src/quarantine.rs: `from` is the relative path.
            cat.log_action("quarantine",
                "{\"file_id\":1,\"volume_id\":\"vol-1\",\"from\":\"docs/report.txt\",\"to\":\"_ToDelete/report.txt\",\"hash\":\"h\"}", 400).unwrap();
        }
        let v = get_json_state(state, "/api/activity").await;
        let arr = v.as_array().unwrap();
        assert!(!arr.is_empty());
        assert_eq!(arr[0]["kind"], "purge");
        assert!(arr[0]["summary"].as_str().unwrap().contains("Purged"));
        assert_eq!(arr[0]["occurred_at"], 500);
        // The quarantine feed entry must name the file (basename of `from`), not render blank.
        let q = arr.iter().find(|e| e["kind"] == "quarantine").expect("quarantine entry present");
        assert!(q["summary"].as_str().unwrap().contains("report.txt"),
            "quarantine summary should name the file: {}", q["summary"]);
    }
}

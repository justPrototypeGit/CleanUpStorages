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
    Html("<!doctype html><title>CleanUpStorages</title><h1>CleanUpStorages</h1>")
}

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
}

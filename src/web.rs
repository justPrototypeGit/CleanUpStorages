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
}

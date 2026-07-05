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

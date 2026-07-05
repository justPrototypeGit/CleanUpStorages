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

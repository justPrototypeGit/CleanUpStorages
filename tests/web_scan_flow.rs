use std::io::{Read, Write};
use std::net::TcpStream;

fn start(db: std::path::PathBuf) -> std::net::SocketAddr {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let state = cleanupstorages::web::AppState {
                catalog_path: db.clone(),
                mounts: cleanupstorages::mounts::MountResolver::Fixed(
                    std::collections::HashMap::new(),
                ),
                csrf_token: "TESTTOKEN".to_string(),
                scan_queue: cleanupstorages::scan_queue::ScanQueue::new(db.clone()),
            };
            tokio::spawn(state.scan_queue.clone().run_worker());
            let app = cleanupstorages::web::build_router_with(state);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
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
fn scan_a_folder_over_http_and_see_it_finish() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("c.db");
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("a.txt"), b"one").unwrap();
    std::fs::write(drive.join("b.txt"), b"two").unwrap();
    {
        cleanupstorages::catalog::Catalog::open(&db).unwrap();
    }
    std::mem::forget(tmp);

    let addr = start(db.clone());

    // enqueue a scan (with token)
    let payload = format!("{{\"path\":{:?},\"force\":false}}", drive.to_string_lossy());
    let post = format!("POST /api/scan HTTP/1.0\r\nHost: x\r\ncontent-type: application/json\r\nx-cleanup-token: TESTTOKEN\r\ncontent-length: {}\r\n\r\n{}", payload.len(), payload);
    let resp = req(addr, &post);
    assert!(resp.contains("200 OK"), "scan enqueue: {resp}");

    // poll status until the scan appears in recent with 2 hashed
    let mut done = false;
    for _ in 0..200 {
        let s = req(addr, "GET /api/scan/status HTTP/1.0\r\nHost: x\r\n\r\n");
        if s.contains("\"hashed\":2") && s.contains("\"error_message\":null") {
            done = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(done, "scan should complete and report 2 hashed");
}

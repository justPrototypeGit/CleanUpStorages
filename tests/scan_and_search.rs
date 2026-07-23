use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cleanupstorages"))
}

#[test]
fn scan_then_search_finds_file() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("thesis_final.pdf"), b"hello thesis").unwrap();

    // Point the catalog at a temp location via env override (see Step 2).
    let data = tmp.path().join("appdata");
    let scan = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("scan")
        .arg(&drive)
        .arg("--readonly-fallback")
        .arg("fingerprint")
        .output()
        .unwrap();
    assert!(
        scan.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&scan.stderr)
    );

    let search = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("search")
        .arg("thesis")
        .output()
        .unwrap();
    assert!(search.status.success());
    let out = String::from_utf8_lossy(&search.stdout);
    assert!(out.contains("thesis_final.pdf"), "search output was: {out}");
}

#[test]
fn scan_prints_where_the_time_went() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("a.txt"), b"hello").unwrap();
    let data = tmp.path().join("appdata");

    let out = bin()
        .env("CLEANUPSTORAGES_DATA_DIR", &data)
        .arg("scan")
        .arg(&drive)
        .arg("--readonly-fallback")
        .arg("fingerprint")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Where the time went"),
        "missing breakdown: {text}"
    );
    for phase in ["walk", "skip_check", "hash", "db_write", "archive"] {
        assert!(text.contains(phase), "missing phase {phase}: {text}");
    }
    assert!(text.contains("files/s"), "missing rates: {text}");
    assert!(
        text.contains("File sizes seen"),
        "missing histogram: {text}"
    );
}

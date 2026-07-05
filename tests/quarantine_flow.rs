use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_cleanupstorages")) }

#[test]
fn scan_quarantine_purge_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    // two identical files (duplicates)
    std::fs::write(drive.join("a.txt"), b"SAME CONTENT").unwrap();
    std::fs::write(drive.join("b.txt"), b"SAME CONTENT").unwrap();
    let data = tmp.path().join("appdata");
    let env = |c: &mut Command| { c.env("CLEANUPSTORAGES_DATA_DIR", &data); };

    // scan
    let mut c = bin(); env(&mut c);
    let out = c.arg("scan").arg(&drive).arg("--readonly-fallback").arg("fingerprint").output().unwrap();
    assert!(out.status.success(), "scan: {}", String::from_utf8_lossy(&out.stderr));

    // duplicates lists the pair (find a file id to quarantine)
    let mut c = bin(); env(&mut c);
    let dups = c.arg("duplicates").output().unwrap();
    let dtext = String::from_utf8_lossy(&dups.stdout);
    assert!(dtext.contains("a.txt") && dtext.contains("b.txt"), "duplicates: {dtext}");

    // The scan used a fingerprint id (read-only marker path); the drive DID get a marker written
    // during scan (it was writable), so quarantine can identify it. Quarantine b.txt by id.
    // Parse the first integer id printed on the line containing "b.txt".
    let id: i64 = dtext.lines().find(|l| l.contains("b.txt"))
        .and_then(|l| l.split_whitespace().find_map(|t| t.trim_start_matches('#').parse().ok()))
        .expect("an id on the b.txt line");

    let mut c = bin(); env(&mut c);
    let q = c.arg("quarantine").arg(&drive).arg(id.to_string()).output().unwrap();
    assert!(q.status.success(), "quarantine: {}", String::from_utf8_lossy(&q.stderr));
    assert!(!drive.join("b.txt").exists(), "b.txt should be moved");
    assert!(drive.join("_ToDelete/b.txt").exists(), "b.txt should be in _ToDelete");
    assert!(drive.join("a.txt").exists(), "a.txt (survivor) stays");

    // purge reclaims
    let mut c = bin(); env(&mut c);
    let p = c.arg("purge").arg(&drive).output().unwrap();
    assert!(p.status.success(), "purge: {}", String::from_utf8_lossy(&p.stderr));
    assert!(!drive.join("_ToDelete").exists(), "_ToDelete removed");
}

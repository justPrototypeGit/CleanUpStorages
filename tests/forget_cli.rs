use std::process::Command;

#[test]
fn forget_removes_a_scanned_drive() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let drive = tmp.path().join("DriveX");
    std::fs::create_dir_all(&drive).unwrap();
    std::fs::write(drive.join("f.txt"), b"hello").unwrap();
    let exe = env!("CARGO_BIN_EXE_cleanupstorages");

    let run = |args: &[&str]| {
        Command::new(exe)
            .args(args)
            .env("CLEANUPSTORAGES_DATA_DIR", &data)
            .output()
            .unwrap()
    };
    // scan (fingerprint fallback so no marker write is needed on odd filesystems)
    let out = run(&[
        "scan",
        drive.to_str().unwrap(),
        "--readonly-fallback",
        "fingerprint",
    ]);
    assert!(
        out.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // forget
    let out = run(&["forget", drive.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "forget failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("removed"));
    // status now shows no volumes
    let out = run(&["status"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("DriveX"),
        "volume should be gone from status: {s}"
    );
}

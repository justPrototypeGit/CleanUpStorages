use std::process::Command;

#[test]
fn rename_sets_display_name_and_description_on_a_scanned_drive() {
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
    // rename
    let out = run(&[
        "rename",
        drive.to_str().unwrap(),
        "--name",
        "My Drive",
        "--description",
        "notes",
    ]);
    assert!(
        out.status.success(),
        "rename failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Updated drive"));
}

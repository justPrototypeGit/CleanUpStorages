use std::io::Write;
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_cleanupstorages")) }

fn write_zip(path: &std::path::Path, files: &[(&str, &[u8])]) {
    let f = std::fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(f);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in files { zw.start_file(*name, opts).unwrap(); zw.write_all(bytes).unwrap(); }
    zw.finish().unwrap();
}

fn zip_has(path: &std::path::Path, name: &str) -> bool {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let found = z.by_name(name).is_ok();
    found
}

#[test]
fn scan_then_repack_removes_archived_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    write_zip(&drive.join("bundle.zip"), &[("keep.txt", b"KEEP"), ("dup.txt", b"SHARED")]);
    std::fs::write(drive.join("loose_dup.txt"), b"SHARED").unwrap(); // the surviving copy
    let data = tmp.path().join("appdata");
    let env = |c: &mut Command| { c.env("CLEANUPSTORAGES_DATA_DIR", &data); };

    let mut c = bin(); env(&mut c);
    assert!(c.arg("scan").arg(&drive).arg("--readonly-fallback").arg("fingerprint").output().unwrap().status.success());

    // find the entry id via `duplicates` (dup.txt appears loose and inside bundle.zip)
    let mut c = bin(); env(&mut c);
    let dups = String::from_utf8(c.arg("duplicates").output().unwrap().stdout).unwrap();
    // the archived member line shows "bundle.zip › dup.txt"
    let id: i64 = dups.lines().find(|l| l.contains("bundle.zip › dup.txt"))
        .and_then(|l| l.split_whitespace().find_map(|t| t.trim_start_matches('#').parse().ok()))
        .expect("archived dup.txt id in duplicates output");

    let mut c = bin(); env(&mut c);
    let out = c.arg("repack").arg(&drive).arg(id.to_string()).output().unwrap();
    assert!(out.status.success(), "repack: {}", String::from_utf8_lossy(&out.stderr));

    assert!(zip_has(&drive.join("bundle.zip"), "keep.txt"));
    assert!(!zip_has(&drive.join("bundle.zip"), "dup.txt")); // removed
    assert!(drive.join("_ToDelete").exists()); // recovery nets present
    assert!(drive.join("loose_dup.txt").exists()); // survivor untouched
}

//! Crash-safe removal of one entry from a top-level zip (Case 4): build a new archive without the
//! entry, verify every retained entry against the catalog, then swap — original never touched until
//! the rebuild is proven good.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RepackOutcome {
    pub removed_entry: String,
    pub retained_entries: usize,
}

/// Bytes of one top-level entry.
pub fn extract_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

/// Build `dest_tmp` = every entry of `src_archive` except `exclude_entry`, raw-copied (no recompress).
pub fn rebuild_without(src_archive: &Path, dest_tmp: &Path, exclude_entry: &str) -> anyhow::Result<()> {
    let src_file = std::fs::File::open(src_archive)?;
    let mut src = zip::ZipArchive::new(src_file)?;
    if let Some(parent) = dest_tmp.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out_file = std::fs::File::create(dest_tmp)?;
    let mut writer = zip::ZipWriter::new(out_file);

    let mut found = false;
    for i in 0..src.len() {
        let entry = src.by_index_raw(i)?;
        if entry.name() == exclude_entry {
            found = true;
            continue;
        }
        writer.raw_copy_file(entry)?;
    }
    writer.finish()?;
    if !found {
        let _ = std::fs::remove_file(dest_tmp);
        anyhow::bail!("entry '{exclude_entry}' not found in {}", src_archive.display());
    }
    Ok(())
}

/// Re-hash every expected retained entry and confirm the removed one is absent.
pub fn verify_rebuilt(
    tmp: &Path,
    expected: &HashMap<String, String>,
    must_be_absent: &str,
) -> anyhow::Result<()> {
    let file = std::fs::File::open(tmp)?;
    let mut zip = zip::ZipArchive::new(file)?;

    if zip.by_name(must_be_absent).is_ok() {
        anyhow::bail!("verify failed: removed entry '{must_be_absent}' is still present");
    }
    for (name, want) in expected {
        let mut entry = zip
            .by_name(name)
            .map_err(|_| anyhow::anyhow!("verify failed: retained entry '{name}' missing"))?;
        let got = crate::hashing::hash_reader(&mut entry)?;
        if &got != want {
            anyhow::bail!("verify failed: entry '{name}' hash mismatch (got {got}, want {want})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in files {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }

    fn blake3_hex(bytes: &[u8]) -> String {
        let mut b: &[u8] = bytes;
        crate::hashing::hash_reader(&mut b).unwrap()
    }

    #[test]
    fn rebuild_drops_the_entry_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP"), ("also/keep2.txt", b"K2")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        assert!(extract_entry(&out, "keep.txt").is_ok());
        assert!(extract_entry(&out, "also/keep2.txt").is_ok());
        assert!(extract_entry(&out, "drop.jpg").is_err()); // gone
    }

    #[test]
    fn verify_passes_for_matching_entries_and_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP"), ("drop.jpg", b"DROP")]);
        rebuild_without(&src, &out, "drop.jpg").unwrap();
        let mut expected = std::collections::HashMap::new();
        expected.insert("keep.txt".to_string(), blake3_hex(b"KEEP"));
        verify_rebuilt(&out, &expected, "drop.jpg").unwrap();

        // a wrong expected hash must fail
        let mut bad = std::collections::HashMap::new();
        bad.insert("keep.txt".to_string(), "deadbeef".to_string());
        assert!(verify_rebuilt(&out, &bad, "drop.jpg").is_err());
        // the removed entry still present would fail
        assert!(verify_rebuilt(&src, &expected, "drop.jpg").is_err());
    }

    #[test]
    fn rebuild_errors_if_excluded_entry_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.zip");
        let out = tmp.path().join("a.tmp");
        make_zip(&src, &[("keep.txt", b"KEEP")]);
        assert!(rebuild_without(&src, &out, "not-there.jpg").is_err());
    }
}

//! Reading into zip archives (recursively) to catalog their contents.

use std::io::{Read, Seek};

use crate::config::Config;
use crate::hashing;

/// Tunable safety limits for archive descent.
#[derive(Debug, Clone)]
pub struct ArchiveLimits {
    pub max_depth: usize,
    pub entry_max_bytes: u64,
    pub ratio_cap: u64,
}

impl ArchiveLimits {
    pub fn from_config(cfg: &Config) -> ArchiveLimits {
        ArchiveLimits {
            max_depth: cfg.max_archive_depth,
            entry_max_bytes: cfg.archive_entry_max_bytes,
            ratio_cap: cfg.archive_ratio_cap,
        }
    }
}

/// True if `name` looks like a zip archive (by extension, case-insensitive).
pub fn is_archive_name(name: &str) -> bool {
    name.rsplit('.').next().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false)
        && name.contains('.')
}

/// One hashed leaf entry found while scanning an archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub container_chain: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
}

/// Result of scanning one archive level: hashed leaf entries plus any skipped/error notes.
#[derive(Debug, Default)]
pub struct ArchiveScanResult {
    pub entries: Vec<ArchiveEntry>,
    pub errors: Vec<(String, String)>,
}

/// Extension (lowercased, no dot) of an internal entry name, or "" if none.
fn entry_extension(name: &str) -> String {
    let leaf = name.rsplit('/').next().unwrap_or(name);
    match leaf.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Scan ONE archive level from a seekable reader. Leaf files are hashed; entries exceeding the
/// zip-bomb caps are skipped with an error note. Nested archives are NOT descended here (Task 3).
pub fn scan_archive<R: Read + Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult {
    let mut result = ArchiveScanResult::default();
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            result.errors.push((String::new(), format!("unreadable archive: {e}")));
            return result;
        }
    };

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                result.errors.push((format!("#{i}"), format!("unreadable archive entry: {e}")));
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let uncompressed = entry.size();
        let compressed = entry.compressed_size().max(1);

        // Zip-bomb guards (declared sizes).
        if uncompressed > limits.entry_max_bytes {
            result.errors.push((
                name.clone(),
                format!("zip bomb: {uncompressed} bytes exceeds cap {}", limits.entry_max_bytes),
            ));
            continue;
        }
        if uncompressed / compressed > limits.ratio_cap {
            result.errors.push((
                name.clone(),
                format!("zip bomb: ratio {} exceeds cap {}", uncompressed / compressed, limits.ratio_cap),
            ));
            continue;
        }

        let filename = name.rsplit('/').next().unwrap_or(&name).to_string();
        let extension = entry_extension(&name);
        let content_hash = match hashing::hash_reader(&mut entry) {
            Ok(h) => h,
            Err(e) => {
                result.errors.push((name.clone(), format!("read error: {e}")));
                continue;
            }
        };
        result.entries.push(ArchiveEntry {
            container_chain: name,
            filename,
            extension,
            size_bytes: uncompressed as i64,
            content_hash,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn detects_zip_names() {
        assert!(is_archive_name("old.zip"));
        assert!(is_archive_name("OLD.ZIP"));
        assert!(is_archive_name("a.b.Zip"));
        assert!(!is_archive_name("notes.txt"));
        assert!(!is_archive_name("zip")); // no extension dot
        assert!(!is_archive_name("archive.zipx"));
    }

    #[test]
    fn limits_from_config() {
        let cfg = Config::default_paths().unwrap();
        let l = ArchiveLimits::from_config(&cfg);
        assert_eq!(l.max_depth, 8);
        assert_eq!(l.entry_max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(l.ratio_cap, 200);
    }

    // Build an in-memory zip: Vec of (name, bytes).
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, bytes) in files {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits { max_depth: 8, entry_max_bytes: 2 * 1024 * 1024 * 1024, ratio_cap: 200 }
    }

    #[test]
    fn hashes_flat_entries() {
        let zip = make_zip(&[("a.txt", b"alpha"), ("dir/b.pdf", b"beta")]);
        let res = scan_archive(Cursor::new(zip), &limits());
        assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
        assert_eq!(res.entries.len(), 2);
        let a = res.entries.iter().find(|e| e.filename == "a.txt").unwrap();
        // hash matches hashing::hash_reader over the same bytes
        let mut raw: &[u8] = b"alpha";
        assert_eq!(a.content_hash, hashing::hash_reader(&mut raw).unwrap());
        assert_eq!(a.container_chain, "a.txt");
        assert_eq!(a.size_bytes, 5);
        let b = res.entries.iter().find(|e| e.filename == "b.pdf").unwrap();
        assert_eq!(b.container_chain, "dir/b.pdf");
        assert_eq!(b.extension, "pdf");
    }

    #[test]
    fn rejects_oversized_entry() {
        // entry_max_bytes tiny -> the entry is skipped and logged, not hashed.
        let zip = make_zip(&[("big.bin", b"0123456789")]);
        let small = ArchiveLimits { max_depth: 8, entry_max_bytes: 4, ratio_cap: 200 };
        let res = scan_archive(Cursor::new(zip), &small);
        assert!(res.entries.is_empty());
        assert_eq!(res.errors.len(), 1);
        assert!(res.errors[0].1.contains("zip bomb"), "reason: {}", res.errors[0].1);
    }
}

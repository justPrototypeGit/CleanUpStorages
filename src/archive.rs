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

/// Join a parent chain and a child name with the guillemet separator.
fn join_chain(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_string() } else { format!("{prefix} › {name}") }
}

/// Read up to `cap` bytes; `Err` if the stream exceeds `cap` (bomb guard for buffering).
fn read_capped<R: Read>(mut reader: R, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut limited = (&mut reader).take(cap + 1);
    limited.read_to_end(&mut buf).map_err(|e| format!("read error: {e}"))?;
    if buf.len() as u64 > cap {
        return Err(format!("zip bomb: nested archive exceeds cap {cap}"));
    }
    Ok(buf)
}

/// Scan an archive (recursively) from a seekable reader. Leaf files are stream-hashed; nested
/// archives are buffered (bounded by `limits.entry_max_bytes`) and descended into up to
/// `limits.max_depth` levels. Entries exceeding the zip-bomb caps are skipped with an error note.
pub fn scan_archive<R: Read + Seek>(reader: R, limits: &ArchiveLimits) -> ArchiveScanResult {
    let mut result = ArchiveScanResult::default();
    scan_level(reader, "", 1, limits, &mut result);
    result
}

/// Scan one archive level. `chain_prefix` is the container chain of THIS archive ("" at top level);
/// `depth` is 1 for a top-level archive. Recurses into nested `.zip` entries until `max_depth`.
fn scan_level<R: Read + Seek>(reader: R, chain_prefix: &str, depth: usize,
    limits: &ArchiveLimits, result: &mut ArchiveScanResult)
{
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            result.errors.push((chain_prefix.to_string(), format!("unreadable archive: {e}")));
            return;
        }
    };

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                result.errors.push((join_chain(chain_prefix, &format!("#{i}")),
                    format!("unreadable archive entry: {e}")));
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let chain = join_chain(chain_prefix, &name);
        let uncompressed = entry.size();
        let compressed = entry.compressed_size().max(1);

        // Zip-bomb guards (declared sizes).
        if uncompressed > limits.entry_max_bytes {
            result.errors.push((chain,
                format!("zip bomb: {uncompressed} bytes exceeds cap {}", limits.entry_max_bytes)));
            continue;
        }
        if uncompressed / compressed > limits.ratio_cap {
            result.errors.push((chain,
                format!("zip bomb: ratio {} exceeds cap {}", uncompressed / compressed, limits.ratio_cap)));
            continue;
        }

        let filename = name.rsplit('/').next().unwrap_or(&name).to_string();
        let extension = entry_extension(&name);

        if is_archive_name(&name) {
            // Nested archive: buffer it (bounded) so we can BOTH hash it and re-open it with Seek
            // to recurse. Only archives are buffered — large leaf files stream (see else branch).
            let bytes = match read_capped(&mut entry, limits.entry_max_bytes) {
                Ok(b) => b,
                Err(reason) => { result.errors.push((chain, reason)); continue; }
            };
            let mut slice: &[u8] = &bytes;
            let content_hash = match hashing::hash_reader(&mut slice) {
                Ok(h) => h,
                Err(e) => { result.errors.push((chain, format!("read error: {e}"))); continue; }
            };
            result.entries.push(ArchiveEntry {
                container_chain: chain.clone(), filename, extension,
                size_bytes: uncompressed as i64, content_hash,
            });
            if depth >= limits.max_depth {
                result.errors.push((chain, format!("max archive depth exceeded ({} levels)", limits.max_depth)));
                continue;
            }
            scan_level(std::io::Cursor::new(bytes), &chain, depth + 1, limits, result);
        } else {
            // Leaf file: stream-hash directly, never buffering the whole entry into memory.
            let content_hash = match hashing::hash_reader(&mut entry) {
                Ok(h) => h,
                Err(e) => { result.errors.push((chain, format!("read error: {e}"))); continue; }
            };
            result.entries.push(ArchiveEntry {
                container_chain: chain, filename, extension,
                size_bytes: uncompressed as i64, content_hash,
            });
        }
    }
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

    // Wrap an existing zip's bytes as a single entry inside an outer zip.
    fn nest_zip(inner_name: &str, inner_zip: Vec<u8>, alongside: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zw.start_file(inner_name, opts).unwrap();
            zw.write_all(&inner_zip).unwrap();
            for (name, bytes) in alongside {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn descends_into_nested_archive() {
        let inner = make_zip(&[("vacation.jpg", b"pixels")]);
        let outer = nest_zip("photos.zip", inner, &[("readme.txt", b"hi")]);
        let res = scan_archive(Cursor::new(outer), &limits());
        assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
        // readme.txt (direct) + vacation.jpg (nested); the inner photos.zip itself is also an entry
        let jpg = res.entries.iter().find(|e| e.filename == "vacation.jpg").unwrap();
        assert_eq!(jpg.container_chain, "photos.zip › vacation.jpg");
        assert!(res.entries.iter().any(|e| e.container_chain == "readme.txt"));
        // the nested archive is itself catalogued as an entry (an identical inner zip is a dup)
        assert!(res.entries.iter().any(|e| e.container_chain == "photos.zip"));
    }

    #[test]
    fn stops_at_max_depth() {
        let inner = make_zip(&[("deep.txt", b"x")]);
        let outer = nest_zip("mid.zip", inner, &[]);
        // max_depth = 1: the top archive's direct entries are scanned, but mid.zip is not descended.
        let shallow = ArchiveLimits { max_depth: 1, entry_max_bytes: 2 * 1024 * 1024 * 1024, ratio_cap: 200 };
        let res = scan_archive(Cursor::new(outer), &shallow);
        assert!(res.entries.iter().any(|e| e.container_chain == "mid.zip")); // still catalogued as a file
        assert!(!res.entries.iter().any(|e| e.filename == "deep.txt")); // not descended
        assert!(res.errors.iter().any(|(_, r)| r.contains("max archive depth")));
    }
}

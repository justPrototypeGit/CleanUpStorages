//! "Will these bytes still exist afterwards?" — the check that guards every destructive action.
//!
//! Both `quarantine` and `repack` remove one copy of some content and must first prove another
//! copy survives. They used to prove it by checking a path *existed*, which trusts the catalogue's
//! `content_hash`. The incremental scan skips re-hashing when size and second-granularity mtime
//! match, so that hash can be stale (#4) — and a stale hash is how a unique file gets mistaken for
//! a duplicate. This module proves it by reading bytes instead.

use crate::catalog::Catalog;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Hashes computed during one destructive operation, keyed by the file read.
///
/// Confirming a K-copy group otherwise costs ~2K full reads — ten 5 GB copies would read ~100 GB
/// for one click. Scoped to a single call, so a cached value cannot outlive the action that took it.
#[derive(Default)]
pub(crate) struct HashCache {
    files: HashMap<PathBuf, String>,
    entries: HashMap<(PathBuf, String), String>,
}

impl HashCache {
    /// Hash a file on disk.
    pub(crate) fn file(&mut self, path: &Path) -> std::io::Result<String> {
        if let Some(h) = self.files.get(path) {
            return Ok(h.clone());
        }
        let h = crate::hashing::hash_file(path)?;
        self.files.insert(path.to_path_buf(), h.clone());
        Ok(h)
    }

    /// Hash one top-level entry inside a zip, by decompressing it.
    pub(crate) fn zip_entry(&mut self, archive: &Path, entry: &str) -> anyhow::Result<String> {
        let key = (archive.to_path_buf(), entry.to_string());
        if let Some(h) = self.entries.get(&key) {
            return Ok(h.clone());
        }
        let bytes = crate::image_preview::read_zip_entry(archive, entry)?;
        let mut slice: &[u8] = &bytes;
        let h = crate::hashing::hash_reader(&mut slice)?;
        self.entries.insert(key, h.clone());
        Ok(h)
    }
}

/// Whether another copy of the bytes was found, and if not, why — the reason is shown to the user,
/// so it has to be actionable rather than a generic refusal.
pub(crate) enum Survivor {
    Verified,
    NotFound(String),
}

/// True if the chain names an entry nested inside another archive, which we cannot extract directly.
fn is_nested(chain: &str) -> bool {
    chain.contains(" › ")
}

/// Look for an active copy, other than `self_id`, that **currently** holds `live_hash`.
///
/// `catalogued_hash` is what the catalogue believes the removed copy contains; `live_hash` is what
/// it actually contains right now. They differ exactly when the incremental scan left a stale hash.
///
/// Verification per candidate:
/// - **loose, this volume** — re-hash the file. Proof.
/// - **archived, this volume, top-level entry** — decompress that entry and hash it. Proof. (An
///   archived copy really does preserve the bytes; refusing here would block deduplicating a loose
///   file against its zipped twin, which is most of a typical corpus.)
/// - **archived inside a nested archive, or on another volume** — cannot be read from here, so it
///   is trusted only while the removed copy still matches its catalogued hash. Once that has
///   drifted there is no evidence the unread copy holds *these* bytes.
pub(crate) fn find_surviving_copy(
    cat: &Catalog,
    mount_root: &Path,
    expected_volume_id: &str,
    self_id: i64,
    catalogued_hash: &str,
    live_hash: &str,
    cache: &mut HashCache,
) -> anyhow::Result<Survivor> {
    let catalogue_is_current = live_hash == catalogued_hash;
    let mut unread: Option<String> = None;
    let mut read_error: Option<String> = None;

    for s in cat.active_copies(catalogued_hash)? {
        if s.id == self_id {
            continue;
        }
        let unreadable_from_here = s.volume_id != expected_volume_id
            || s.container_chain.as_deref().is_some_and(is_nested);

        if unreadable_from_here {
            if catalogue_is_current {
                return Ok(Survivor::Verified);
            }
            unread = Some(s.relative_path.clone());
            continue;
        }

        let path = mount_root.join(&s.relative_path);
        let hashed = match &s.container_chain {
            Some(entry) => cache.zip_entry(&path, entry).map_err(|e| e.to_string()),
            None if path.is_file() => cache.file(&path).map_err(|e| e.to_string()),
            None => Err("file is no longer on disk".to_string()),
        };
        match hashed {
            Ok(h) if h == live_hash => return Ok(Survivor::Verified),
            Ok(_) => {} // a real copy of something else — not a survivor for these bytes
            Err(e) => read_error = Some(format!("{}: {e}", s.relative_path)),
        }
    }

    let reason = if !catalogue_is_current {
        format!(
            "content changed since the last scan ({} on disk, {} catalogued) — rescan the drive \
             and review this file again",
            &live_hash[..16.min(live_hash.len())],
            &catalogued_hash[..16.min(catalogued_hash.len())]
        )
    } else if let Some(e) = read_error {
        format!("could not verify the surviving copy ({e})")
    } else if let Some(p) = unread {
        format!("the only other copy ({p}) could not be read from here to verify it")
    } else {
        "no surviving copy verified on disk (a same-drive duplicate may have been deleted outside \
         the tool — rescan the drive and retry)"
            .to_string()
    };
    Ok(Survivor::NotFound(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_chains_are_detected() {
        assert!(is_nested("outer.zip › inner.zip › a.txt"));
        assert!(!is_nested("a.txt"));
        assert!(!is_nested("dir/a.txt"));
    }

    #[test]
    fn a_file_is_hashed_once_per_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.bin");
        std::fs::write(&p, b"hello").unwrap();
        let mut c = HashCache::default();
        let first = c.file(&p).unwrap();

        // Change the bytes; the cache must answer with what it already read, which is the whole
        // point (and the documented trade-off) of caching within one operation.
        std::fs::write(&p, b"world").unwrap();
        assert_eq!(c.file(&p).unwrap(), first);

        let mut fresh = HashCache::default();
        assert_ne!(fresh.file(&p).unwrap(), first, "a new cache re-reads");
    }
}

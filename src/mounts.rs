//! Resolve each catalogued volume to its CURRENT mount path (drive letters/mounts change), by
//! reading the `.cleanupstorages_id` marker at each connected drive root.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone)]
pub enum MountResolver {
    /// Detect mounts live from the OS (production).
    Live,
    /// A fixed volume_id → root map (tests).
    Fixed(HashMap<String, PathBuf>),
}

impl MountResolver {
    /// The current mount root for `volume_id`, if the drive is connected.
    pub fn resolve(&self, volume_id: &str) -> Option<PathBuf> {
        match self {
            MountResolver::Live => live_mounts().get(volume_id).cloned(),
            MountResolver::Fixed(m) => m.get(volume_id).cloned(),
        }
    }

    /// Snapshot the current volume_id → mount map once (avoids re-enumerating disks per lookup).
    pub fn snapshot(&self) -> std::collections::HashMap<String, std::path::PathBuf> {
        match self {
            MountResolver::Live => live_mounts(),
            MountResolver::Fixed(m) => m.clone(),
        }
    }
}

/// Build volume_id → root by reading the identity marker at each candidate root.
pub fn scan_mounts<I: IntoIterator<Item = PathBuf>>(roots: I) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for root in roots {
        if let Some(vid) = crate::volume::read_volume_id(&root) {
            map.entry(vid).or_insert(root);
        }
    }
    map
}

/// All currently-mounted drives that carry our marker, by volume_id.
pub fn live_mounts() -> HashMap<String, PathBuf> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    scan_mounts(disks.list().iter().map(|d| d.mount_point().to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reads_markers_at_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("driveA");
        let b = tmp.path().join("driveB");
        let c = tmp.path().join("driveC_nomark");
        for d in [&a, &b, &c] { std::fs::create_dir_all(d).unwrap(); }
        std::fs::write(a.join(".cleanupstorages_id"), "vol-A").unwrap();
        std::fs::write(b.join(".cleanupstorages_id"), "vol-B").unwrap();
        // c has no marker

        let map = scan_mounts([a.clone(), b.clone(), c.clone()]);
        assert_eq!(map.get("vol-A"), Some(&a));
        assert_eq!(map.get("vol-B"), Some(&b));
        assert_eq!(map.len(), 2); // c skipped

        let r = MountResolver::Fixed(map);
        assert_eq!(r.resolve("vol-A"), Some(a));
        assert_eq!(r.resolve("vol-missing"), None);
    }
}

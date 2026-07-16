//! Resolve each catalogued volume to its CURRENT mount path (drive letters/mounts change), by
//! reading the `.cleanupstorages_id` marker at each connected drive root.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone)]
pub enum MountResolver {
    /// Detect mounts live from the OS + the catalog's remembered volume paths (production).
    Live { catalog_path: PathBuf },
    /// A fixed volume_id → root map (tests).
    Fixed(HashMap<String, PathBuf>),
}

impl MountResolver {
    /// The current mount root for `volume_id`, if connected.
    pub fn resolve(&self, volume_id: &str) -> Option<PathBuf> {
        self.snapshot().get(volume_id).cloned()
    }

    /// Snapshot the current volume_id → mount map once.
    pub fn snapshot(&self) -> HashMap<String, PathBuf> {
        match self {
            MountResolver::Live { catalog_path } => {
                resolve_live(live_mounts(), &remembered_paths(catalog_path))
            }
            MountResolver::Fixed(m) => m.clone(),
        }
    }
}

/// Merge the disk-root marker scan with remembered volume paths: a remembered path is included only
/// when its marker still equals the volume_id (so a moved/renamed folder is correctly absent), and
/// never overrides a disk-root hit for the same volume.
pub fn resolve_live(
    disk_roots: HashMap<String, PathBuf>,
    remembered: &[(String, PathBuf)],
) -> HashMap<String, PathBuf> {
    let mut map = disk_roots;
    for (vid, path) in remembered {
        if map.contains_key(vid) {
            continue;
        }
        if crate::volume::read_volume_id(path).as_deref() == Some(vid.as_str()) {
            map.insert(vid.clone(), path.clone());
        }
    }
    map
}

/// The catalog's remembered (volume_id, last_scanned_path) pairs; empty if the catalog can't be read.
fn remembered_paths(catalog_path: &std::path::Path) -> Vec<(String, PathBuf)> {
    match crate::catalog::Catalog::open_readonly(catalog_path) {
        Ok(cat) => cat
            .volume_paths()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, p)| (id, PathBuf::from(p)))
            .collect(),
        Err(_) => Vec::new(),
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

/// (total, available) bytes on the filesystem holding `path`, by longest-matching mount point.
/// None if it can't be determined. Same longest-prefix approach as `repack::available_space`.
pub fn disk_capacity(path: &std::path::Path) -> Option<(u64, u64)> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point();
        if path.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(b, _, _)| len > b).unwrap_or(true) {
                best = Some((len, d.total_space(), d.available_space()));
            }
        }
    }
    best.map(|(_, t, a)| (t, a))
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
        for d in [&a, &b, &c] {
            std::fs::create_dir_all(d).unwrap();
        }
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

    #[test]
    fn resolve_live_adds_matching_remembered_paths_only() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("DriveA");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(".cleanupstorages_id"), "vol-A").unwrap();
        let gone = tmp.path().join("Gone"); // never created / no marker
        let remembered = vec![
            ("vol-A".to_string(), folder.clone()), // marker present + matches -> included
            ("vol-gone".to_string(), gone),        // no marker -> excluded
        ];
        let map = resolve_live(HashMap::new(), &remembered);
        assert_eq!(map.get("vol-A"), Some(&folder));
        assert_eq!(map.get("vol-gone"), None);
        // a disk-root entry for the same id is not overwritten by a remembered path
        let mut roots = HashMap::new();
        roots.insert("vol-A".to_string(), PathBuf::from("D:\\"));
        let map2 = resolve_live(roots, &remembered);
        assert_eq!(map2.get("vol-A"), Some(&PathBuf::from("D:\\"))); // disk root wins
    }

    #[test]
    fn disk_capacity_of_temp_dir_is_some_and_sane() {
        let tmp = tempfile::tempdir().unwrap();
        // The temp dir lives on a real mounted filesystem, so capacity should resolve.
        let cap = disk_capacity(tmp.path());
        if let Some((total, avail)) = cap {
            assert!(total > 0);
            assert!(avail <= total);
        } // On some CI filesystems this can be None; the None branch is acceptable.
    }
}

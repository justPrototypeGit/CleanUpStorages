use std::io::Write;
use std::path::Path;

pub const MARKER: &str = ".cleanupstorages_id";
pub const QUARANTINE_DIR: &str = "_ToDelete";

#[derive(Debug, Clone)]
pub struct VolumeIdentity {
    pub volume_id: String,
    pub label: String,
    pub identified_by: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ReadonlyMode {
    Ask,
    Fingerprint,
    Skip,
}

/// Best-effort human label for the drive (its root folder name, else the path).
fn label_for(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// Total capacity (bytes) of the filesystem containing `root`, or 0 if unknown.
fn total_capacity(root: &Path) -> u64 {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None; // (mount path len, capacity)
    for d in disks.list() {
        let mp = d.mount_point();
        if root.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, d.total_space()));
            }
        }
    }
    best.map(|(_, c)| c).unwrap_or(0)
}

pub fn fingerprint(root: &Path) -> anyhow::Result<String> {
    let label = label_for(root);
    let cap = total_capacity(root);
    let mut hasher = blake3::Hasher::new();
    hasher.update(label.as_bytes());
    hasher.update(&cap.to_le_bytes());
    Ok(format!("fp-{}", &hasher.finalize().to_hex()[..24]))
}

fn read_marker(root: &Path) -> Option<String> {
    let p = root.join(MARKER);
    std::fs::read_to_string(&p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the drive's existing identity marker without creating one. None if absent/unreadable.
pub fn read_volume_id(root: &Path) -> Option<String> {
    read_marker(root)
}

fn try_write_marker(root: &Path) -> std::io::Result<String> {
    use std::fs::OpenOptions;
    let id = uuid::Uuid::new_v4().to_string();
    let p = root.join(MARKER);
    let mut f = OpenOptions::new().write(true).create_new(true).open(&p)?;
    f.write_all(id.as_bytes())?;
    f.sync_all()?;
    Ok(id)
}

/// Resolve the identity of the drive rooted at `root`. `None` = read-only drive skipped.
pub fn resolve(root: &Path, fallback: ReadonlyMode) -> anyhow::Result<Option<VolumeIdentity>> {
    let label = label_for(root);
    if let Some(existing) = read_marker(root) {
        return Ok(Some(VolumeIdentity {
            volume_id: existing,
            label,
            identified_by: "marker".into(),
        }));
    }
    match try_write_marker(root) {
        Ok(id) => Ok(Some(VolumeIdentity {
            volume_id: id,
            label,
            identified_by: "marker".into(),
        })),
        Err(_) => {
            let mode = match fallback {
                ReadonlyMode::Ask => prompt_readonly(root),
                other => other,
            };
            match mode {
                ReadonlyMode::Skip => Ok(None),
                _ => {
                    let fp = fingerprint(root)?;
                    Ok(Some(VolumeIdentity {
                        volume_id: fp,
                        label,
                        identified_by: "fingerprint".into(),
                    }))
                }
            }
        }
    }
}

/// Ask the user how to handle a read-only drive. Defaults to Fingerprint on non-interactive input.
fn prompt_readonly(root: &Path) -> ReadonlyMode {
    use std::io::{self, BufRead};
    eprintln!(
        "Drive at {} is read-only; cannot write identity marker.\n  [f] proceed read-only (fingerprint)  [s] skip  (default: f): ",
        root.display()
    );
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return ReadonlyMode::Fingerprint;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "s" | "skip" => ReadonlyMode::Skip,
        _ => ReadonlyMode::Fingerprint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_marker_and_reuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let id1 = resolve(tmp.path(), ReadonlyMode::Fingerprint).unwrap().unwrap();
        assert_eq!(id1.identified_by, "marker");
        // marker file now exists and second resolve returns the same id
        let id2 = resolve(tmp.path(), ReadonlyMode::Fingerprint).unwrap().unwrap();
        assert_eq!(id1.volume_id, id2.volume_id);
        assert_eq!(id2.identified_by, "marker");
    }

    #[test]
    fn fingerprint_is_stable_and_prefixed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fingerprint(tmp.path()).unwrap();
        let b = fingerprint(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("fp-"));
    }

    #[test]
    fn readonly_fingerprint_fallback_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_path = tmp.path().join(MARKER);

        // Create .cleanupstorages_id as a directory to block file writes
        std::fs::create_dir(&marker_path).unwrap();
        assert!(marker_path.is_dir());

        // Resolve should fall back to fingerprint and NOT write
        let identity = resolve(tmp.path(), ReadonlyMode::Fingerprint)
            .unwrap()
            .unwrap();

        assert_eq!(identity.identified_by, "fingerprint");
        assert!(identity.volume_id.starts_with("fp-"));

        // Marker should still be a directory (no file was written)
        assert!(marker_path.is_dir());
    }

    #[test]
    fn readonly_skip_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_path = tmp.path().join(MARKER);

        // Create .cleanupstorages_id as a directory to block file writes
        std::fs::create_dir(&marker_path).unwrap();
        assert!(marker_path.is_dir());

        // Resolve should return Ok(None) when asked to skip
        let result = resolve(tmp.path(), ReadonlyMode::Skip).unwrap();
        assert!(result.is_none());

        // Marker should still be a directory (no file was written)
        assert!(marker_path.is_dir());
    }
}

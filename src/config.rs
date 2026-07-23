use directories::ProjectDirs;
use std::path::PathBuf;

/// Runtime configuration. Defaults live on the computer, never on scanned drives.
pub struct Config {
    pub catalog_path: PathBuf,
    pub snapshot_retention: usize,
    pub max_archive_depth: usize,
    pub archive_entry_max_bytes: u64,
    pub archive_ratio_cap: u64,
    /// Nested-archive bytes held in memory at once across a whole descent. `archive_entry_max_bytes`
    /// bounds one level; without this a deep chain keeps every ancestor's buffer alive at once.
    pub archive_total_buffer_bytes: u64,
}

impl Config {
    /// Build a Config with default paths in the OS app-data directory.
    pub fn default_paths() -> anyhow::Result<Config> {
        if let Ok(dir) = std::env::var("CLEANUPSTORAGES_DATA_DIR") {
            let data_dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&data_dir)?;
            return Ok(Config {
                catalog_path: data_dir.join("catalog.db"),
                snapshot_retention: 10,
                max_archive_depth: 8,
                archive_entry_max_bytes: 2 * 1024 * 1024 * 1024,
                archive_ratio_cap: 200,
                archive_total_buffer_bytes: 2 * 1024 * 1024 * 1024,
            });
        }

        let dirs = ProjectDirs::from("dev", "justPrototype", "CleanUpStorages")
            .ok_or_else(|| anyhow::anyhow!("could not determine app data directory"))?;
        let data_dir = dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Config {
            catalog_path: data_dir.join("catalog.db"),
            snapshot_retention: 10,
            max_archive_depth: 8,
            archive_entry_max_bytes: 2 * 1024 * 1024 * 1024,
            archive_ratio_cap: 200,
            archive_total_buffer_bytes: 2 * 1024 * 1024 * 1024,
        })
    }

    /// Directory holding timestamped catalog snapshots (sibling of the DB file).
    pub fn backups_dir(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(|p| p.join("catalog.backups"))
            .unwrap_or_else(|| PathBuf::from("catalog.backups"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default_paths().unwrap();
        assert_eq!(cfg.snapshot_retention, 10);
        assert!(cfg.catalog_path.ends_with("catalog.db"));
        // backups dir is a sibling "catalog.backups" of the catalog file
        assert!(cfg.backups_dir().ends_with("catalog.backups"));
    }
}

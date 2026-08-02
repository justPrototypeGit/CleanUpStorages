use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Runtime configuration. Defaults live on the computer, never on scanned drives.
pub struct Config {
    pub catalog_path: PathBuf,
    pub snapshot_retention: usize,
    pub max_archive_depth: usize,
    pub archive_buffer_max_bytes: u64,
    /// Nested-archive bytes held in memory at once across a whole descent. `archive_buffer_max_bytes`
    /// bounds one level; without this a deep chain keeps every ancestor's buffer alive at once.
    pub archive_total_buffer_bytes: u64,
    /// None = unlimited. Leaf files stream in constant memory, so a ceiling here bounds time, not
    /// memory.
    pub archive_entry_max_bytes: Option<u64>,
    pub archive_ratio_cap: u64,
}

impl Config {
    /// Build a Config with default paths in the OS app-data directory.
    pub fn default_paths() -> anyhow::Result<Config> {
        if let Ok(dir) = std::env::var("CLEANUPSTORAGES_DATA_DIR") {
            let data_dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&data_dir)?;
            return Ok(Self::from_data_dir(data_dir));
        }

        let dirs = ProjectDirs::from("dev", "justPrototype", "CleanUpStorages")
            .ok_or_else(|| anyhow::anyhow!("could not determine app data directory"))?;
        let data_dir = dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self::from_data_dir(data_dir))
    }

    /// Build defaults for a given data dir, then apply any user overrides from `settings.json`.
    fn from_data_dir(data_dir: PathBuf) -> Config {
        let s = load_settings(&data_dir.join("settings.json"));
        Config {
            catalog_path: data_dir.join("catalog.db"),
            snapshot_retention: 10,
            max_archive_depth: s.max_archive_depth.unwrap_or(8),
            archive_buffer_max_bytes: s.archive_buffer_max_bytes.unwrap_or(2 * 1024 * 1024 * 1024),
            archive_total_buffer_bytes: s
                .archive_total_buffer_bytes
                .unwrap_or(2 * 1024 * 1024 * 1024),
            archive_entry_max_bytes: s
                .archive_entry_max_bytes
                .unwrap_or(Some(64 * 1024 * 1024 * 1024)),
            archive_ratio_cap: s.archive_ratio_cap.unwrap_or(10_000),
        }
    }

    /// Directory holding timestamped catalog snapshots (sibling of the DB file).
    pub fn backups_dir(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(|p| p.join("catalog.backups"))
            .unwrap_or_else(|| PathBuf::from("catalog.backups"))
    }

    /// Where user settings live: beside the catalog, never on a scanned drive.
    pub fn settings_path(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(|p| p.join("settings.json"))
            .unwrap_or_else(|| PathBuf::from("settings.json"))
    }
}

/// User-set overrides, read from `settings.json`. Every field is optional: absent means "use the
/// default", so a file written by an older build, or trimmed by hand, still works.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_archive_depth: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_buffer_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_total_buffer_bytes: Option<u64>,
    /// Outer `None` = not set. Inner `None` = the user explicitly chose unlimited.
    ///
    /// `deserialize_with` is REQUIRED here and is not decoration: serde maps a JSON `null` onto the
    /// OUTER `Option`, so a plain `Option<Option<u64>>` collapses "explicitly unlimited" into
    /// "unset" and the two become indistinguishable. `double_option` is only invoked when the key
    /// is present, so absent stays `None` while `null` becomes `Some(None)`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub archive_entry_max_bytes: Option<Option<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_ratio_cap: Option<u64>,
}

/// Distinguishes an absent key from an explicit `null`. See `archive_entry_max_bytes`.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

/// Read settings, falling back to defaults for anything unreadable.
///
/// **Never fails.** A missing file is normal; a corrupt one is a warning. Losing a preference is
/// acceptable, stopping a five-day scan is not.
pub fn load_settings(path: &Path) -> Settings {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            tracing::warn!(
                "could not read {}: {e}; using default limits",
                path.display()
            );
            return Settings::default();
        }
    };
    match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "{} is not valid settings JSON: {e}; using default limits",
                path.display()
            );
            Settings::default()
        }
    }
}

pub fn save_settings(path: &Path, s: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn defaults_are_sane() {
        let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = Config::default_paths().unwrap();
        assert_eq!(cfg.snapshot_retention, 10);
        assert!(cfg.catalog_path.ends_with("catalog.db"));
        // backups dir is a sibling "catalog.backups" of the catalog file
        assert!(cfg.backups_dir().ends_with("catalog.backups"));
    }

    #[test]
    fn a_missing_settings_file_yields_defaults_without_error() {
        let t = tempfile::tempdir().unwrap();
        let s = load_settings(&t.path().join("settings.json"));
        assert!(
            s.archive_ratio_cap.is_none(),
            "absent means 'use the default'"
        );
    }

    #[test]
    fn a_corrupt_settings_file_yields_defaults_rather_than_failing() {
        // A malformed preferences file must never be able to stop a five-day scan.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, b"{ this is not json at all ").unwrap();
        let s = load_settings(&p);
        assert!(s.archive_ratio_cap.is_none());
    }

    #[test]
    fn settings_round_trip_and_partial_files_are_honoured() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        // Only one field set; everything else must stay "use the default".
        std::fs::write(&p, br#"{"archive_ratio_cap": 5000}"#).unwrap();
        let s = load_settings(&p);
        assert_eq!(s.archive_ratio_cap, Some(5000));
        assert!(s.max_archive_depth.is_none());

        let written = Settings {
            archive_ratio_cap: Some(1234),
            ..Default::default()
        };
        save_settings(&p, &written).unwrap();
        assert_eq!(load_settings(&p).archive_ratio_cap, Some(1234));
    }

    #[test]
    fn an_explicitly_unlimited_leaf_ceiling_survives_a_round_trip() {
        // Some(None) means "the user chose unlimited" and must not collapse into "unset" -- serde
        // maps a JSON null onto the OUTER Option unless double_option intervenes, so this test is
        // what proves that attribute is present and working.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        save_settings(
            &p,
            &Settings {
                archive_entry_max_bytes: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, Some(None));

        // And an unset field must NOT come back as "explicitly unlimited".
        save_settings(&p, &Settings::default()).unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, None);
    }

    #[test]
    fn settings_override_the_defaults_in_config() {
        let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let t = tempfile::tempdir().unwrap();
        std::env::set_var("CLEANUPSTORAGES_DATA_DIR", t.path());
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_ratio_cap": 777, "max_archive_depth": 3}"#).unwrap();
        let cfg = Config::default_paths().unwrap();
        std::env::remove_var("CLEANUPSTORAGES_DATA_DIR");
        assert_eq!(cfg.archive_ratio_cap, 777);
        assert_eq!(cfg.max_archive_depth, 3);
        assert_eq!(
            cfg.archive_buffer_max_bytes,
            2 * 1024 * 1024 * 1024,
            "unset fields keep the default"
        );
    }
}

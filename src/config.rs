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
    pub archive_deny_extensions: Vec<String>,
    pub archive_allow_extensions: Vec<String>,
}

/// Zip-format files that are documents or packages, not archives worth exploding into parts.
/// Extending this needs no release -- it is editable in settings.json and on the Scan page.
pub(crate) const DEFAULT_DENY: &[&str] = &[
    "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "jar", "apk", "war", "ear", "epub", "odt",
    "ods", "odp", "nupkg", "vsix", "ipa",
];

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
            archive_deny_extensions: s
                .archive_deny_extensions
                .unwrap_or_else(|| DEFAULT_DENY.iter().map(|s| s.to_string()).collect()),
            archive_allow_extensions: s.archive_allow_extensions.unwrap_or_default(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_deny_extensions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_allow_extensions: Option<Vec<String>>,
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
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "{} is not valid settings JSON: {e}; using default limits",
                path.display()
            );
            return Settings::default();
        }
    };
    // A settings file must be a JSON object. Anything else (array, string, number, bool, null) is
    // technically valid JSON that serde's struct deserializer would otherwise accept by silently
    // assigning fields positionally (e.g. `[1,2,3]` -> max_archive_depth: 1, ...), which is not a
    // parse error but is not what the user meant either. Treat it the same as corrupt JSON.
    if !value.is_object() {
        tracing::warn!(
            "{} is not a JSON object (settings must be a top-level object); using default limits",
            path.display()
        );
        return Settings::default();
    }
    let mut s: Settings = match serde_json::from_value(value) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "{} is not valid settings JSON: {e}; using default limits",
                path.display()
            );
            return Settings::default();
        }
    };
    // Per field, never the whole file: a bad preference is a warning, a stopped five-day scan is not.
    drop_out_of_range(&mut s, path);
    s
}

/// Range rules for the archive limits, in ONE place.
///
/// Applied both when `settings.json` is read and at the HTTP boundary. The two used to differ, so a
/// hand-edited file could set a 0-byte entry ceiling that the UI would have refused -- and a 0-byte
/// ceiling converts present files to `missing`.
///
/// Returns `(field, reason)` for each out-of-range field; empty when everything is fine.
pub fn check_ranges(s: &Settings) -> Vec<(&'static str, String)> {
    let mut bad = Vec::new();
    if matches!(s.max_archive_depth, Some(0)) {
        bad.push(("max_archive_depth", "must be at least 1".to_string()));
    }
    if matches!(s.archive_ratio_cap, Some(0)) {
        bad.push(("archive_ratio_cap", "must be at least 1".to_string()));
    }
    if matches!(s.archive_buffer_max_bytes, Some(0)) {
        bad.push(("archive_buffer_max_bytes", "must be at least 1".to_string()));
    }
    if matches!(s.archive_total_buffer_bytes, Some(0)) {
        bad.push((
            "archive_total_buffer_bytes",
            "must be at least 1".to_string(),
        ));
    }
    // Some(Some(0)) is a zero ceiling; Some(None) is "explicitly unlimited" and is fine.
    if matches!(s.archive_entry_max_bytes, Some(Some(0))) {
        bad.push((
            "archive_entry_max_bytes",
            "must be at least 1; unlimited is null, not 0".to_string(),
        ));
    }
    if let (Some(per), Some(total)) = (s.archive_buffer_max_bytes, s.archive_total_buffer_bytes) {
        if per > total {
            bad.push((
                "archive_buffer_max_bytes",
                format!(
                    "{per} exceeds archive_total_buffer_bytes ({total}); a per-archive bound \
                         larger than the whole descent's budget has no effect"
                ),
            ));
        }
    }
    bad
}

/// Clear every field `check_ranges` rejected, so it falls back to the compiled-in default.
fn drop_out_of_range(s: &mut Settings, where_: &Path) {
    for (field, reason) in check_ranges(s) {
        tracing::warn!("{}: {field} {reason}; using the default", where_.display());
        match field {
            "max_archive_depth" => s.max_archive_depth = None,
            "archive_ratio_cap" => s.archive_ratio_cap = None,
            "archive_buffer_max_bytes" => s.archive_buffer_max_bytes = None,
            "archive_total_buffer_bytes" => s.archive_total_buffer_bytes = None,
            "archive_entry_max_bytes" => s.archive_entry_max_bytes = None,
            _ => {}
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

/// Serializes tests that mutate `CLEANUPSTORAGES_DATA_DIR` (a process-global env var) so
/// concurrent test threads in this binary -- including ones in `web.rs` -- never race on it and
/// momentarily fall through to the real app-data directory.
#[cfg(test)]
pub(crate) static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_top_level_array_yields_defaults_rather_than_positional_assignment() {
        // Without an is_object() guard, serde's struct deserializer accepts a JSON array and
        // assigns fields POSITIONALLY: [1,2,3] -> max_archive_depth: 1, archive_buffer_max_bytes: 2
        // (TWO BYTES), archive_total_buffer_bytes: 3 (THREE BYTES). That is valid JSON and not a
        // parse error, so it must be caught separately and treated like corrupt JSON.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, b"[1,2,3]").unwrap();
        let s = load_settings(&p);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn a_top_level_string_yields_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#""hello""#).unwrap();
        let s = load_settings(&p);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn a_top_level_number_yields_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, b"42").unwrap();
        let s = load_settings(&p);
        assert_eq!(s, Settings::default());
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
    fn a_zero_entry_ceiling_in_the_file_is_refused_and_falls_back() {
        // Confirmed live during the #41/#42 review: a 0-byte ceiling marks present files `missing`.
        // A preferences file must not be able to do that.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(
            &p,
            br#"{"archive_entry_max_bytes": 0, "archive_ratio_cap": 5000}"#,
        )
        .unwrap();
        let s = load_settings(&p);
        assert_eq!(
            s.archive_entry_max_bytes, None,
            "the bad field falls back to the default"
        );
        assert_eq!(
            s.archive_ratio_cap,
            Some(5000),
            "a VALID field beside it must survive"
        );
    }

    #[test]
    fn a_zero_buffer_in_the_file_is_refused_and_falls_back() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(
            &p,
            br#"{"archive_buffer_max_bytes": 0, "max_archive_depth": 3}"#,
        )
        .unwrap();
        let s = load_settings(&p);
        assert_eq!(s.archive_buffer_max_bytes, None);
        assert_eq!(s.max_archive_depth, Some(3));
    }

    #[test]
    fn an_explicit_null_ceiling_is_still_unlimited_not_out_of_range() {
        // `null` means "the user chose unlimited" and must survive the range check untouched.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_entry_max_bytes": null}"#).unwrap();
        assert_eq!(load_settings(&p).archive_entry_max_bytes, Some(None));
    }

    #[test]
    fn a_buffer_larger_than_the_total_budget_is_refused_at_load() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("settings.json");
        std::fs::write(
            &p,
            br#"{"archive_buffer_max_bytes": 4000, "archive_total_buffer_bytes": 1000}"#,
        )
        .unwrap();
        let s = load_settings(&p);
        // The pair is inconsistent, so the per-archive bound is the one dropped: the total budget
        // is the harder ceiling and keeping it is the safer of the two.
        assert_eq!(s.archive_buffer_max_bytes, None);
        assert_eq!(s.archive_total_buffer_bytes, Some(1000));
    }

    #[test]
    fn settings_override_the_defaults_in_config() {
        let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let t = tempfile::tempdir().unwrap();
        // F3: save and restore whatever was there before, rather than unconditionally removing the
        // var. `remove_var` unconditionally is exactly what broke the documented mitigation for the
        // known `snapshot_best_effort` issue ("set CLEANUPSTORAGES_DATA_DIR before running the
        // suite") -- if a caller had already scoped the var to a throwaway dir before this test ran,
        // an unconditional remove would fall the rest of the suite through to the user's real
        // app-data directory the moment this test finished.
        let prev = std::env::var("CLEANUPSTORAGES_DATA_DIR").ok();
        std::env::set_var("CLEANUPSTORAGES_DATA_DIR", t.path());
        let p = t.path().join("settings.json");
        std::fs::write(&p, br#"{"archive_ratio_cap": 777, "max_archive_depth": 3}"#).unwrap();
        let cfg = Config::default_paths().unwrap();
        match prev {
            Some(v) => std::env::set_var("CLEANUPSTORAGES_DATA_DIR", v),
            None => std::env::remove_var("CLEANUPSTORAGES_DATA_DIR"),
        }
        assert_eq!(cfg.archive_ratio_cap, 777);
        assert_eq!(cfg.max_archive_depth, 3);
        assert_eq!(
            cfg.archive_buffer_max_bytes,
            2 * 1024 * 1024 * 1024,
            "unset fields keep the default"
        );
    }
}

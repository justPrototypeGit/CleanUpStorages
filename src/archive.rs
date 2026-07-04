//! Reading into zip archives (recursively) to catalog their contents.

use crate::config::Config;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

pub use crate::category::Category;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus { Active, Missing, Quarantined, Purged }

impl FileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileStatus::Active => "active",
            FileStatus::Missing => "missing",
            FileStatus::Quarantined => "quarantined",
            FileStatus::Purged => "purged",
        }
    }
    pub fn from_db(s: &str) -> FileStatus {
        match s {
            "missing" => FileStatus::Missing,
            "quarantined" => FileStatus::Quarantined,
            "purged" => FileStatus::Purged,
            _ => FileStatus::Active,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Volume {
    pub volume_id: String,
    pub label: String,
    /// "marker" or "fingerprint".
    pub identified_by: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

/// A file the scanner found, ready to upsert. No DB id yet.
#[derive(Debug, Clone)]
pub struct NewFile {
    pub volume_id: String,
    pub relative_path: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub created_time: Option<i64>,
    pub modified_time: Option<i64>,
    pub accessed_time: Option<i64>,
    pub category: Category,
    /// None for loose files. Reserved for archive entries in a later plan.
    pub container_chain: Option<String>,
}

/// A file row as stored, including identity and lifecycle.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub volume_id: String,
    pub relative_path: String,
    pub filename: String,
    pub extension: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub created_time: Option<i64>,
    pub modified_time: Option<i64>,
    pub accessed_time: Option<i64>,
    pub category: Category,
    pub container_chain: Option<String>,
    pub status: FileStatus,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub original_path: Option<String>,
}

impl FileRecord {
    /// Human location: origin path if quarantined (original_path), else current relative_path,
    /// with the archive container chain appended for archived entries.
    pub fn display_location(&self) -> String {
        let base = self.original_path.as_deref().unwrap_or(&self.relative_path);
        match &self.container_chain {
            Some(chain) => format!("{base} › {chain}"),
            None => base.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip() {
        for s in [FileStatus::Active, FileStatus::Missing, FileStatus::Quarantined, FileStatus::Purged] {
            assert_eq!(FileStatus::from_db(s.as_str()), s);
        }
        // unknown falls back to Active defensively but is logged elsewhere
        assert_eq!(FileStatus::from_db("weird"), FileStatus::Active);
    }
}

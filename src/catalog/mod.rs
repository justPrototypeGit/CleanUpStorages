pub mod backup;
pub mod dedup;
pub mod models;
pub mod schema;
pub mod store;

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// An open handle to the catalog database.
pub struct Catalog {
    pub conn: Connection,
}

impl Catalog {
    /// Open (creating if needed) the catalog at `path`, enabling WAL and the schema.
    pub fn open(path: &Path) -> anyhow::Result<Catalog> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::apply(&conn)?;
        Ok(Catalog { conn })
    }

    /// Open the catalog READ-ONLY (no directory creation, no schema DDL, no WAL switch).
    /// The file must already exist. For query-only consumers like the browse server.
    pub fn open_readonly(path: &Path) -> anyhow::Result<Catalog> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Catalog { conn })
    }

    /// Run PRAGMA integrity_check; true if the DB reports "ok".
    pub fn integrity_ok(&self) -> anyhow::Result<bool> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result == "ok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_readonly_reads_existing_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        {
            let cat = Catalog::open(&db).unwrap();
            cat.upsert_volume(&crate::catalog::models::Volume {
                volume_id: "vol-1".into(),
                label: "Test HDD".into(),
                identified_by: "marker".into(),
                first_seen_at: 1,
                last_seen_at: 1,
            })
            .unwrap();
        } // dropped: closes the write handle

        let ro = Catalog::open_readonly(&db).unwrap();
        let stats = ro.volume_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "vol-1");
    }
}

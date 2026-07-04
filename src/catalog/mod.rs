pub mod models;
pub mod schema;
pub mod store;
pub mod backup;

use std::path::Path;
use rusqlite::Connection;

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
        schema::apply(&conn)?;
        Ok(Catalog { conn })
    }

    /// Run PRAGMA integrity_check; true if the DB reports "ok".
    pub fn integrity_ok(&self) -> anyhow::Result<bool> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result == "ok")
    }
}

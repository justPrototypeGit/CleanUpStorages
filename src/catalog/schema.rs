use rusqlite::Connection;

/// Create all tables and indexes if they do not exist.
pub fn apply(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS volumes (
            volume_id     TEXT PRIMARY KEY,
            label         TEXT NOT NULL,
            identified_by TEXT NOT NULL,
            first_seen_at INTEGER NOT NULL,
            last_seen_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS files (
            id             INTEGER PRIMARY KEY,
            volume_id      TEXT NOT NULL REFERENCES volumes(volume_id),
            relative_path  TEXT NOT NULL,
            filename       TEXT NOT NULL,
            extension      TEXT NOT NULL,
            size_bytes     INTEGER NOT NULL,
            content_hash   TEXT NOT NULL,
            created_time   INTEGER,
            modified_time  INTEGER,
            accessed_time  INTEGER,
            category       TEXT NOT NULL,
            container_chain TEXT,
            status         TEXT NOT NULL,
            first_seen_at  INTEGER NOT NULL,
            last_seen_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
        CREATE INDEX IF NOT EXISTS idx_files_volume ON files(volume_id);
        CREATE INDEX IF NOT EXISTS idx_files_status ON files(status);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_files_loose_identity
            ON files(volume_id, relative_path) WHERE container_chain IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_files_archived_identity
            ON files(volume_id, relative_path, container_chain) WHERE container_chain IS NOT NULL;

        CREATE TABLE IF NOT EXISTS scan_errors (
            id         INTEGER PRIMARY KEY,
            volume_id  TEXT,
            path       TEXT NOT NULL,
            reason     TEXT NOT NULL,
            occurred_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS actions_log (
            id          INTEGER PRIMARY KEY,
            action      TEXT NOT NULL,
            details     TEXT,
            occurred_at INTEGER NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts
            USING fts5(filename, relative_path, content='files', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, filename, relative_path)
            VALUES (new.id, new.filename, new.relative_path);
        END;
        CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path)
            VALUES('delete', old.id, old.filename, old.relative_path);
        END;
        CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path)
            VALUES('delete', old.id, old.filename, old.relative_path);
            INSERT INTO files_fts(rowid, filename, relative_path)
            VALUES (new.id, new.filename, new.relative_path);
        END;
        "#,
    )
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;

    #[test]
    fn open_creates_schema_and_passes_integrity() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let cat = Catalog::open(&db).unwrap();
        assert!(cat.integrity_ok().unwrap());
        // WAL mode is active
        let mode: String = cat.conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // core tables exist
        let count: i64 = cat.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('volumes','files','scan_errors','actions_log')",
            [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4);
    }
}

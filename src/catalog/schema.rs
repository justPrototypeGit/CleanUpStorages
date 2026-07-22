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
            last_seen_at   INTEGER NOT NULL,
            original_path  TEXT
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
    )?;
    // The single definition of "loose active duplicate + which copy is kept". Built from
    // dedup::KEEP_ORDER so the rule lives in exactly one place. DROP-then-CREATE makes it
    // self-migrating: an existing database picks up a changed KEEP_ORDER on next open.
    conn.execute_batch(&format!(
        r#"
        DROP VIEW IF EXISTS dup_loose;
        CREATE VIEW dup_loose AS
        SELECT id, volume_id, content_hash, size_bytes,
               ROW_NUMBER() OVER (PARTITION BY content_hash ORDER BY {order}) AS rn,
               COUNT(*)     OVER (PARTITION BY content_hash)                  AS copies
        FROM files
        WHERE status = 'active' AND container_chain IS NULL;
        "#,
        order = crate::catalog::dedup::KEEP_ORDER
    ))?;
    ensure_column(conn, "files", "original_path", "TEXT")?;
    ensure_column(conn, "volumes", "last_scanned_path", "TEXT")?;
    ensure_column(conn, "volumes", "display_name", "TEXT")?;
    ensure_column(conn, "volumes", "description", "TEXT")?;
    Ok(())
}

/// Add `<table>.<column> <decl>` if it does not already exist (idempotent, data-preserving).
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |r| r.get(0),
    )?;
    if exists == 0 {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
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
        let mode: String = cat
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // core tables exist
        let count: i64 = cat.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('volumes','files','scan_errors','actions_log')",
            [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4);
    }

    /// Seed `n` rows sharing patterns needed by the dup_loose tests.
    fn seed_view_fixture(cat: &Catalog, rows: &str) {
        cat.conn
            .execute_batch(&format!(
                "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
                     VALUES ('v','V','marker',1,1);
                 INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,
                     content_hash,created_time,modified_time,accessed_time,category,
                     container_chain,status,first_seen_at,last_seen_at) VALUES {rows};"
            ))
            .unwrap();
    }

    #[test]
    fn dup_loose_view_flags_keep_and_counts_loose_copies_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        seed_view_fixture(
            &cat,
            "('v','new.txt','new.txt','txt',10,'H',200,200,NULL,'other',NULL,'active',1,1),
             ('v','old.txt','old.txt','txt',10,'H',100,100,NULL,'other',NULL,'active',1,1),
             ('v','solo.txt','solo.txt','txt',10,'U',100,100,NULL,'other',NULL,'active',1,1),
             ('v','in.zip','x.txt','txt',10,'H',100,100,NULL,'other','x.txt','active',1,1)",
        );

        let keep: String = cat
            .conn
            .query_row(
                "SELECT relative_path FROM files
             WHERE id=(SELECT id FROM dup_loose WHERE content_hash='H' AND rn=1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(keep, "old.txt");

        let copies: i64 = cat
            .conn
            .query_row(
                "SELECT DISTINCT copies FROM dup_loose WHERE content_hash='H'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(copies, 2, "archived row must not count toward loose copies");

        let solo: i64 = cat
            .conn
            .query_row(
                "SELECT copies FROM dup_loose WHERE content_hash='U'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(solo, 1);
    }

    #[test]
    fn dup_loose_sorts_null_timestamps_last() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        seed_view_fixture(
            &cat,
            "('v','nulls.txt','nulls.txt','txt',10,'H',NULL,NULL,NULL,'other',NULL,'active',1,1),
             ('v','dated.txt','dated.txt','txt',10,'H',500,500,NULL,'other',NULL,'active',1,1)",
        );
        let keep: String = cat
            .conn
            .query_row(
                "SELECT relative_path FROM files
             WHERE id=(SELECT id FROM dup_loose WHERE content_hash='H' AND rn=1)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            keep, "dated.txt",
            "a NULL timestamp must never win the keep slot"
        );
    }

    #[test]
    fn migration_adds_original_path_to_preexisting_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        // Simulate an OLD catalog created WITHOUT original_path.
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE files (id INTEGER PRIMARY KEY, volume_id TEXT NOT NULL,
                    relative_path TEXT NOT NULL, filename TEXT NOT NULL, extension TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL, content_hash TEXT NOT NULL, created_time INTEGER,
                    modified_time INTEGER, accessed_time INTEGER, category TEXT NOT NULL,
                    container_chain TEXT, status TEXT NOT NULL, first_seen_at INTEGER NOT NULL,
                    last_seen_at INTEGER NOT NULL);",
            )
            .unwrap();
        }
        // Opening through Catalog must migrate it in, not fail.
        let cat = crate::catalog::Catalog::open(&db).unwrap();
        let has_col: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('files') WHERE name='original_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_col, 1);
        assert!(cat.integrity_ok().unwrap());
    }
}

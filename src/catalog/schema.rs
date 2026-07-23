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

        CREATE TABLE IF NOT EXISTS scan_runs (
            id              INTEGER PRIMARY KEY,
            volume_id       TEXT,
            root_path       TEXT NOT NULL,
            started_at      INTEGER NOT NULL,
            finished_at     INTEGER,
            wall_ms         INTEGER,
            forced          INTEGER NOT NULL,
            status          TEXT NOT NULL,
            error_message   TEXT,
            files_seen      INTEGER NOT NULL DEFAULT 0,
            hashed          INTEGER NOT NULL DEFAULT 0,
            skipped         INTEGER NOT NULL DEFAULT 0,
            errors          INTEGER NOT NULL DEFAULT 0,
            archive_entries INTEGER NOT NULL DEFAULT 0,
            bytes_hashed    INTEGER NOT NULL DEFAULT 0,
            bytes_skipped   INTEGER NOT NULL DEFAULT 0,
            walk_ms         INTEGER NOT NULL DEFAULT 0,
            skip_check_ms   INTEGER NOT NULL DEFAULT 0,
            hash_ms         INTEGER NOT NULL DEFAULT 0,
            db_write_ms     INTEGER NOT NULL DEFAULT 0,
            archive_ms      INTEGER NOT NULL DEFAULT 0,
            size_histogram  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_scan_runs_started ON scan_runs(started_at DESC);

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts
            USING fts5(filename, relative_path, container_chain,
                       content='files', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
            VALUES (new.id, new.filename, new.relative_path, new.container_chain);
        END;
        CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path, container_chain)
            VALUES('delete', old.id, old.filename, old.relative_path, old.container_chain);
        END;
        CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path, container_chain)
            VALUES('delete', old.id, old.filename, old.relative_path, old.container_chain);
            INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
            VALUES (new.id, new.filename, new.relative_path, new.container_chain);
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
    rebuild_fts_if_stale(conn)?;
    ensure_column(conn, "files", "original_path", "TEXT")?;
    ensure_column(conn, "volumes", "last_scanned_path", "TEXT")?;
    ensure_column(conn, "volumes", "display_name", "TEXT")?;
    ensure_column(conn, "volumes", "description", "TEXT")?;
    Ok(())
}

/// Bring a pre-existing `files_fts` up to the current column set.
///
/// `CREATE VIRTUAL TABLE IF NOT EXISTS` leaves an older index alone, so a catalog built before
/// `container_chain` was indexed would keep searching only filename+path forever. An FTS index is
/// derived data — dropping and rebuilding it from `files` cannot lose anything.
///
/// Guarded on the column actually being absent: the rebuild walks every row, which is seconds on a
/// million-file catalog and must not happen on every open.
fn rebuild_fts_if_stale(conn: &Connection) -> rusqlite::Result<()> {
    let has_chain: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('files_fts') WHERE name='container_chain'",
        [],
        |r| r.get(0),
    )?;
    // Second condition: an index with the right columns but no rows, over a non-empty catalog.
    // The column check alone cannot see that state — and it is reachable, because `apply`'s
    // CREATE VIRTUAL TABLE IF NOT EXISTS above would recreate a dropped index with the current
    // columns and no content. Checking emptiness makes the repair self-healing whatever the cause.
    // Probed via the fts5 shadow table, not `SELECT ... FROM files_fts`: on an external-content
    // index an unindexed scan falls through to the content table, so it reports rows even when the
    // index is empty. `files_fts_docsize` holds one row per indexed document and tells the truth.
    // If that table is somehow unavailable, assume healthy — a wrong guess here means a needless
    // multi-second rebuild on every open.
    let empty_over_rows = has_chain > 0 && {
        let indexed: i64 = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM files_fts_docsize)", [], |r| {
                r.get(0)
            })
            .unwrap_or(1);
        let rows: i64 = conn.query_row("SELECT EXISTS(SELECT 1 FROM files)", [], |r| r.get(0))?;
        indexed == 0 && rows == 1
    };
    if has_chain > 0 && !empty_over_rows {
        return Ok(());
    }
    // All-or-nothing: without the transaction a crash between the DROP and the rebuild would leave
    // a catalog whose index looks migrated (the column is there) but is empty, which the guard
    // above would then skip forever.
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        DROP TRIGGER IF EXISTS files_ai;
        DROP TRIGGER IF EXISTS files_ad;
        DROP TRIGGER IF EXISTS files_au;
        DROP TABLE IF EXISTS files_fts;

        CREATE VIRTUAL TABLE files_fts
            USING fts5(filename, relative_path, container_chain,
                       content='files', content_rowid='id');
        INSERT INTO files_fts(files_fts) VALUES('rebuild');

        CREATE TRIGGER files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
            VALUES (new.id, new.filename, new.relative_path, new.container_chain);
        END;
        CREATE TRIGGER files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path, container_chain)
            VALUES('delete', old.id, old.filename, old.relative_path, old.container_chain);
        END;
        CREATE TRIGGER files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, filename, relative_path, container_chain)
            VALUES('delete', old.id, old.filename, old.relative_path, old.container_chain);
            INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
            VALUES (new.id, new.filename, new.relative_path, new.container_chain);
        END;
        COMMIT;
        "#,
    )
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
    fn an_old_two_column_fts_is_rebuilt_with_container_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        {
            // A catalog whose FTS predates container_chain indexing.
            let cat = Catalog::open(&db).unwrap();
            cat.conn
                .execute_batch(
                    "DROP TRIGGER files_ai; DROP TRIGGER files_ad; DROP TRIGGER files_au;
                     DROP TABLE files_fts;
                     CREATE VIRTUAL TABLE files_fts
                        USING fts5(filename, relative_path, content='files', content_rowid='id');
                     INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
                        VALUES ('v','V','marker',1,1);
                     INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,
                        content_hash,created_time,modified_time,accessed_time,category,
                        container_chain,status,first_seen_at,last_seen_at)
                     VALUES ('v','backups/old.zip','vacation.jpg','jpg',9,'h',1,1,NULL,'photo',
                             'photos.zip › vacation.jpg','active',1,1);
                     INSERT INTO files_fts(files_fts) VALUES('rebuild');",
                )
                .unwrap();
            let n: i64 = cat
                .conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('files_fts') WHERE name='container_chain'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "fixture must start on the old shape");
        }

        // Re-opening migrates the index and backfills it from the rows already present.
        let cat = Catalog::open(&db).unwrap();
        let hits = cat.search("photos", None, None, None).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "an intermediate archive name must be searchable after the rebuild"
        );
        assert_eq!(hits[0].filename, "vacation.jpg");
        assert!(cat.integrity_ok().unwrap());
    }

    #[test]
    fn an_index_with_the_right_columns_but_no_rows_is_repaired() {
        // The state a crash mid-migration leaves behind: the index was dropped, then recreated by
        // CREATE VIRTUAL TABLE IF NOT EXISTS with the current columns and no content. The column
        // check alone would call that migrated and skip it forever, silently unsearchable.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        {
            let cat = Catalog::open(&db).unwrap();
            cat.conn
                .execute_batch(
                    "INSERT INTO volumes(volume_id,label,identified_by,first_seen_at,last_seen_at)
                        VALUES ('v','V','marker',1,1);
                     INSERT INTO files(volume_id,relative_path,filename,extension,size_bytes,
                        content_hash,created_time,modified_time,accessed_time,category,
                        container_chain,status,first_seen_at,last_seen_at)
                     VALUES ('v','backups/old.zip','vacation.jpg','jpg',9,'h',1,1,NULL,'photo',
                             'photos.zip › vacation.jpg','active',1,1);
                     DELETE FROM files_fts;",
                )
                .unwrap();
            assert!(
                cat.search("photos", None, None, None).unwrap().is_empty(),
                "fixture must start with an empty index"
            );
        }

        let cat = Catalog::open(&db).unwrap();
        assert_eq!(
            cat.search("photos", None, None, None).unwrap().len(),
            1,
            "an empty index over a non-empty catalog must be rebuilt, not accepted"
        );
    }

    #[test]
    fn an_empty_catalog_is_not_rebuilt_on_every_open() {
        // No files at all is a legitimately empty index -- it must not look like damage.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let cat = Catalog::open(&db).unwrap();
        cat.conn
            .execute_batch(
                "INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
                 VALUES (999999, 'sentinel', 'sentinel', '');",
            )
            .unwrap();
        drop(cat);

        let cat = Catalog::open(&db).unwrap();
        let n: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM files_fts WHERE files_fts MATCH 'sentinel'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "an empty catalog must not trigger a rebuild");
    }

    #[test]
    fn rebuilding_is_skipped_once_the_index_is_current() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("c.db");
        let cat = Catalog::open(&db).unwrap();
        // Sentinel: a row inserted straight into the index would be wiped by an unnecessary rebuild
        // (the content table has no such row).
        cat.conn
            .execute_batch(
                "INSERT INTO files_fts(rowid, filename, relative_path, container_chain)
                 VALUES (999999, 'sentinel', 'sentinel', '');",
            )
            .unwrap();
        drop(cat);

        let cat = Catalog::open(&db).unwrap();
        let n: i64 = cat
            .conn
            .query_row(
                "SELECT count(*) FROM files_fts WHERE files_fts MATCH 'sentinel'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "a current index must not be rebuilt on every open");
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

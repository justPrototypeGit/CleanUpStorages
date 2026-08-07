use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Snapshot a catalogue into ITS OWN sibling `catalog.backups`, keeping the newest few.
///
/// Prefer this over calling `snapshot` directly. Taking only the catalogue path makes the
/// destination impossible to get wrong, which matters because getting it wrong is silent and
/// destructive: two separate call sites (#44) passed a backups directory resolved from the ambient
/// configuration, so a process working on one catalogue wrote snapshots beside a different one --
/// in practice, `cargo test` evicting a user's genuine pre-migration snapshots.
pub fn snapshot_beside(catalog_path: &Path, now: i64) -> anyhow::Result<PathBuf> {
    snapshot(
        catalog_path,
        &crate::config::backups_dir_for(catalog_path),
        crate::config::DEFAULT_SNAPSHOT_RETENTION,
        now,
    )
}

/// Copy the live catalog to a timestamped snapshot, then keep only the newest `retention`.
pub fn snapshot(
    catalog_path: &Path,
    backups_dir: &Path,
    retention: usize,
    now: i64,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(backups_dir)?;
    let dest = backups_dir.join(format!("catalog-{now}.db"));

    let src = Connection::open_with_flags(catalog_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut dst = Connection::open(&dest)?;
    let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
    backup.run_to_completion(64, std::time::Duration::from_millis(0), None)?;
    drop(backup);

    prune(backups_dir, retention)?;
    Ok(dest)
}

fn prune(backups_dir: &Path, retention: usize) -> anyhow::Result<()> {
    let mut snaps: Vec<PathBuf> = std::fs::read_dir(backups_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "db").unwrap_or(false))
        .collect();
    // Sort by filename (embeds the timestamp), newest last.
    snaps.sort();
    if snaps.len() > retention {
        for old in &snaps[..snaps.len() - retention] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn prune_holds_the_retention_bound_over_many_snapshots() {
        // The issue reports 30 files against a retention of 10. Reproduce before fixing: if the
        // bound holds here, the accumulation came from somewhere other than prune's own logic.
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let backups = tmp.path().join("catalog.backups");
        {
            Catalog::open(&db).unwrap();
        }
        for t in 1..=30 {
            snapshot(&db, &backups, 10, t).unwrap();
        }
        let kept = std::fs::read_dir(&backups).unwrap().count();
        assert_eq!(kept, 10, "retention bound must hold; found {kept} files");
    }

    #[test]
    fn snapshot_creates_file_and_prunes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("catalog.db");
        let backups = tmp.path().join("catalog.backups");
        {
            Catalog::open(&db).unwrap();
        } // create a real DB

        let mut made = Vec::new();
        for t in 1..=3 {
            made.push(snapshot(&db, &backups, 2, t).unwrap());
        }
        // retention=2 keeps only the two newest
        let kept: Vec<_> = std::fs::read_dir(&backups)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|p| p == made.last().unwrap()));
        assert!(!kept.iter().any(|p| p == &made[0])); // oldest pruned
    }
}

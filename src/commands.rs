use std::path::Path;
use clap::ValueEnum;

use crate::config::Config;
use crate::catalog::Catalog;
use crate::catalog::models::{Volume, FileStatus};
use crate::volume::{self, ReadonlyMode};
use crate::scanner;
use crate::catalog::backup;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadonlyFallback { Ask, Fingerprint, Skip }

impl From<ReadonlyFallback> for ReadonlyMode {
    fn from(f: ReadonlyFallback) -> Self {
        match f {
            ReadonlyFallback::Ask => ReadonlyMode::Ask,
            ReadonlyFallback::Fingerprint => ReadonlyMode::Fingerprint,
            ReadonlyFallback::Skip => ReadonlyMode::Skip,
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub fn cmd_scan(path: &Path, force: bool, fallback: ReadonlyFallback) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }

    let identity = match volume::resolve(path, fallback.into())? {
        Some(id) => id,
        None => { println!("Skipped read-only drive at {}", path.display()); return Ok(()); }
    };
    let now = now_secs();
    cat.upsert_volume(&Volume {
        volume_id: identity.volume_id.clone(),
        label: identity.label.clone(),
        identified_by: identity.identified_by.clone(),
        first_seen_at: now, last_seen_at: now,
    })?;

    println!("Scanning {} (volume {}, id by {})...",
        path.display(), identity.label, identity.identified_by);
    let s = scanner::scan_volume(&cat, path, &identity, force, now)?;
    println!("Done: {} hashed, {} unchanged, {} errors, {} newly missing.",
        s.hashed, s.skipped, s.errors, s.marked_missing);

    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_search(query: &str, category: Option<&str>, volume: Option<&str>, status: Option<&str>)
    -> anyhow::Result<()>
{
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let hits = cat.search(query, category, volume, status)?;
    if hits.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    for f in &hits {
        let flag = match f.status {
            FileStatus::Active => "",
            FileStatus::Missing => "  [MISSING]",
            FileStatus::Quarantined => "  [QUARANTINED]",
            FileStatus::Purged => "  [PURGED]",
        };
        println!("{}  [{}]  {}  ({} bytes){}",
            f.relative_path, f.volume_id, f.category.as_str(), f.size_bytes, flag);
    }
    println!("{} match(es).", hits.len());
    Ok(())
}

pub fn cmd_status() -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let groups = cat.duplicate_group_count()?;
    println!("Duplicate groups (same content hash): {groups}");
    println!("Per-volume (active files):");
    for (id, label, count, bytes) in cat.volume_stats()? {
        println!("  {label} [{id}]: {count} files, {} MiB", bytes / (1024 * 1024));
    }
    Ok(())
}

use std::path::Path;
use clap::ValueEnum;

use crate::config::Config;
use crate::catalog::Catalog;
use crate::catalog::models::{Volume, FileStatus};
use crate::volume::{self, ReadonlyMode};
use crate::scanner;
use crate::catalog::backup;
use crate::web;
use crate::{quarantine, purge};

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
    println!("Done: {} hashed, {} unchanged, {} errors, {} newly missing, {} archive entries.",
        s.hashed, s.skipped, s.errors, s.marked_missing, s.archive_entries);

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
        let location = match &f.container_chain {
            Some(chain) => format!("{} › {}", f.relative_path, chain),
            None => f.relative_path.clone(),
        };
        println!("{}  [{}]  {}  ({} bytes){}",
            location, f.volume_id, f.category.as_str(), f.size_bytes, flag);
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
        let recoverable = cat.recoverable_bytes(&id)?;
        println!("  {label} [{id}]: {count} files, {} MiB (recoverable: {} MiB in _ToDelete)",
            bytes / (1024 * 1024), recoverable / (1024 * 1024));
    }
    Ok(())
}

pub fn cmd_duplicates() -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let groups = cat.duplicate_groups()?;
    if groups.is_empty() { println!("No duplicate groups."); return Ok(()); }
    for group in &groups {
        println!("hash {} — {} copies:", &group[0].content_hash[..16.min(group[0].content_hash.len())], group.len());
        for f in group {
            let loc = display_location(f);
            println!("  #{}  {}  [{}]  {} bytes  {}",
                f.id, loc, f.volume_id, f.size_bytes, f.status.as_str());
        }
    }
    Ok(())
}

/// Where a file is / came from, for display.
fn display_location(f: &crate::catalog::models::FileRecord) -> String {
    let base = f.original_path.as_deref().unwrap_or(&f.relative_path);
    match &f.container_chain {
        Some(chain) => format!("{base} › {chain}"),
        None => base.to_string(),
    }
}

pub fn cmd_quarantine(mount: &Path, ids: &[i64]) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}; scan the drive first", mount.display()))?;
    let now = now_secs();
    let out = quarantine::quarantine_files(&cat, mount, &vid, ids, now)?;
    println!("Quarantined {} file(s), skipped {}.", out.quarantined, out.skipped);
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_purge(mount: &Path) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}", mount.display()))?;
    let now = now_secs();
    // snapshot BEFORE the irreversible delete
    let snap = backup::snapshot(&cfg.catalog_path, &cfg.backups_dir(), cfg.snapshot_retention, now)?;
    println!("Catalog snapshot (pre-purge): {}", snap.display());
    let out = purge::purge_volume(&cat, mount, &vid, now)?;
    println!("Purged {} file(s), reclaimed {} MiB.", out.files_purged, out.bytes_reclaimed / (1024*1024));
    Ok(())
}

pub fn cmd_browse(open: bool) -> anyhow::Result<()> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    if !cat.integrity_ok()? {
        anyhow::bail!("catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display());
    }
    drop(cat); // handlers open their own short-lived connections
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(web::serve(cfg.catalog_path.clone(), open))
}

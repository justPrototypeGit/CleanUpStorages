use clap::ValueEnum;
use std::path::Path;

use crate::catalog::backup;
use crate::catalog::models::FileStatus;
use crate::catalog::Catalog;
use crate::config::Config;
use crate::scanner;
use crate::volume::ReadonlyMode;
use crate::web;
use crate::{purge, quarantine, repack};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadonlyFallback {
    Ask,
    Fingerprint,
    Skip,
}

impl From<ReadonlyFallback> for ReadonlyMode {
    fn from(f: ReadonlyFallback) -> Self {
        match f {
            ReadonlyFallback::Ask => ReadonlyMode::Ask,
            ReadonlyFallback::Fingerprint => ReadonlyMode::Fingerprint,
            ReadonlyFallback::Skip => ReadonlyMode::Skip,
        }
    }
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Open the config and catalog — the prologue every command shares.
fn open_catalog() -> anyhow::Result<(Config, Catalog)> {
    let cfg = Config::default_paths()?;
    let cat = Catalog::open(&cfg.catalog_path)?;
    Ok((cfg, cat))
}

/// Like `open_catalog`, plus the integrity guard used before scanning/serving: refuse to act on a
/// catalog that fails its check and point at the snapshots.
fn open_catalog_checked() -> anyhow::Result<(Config, Catalog)> {
    let (cfg, cat) = open_catalog()?;
    if !cat.integrity_ok()? {
        anyhow::bail!(
            "catalog failed integrity check; restore the latest snapshot from {}",
            cfg.backups_dir().display()
        );
    }
    Ok((cfg, cat))
}

/// Timestamped catalog snapshot (the CLI's audit/rollback point).
fn snapshot(cfg: &Config, now: i64) -> anyhow::Result<std::path::PathBuf> {
    backup::snapshot(
        &cfg.catalog_path,
        &cfg.backups_dir(),
        cfg.snapshot_retention,
        now,
    )
}

pub fn cmd_scan(path: &Path, force: bool, fallback: ReadonlyFallback) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog_checked()?;
    let now = now_secs();
    match scanner::run_scan(&cat, path, force, fallback.into(), now, None)? {
        None => {
            println!("Skipped read-only drive at {}", path.display());
            return Ok(());
        }
        Some((identity, s)) => {
            println!(
                "Scanned {} (volume {}, id by {})",
                path.display(),
                identity.label,
                identity.identified_by
            );
            println!(
                "Done: {} hashed, {} unchanged, {} errors, {} newly missing, {} archive entries.",
                s.hashed, s.skipped, s.errors, s.marked_missing, s.archive_entries
            );
        }
    }
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_search(
    query: &str,
    category: Option<&str>,
    volume: Option<&str>,
    status: Option<&str>,
) -> anyhow::Result<()> {
    let (_cfg, cat) = open_catalog()?;
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
        // The id is printed because it is the handle every acting verb takes (`quarantine`,
        // `repack`). `duplicates` only lists loose files, so this is the way to find an
        // archived entry's id.
        println!(
            "#{}  {}  [{}]  {}  ({} bytes){}",
            f.id,
            location,
            f.volume_id,
            f.category.as_str(),
            f.size_bytes,
            flag
        );
    }
    println!("{} match(es).", hits.len());
    Ok(())
}

pub fn cmd_status() -> anyhow::Result<()> {
    let (_cfg, cat) = open_catalog()?;
    let totals = cat.duplicate_totals(0)?;
    println!(
        "Duplicate groups (loose, same content hash): {}",
        totals.groups_all
    );
    println!(
        "Reclaimable by quarantine: {} MiB (+{} MiB locked inside archives — needs repack)",
        totals.reclaimable_all_bytes / (1024 * 1024),
        totals.archive_locked_bytes / (1024 * 1024)
    );
    println!("Per-volume (active files):");
    for (id, label, count, bytes) in cat.volume_stats()? {
        let recoverable = cat.recoverable_bytes(&id)?;
        println!(
            "  {label} [{id}]: {count} files, {} MiB (recoverable: {} MiB in _ToDelete)",
            bytes / (1024 * 1024),
            recoverable / (1024 * 1024)
        );
    }
    Ok(())
}

/// The biggest-first duplicate worklist. Bounded and floored: printing all 250k+ groups of a real
/// catalogue is not a review, it is a wall of text.
pub fn cmd_duplicates(min_size: i64, limit: usize) -> anyhow::Result<()> {
    let (_cfg, cat) = open_catalog()?;
    let totals = cat.duplicate_totals(min_size)?;
    let groups = cat.duplicate_groups_ranked(min_size, limit, None)?;
    if groups.is_empty() {
        println!("No duplicate groups at or above {min_size} bytes.");
    }
    let hashes: Vec<String> = groups.iter().map(|g| g.content_hash.clone()).collect();
    let members = cat.duplicate_members_for(&hashes)?;
    for g in &groups {
        println!(
            "{} bytes reclaimable — {} copies × {} bytes  (hash {})",
            g.reclaimable_bytes,
            g.copies,
            g.size_bytes,
            &g.content_hash[..16.min(g.content_hash.len())]
        );
        for m in members.get(&g.content_hash).into_iter().flatten() {
            println!(
                "  {} #{}  {}  [{}]",
                if m.is_suggested_keep { "KEEP" } else { "    " },
                m.record.id,
                m.record.display_location(),
                m.record.volume_id
            );
        }
    }
    println!(
        "\nShowing top {} of {} groups at/above {} bytes. Reclaimable: {} bytes shown, \
         {} bytes total (floor-free). Archive-locked: {} bytes (needs repack).",
        groups.len(),
        totals.groups,
        min_size,
        totals.reclaimable_bytes,
        totals.reclaimable_all_bytes,
        totals.archive_locked_bytes
    );
    Ok(())
}

pub fn cmd_quarantine(mount: &Path, ids: &[i64]) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let vid = crate::volume::read_volume_id(mount).ok_or_else(|| {
        anyhow::anyhow!(
            "no identity marker at {}; scan the drive first",
            mount.display()
        )
    })?;
    let now = now_secs();
    let out = quarantine::quarantine_files(&cat, mount, &vid, ids, now)?;
    println!(
        "Quarantined {} file(s), skipped {}.",
        out.quarantined, out.skipped
    );
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot: {}", snap.display());
    Ok(())
}

pub fn cmd_purge(mount: Option<&Path>, all: bool) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let now = now_secs();
    // snapshot BEFORE the irreversible delete
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot (pre-purge): {}", snap.display());
    if all {
        let mounts = crate::mounts::live_mounts();
        let out = purge::purge_all(&cat, &mounts, now)?;
        let total: i64 = out.purged.iter().map(|(_, _, b)| *b).sum();
        println!(
            "Purged {} volume(s), reclaimed {} MiB total.",
            out.purged.len(),
            total / (1024 * 1024)
        );
        for v in &out.skipped_unmounted {
            println!("  skipped (not connected): {v}");
        }
        for e in &out.errors {
            println!("  error: {e}");
        }
        return Ok(());
    }
    let mount =
        mount.ok_or_else(|| anyhow::anyhow!("a mount path is required unless --all is given"))?;
    let vid = crate::volume::read_volume_id(mount)
        .ok_or_else(|| anyhow::anyhow!("no identity marker at {}", mount.display()))?;
    let out = purge::purge_volume(&cat, mount, &vid, now)?;
    println!(
        "Purged {} file(s), reclaimed {} MiB.",
        out.files_purged,
        out.bytes_reclaimed / (1024 * 1024)
    );
    Ok(())
}

pub fn cmd_forget(mount: &Path) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let vid = crate::volume::read_volume_id(mount).ok_or_else(|| {
        anyhow::anyhow!(
            "no identity marker at {}; nothing to forget",
            mount.display()
        )
    })?;
    let now = now_secs();
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot (pre-forget): {}", snap.display());
    let removed = cat.forget_volume(&vid, now)?;
    println!("Forgot volume {vid}: removed {removed} catalog entries. Files on disk are untouched; rescan to re-add.");
    Ok(())
}

pub fn cmd_rename(
    mount: &Path,
    name: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let vid = crate::volume::read_volume_id(mount).ok_or_else(|| {
        anyhow::anyhow!(
            "no identity marker at {}; scan the drive first",
            mount.display()
        )
    })?;
    let now = now_secs();
    cat.set_volume_meta(&vid, name, description, now)?;
    let _ = snapshot(&cfg, now);
    println!("Updated drive {vid}.");
    Ok(())
}

pub fn cmd_repack(mount: &Path, entry_id: i64) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let vid = crate::volume::read_volume_id(mount).ok_or_else(|| {
        anyhow::anyhow!(
            "no identity marker at {}; scan the drive first",
            mount.display()
        )
    })?;
    let now = now_secs();
    // snapshot BEFORE modifying an archive
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot (pre-repack): {}", snap.display());
    let out = repack::repack_entry(&cat, mount, &vid, entry_id, now)?;
    println!("Repacked: removed '{}', {} entries retained. Original archive and removed item saved in _ToDelete (recoverable until purge).",
        out.removed_entry, out.retained_entries);
    Ok(())
}

pub fn cmd_browse(open: bool) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog_checked()?;
    drop(cat); // handlers open their own short-lived connections
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(web::serve(cfg.catalog_path.clone(), open))
}

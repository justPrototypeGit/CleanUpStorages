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

pub fn cmd_scan(
    path: &Path,
    force: bool,
    fallback: ReadonlyFallback,
    no_count: bool,
) -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog_checked()?;
    let now = now_secs();

    // Refuse a collision up front rather than dying on the write lock minutes later (#60). One
    // writer is the correct design for a SQLite catalogue; the defect was that the user found out
    // via `database is locked`, four minutes into walking a 4 TB drive.
    if let Some((id, other, started_at)) = cat.running_scan()? {
        let mins = now.saturating_sub(started_at) / 60;
        anyhow::bail!(
            "a scan is already running against this catalogue (run #{id}, {other}, started {mins}              minutes ago). Wait for it to finish or stop it first -- two scans cannot write to the              catalogue at once."
        );
    }
    let stop = crate::scan_control::install_signal_handler();
    let progress = crate::scan_control::CliProgress::new();

    let limits = crate::archive::ArchiveLimits::from_config(&cfg);
    println!("{}", limits.summary_line());

    if !no_count {
        eprintln!("Counting files…");
        let totals = scanner::count_tree(path, &stop);
        // A stop during counting leaves a partial total. Publishing it would show a percentage that
        // is wrong rather than absent, so stop here instead: nothing has been scanned yet.
        if stop.is_requested() {
            println!("STOPPED while counting — nothing was scanned.");
            return Ok(());
        }
        eprintln!(
            "Counting… {} files ({:.1} GB)",
            totals.files,
            totals.bytes as f64 / 1_073_741_824.0
        );
        crate::scanner::Progress::on_total(&progress, totals.files, totals.bytes);
    }

    let outcome = scanner::run_scan(
        &cat,
        path,
        force,
        fallback.into(),
        now,
        Some(&progress),
        &stop,
        &limits,
    );
    progress.finish();
    match outcome? {
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
            if s.stopped {
                println!("STOPPED before the end of the tree — nothing was marked missing.");
                println!("Re-run the same command to continue; catalogued files are skipped fast.");
            }
            println!(
                "Done: {} hashed, {} unchanged, {} errors, {} newly missing, {} archive entries.",
                s.hashed, s.skipped, s.errors, s.marked_missing, s.archive_entries
            );
            if s.stopped {
                // A scan stopped early has only assessed a fraction of the tree; claiming
                // completeness (even "complete") from that fraction would assert about the whole
                // volume from a partial walk. Say nothing was concluded, not that it is fine.
                println!(
                    "Completeness: not assessed — the scan stopped before the end of the tree."
                );
            } else {
                println!(
                    "{}",
                    cat.volume_completeness(&identity.volume_id)?.summary_line()
                );
                // Derived from the rows this scan just wrote. Skipped for a stopped scan: its
                // picture of the volume is incomplete by definition, and hashing a half-seen tree
                // would invent folders that differ only because the scan never reached the rest.
                let dirs = cat.rebuild_directory_trees(&identity.volume_id, now)?;
                cat.refresh_volume_totals(&identity.volume_id)?;
                tracing::info!(directories = dirs, "rebuilt directory trees");
            }
            print!("{}", s.metrics.report());
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
        let c = cat.volume_completeness(&id)?;
        let marker = if c.is_complete() { " " } else { "⚠" };
        println!(
            "     {marker} {}",
            c.summary_line().trim_start_matches("Completeness: ")
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
    // What is active just changed, so any identical-tree group involving these files is stale.
    // Leaving it would keep offering a pair whose other side is already in _ToDelete.
    cat.rebuild_directory_trees(&vid, now)?;
    cat.refresh_volume_totals(&vid)?;
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

/// Drop catalogued system folders from every known volume, then rebuild what depended on them.
///
/// Scans skip these folders now, but a catalogue built before that still holds their entries --
/// on the real corpus, 77,493 `$RECYCLE.BIN` rows, which sort ahead of every real path (`$` sorts
/// before letters) and so filled the Browse listing entirely. Deleting the rows is cheap; waiting
/// for a rescan of several TB to do the same thing is not, which is why this exists as its own verb.
///
/// Nothing on disk is touched -- this only forgets entries. Directory trees and volume totals are
/// derived from the rows, so both are rebuilt for any volume that actually lost some.
pub fn cmd_tidy() -> anyhow::Result<()> {
    let (cfg, cat) = open_catalog()?;
    let now = now_secs();
    let snap = snapshot(&cfg, now)?;
    println!("Catalog snapshot (pre-tidy): {}", snap.display());

    let labels = cat.effective_labels()?;
    let mut total = 0usize;
    let mut drives = 0usize;
    for (vid, label) in &labels {
        let removed = cat.forget_system_paths(vid)?;
        if removed == 0 {
            continue;
        }
        total += removed;
        drives += 1;
        cat.rebuild_directory_trees(vid, now)?;
        cat.refresh_volume_totals(vid)?;
        println!("{label}: forgot {removed} system-folder entries");
    }

    if total == 0 {
        println!("Nothing to tidy: no system-folder entries are catalogued.");
    } else {
        println!(
            "Forgot {total} system-folder entries across {drives} drive(s). Files on disk are untouched."
        );
    }
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
    // Deliberately NOT `open_catalog_checked`: PRAGMA integrity_check over a 3.9 GB catalogue takes
    // 80 seconds, and it ran before the port opened, on every launch, to re-verify something that
    // had not changed since last time. That was the single worst number in the UI.
    //
    // The check still runs -- on a background thread, once the server is already answering -- and
    // still reports. What moved is only the blocking. Nothing destructive is reachable from the web
    // UI without its own guard, and every CLI verb that mutates still checks synchronously first,
    // because there the user is already waiting on a long operation.
    let (cfg, cat) = open_catalog()?;
    drop(cat); // handlers open their own short-lived connections

    let check_path = cfg.catalog_path.clone();
    std::thread::spawn(move || match Catalog::open_readonly(&check_path) {
        Ok(cat) => match cat.integrity_ok() {
            Ok(true) => tracing::info!("catalog integrity check passed"),
            // Loud, and it names the recovery path. A corrupt catalogue must never be quiet just
            // because the check moved off the startup path.
            Ok(false) => tracing::error!(
                "CATALOG FAILED ITS INTEGRITY CHECK -- restore the newest snapshot from {}",
                crate::config::backups_dir_for(&check_path)
                    .join("")
                    .display()
            ),
            Err(e) => tracing::warn!("could not run the catalog integrity check: {e}"),
        },
        Err(e) => tracing::warn!("could not open the catalog to check its integrity: {e}"),
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(web::serve(cfg.catalog_path.clone(), open))
}

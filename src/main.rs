use cleanupstorages::commands;
use clap::{Parser, Subcommand};

/// Reliable catalog + deduplication tool for messy external drives.
#[derive(Parser)]
#[command(name = "cleanupstorages", version, about)]
struct Cli {
    /// Verbose logging (debug level). RUST_LOG, if set, overrides this.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl a mounted drive/path, hash files, and update the catalog.
    Scan {
        /// Path to the mounted drive or directory to scan.
        path: std::path::PathBuf,
        /// Re-hash every file, ignoring the incremental skip fast-path.
        #[arg(long)]
        force: bool,
        /// How to handle read-only drives where the marker cannot be written.
        #[arg(long, value_enum, default_value = "ask")]
        readonly_fallback: commands::ReadonlyFallback,
    },
    /// Search the catalog for files by name/path.
    Search {
        /// Free-text query matched against filename and path.
        query: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        volume: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Show catalog statistics (files, duplicate groups, per-volume totals).
    Status,
    /// Start a local web UI (127.0.0.1) to search and browse the catalog.
    Browse {
        /// Do not try to open a browser automatically.
        #[arg(long)]
        no_open: bool,
    },
    /// List duplicate groups (files sharing a content hash), with ids to act on.
    Duplicates,
    /// Move confirmed-duplicate files (by id) to the drive's _ToDelete quarantine.
    Quarantine {
        /// Current mount path of the drive holding the files.
        mount: std::path::PathBuf,
        /// Catalog ids of the files to quarantine (from `duplicates`).
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Permanently delete a drive's _ToDelete quarantine and reclaim space.
    Purge {
        /// Current mount path of the drive to purge (omit when using --all).
        mount: Option<std::path::PathBuf>,
        /// Purge every currently-connected drive that has quarantined files.
        #[arg(long)]
        all: bool,
    },
    /// Remove one entry from a top-level zip by rebuilding it (Case 4; needs a surviving copy).
    Repack {
        /// Current mount path of the drive holding the archive.
        mount: std::path::PathBuf,
        /// Catalog id of the archived entry to remove (from `duplicates`).
        entry_id: i64,
    },
    /// Remove a drive's catalog entries (files on disk untouched; rescan to re-add).
    Forget {
        /// Current mount path of the drive to forget.
        mount: std::path::PathBuf,
    },
    /// Set a drive's custom name and/or description (shown in the UI).
    Rename {
        /// Current mount path of the drive.
        mount: std::path::PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    // Mark the process per-monitor DPI-aware so the native folder dialog (Browse… on the Scan page)
    // renders crisply instead of being bitmap-scaled (blurry) on high-DPI displays. Must run before
    // any window/dialog is created.
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let cli = Cli::parse();
    cleanupstorages::observability::init(cli.verbose);
    let name = match &cli.command {
        Command::Scan { .. } => "scan",
        Command::Search { .. } => "search",
        Command::Status => "status",
        Command::Browse { .. } => "browse",
        Command::Duplicates => "duplicates",
        Command::Quarantine { .. } => "quarantine",
        Command::Purge { .. } => "purge",
        Command::Repack { .. } => "repack",
        Command::Forget { .. } => "forget",
        Command::Rename { .. } => "rename",
    };
    // Groups a command's log events under `command{name=...}`. Note: this uses a thread-local
    // context, so it reliably nests only synchronous commands; `browse`'s per-connection request
    // spans run on separate tokio worker tasks and appear as their own top-level spans.
    let _span = tracing::info_span!("command", name).entered();
    match cli.command {
        Command::Scan { path, force, readonly_fallback } => {
            commands::cmd_scan(&path, force, readonly_fallback)
        }
        Command::Search { query, category, volume, status } => {
            commands::cmd_search(&query, category.as_deref(), volume.as_deref(), status.as_deref())
        }
        Command::Status => commands::cmd_status(),
        Command::Browse { no_open } => commands::cmd_browse(!no_open),
        Command::Duplicates => commands::cmd_duplicates(),
        Command::Quarantine { mount, ids } => commands::cmd_quarantine(&mount, &ids),
        Command::Purge { mount, all } => commands::cmd_purge(mount.as_deref(), all),
        Command::Repack { mount, entry_id } => commands::cmd_repack(&mount, entry_id),
        Command::Forget { mount } => commands::cmd_forget(&mount),
        Command::Rename { mount, name, description } =>
            commands::cmd_rename(&mount, name.as_deref(), description.as_deref()),
    }
}

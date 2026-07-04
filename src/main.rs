mod config;
mod hashing;
mod category;
mod catalog;
mod volume;
mod scanner;
mod commands;

use clap::{Parser, Subcommand};

/// Reliable catalog + deduplication tool for messy external drives.
#[derive(Parser)]
#[command(name = "cleanupstorages", version, about)]
struct Cli {
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, force, readonly_fallback } => {
            commands::cmd_scan(&path, force, readonly_fallback)
        }
        Command::Search { query, category, volume, status } => {
            commands::cmd_search(&query, category.as_deref(), volume.as_deref(), status.as_deref())
        }
        Command::Status => commands::cmd_status(),
    }
}

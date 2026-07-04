use std::path::Path;
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReadonlyFallback { Ask, Fingerprint, Skip }

pub fn cmd_scan(_path: &Path, _force: bool, _fallback: ReadonlyFallback) -> anyhow::Result<()> {
    println!("scan: not yet implemented");
    Ok(())
}

pub fn cmd_search(
    _query: &str, _category: Option<&str>, _volume: Option<&str>, _status: Option<&str>,
) -> anyhow::Result<()> {
    println!("search: not yet implemented");
    Ok(())
}

pub fn cmd_status() -> anyhow::Result<()> {
    println!("status: not yet implemented");
    Ok(())
}

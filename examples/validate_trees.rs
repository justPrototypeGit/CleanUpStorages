//! Throwaway: rebuild directory_trees over a COPY of a real catalogue and print the totals, so the
//! Rust implementation can be checked against scripts/measure-tree-collapse.py.
fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: validate_trees <catalog.db>");
    let cat = cleanupstorages::catalog::Catalog::open(std::path::Path::new(&path))?;
    let vols: Vec<String> = {
        let mut s = cat.conn.prepare("SELECT volume_id FROM volumes")?;
        let v = s
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let mut total = 0usize;
    for v in &vols {
        total += cat.rebuild_directory_trees(v, 1)?;
    }
    println!("volumes: {}  directory nodes: {}", vols.len(), total);
    let groups = cat.tree_duplicate_groups()?;
    let reclaim: i64 = groups.iter().map(|g| g.reclaimable_bytes).sum();
    let members: usize = groups.iter().map(|g| g.members.len()).sum();
    let in_archive = groups
        .iter()
        .flat_map(|g| g.members.iter())
        .filter(|m| m.archive_container().is_some())
        .count();
    println!("MAXIMAL groups: {}", groups.len());
    println!("  folders involved: {members}");
    println!("  reclaimable: {:.1} GB", reclaim as f64 / 1e9);
    println!("  members inside an archive (needs repack): {in_archive}");
    for g in groups.iter().take(8) {
        println!(
            "  {:7.2} GB  x{}  {:>7} files  {}",
            g.reclaimable_bytes as f64 / 1e9,
            g.members.len(),
            g.members[0].file_count,
            g.members[0].path
        );
    }
    Ok(())
}

# Identical-Tree Collapse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse folders whose contents are byte-identical into a single review item, so 125,977 duplicate decisions become ~1,458.

**Architecture:** A git-style Merkle hash per directory, derived entirely from the BLAKE3 content hashes already in the catalogue (no drive re-read, works for unplugged drives). Hashes land in a derived `directory_trees` table rebuilt after a scan or any change to what is active. Confirming a redundant loose tree is **one directory rename** into `_ToDelete` followed by the same per-file catalogue bookkeeping N individual quarantines would have done.

**Tech Stack:** Rust, rusqlite/SQLite, BLAKE3 (`blake3` crate, already a dependency), axum + plain HTML/JS.

**Spec:** [docs/superpowers/specs/2026-08-06-identical-tree-collapse-design.md](../specs/2026-08-06-identical-tree-collapse-design.md)

## Global Constraints

- **Nothing may ever be lost or corrupted.** Quarantine is a rename into a same-drive `_ToDelete`; `purge` is the only real delete and stays manual. When in doubt, choose the option that cannot lose data.
- Only rows with `status='active'` participate in tree matching.
- Empty trees (`file_count == 0`) are never hashed and never reported.
- A directory's **own name** is never an input to its hash — only its children's names are.
- An archive is a **directory of its entries**; its own `content_hash` is ignored when it has entry rows. Never both.
- Only **maximal** matching subtrees are reported; descendants of a matched folder are suppressed.
- A folder inside an archive is **reported but never deletable** — label "needs repack", no quarantine action.
- Archive-internal per-file duplicates are excluded from the per-file duplicate queue.
- Every destructive path re-checks that all files are still `active` immediately before acting, and refuses otherwise.
- Tests must never touch the real data dir. Set `CLEANUPSTORAGES_DATA_DIR` to a temp dir on every `cargo` invocation (issue #44).
- Gates before each commit: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Conventional Commits, with both trailers:
  `Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`

## File Structure

| File | Responsibility |
| --- | --- |
| `src/tree_hash.rs` **(new)** | Pure computation. Rows in → directory hashes and maximal duplicate groups out. No DB writes, no filesystem. This is where all the tricky logic lives, and it is unit-testable without a drive |
| `src/catalog/schema.rs` | Add the `directory_trees` table and its hash index |
| `src/catalog/store.rs` | Persist/read `directory_trees`; feed rows to `tree_hash`; query maximal groups |
| `src/tree_quarantine.rs` **(new)** | The confirm action: one directory rename plus per-file bookkeeping, with the still-active re-check |
| `src/web.rs` | `GET /api/tree-duplicates`, `POST /api/quarantine-tree` |
| `src/web_ui.rs` | The Duplicates-page section that renders tree groups and their blast radius |
| `src/commands.rs` | Rebuild `directory_trees` after a completed scan, and after quarantine/purge |

---

### Task 1: The directory hash

**Files:**
- Create: `src/tree_hash.rs`
- Modify: `src/lib.rs` (add `pub mod tree_hash;`)

**Interfaces:**
- Produces:
  ```rust
  pub struct TreeInput { pub volume_id: String, pub path: String, pub content_hash: String,
                         pub size_bytes: i64, pub is_archive_root: bool }
  pub struct DirNode { pub volume_id: String, pub path: String, pub dir_hash: String,
                       pub file_count: i64, pub total_bytes: i64 }
  pub fn build_dir_hashes(rows: Vec<TreeInput>) -> Vec<DirNode>
  ```
  `path` for a loose file is `relative_path`; for an archive entry it is
  `relative_path + "/" + container_chain`. `is_archive_root` marks the archive's OWN file row —
  those rows are dropped when the same `(volume_id, path)` also has entry rows.

- [ ] **Step 1: Write the failing tests**

Add to `src/tree_hash.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn f(vol: &str, path: &str, hash: &str, size: i64) -> TreeInput {
        TreeInput { volume_id: vol.into(), path: path.into(), content_hash: hash.into(),
                    size_bytes: size, is_archive_root: false }
    }
    fn hash_of(nodes: &[DirNode], path: &str) -> Option<String> {
        nodes.iter().find(|n| n.path == path).map(|n| n.dir_hash.clone())
    }

    #[test]
    fn two_folders_with_the_same_contents_match_despite_different_names() {
        // The whole point: a copied folder is usually renamed, so the folder's OWN name must not
        // be an input to its hash.
        let nodes = build_dir_hashes(vec![
            f("v", "Photos2019/a.jpg", "H1", 10),
            f("v", "Photos2019/b.jpg", "H2", 20),
            f("v", "Photos 2019 copy/a.jpg", "H1", 10),
            f("v", "Photos 2019 copy/b.jpg", "H2", 20),
        ]);
        assert_eq!(hash_of(&nodes, "Photos2019"), hash_of(&nodes, "Photos 2019 copy"));
    }

    #[test]
    fn a_child_filename_difference_breaks_the_match() {
        // Children's names ARE part of the hash: same bytes under a different name is a different
        // folder, or renaming one file inside a backup would silently make it "identical".
        let nodes = build_dir_hashes(vec![
            f("v", "A/a.jpg", "H1", 10),
            f("v", "B/renamed.jpg", "H1", 10),
        ]);
        assert_ne!(hash_of(&nodes, "A"), hash_of(&nodes, "B"));
    }

    #[test]
    fn an_archive_becomes_a_directory_and_its_own_file_hash_is_ignored() {
        // The trap this test exists for: an archive is BOTH a file row and a set of entry rows.
        // If both fed the hash, the archive would appear as its own sibling and every ancestor's
        // hash would be wrong. Here the two archives have DIFFERENT content_hash (different
        // compression) but identical contents, so they must match.
        let rows = vec![
            TreeInput { volume_id: "v".into(), path: "x/one.zip".into(),
                        content_hash: "ZIP_A".into(), size_bytes: 99, is_archive_root: true },
            f("v", "x/one.zip/inner.txt", "H1", 10),
            TreeInput { volume_id: "v".into(), path: "y/two.zip".into(),
                        content_hash: "ZIP_B".into(), size_bytes: 77, is_archive_root: true },
            f("v", "y/two.zip/inner.txt", "H1", 10),
        ];
        let nodes = build_dir_hashes(rows);
        assert_eq!(hash_of(&nodes, "x/one.zip"), hash_of(&nodes, "y/two.zip"),
                   "identical contents, different compression, must still match");
        let z = nodes.iter().find(|n| n.path == "x/one.zip").unwrap();
        assert_eq!(z.file_count, 1, "the archive's own row must not be counted as a child file");
        assert_eq!(z.total_bytes, 10, "bytes come from the entries, not the .zip's own size");
    }

    #[test]
    fn an_archive_with_no_entries_stays_an_ordinary_file() {
        // A zip we never descended into (deny-listed .docx, or unreadable) has no entry rows. It
        // must remain a leaf with its own content hash, not vanish.
        let nodes = build_dir_hashes(vec![
            TreeInput { volume_id: "v".into(), path: "d/report.docx".into(),
                        content_hash: "DOCX".into(), size_bytes: 5, is_archive_root: true },
        ]);
        let d = nodes.iter().find(|n| n.path == "d").unwrap();
        assert_eq!(d.file_count, 1);
        assert_eq!(d.total_bytes, 5);
        assert!(hash_of(&nodes, "d/report.docx").is_none(), "no entries, so not a directory");
    }

    #[test]
    fn empty_directories_are_not_emitted() {
        let nodes = build_dir_hashes(vec![f("v", "a/b/c.txt", "H1", 1)]);
        assert!(nodes.iter().all(|n| n.file_count > 0));
    }

    #[test]
    fn counts_and_bytes_roll_up_through_subdirectories() {
        let nodes = build_dir_hashes(vec![
            f("v", "top/one.txt", "H1", 10),
            f("v", "top/sub/two.txt", "H2", 20),
            f("v", "top/sub/deep/three.txt", "H3", 30),
        ]);
        let top = nodes.iter().find(|n| n.path == "top").unwrap();
        assert_eq!(top.file_count, 3);
        assert_eq!(top.total_bytes, 60);
    }

    #[test]
    fn volumes_do_not_bleed_into_each_other() {
        let nodes = build_dir_hashes(vec![
            f("v1", "A/a.txt", "H1", 1),
            f("v2", "A/a.txt", "H1", 1),
        ]);
        let a: Vec<_> = nodes.iter().filter(|n| n.path == "A").collect();
        assert_eq!(a.len(), 2, "one node per volume");
        assert_eq!(a[0].dir_hash, a[1].dir_hash, "same contents, so same hash");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash
```
Expected: FAIL to compile — `build_dir_hashes` and the types do not exist.

- [ ] **Step 3: Implement**

Write above the test module in `src/tree_hash.rs`:

```rust
//! Merkle hash per directory, derived from content hashes the catalogue already holds.
//!
//! No filesystem access and no drive re-read: every input is a row already in `files`. That is what
//! makes collapsing identical trees cheap, and what lets it work for drives that are not plugged in.

use std::collections::{BTreeMap, HashMap, HashSet};

/// One catalogued file, flattened into a path within its volume.
///
/// For a loose file `path` is `relative_path`. For an archive entry it is
/// `relative_path + "/" + container_chain`, so an archive's insides form an ordinary subtree.
#[derive(Debug, Clone)]
pub struct TreeInput {
    pub volume_id: String,
    pub path: String,
    pub content_hash: String,
    pub size_bytes: i64,
    /// True for an archive's OWN file row. Such a row is dropped when entry rows exist for the
    /// same path, because the archive is then a directory rather than a leaf.
    pub is_archive_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirNode {
    pub volume_id: String,
    pub path: String,
    pub dir_hash: String,
    pub file_count: i64,
    pub total_bytes: i64,
}

#[derive(Clone)]
enum Child {
    File { hash: String, size: i64 },
    Dir,
}

/// Build a Merkle hash for every non-empty directory.
///
/// `dir_hash(D) = BLAKE3(sorted (child_name, kind, child_hash) lines)`. The directory's own name is
/// deliberately absent, so `Photos2019/` and `Photos 2019 copy/` match when their contents match.
pub fn build_dir_hashes(rows: Vec<TreeInput>) -> Vec<DirNode> {
    // An archive that HAS entries is a directory; its own file row must not also appear as a leaf.
    let mut has_entries: HashSet<(String, String)> = HashSet::new();
    for r in &rows {
        if let Some((parent, _)) = r.path.rsplit_once('/') {
            has_entries.insert((r.volume_id.clone(), parent.to_string()));
        }
    }

    let mut children: HashMap<(String, String), BTreeMap<String, Child>> = HashMap::new();
    let mut dirs: HashSet<(String, String)> = HashSet::new();

    for r in rows {
        if r.is_archive_root && has_entries.contains(&(r.volume_id.clone(), r.path.clone())) {
            continue; // replaced by its entry tree
        }
        let parts: Vec<&str> = r.path.split('/').collect();
        let leaf_parent = parts[..parts.len() - 1].join("/");
        children
            .entry((r.volume_id.clone(), leaf_parent))
            .or_default()
            .insert(
                parts[parts.len() - 1].to_string(),
                Child::File { hash: r.content_hash, size: r.size_bytes },
            );
        // Register every ancestor. A name already recorded as a file stays a file unless it turns
        // out to have children, in which case Dir wins -- that is the archive case.
        for i in (1..parts.len()).rev() {
            let me = parts[..i].join("/");
            let parent = parts[..i - 1].join("/");
            dirs.insert((r.volume_id.clone(), me));
            children
                .entry((r.volume_id.clone(), parent))
                .or_default()
                .insert(parts[i - 1].to_string(), Child::Dir);
        }
        dirs.insert((r.volume_id.clone(), String::new()));
    }

    let depth = |p: &str| if p.is_empty() { 0 } else { p.matches('/').count() + 1 };
    let mut ordered: Vec<(String, String)> = dirs.into_iter().collect();
    ordered.sort_by(|a, b| depth(&b.1).cmp(&depth(&a.1)).then(a.cmp(b)));

    let mut out: HashMap<(String, String), DirNode> = HashMap::new();
    for key in ordered {
        let Some(kids) = children.get(&key) else { continue };
        let mut lines = Vec::new();
        let (mut nf, mut nb) = (0i64, 0i64);
        for (name, child) in kids {
            match child {
                Child::File { hash, size } => {
                    lines.push(format!("{name}\u{0}f\u{0}{hash}"));
                    nf += 1;
                    nb += size;
                }
                Child::Dir => {
                    let sub = if key.1.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{name}", key.1)
                    };
                    if let Some(n) = out.get(&(key.0.clone(), sub)) {
                        lines.push(format!("{name}\u{0}d\u{0}{}", n.dir_hash));
                        nf += n.file_count;
                        nb += n.total_bytes;
                    }
                }
            }
        }
        if nf == 0 {
            continue; // empty tree: they would all hash alike and mean nothing
        }
        let dir_hash = blake3::hash(lines.join("\n").as_bytes()).to_hex().to_string();
        out.insert(
            key.clone(),
            DirNode { volume_id: key.0, path: key.1, dir_hash, file_count: nf, total_bytes: nb },
        );
    }
    let mut v: Vec<DirNode> = out.into_values().collect();
    v.sort_by(|a, b| (&a.volume_id, &a.path).cmp(&(&b.volume_id, &b.path)));
    v
}
```

Add to `src/lib.rs` beside the other `pub mod` lines:

```rust
pub mod tree_hash;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash
```
Expected: PASS, 7 tests.

- [ ] **Step 5: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/tree_hash.rs src/lib.rs
git commit   # feat(dedup): Merkle directory hash from stored content hashes
```

---

### Task 2: Maximal duplicate groups

**Files:**
- Modify: `src/tree_hash.rs`

**Interfaces:**
- Consumes: `DirNode` from Task 1.
- Produces:
  ```rust
  pub struct TreeGroup { pub dir_hash: String, pub members: Vec<DirNode>,
                         pub reclaimable_bytes: i64 }
  pub fn maximal_groups(nodes: &[DirNode]) -> Vec<TreeGroup>
  ```
  Sorted by `reclaimable_bytes` descending. `reclaimable_bytes = total_bytes * (members - 1)`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/tree_hash.rs`:

```rust
    #[test]
    fn only_the_topmost_matching_folder_is_reported() {
        // A duplicated tree would otherwise report every subfolder inside it, which is exactly the
        // 125,977-decision problem this feature exists to remove.
        let nodes = build_dir_hashes(vec![
            f("v", "orig/sub/deep/a.txt", "H1", 10),
            f("v", "orig/sub/b.txt", "H2", 20),
            f("v", "copy/sub/deep/a.txt", "H1", 10),
            f("v", "copy/sub/b.txt", "H2", 20),
        ]);
        let groups = maximal_groups(&nodes);
        let reported: Vec<&str> = groups.iter()
            .flat_map(|g| g.members.iter().map(|m| m.path.as_str()))
            .collect();
        assert_eq!(groups.len(), 1, "one decision, not three");
        assert!(reported.contains(&"orig") && reported.contains(&"copy"));
        assert!(!reported.iter().any(|p| p.contains('/')), "no subfolder may be reported: {reported:?}");
    }

    #[test]
    fn reclaimable_bytes_keeps_one_copy() {
        let nodes = build_dir_hashes(vec![
            f("v", "a/x.bin", "H1", 100),
            f("v", "b/x.bin", "H1", 100),
            f("v", "c/x.bin", "H1", 100),
        ]);
        let groups = maximal_groups(&nodes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 3);
        assert_eq!(groups[0].reclaimable_bytes, 200, "3 copies of 100 bytes reclaims 200");
    }

    #[test]
    fn a_folder_without_a_twin_is_not_a_group() {
        let nodes = build_dir_hashes(vec![f("v", "lonely/x.txt", "H1", 1)]);
        assert!(maximal_groups(&nodes).is_empty());
    }

    #[test]
    fn groups_are_ranked_by_reclaimable_bytes() {
        let nodes = build_dir_hashes(vec![
            f("v", "small1/s.bin", "S", 1),
            f("v", "small2/s.bin", "S", 1),
            f("v", "big1/b.bin", "B", 5000),
            f("v", "big2/b.bin", "B", 5000),
        ]);
        let groups = maximal_groups(&nodes);
        assert_eq!(groups[0].reclaimable_bytes, 5000, "biggest win first");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash
```
Expected: FAIL — `maximal_groups` not found.

- [ ] **Step 3: Implement**

Add to `src/tree_hash.rs`:

```rust
#[derive(Debug, Clone)]
pub struct TreeGroup {
    pub dir_hash: String,
    pub members: Vec<DirNode>,
    pub reclaimable_bytes: i64,
}

/// Group identical directories, reporting only the MAXIMAL match in each tree.
///
/// A folder is suppressed when its parent is itself part of a duplicate group. That is safe rather
/// than merely convenient: `dir_hash(parent)` includes every child hash, so a duplicated parent
/// guarantees this child's twin sits inside the parent's twin. The one case it misses -- a folder
/// paired with a DIFFERENT partner than its parent's -- under-reports rather than over-reports,
/// which is the right direction for a UI that offers a destructive action.
pub fn maximal_groups(nodes: &[DirNode]) -> Vec<TreeGroup> {
    let mut by_hash: HashMap<&str, Vec<&DirNode>> = HashMap::new();
    for n in nodes {
        by_hash.entry(n.dir_hash.as_str()).or_default().push(n);
    }
    let twinned: HashSet<(&str, &str)> = by_hash
        .values()
        .filter(|v| v.len() > 1)
        .flat_map(|v| v.iter().map(|n| (n.volume_id.as_str(), n.path.as_str())))
        .collect();

    let mut out = Vec::new();
    for (hash, members) in by_hash {
        if members.len() < 2 {
            continue;
        }
        let maximal: Vec<DirNode> = members
            .iter()
            .filter(|n| match n.path.rsplit_once('/') {
                Some((parent, _)) => !twinned.contains(&(n.volume_id.as_str(), parent)),
                None => !n.path.is_empty() && !twinned.contains(&(n.volume_id.as_str(), "")),
            })
            .map(|n| (*n).clone())
            .collect();
        if maximal.len() < 2 {
            continue;
        }
        let reclaimable_bytes = maximal[0].total_bytes * (maximal.len() as i64 - 1);
        out.push(TreeGroup { dir_hash: hash.to_string(), members: maximal, reclaimable_bytes });
    }
    out.sort_by(|a, b| b.reclaimable_bytes.cmp(&a.reclaimable_bytes).then(a.dir_hash.cmp(&b.dir_hash)));
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash
```
Expected: PASS, 11 tests.

- [ ] **Step 5: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/tree_hash.rs
git commit   # feat(dedup): report only maximal identical subtrees
```

---

### Task 3: The `directory_trees` table and its store API

**Files:**
- Modify: `src/catalog/schema.rs:82` (after `pending_archive_formats`, before the FTS block)
- Modify: `src/catalog/store.rs`

**Interfaces:**
- Consumes: `build_dir_hashes`, `maximal_groups`, `TreeInput`, `DirNode`, `TreeGroup`.
- Produces:
  ```rust
  impl Catalog {
      pub fn rebuild_directory_trees(&self, volume_id: &str, now: i64) -> anyhow::Result<usize>;
      pub fn tree_duplicate_groups(&self) -> anyhow::Result<Vec<crate::tree_hash::TreeGroup>>;
  }
  ```

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/catalog/store.rs`:

```rust
    #[test]
    fn rebuilding_directory_trees_finds_an_identical_pair() {
        let (_t, cat) = setup();
        for (path, hash, size) in [
            ("orig/a.txt", "H1", 10i64), ("orig/b.txt", "H2", 20),
            ("copy/a.txt", "H1", 10),    ("copy/b.txt", "H2", 20),
            ("unique/z.txt", "H9", 5),
        ] {
            cat.upsert_file(&NewFile {
                volume_id: "vol-1".into(), relative_path: path.into(),
                filename: path.rsplit('/').next().unwrap().into(), extension: "txt".into(),
                size_bytes: size, content_hash: hash.into(),
                created_time: None, modified_time: None, accessed_time: None,
                category: Category::Document, container_chain: None,
            }, 100).unwrap();
        }
        let n = cat.rebuild_directory_trees("vol-1", 100).unwrap();
        assert!(n >= 3, "root plus three folders, got {n}");

        let groups = cat.tree_duplicate_groups().unwrap();
        assert_eq!(groups.len(), 1, "orig and copy, and nothing else");
        let mut paths: Vec<&str> = groups[0].members.iter().map(|m| m.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["copy", "orig"]);
        assert_eq!(groups[0].reclaimable_bytes, 30);
    }

    #[test]
    fn a_quarantined_file_removes_its_folder_from_the_groups() {
        // Only active rows participate: once a twin is quarantined the pair is no longer a
        // duplicate, and continuing to offer it would invite quarantining the last copy.
        let (_t, cat) = setup();
        for path in ["orig/a.txt", "copy/a.txt"] {
            cat.upsert_file(&NewFile {
                volume_id: "vol-1".into(), relative_path: path.into(),
                filename: "a.txt".into(), extension: "txt".into(),
                size_bytes: 10, content_hash: "H1".into(),
                created_time: None, modified_time: None, accessed_time: None,
                category: Category::Document, container_chain: None,
            }, 100).unwrap();
        }
        cat.rebuild_directory_trees("vol-1", 100).unwrap();
        assert_eq!(cat.tree_duplicate_groups().unwrap().len(), 1);

        let id: i64 = cat.conn.query_row(
            "SELECT id FROM files WHERE volume_id='vol-1' AND relative_path='copy/a.txt'",
            [], |r| r.get(0)).unwrap();
        cat.mark_quarantined(id, "_ToDelete/copy/a.txt", "copy/a.txt", 200).unwrap();
        cat.rebuild_directory_trees("vol-1", 200).unwrap();
        assert!(cat.tree_duplicate_groups().unwrap().is_empty(),
                "the pair is gone once one side is quarantined");
    }

    #[test]
    fn rebuilding_twice_is_idempotent() {
        let (_t, cat) = setup();
        cat.upsert_file(&NewFile {
            volume_id: "vol-1".into(), relative_path: "d/a.txt".into(),
            filename: "a.txt".into(), extension: "txt".into(),
            size_bytes: 1, content_hash: "H1".into(),
            created_time: None, modified_time: None, accessed_time: None,
            category: Category::Document, container_chain: None,
        }, 100).unwrap();
        let first = cat.rebuild_directory_trees("vol-1", 100).unwrap();
        let second = cat.rebuild_directory_trees("vol-1", 200).unwrap();
        assert_eq!(first, second, "a rebuild must replace, not accumulate");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib catalog::store::tests::rebuilding
```
Expected: FAIL — `rebuild_directory_trees` not found.

- [ ] **Step 3: Implement the schema**

In `src/catalog/schema.rs`, insert after the `pending_archive_formats` index (line ~90) and before `CREATE VIRTUAL TABLE ... files_fts`:

```sql
        CREATE TABLE IF NOT EXISTS directory_trees (
            volume_id   TEXT NOT NULL REFERENCES volumes(volume_id),
            path        TEXT NOT NULL,
            dir_hash    TEXT NOT NULL,
            file_count  INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            computed_at INTEGER NOT NULL,
            PRIMARY KEY (volume_id, path)
        );
        CREATE INDEX IF NOT EXISTS idx_directory_trees_hash ON directory_trees(dir_hash);
```

- [ ] **Step 4: Implement the store API**

Add to `src/catalog/store.rs`:

```rust
    /// Recompute this volume's directory hashes from its active rows.
    ///
    /// Derived data: dropped and rebuilt wholesale, never migrated. Cheap because every content
    /// hash is already stored -- this reads rows and sorts, it does not touch the drive, so it
    /// works for a volume that is not currently plugged in.
    pub fn rebuild_directory_trees(&self, volume_id: &str, now: i64) -> anyhow::Result<usize> {
        let rows: Vec<crate::tree_hash::TreeInput> = {
            let mut stmt = self.conn.prepare(
                "SELECT relative_path, container_chain, content_hash, size_bytes
                   FROM files WHERE volume_id=?1 AND status='active'",
            )?;
            stmt.query_map(params![volume_id], |r| {
                let rel: String = r.get(0)?;
                let chain: Option<String> = r.get(1)?;
                Ok(crate::tree_hash::TreeInput {
                    volume_id: volume_id.to_string(),
                    path: match &chain {
                        Some(c) => format!("{rel}/{c}"),
                        None => rel,
                    },
                    content_hash: r.get(2)?,
                    size_bytes: r.get(3)?,
                    is_archive_root: chain.is_none(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let nodes = crate::tree_hash::build_dir_hashes(rows);

        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM directory_trees WHERE volume_id=?1", params![volume_id])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO directory_trees(volume_id, path, dir_hash, file_count, total_bytes,
                                             computed_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            for n in &nodes {
                ins.execute(params![n.volume_id, n.path, n.dir_hash, n.file_count,
                                    n.total_bytes, now])?;
            }
        }
        tx.commit()?;
        Ok(nodes.len())
    }

    /// Maximal identical-tree groups across every volume, ranked by reclaimable bytes.
    pub fn tree_duplicate_groups(&self) -> anyhow::Result<Vec<crate::tree_hash::TreeGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT volume_id, path, dir_hash, file_count, total_bytes FROM directory_trees
              WHERE dir_hash IN (SELECT dir_hash FROM directory_trees
                                  GROUP BY dir_hash HAVING COUNT(*)>1)",
        )?;
        let nodes = stmt
            .query_map([], |r| {
                Ok(crate::tree_hash::DirNode {
                    volume_id: r.get(0)?, path: r.get(1)?, dir_hash: r.get(2)?,
                    file_count: r.get(3)?, total_bytes: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::tree_hash::maximal_groups(&nodes))
    }
```

Note: `is_archive_root: chain.is_none()` is correct — a loose row is a candidate archive root, and
`build_dir_hashes` drops it only when entry rows actually exist for that path.

- [ ] **Step 5: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib catalog::store
```
Expected: PASS.

- [ ] **Step 6: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/catalog/schema.rs src/catalog/store.rs
git commit   # feat(catalog): directory_trees table, rebuilt from active rows
```

---

### Task 4: Quarantine a whole loose tree with one rename

**Files:**
- Create: `src/tree_quarantine.rs`
- Modify: `src/lib.rs` (add `pub mod tree_quarantine;`)

**Interfaces:**
- Consumes: `Catalog`, `crate::volume::QUARANTINE_DIR`, `Catalog::mark_quarantined`, `Catalog::log_action`.
- Produces:
  ```rust
  pub struct TreeQuarantineOutcome { pub files_updated: usize, pub dest_relative_path: String }
  pub fn quarantine_tree(cat: &Catalog, mount_root: &Path, expected_volume_id: &str,
                         tree_path: &str, now: i64) -> anyhow::Result<TreeQuarantineOutcome>;
  ```

- [ ] **Step 1: Write the failing tests**

Create `src/tree_quarantine.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::models::{Category, FileStatus, NewFile, Volume};
    use std::fs;

    fn drive_with_tree() -> (tempfile::TempDir, Catalog, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(root.join("copy/sub")).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("copy/a.txt"), b"AAA").unwrap();
        fs::write(root.join("copy/sub/b.txt"), b"BBB").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        for (p, h) in [("copy/a.txt", "HA"), ("copy/sub/b.txt", "HB")] {
            cat.upsert_file(&NewFile {
                volume_id: "vol-1".into(), relative_path: p.into(),
                filename: p.rsplit('/').next().unwrap().into(), extension: "txt".into(),
                size_bytes: 3, content_hash: h.into(),
                created_time: None, modified_time: None, accessed_time: None,
                category: Category::Document, container_chain: None,
            }, 100).unwrap();
        }
        (tmp, cat, root)
    }

    #[test]
    fn moves_the_whole_tree_with_one_rename_and_updates_every_row() {
        let (_t, cat, root) = drive_with_tree();
        let out = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap();

        assert!(!root.join("copy").exists(), "the original tree must be gone from its place");
        assert!(root.join("_ToDelete/copy/a.txt").is_file(), "moved, preserving structure");
        assert!(root.join("_ToDelete/copy/sub/b.txt").is_file(), "including subfolders");
        assert_eq!(out.files_updated, 2);

        // The bookkeeping is the point: a rename that left the catalogue stale would make the next
        // scan report two present files as missing.
        let rows = cat.quarantined_rows("vol-1").unwrap();
        assert_eq!(rows.len(), 2);
        let mut paths: Vec<String> = rows.iter().map(|r| r.relative_path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["_ToDelete/copy/a.txt", "_ToDelete/copy/sub/b.txt"]);
        assert!(rows.iter().all(|r| r.status == FileStatus::Quarantined));
        let mut origins: Vec<String> =
            rows.iter().filter_map(|r| r.original_path.clone()).collect();
        origins.sort();
        assert_eq!(origins, vec!["copy/a.txt", "copy/sub/b.txt"],
                   "original paths must survive, or the move is not reversible by hand");
    }

    #[test]
    fn refuses_a_tree_whose_files_are_not_all_active() {
        // Guards the window between the UI rendering a group and the user confirming it.
        let (_t, cat, root) = drive_with_tree();
        let id: i64 = cat.conn.query_row(
            "SELECT id FROM files WHERE relative_path='copy/a.txt'", [], |r| r.get(0)).unwrap();
        cat.mark_quarantined(id, "_ToDelete/x", "copy/a.txt", 150).unwrap();

        let err = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap_err();
        assert!(err.to_string().contains("no longer active"), "got: {err}");
        assert!(root.join("copy/a.txt").exists() || root.join("copy/sub/b.txt").exists(),
                "a refusal must not move anything");
    }

    #[test]
    fn refuses_when_the_drive_is_not_the_expected_volume() {
        let (_t, cat, root) = drive_with_tree();
        fs::write(root.join(".cleanupstorages_id"), "vol-OTHER").unwrap();
        let err = quarantine_tree(&cat, &root, "vol-1", "copy", 200).unwrap_err();
        assert!(err.to_string().contains("vol-OTHER"), "got: {err}");
        assert!(root.join("copy/a.txt").exists(), "nothing may move on the wrong drive");
    }

    #[test]
    fn refuses_to_quarantine_the_quarantine_or_the_volume_root() {
        let (_t, cat, root) = drive_with_tree();
        for bad in ["", "_ToDelete", "_ToDelete/copy"] {
            let err = quarantine_tree(&cat, &root, "vol-1", bad, 200).unwrap_err();
            assert!(err.to_string().contains("refusing"), "path {bad:?} gave: {err}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_quarantine
```
Expected: FAIL to compile — `quarantine_tree` does not exist.

- [ ] **Step 3: Implement**

Write above the tests in `src/tree_quarantine.rs`:

```rust
//! Quarantine a whole redundant directory with a single rename.
//!
//! The rename is an optimisation, NOT a shortcut around the bookkeeping: every file beneath the
//! tree still gets its catalogue row updated exactly as N individual quarantines would have. If the
//! rows were left stale the next scan would report present files as missing.

use crate::catalog::models::FileStatus;
use crate::catalog::Catalog;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct TreeQuarantineOutcome {
    pub files_updated: usize,
    pub dest_relative_path: String,
}

/// Move `tree_path` (relative to `mount_root`) into `_ToDelete`, then update every row beneath it.
pub fn quarantine_tree(
    cat: &Catalog,
    mount_root: &Path,
    expected_volume_id: &str,
    tree_path: &str,
    now: i64,
) -> anyhow::Result<TreeQuarantineOutcome> {
    let tree_path = tree_path.trim_end_matches('/');
    if tree_path.is_empty()
        || tree_path == crate::volume::QUARANTINE_DIR
        || tree_path.starts_with(&format!("{}/", crate::volume::QUARANTINE_DIR))
    {
        anyhow::bail!("refusing to quarantine {tree_path:?}: the volume root and the quarantine itself are off limits");
    }

    match crate::volume::read_volume_id(mount_root) {
        Some(vid) if vid == expected_volume_id => {}
        Some(vid) => anyhow::bail!(
            "drive at {} is volume {vid}, not the expected {expected_volume_id}; aborting",
            mount_root.display()
        ),
        None => anyhow::bail!(
            "no identity marker at {}; refusing to quarantine on an unidentified drive",
            mount_root.display()
        ),
    }

    // Everything beneath the tree, loose and active. Re-read now rather than trusting what the UI
    // was showing when the user clicked.
    let prefix = format!("{tree_path}/");
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = cat.conn.prepare(
            "SELECT id, relative_path, status FROM files
              WHERE volume_id=?1 AND container_chain IS NULL
                AND (relative_path=?2 OR relative_path LIKE ?3 ESCAPE '\\')",
        )?;
        let like = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_") + "%";
        stmt.query_map(rusqlite::params![expected_volume_id, tree_path, like], |r| {
            // Status stays a raw string: `FileStatus` has `as_str()` but no parse, and comparing
            // against the literal the schema stores is exactly as strict.
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    if rows.is_empty() {
        anyhow::bail!("no catalogued files under {tree_path:?} on volume {expected_volume_id}");
    }
    if let Some((_, p, s)) = rows.iter().find(|(_, _, s)| s != FileStatus::Active.as_str()) {
        anyhow::bail!("{p} is {s}, no longer active; refusing to quarantine this tree");
    }

    let src = mount_root.join(tree_path);
    if !src.is_dir() {
        anyhow::bail!("{} is not a directory on disk", src.display());
    }

    let dest_rel = tree_dest(cat, mount_root, tree_path);
    let dest = mount_root.join(&dest_rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Record the intent BEFORE the rename. If the process dies between the rename and the row
    // updates, this line is what tells the user where their files went.
    cat.log_action(
        "quarantine_tree_begin",
        &serde_json::json!({"volume_id": expected_volume_id, "from": tree_path,
                           "to": dest_rel, "files": rows.len()})
        .to_string(),
        now,
    )?;

    std::fs::rename(&src, &dest)
        .map_err(|e| anyhow::anyhow!("could not move {} to {}: {e}", src.display(), dest.display()))?;

    let mut files_updated = 0usize;
    for (id, rel, _) in &rows {
        let suffix = rel.strip_prefix(tree_path).unwrap_or(rel);
        let new_rel = format!("{dest_rel}{suffix}");
        cat.mark_quarantined(*id, &new_rel, rel, now)?;
        files_updated += 1;
    }

    cat.log_action(
        "quarantine_tree",
        &serde_json::json!({"volume_id": expected_volume_id, "from": tree_path,
                           "to": dest_rel, "files": files_updated})
        .to_string(),
        now,
    )?;

    Ok(TreeQuarantineOutcome { files_updated, dest_relative_path: dest_rel })
}

/// `_ToDelete/<tree_path>`, suffixed ` (n)` if that directory is already taken.
fn tree_dest(_cat: &Catalog, mount_root: &Path, tree_path: &str) -> String {
    let base = format!("{}/{tree_path}", crate::volume::QUARANTINE_DIR);
    if !mount_root.join(&base).exists() {
        return base;
    }
    for n in 1.. {
        let cand = format!("{base} ({n})");
        if !mount_root.join(&cand).exists() {
            return cand;
        }
    }
    unreachable!()
}
```

Add to `src/lib.rs`:
```rust
pub mod tree_quarantine;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_quarantine
```
Expected: PASS, 4 tests.

- [ ] **Step 5: Handle the case where the redundant "tree" is a whole archive**

A redundant `x/backup.zip` is a `DirNode` (its entries form its tree) but a **file** on disk, so the
implementation above would bail with "is not a directory on disk". Moving it is still just a rename,
so it must work — it is the second row of the spec's three-way table.

Write the failing test first, in the same test module:

```rust
    #[test]
    fn a_whole_redundant_archive_is_quarantined_as_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        fs::write(root.join("backup.zip"), b"ZIPBYTES").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&Volume { volume_id: "vol-1".into(), label: "D".into(),
            identified_by: "marker".into(), first_seen_at: 1, last_seen_at: 1 }).unwrap();
        // The archive's own row, plus one entry row -- exactly the shape that makes it a DirNode.
        cat.upsert_file(&NewFile {
            volume_id: "vol-1".into(), relative_path: "backup.zip".into(),
            filename: "backup.zip".into(), extension: "zip".into(),
            size_bytes: 8, content_hash: "ZIPHASH".into(),
            created_time: None, modified_time: None, accessed_time: None,
            category: Category::Archive, container_chain: None }, 100).unwrap();
        cat.conn.execute(
            "INSERT INTO files(volume_id, relative_path, filename, extension, size_bytes,
                 content_hash, category, container_chain, status, first_seen_at, last_seen_at)
             VALUES ('vol-1','backup.zip','inner.txt','txt',3,'HI','document','inner.txt',
                     'active',100,100)", []).unwrap();

        let out = quarantine_tree(&cat, &root, "vol-1", "backup.zip", 200).unwrap();
        assert!(!root.join("backup.zip").exists());
        assert!(root.join("_ToDelete/backup.zip").is_file(), "moved as one file");
        assert_eq!(out.files_updated, 2, "the archive row AND its entry row must both update");
    }
```

Run it to see it fail with "is not a directory on disk", then relax the check in `quarantine_tree`:

```rust
    let src = mount_root.join(tree_path);
    // A whole redundant archive is a DirNode (its entries are its tree) but a FILE on disk. Moving
    // it is still one rename, so accept either shape; only a path that is neither can be wrong.
    if !src.is_dir() && !src.is_file() {
        anyhow::bail!("{} does not exist on disk", src.display());
    }
```

and widen the row query so an archive's entry rows are included — they live under the SAME
`relative_path` with a non-null `container_chain`, so the `container_chain IS NULL` filter would miss
them and leave them stale:

```rust
    let mut stmt = cat.conn.prepare(
        "SELECT id, relative_path, container_chain, status FROM files
          WHERE volume_id=?1
            AND (relative_path=?2 OR relative_path LIKE ?3 ESCAPE '\\')",
    )?;
```

`mark_quarantined` already clears `container_chain`, turning an extracted entry into a proper loose
quarantined row — so the per-row update below needs no special case. Keep building `new_rel` from
`relative_path` only; an entry's `container_chain` is not part of its on-disk location.

Re-run: PASS, 5 tests.

- [ ] **Step 6: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/tree_quarantine.rs src/lib.rs
git commit   # feat(dedup): quarantine a redundant tree with one rename
```

---

### Task 5: Classify in-archive trees as "needs repack"

**Files:**
- Modify: `src/tree_hash.rs`

**Interfaces:**
- Produces:
  ```rust
  impl DirNode { pub fn archive_container(&self) -> Option<&str>; }
  ```
  Returns the archive path when this node sits inside one, else `None`. A node whose own path IS the
  archive returns `None` — a whole redundant archive is quarantined as one file, which is safe.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/tree_hash.rs`:

```rust
    fn node(path: &str) -> DirNode {
        DirNode { volume_id: "v".into(), path: path.into(), dir_hash: "H".into(),
                  file_count: 1, total_bytes: 1 }
    }

    #[test]
    fn a_folder_inside_an_archive_is_flagged_for_repack() {
        // 1,966 of the collapsed folders in the live catalogue are this case. A file inside a zip
        // cannot be renamed out of it, so offering a delete here would be offering an action the
        // tool cannot safely perform.
        assert_eq!(node("x/backup.zip/Photos").archive_container(), Some("x/backup.zip"));
        assert_eq!(node("x/backup.zip/a/b").archive_container(), Some("x/backup.zip"));
    }

    #[test]
    fn a_whole_archive_is_not_flagged_because_it_can_be_moved_as_one_file() {
        assert_eq!(node("x/backup.zip").archive_container(), None);
    }

    #[test]
    fn an_ordinary_folder_is_not_flagged() {
        assert_eq!(node("Photos/2019").archive_container(), None);
        assert_eq!(node("zipped things/2019").archive_container(), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash::tests::a_
```
Expected: FAIL — no method `archive_container`.

- [ ] **Step 3: Implement**

Add to `src/tree_hash.rs`:

```rust
/// Extensions that make a path segment an archive container. Matches the scanner's descent policy:
/// only formats we actually descend into can produce entry rows, so only they can appear here.
const ARCHIVE_SUFFIXES: &[&str] = &[".zip"];

impl DirNode {
    /// The archive this node lives inside, if any.
    ///
    /// `Some(..)` means the node CANNOT be quarantined: a file inside a zip cannot be renamed out
    /// of it, and the only correct remedy is the verified repack path. A node that IS the archive
    /// returns `None`, because moving the whole `.zip` is an ordinary single-file rename.
    pub fn archive_container(&self) -> Option<&str> {
        let mut end = 0usize;
        for (idx, _) in self.path.match_indices('/') {
            let seg = &self.path[..idx];
            let lower = seg.to_ascii_lowercase();
            if ARCHIVE_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
                end = idx;
            }
        }
        if end == 0 { None } else { Some(&self.path[..end]) }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib tree_hash
```
Expected: PASS, 14 tests.

- [ ] **Step 5: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/tree_hash.rs
git commit   # feat(dedup): flag in-archive trees as needing a repack
```

---

### Task 6: Rebuild the trees when what is active changes

**Files:**
- Modify: `src/commands.rs` (after a completed scan, after quarantine, after purge)

**Interfaces:**
- Consumes: `Catalog::rebuild_directory_trees(volume_id, now)`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/commands.rs` (or `src/catalog/store.rs` if `commands.rs` has none):

```rust
    #[test]
    fn a_completed_scan_leaves_usable_directory_trees() {
        // Without this wiring the feature silently shows nothing: the table stays empty and the
        // Duplicates page reports no trees on a catalogue full of them.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(root.join("orig")).unwrap();
        std::fs::create_dir_all(root.join("copy")).unwrap();
        std::fs::write(root.join(".cleanupstorages_id"), "vol-1").unwrap();
        std::fs::write(root.join("orig/a.txt"), b"SAME").unwrap();
        std::fs::write(root.join("copy/a.txt"), b"SAME").unwrap();

        let cat = Catalog::open(&tmp.path().join("c.db")).unwrap();
        cat.upsert_volume(&crate::catalog::models::Volume {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into(),
            first_seen_at: 1, last_seen_at: 1 }).unwrap();
        let ident = crate::volume::VolumeIdentity {
            volume_id: "vol-1".into(), label: "D".into(), identified_by: "marker".into() };
        crate::scanner::scan_volume(&cat, &root, &ident, false, 100, &test_limits()).unwrap();
        cat.rebuild_directory_trees("vol-1", 100).unwrap();

        let groups = cat.tree_duplicate_groups().unwrap();
        assert_eq!(groups.len(), 1, "orig and copy hold the same file");
        assert_eq!(groups[0].members.len(), 2);
    }
```

Reuse the `test_limits()` helper already defined in `src/quarantine.rs`'s tests; copy it into this
test module if it is not in scope.

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib a_completed_scan_leaves
```
Expected: FAIL — no groups, because nothing rebuilds the table.

- [ ] **Step 3: Implement the wiring**

In `src/commands.rs`, in the scan command, after the scan returns successfully **and only when it was
not stopped early**, add:

```rust
    // Derived from the rows the scan just wrote. Skipped for a stopped scan: its picture of the
    // volume is incomplete by definition, and hashing a half-seen tree would invent folders that
    // differ only because the scan did not reach the rest of them.
    if !summary.stopped {
        let n = cat.rebuild_directory_trees(&ident.volume_id, now)?;
        tracing::info!(directories = n, "rebuilt directory trees");
    }
```

In `cmd_quarantine`, after `quarantine_files` returns, and in the purge command after it completes,
add:

```rust
    cat.rebuild_directory_trees(volume_id, now)?;
```

Use whichever local variable holds the volume id at each site (`expected_volume_id`, `ident.volume_id`
or `rec.volume_id`). If `ScanSummary` has no `stopped` field, use the existing signal the scan
command already checks to decide whether to run the missing-file sweep.

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib a_completed_scan_leaves
```
Expected: PASS.

- [ ] **Step 5: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/commands.rs
git commit   # feat(dedup): rebuild directory trees after scan, quarantine and purge
```

---

### Task 7: The web API

**Files:**
- Modify: `src/web.rs` (routes at lines 54-80; handlers near `api_duplicates` at line 675)

**Interfaces:**
- Produces:
  - `GET /api/tree-duplicates` →
    ```json
    { "groups": [ { "dir_hash": "...", "reclaimable_bytes": 13440000000,
                    "file_count": 113013,
                    "members": [ { "volume_id": "...", "volume_label": "D:\\",
                                   "path": "backupApple/Gio/Progetti/LLM",
                                   "total_bytes": 13440000000,
                                   "needs_repack": false, "archive": null } ] } ] }
    ```
  - `POST /api/quarantine-tree` with `{"volume_id": "...", "path": "...", "mount": "D:\\"}` →
    `{"files_updated": 2, "dest": "_ToDelete/copy"}`. Requires the `x-cleanup-token` CSRF header.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/web.rs`:

```rust
    #[tokio::test]
    async fn tree_duplicates_lists_a_group_with_its_blast_radius() {
        let (_t, db) = seeded_db_with_identical_trees();
        let v = get_json(&db, "/api/tree-duplicates").await;
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["members"].as_array().unwrap().len(), 2);
        assert!(groups[0]["file_count"].as_i64().unwrap() > 0,
                "the UI cannot show a blast radius without a file count");
        assert!(groups[0]["reclaimable_bytes"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn quarantine_tree_requires_csrf_token() {
        // Same contract as every other mutating endpoint: the header is x-cleanup-token.
        let (_t, db) = seeded_db_with_identical_trees();
        let app = super::app(state_for(db));
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/quarantine-tree")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"volume_id":"vol-1","path":"copy","mount":"X"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_tree_inside_an_archive_is_marked_needs_repack() {
        let (_t, db) = seeded_db_with_identical_archive_trees();
        let v = get_json(&db, "/api/tree-duplicates").await;
        let m = &v["groups"][0]["members"][0];
        assert_eq!(m["needs_repack"], true);
        assert!(m["archive"].is_string(), "the UI must be able to name the container");
    }
```

Write the two seed helpers beside the existing seed helpers in that test module, following whatever
pattern they use to build a `Catalog` and return the state; each inserts two identical trees (one
loose pair, one pair inside `x/backup.zip`) and calls `cat.rebuild_directory_trees("vol-1", 100)`.

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib web::tests::tree_
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib web::tests::quarantine_tree
```
Expected: FAIL — routes return 404.

- [ ] **Step 3: Implement**

Add the routes beside the existing ones in `src/web.rs`:

```rust
        .route("/api/tree-duplicates", get(api_tree_duplicates))
        .route("/api/quarantine-tree", post(api_quarantine_tree))
```

Add the handlers near `api_duplicates`:

```rust
async fn api_tree_duplicates(State(st): State<AppState>) -> impl IntoResponse {
    let cat = st.catalog.lock().await;
    // `volume_stats()` returns (volume_id, label, ..) -- there is no `volumes()` on Catalog.
    let labels: std::collections::HashMap<String, String> = match cat.volume_stats() {
        Ok(vs) => vs.into_iter().map(|(id, label, _, _)| (id, label)).collect(),
        Err(_) => Default::default(),
    };
    match cat.tree_duplicate_groups() {
        Ok(groups) => {
            let out: Vec<serde_json::Value> = groups
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "dir_hash": g.dir_hash,
                        "reclaimable_bytes": g.reclaimable_bytes,
                        "file_count": g.members.first().map(|m| m.file_count).unwrap_or(0),
                        "members": g.members.iter().map(|m| serde_json::json!({
                            "volume_id": m.volume_id,
                            "volume_label": labels.get(&m.volume_id).cloned()
                                                  .unwrap_or_else(|| m.volume_id.clone()),
                            "path": m.path,
                            "total_bytes": m.total_bytes,
                            "needs_repack": m.archive_container().is_some(),
                            "archive": m.archive_container(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            axum::Json(serde_json::json!({ "groups": out })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct QuarantineTreeReq {
    volume_id: String,
    path: String,
    mount: String,
}

async fn api_quarantine_tree(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<QuarantineTreeReq>,
) -> impl IntoResponse {
    if let Err(r) = check_csrf(&st, &headers) {
        return r;
    }
    let cat = st.catalog.lock().await;
    let now = crate::commands::now_secs(); // pub(crate) in commands.rs, not observability
    match crate::tree_quarantine::quarantine_tree(
        &cat,
        std::path::Path::new(&req.mount),
        &req.volume_id,
        &req.path,
        now,
    ) {
        Ok(out) => {
            // The group the user just acted on is stale now; rebuild before anyone reads it again.
            let _ = cat.rebuild_directory_trees(&req.volume_id, now);
            axum::Json(serde_json::json!({
                "files_updated": out.files_updated, "dest": out.dest_relative_path
            }))
            .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
```

Match the surrounding handlers' exact style for `State`, locking and `check_csrf` — copy from
`api_quarantine` rather than assuming the shapes above are identical to this codebase's.

- [ ] **Step 4: Keep archive-internal duplicates out of the per-file queue**

Descending into archives adds 18,396 duplicate groups that exist only *inside* archives. Deleting one
entry requires repacking its container, so listing them as ordinary review items hands the user
~43,000 decisions with no safe action attached — the opposite of what this epic is for. They still
participate fully in tree matching, which is where their value is.

Write the failing test first:

```rust
    #[tokio::test]
    async fn per_file_duplicates_exclude_entries_inside_archives() {
        // Not actionable one at a time: you cannot delete a file inside a zip without repacking it.
        let (_t, db) = seeded_db_with_identical_archive_trees();
        let v = get_json(&db, "/api/duplicates").await;
        let groups = v["groups"].as_array().unwrap();
        assert!(
            groups.iter().all(|g| g["files"].as_array().unwrap().iter()
                .all(|f| f["container_chain"].is_null())),
            "archive entries must not appear in the per-file duplicate queue: {groups:?}"
        );
    }
```

Adjust the JSON field names to match what `api_duplicates` actually returns. Then add
`AND container_chain IS NULL` to the duplicate-group query behind `api_duplicates`, with a comment
saying why. If that query lives in `src/catalog/store.rs`, change it there and note that
`tree_duplicate_groups` deliberately does **not** share the restriction.

Run: PASS.

- [ ] **Step 5: Run the tests to verify they pass**

Run:
```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test --lib web
```
Expected: PASS.

- [ ] **Step 6: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/web.rs src/catalog/store.rs
git commit   # feat(review): tree-duplicates API and tree quarantine endpoint
```

---

### Task 8: The Duplicates-page section

**Files:**
- Modify: `src/web_ui.rs`

**Interfaces:**
- Consumes: `GET /api/tree-duplicates`, `POST /api/quarantine-tree`, and the shared `apiGet`/`apiPost`
  helpers in the page shell (`src/web_ui.rs:370`). `apiPost` **throws** on a non-2xx response.

- [ ] **Step 1: Add the section**

At the top of the Duplicates page, above the existing per-file list, render identical trees first —
they are the decisions worth making. Follow the page's existing markup and class names.

```html
<section id="tree-dupes">
  <h2>Identical folders</h2>
  <p class="hint">Whole folders whose contents match exactly. Confirming one moves the entire folder
     to <code>_ToDelete</code> in a single rename — nothing is deleted until you empty it yourself.</p>
  <div id="tree-dupes-list"></div>
</section>
```

```js
async function loadTreeDupes() {
  const data = await apiGet('/api/tree-duplicates');
  const host = document.getElementById('tree-dupes-list');
  host.innerHTML = '';
  if (!data.groups.length) {
    host.innerHTML = '<p class="hint">No identical folders found.</p>';
    return;
  }
  for (const g of data.groups) {
    const el = document.createElement('div');
    el.className = 'tree-group';
    // Blast radius first: file count, bytes, and the FULL path of every side. The user is about to
    // move thousands of files with one click and must see the size of that before deciding.
    el.innerHTML =
      '<div class="tree-head"><strong>' + g.file_count.toLocaleString() + ' files</strong>' +
      ' &middot; ' + fmtBytes(g.reclaimable_bytes) + ' reclaimable</div>';
    for (const m of g.members) {
      const row = document.createElement('div');
      row.className = 'tree-member';
      const label = document.createElement('code');
      label.textContent = m.volume_label + ' / ' + m.path;
      row.appendChild(label);
      if (m.needs_repack) {
        const note = document.createElement('span');
        note.className = 'badge';
        note.textContent = 'inside ' + m.archive + ' — needs repack';
        row.appendChild(note);
      } else {
        const btn = document.createElement('button');
        btn.textContent = 'Quarantine this copy';
        btn.onclick = () => confirmTreeQuarantine(g, m, btn);
        row.appendChild(btn);
      }
      el.appendChild(row);
    }
    host.appendChild(el);
  }
}

async function confirmTreeQuarantine(group, member, btn) {
  const ok = confirm(
    'Move this entire folder to _ToDelete?\n\n' +
    member.volume_label + ' / ' + member.path + '\n' +
    group.file_count.toLocaleString() + ' files, ' + fmtBytes(member.total_bytes) + '\n\n' +
    'The other copy stays where it is. Nothing is deleted until you empty _ToDelete yourself.');
  if (!ok) return;
  btn.disabled = true;
  try {
    const mount = await promptMountFor(member.volume_id);
    if (!mount) { btn.disabled = false; return; }
    const res = await apiPost('/api/quarantine-tree', {
      volume_id: member.volume_id, path: member.path, mount });
    toast(res.files_updated + ' files moved to ' + res.dest);
    await loadTreeDupes();
  } catch (e) {
    // apiPost throws on non-2xx; a refusal (tree no longer active, wrong drive) lands here and
    // must be shown, not swallowed.
    alert('Could not quarantine: ' + e.message);
    btn.disabled = false;
  }
}
```

Reuse the page's existing helpers for `fmtBytes`, `toast`, and whatever the Duplicates page already
uses to resolve a volume's mount point — do not invent `promptMountFor` if an equivalent exists;
substitute the existing one.

- [ ] **Step 2: Verify in a browser**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo run -- browse
```
Open the Duplicates page. Confirm: identical folders appear above the per-file list, each shows file
count and reclaimable bytes, in-archive members show the "needs repack" badge with **no** button, and
the confirm dialog names both the path and the file count.

Headless check, if a browser is not available:
```bash
"/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" \
  --headless=new --disable-gpu --screenshot=dupes.png http://127.0.0.1:PORT/duplicates
```

- [ ] **Step 3: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add src/web_ui.rs
git commit   # feat(review): show identical folders above per-file duplicates
```

---

### Task 9: Validate against the live catalogue and write it down

**Files:**
- Modify: `docs/benchmarking-scans.md` (new section) or create `docs/tree-collapse-results.md`

- [ ] **Step 1: Reproduce the measured numbers with the real implementation**

The Python script that produced the spec's numbers is committed at
`scripts/measure-tree-collapse.py`. The Rust implementation must agree with it, or one of them is
wrong.

Copy the live catalogue (never open the original for writing):

```bash
cp "$APPDATA/justPrototype/CleanUpStorages/data/catalog.db" /tmp/verify.db
python scripts/measure-tree-collapse.py descend   # run from the dir holding cat-copy.db
```

Then run the Rust path over the same copy — a small `#[test]`, or a temporary binary that opens
`/tmp/verify.db`, calls `rebuild_directory_trees` for each volume and prints
`tree_duplicate_groups().len()` and the summed `reclaimable_bytes`.

Expected: **≈1,458 maximal groups, ≈53.2 GB reclaimable.**

- [ ] **Step 2: If the numbers disagree, find out which is right before continuing**

Do not adjust one to match the other. The most likely causes, in order: the archive-root replacement
(is a `.zip` with entries being dropped as a leaf?), empty-tree exclusion, and the maximal rule's
parent lookup. Write down what the discrepancy was and which implementation was wrong.

- [ ] **Step 3: Record the results**

Document, next to the existing tables: the number of maximal groups, folders involved, reclaimable
bytes, how many groups still need per-file review, and how many members are `needs_repack`. State
plainly that this is the partial catalogue (3 volumes, 808,588 rows), not the full 20 TB, so the
ratio is directional.

- [ ] **Step 4: Gates and commit**

```bash
CLEANUPSTORAGES_DATA_DIR=$(mktemp -d) cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git add docs/
git commit   # docs(dedup): identical-tree collapse measured against the live catalogue
```

---

## Final review

Before opening the PR, check:

1. **Does the Rust implementation agree with the committed Python measurement?** If not, the feature's
   headline number is unsupported.
2. **Is the archive-root replacement right?** An archive must never appear both as a leaf file and as
   a directory. This is the single easiest thing here to get wrong, and it silently corrupts every
   ancestor hash — it already broke the first measurement run.
3. **Can any path offer a delete for a folder inside an archive?** It must not.
4. **Does a tree quarantine leave the catalogue and disk in agreement?** Run a scan afterwards and
   confirm nothing is reported missing.
5. **Is the still-active re-check actually before the rename, and does a refusal move nothing?**
6. **Does the confirm dialog show file count, bytes, and both full paths?** The blast radius is the
   mitigation for making decisions coarser.

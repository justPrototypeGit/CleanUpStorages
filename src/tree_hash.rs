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
    pub path: String,
    pub content_hash: String,
    pub size_bytes: i64,
    /// True for a loose row, which is a *candidate* archive root. Such a row is dropped only when
    /// entry rows actually exist for the same path, because the archive is then a directory rather
    /// than a leaf.
    pub is_archive_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirNode {
    pub volume_id: String,
    pub path: String,
    pub dir_hash: String,
    pub file_count: i64,
    pub total_bytes: i64,
    /// The archive this node lives *inside*, if any.
    ///
    /// `Some(..)` means the node CANNOT be quarantined: a file inside a zip cannot be renamed out
    /// of it, and the only correct remedy is the verified repack path. A node that IS the archive
    /// holds `None`, because moving the whole container is an ordinary single-file rename.
    ///
    /// Derived from which paths actually have entry rows, NOT from a list of archive extensions.
    /// The scanner's descent policy is user-configurable (the allow-list can add `.kra`, `.3mf`,
    /// anything), so a hard-coded suffix list would silently misclassify every format added after
    /// this was written.
    pub archive_root: Option<String>,
}

impl DirNode {
    /// The archive containing this node, or `None` when it can be moved as an ordinary path.
    pub fn archive_container(&self) -> Option<&str> {
        self.archive_root.as_deref()
    }
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
pub fn build_dir_hashes(volume_id: &str, rows: Vec<TreeInput>) -> Vec<DirNode> {
    // Keyed on path alone: a rebuild is always scoped to ONE volume, so carrying the volume id in
    // every row and every map key cost a String clone per file for no information. On a 20 TB
    // corpus that is tens of millions of redundant allocations.
    //
    // An archive that HAS entries is a directory; its own file row must not also appear as a leaf.
    // Getting this wrong makes the archive its own sibling and corrupts every ancestor hash.
    // EVERY ancestor, not just the immediate parent: an archive's entries are usually nested
    // (`backup.zip/Project/.git/config`), so checking only the immediate parent would fail to
    // recognise the archive at all -- leaving it both a leaf file and a directory, which is
    // precisely the corruption this replacement exists to prevent.
    // Paths that turned out to be real archives: a loose row whose entries also exist. Recorded
    // from the data rather than from an extension list, so a newly allow-listed format is handled
    // without a code change.
    //
    // Computed in its own scope so `has_children` can BORROW the paths -- it never outlives this
    // block, which lets the main loop below CONSUME `rows` and move each content_hash instead of
    // cloning it. Holding rows alive to keep a borrow costs a full extra copy of every hash.
    let archive_roots: HashSet<String> = {
        let mut has_children: HashSet<&str> = HashSet::new();
        for r in &rows {
            for (i, _) in r.path.match_indices('/') {
                has_children.insert(&r.path[..i]);
            }
        }
        rows.iter()
            .filter(|r| r.is_archive_root && has_children.contains(r.path.as_str()))
            .map(|r| r.path.clone())
            .collect()
    };

    let mut children: HashMap<String, BTreeMap<String, Child>> = HashMap::new();
    let mut dirs: HashSet<String> = HashSet::new();

    for r in rows {
        if archive_roots.contains(&r.path) {
            continue; // replaced by its entry tree
        }
        let parts: Vec<&str> = r.path.split('/').collect();
        let leaf_parent = parts[..parts.len() - 1].join("/");
        children.entry(leaf_parent).or_default().insert(
            parts[parts.len() - 1].to_string(),
            Child::File {
                hash: r.content_hash,
                size: r.size_bytes,
            },
        );
        // Register every ancestor. `Dir` overwrites a `File` of the same name, which is exactly the
        // archive case: the .zip row was inserted as a leaf before its entries were seen.
        for i in (1..parts.len()).rev() {
            let me = parts[..i].join("/");
            let parent = parts[..i - 1].join("/");
            dirs.insert(me);
            children
                .entry(parent)
                .or_default()
                .insert(parts[i - 1].to_string(), Child::Dir);
        }
        dirs.insert(String::new());
    }

    let depth = |p: &str| {
        if p.is_empty() {
            0
        } else {
            p.matches('/').count() + 1
        }
    };
    let mut ordered: Vec<String> = dirs.into_iter().collect();
    // Deepest first, so a directory's children are always hashed before it is.
    ordered.sort_by(|a, b| depth(b).cmp(&depth(a)).then(a.cmp(b)));

    let mut out: HashMap<String, DirNode> = HashMap::new();
    for key in ordered {
        let Some(kids) = children.get(&key) else {
            continue;
        };
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
                    let sub = if key.is_empty() {
                        name.clone()
                    } else {
                        format!("{key}/{name}")
                    };
                    if let Some(n) = out.get(&sub) {
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
        let dir_hash = blake3::hash(lines.join("\n").as_bytes())
            .to_hex()
            .to_string();
        // The innermost archive that is a STRICT ancestor. The archive itself is not "inside" one:
        // moving a whole redundant .zip is a single rename, and must stay offerable.
        let archive_root = archive_roots
            .iter()
            .filter(|a| {
                key.len() > a.len()
                    && key.starts_with(a.as_str())
                    && key.as_bytes()[a.len()] == b'/'
            })
            .cloned()
            .max_by_key(|a| a.len());
        out.insert(
            key.clone(),
            DirNode {
                volume_id: volume_id.to_string(),
                path: key,
                dir_hash,
                file_count: nf,
                total_bytes: nb,
                archive_root,
            },
        );
    }
    let mut v: Vec<DirNode> = out.into_values().collect();
    v.sort_by(|a, b| (&a.volume_id, &a.path).cmp(&(&b.volume_id, &b.path)));
    v
}

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
                // A top-level folder's parent is the volume root, "".
                None => !n.path.is_empty() && !twinned.contains(&(n.volume_id.as_str(), "")),
            })
            .map(|n| (*n).clone())
            .collect();
        if maximal.len() < 2 {
            continue;
        }
        let reclaimable_bytes = maximal[0].total_bytes * (maximal.len() as i64 - 1);
        out.push(TreeGroup {
            dir_hash: hash.to_string(),
            members: maximal,
            reclaimable_bytes,
        });
    }
    out.sort_by(|a, b| {
        b.reclaimable_bytes
            .cmp(&a.reclaimable_bytes)
            .then(a.dir_hash.cmp(&b.dir_hash))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(_vol: &str, path: &str, hash: &str, size: i64) -> TreeInput {
        TreeInput {
            path: path.into(),
            content_hash: hash.into(),
            size_bytes: size,
            is_archive_root: false,
        }
    }
    fn hash_of(nodes: &[DirNode], path: &str) -> Option<String> {
        nodes
            .iter()
            .find(|n| n.path == path)
            .map(|n| n.dir_hash.clone())
    }

    #[test]
    fn two_folders_with_the_same_contents_match_despite_different_names() {
        // The whole point: a copied folder is usually renamed, so the folder's OWN name must not
        // be an input to its hash.
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "Photos2019/a.jpg", "H1", 10),
                f("v", "Photos2019/b.jpg", "H2", 20),
                f("v", "Photos 2019 copy/a.jpg", "H1", 10),
                f("v", "Photos 2019 copy/b.jpg", "H2", 20),
            ],
        );
        assert_eq!(
            hash_of(&nodes, "Photos2019"),
            hash_of(&nodes, "Photos 2019 copy")
        );
    }

    #[test]
    fn a_child_filename_difference_breaks_the_match() {
        // Children's names ARE part of the hash: same bytes under a different name is a different
        // folder, or renaming one file inside a backup would silently make it "identical".
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "A/a.jpg", "H1", 10),
                f("v", "B/renamed.jpg", "H1", 10),
            ],
        );
        assert_ne!(hash_of(&nodes, "A"), hash_of(&nodes, "B"));
    }

    #[test]
    fn an_archive_becomes_a_directory_and_its_own_file_hash_is_ignored() {
        // The trap this test exists for: an archive is BOTH a file row and a set of entry rows.
        // If both fed the hash, the archive would appear as its own sibling and every ancestor's
        // hash would be wrong. Here the two archives have DIFFERENT content_hash (different
        // compression) but identical contents, so they must match.
        let rows = vec![
            TreeInput {
                path: "x/one.zip".into(),
                content_hash: "ZIP_A".into(),
                size_bytes: 99,
                is_archive_root: true,
            },
            f("v", "x/one.zip/inner.txt", "H1", 10),
            TreeInput {
                path: "y/two.zip".into(),
                content_hash: "ZIP_B".into(),
                size_bytes: 77,
                is_archive_root: true,
            },
            f("v", "y/two.zip/inner.txt", "H1", 10),
        ];
        let nodes = build_dir_hashes("v", rows);
        assert_eq!(
            hash_of(&nodes, "x/one.zip"),
            hash_of(&nodes, "y/two.zip"),
            "identical contents, different compression, must still match"
        );
        let z = nodes.iter().find(|n| n.path == "x/one.zip").unwrap();
        assert_eq!(
            z.file_count, 1,
            "the archive's own row must not be counted as a child file"
        );
        assert_eq!(
            z.total_bytes, 10,
            "bytes come from the entries, not the .zip's own size"
        );
    }

    #[test]
    fn an_archive_whose_entries_are_deeply_nested_is_still_replaced() {
        // Regression: recognising an archive by its entries' IMMEDIATE parent only works when the
        // entries sit directly inside it. Real archives nest (backup.zip/Project/.git/config), and
        // failing to recognise them leaves the .zip as BOTH a leaf file and a directory -- which
        // corrupts every ancestor hash silently. The live catalogue is full of this shape.
        let nodes = build_dir_hashes(
            "v",
            vec![
                TreeInput {
                    path: "x/backup.zip".into(),
                    content_hash: "ZIPBYTES".into(),
                    size_bytes: 9999,
                    is_archive_root: true,
                },
                f("v", "x/backup.zip/Project/.git/config", "H1", 10),
            ],
        );
        let x = nodes.iter().find(|n| n.path == "x").unwrap();
        assert_eq!(
            x.file_count, 1,
            "the .zip's own row must not be counted alongside its entries"
        );
        assert_eq!(
            x.total_bytes, 10,
            "bytes must come from the entries, not the container's compressed size"
        );
    }

    #[test]
    fn an_archive_with_no_entries_stays_an_ordinary_file() {
        // A zip we never descended into (deny-listed .docx, or unreadable) has no entry rows. It
        // must remain a leaf with its own content hash, not vanish.
        let nodes = build_dir_hashes(
            "v",
            vec![TreeInput {
                path: "d/report.docx".into(),
                content_hash: "DOCX".into(),
                size_bytes: 5,
                is_archive_root: true,
            }],
        );
        let d = nodes.iter().find(|n| n.path == "d").unwrap();
        assert_eq!(d.file_count, 1);
        assert_eq!(d.total_bytes, 5);
        assert!(
            hash_of(&nodes, "d/report.docx").is_none(),
            "no entries, so not a directory"
        );
    }

    #[test]
    fn empty_directories_are_not_emitted() {
        let nodes = build_dir_hashes("v", vec![f("v", "a/b/c.txt", "H1", 1)]);
        assert!(nodes.iter().all(|n| n.file_count > 0));
    }

    #[test]
    fn counts_and_bytes_roll_up_through_subdirectories() {
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "top/one.txt", "H1", 10),
                f("v", "top/sub/two.txt", "H2", 20),
                f("v", "top/sub/deep/three.txt", "H3", 30),
            ],
        );
        let top = nodes.iter().find(|n| n.path == "top").unwrap();
        assert_eq!(top.file_count, 3);
        assert_eq!(top.total_bytes, 60);
    }

    #[test]
    fn the_same_folder_on_two_drives_matches_across_them() {
        // A rebuild is scoped to one volume, so cross-drive matching happens when the resulting
        // nodes are grouped together. This is the case the whole project cares about: the same
        // folder copied onto a second HDD.
        let mut nodes = build_dir_hashes("v1", vec![f("", "A/a.txt", "H1", 1)]);
        nodes.extend(build_dir_hashes(
            "v2",
            vec![f("", "Backup of A/a.txt", "H1", 1)],
        ));

        let a = nodes.iter().find(|n| n.path == "A").unwrap();
        let b = nodes.iter().find(|n| n.path == "Backup of A").unwrap();
        assert_eq!(a.volume_id, "v1");
        assert_eq!(b.volume_id, "v2");
        assert_eq!(
            a.dir_hash, b.dir_hash,
            "same contents on different drives must hash alike"
        );

        let groups = maximal_groups(&nodes);
        assert_eq!(groups.len(), 1, "and must be reported as one decision");
        assert_eq!(groups[0].members.len(), 2);
    }

    #[test]
    fn only_the_topmost_matching_folder_is_reported() {
        // A duplicated tree would otherwise report every subfolder inside it, which is exactly the
        // 125,977-decision problem this feature exists to remove.
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "orig/sub/deep/a.txt", "H1", 10),
                f("v", "orig/sub/b.txt", "H2", 20),
                f("v", "copy/sub/deep/a.txt", "H1", 10),
                f("v", "copy/sub/b.txt", "H2", 20),
            ],
        );
        let groups = maximal_groups(&nodes);
        let reported: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.members.iter().map(|m| m.path.as_str()))
            .collect();
        assert_eq!(groups.len(), 1, "one decision, not three");
        assert!(reported.contains(&"orig") && reported.contains(&"copy"));
        assert!(
            !reported.iter().any(|p| p.contains('/')),
            "no subfolder may be reported: {reported:?}"
        );
    }

    #[test]
    fn reclaimable_bytes_keeps_one_copy() {
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "a/x.bin", "H1", 100),
                f("v", "b/x.bin", "H1", 100),
                f("v", "c/x.bin", "H1", 100),
            ],
        );
        let groups = maximal_groups(&nodes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 3);
        assert_eq!(
            groups[0].reclaimable_bytes, 200,
            "3 copies of 100 bytes reclaims 200"
        );
    }

    #[test]
    fn a_folder_without_a_twin_is_not_a_group() {
        let nodes = build_dir_hashes("v", vec![f("v", "lonely/x.txt", "H1", 1)]);
        assert!(maximal_groups(&nodes).is_empty());
    }

    #[test]
    fn a_folder_inside_an_archive_is_flagged_for_repack() {
        // 1,966 of the collapsed folders in the live catalogue are this case. A file inside a zip
        // cannot be renamed out of it, so offering a delete here would be offering an action the
        // tool cannot safely perform.
        let nodes = build_dir_hashes(
            "v",
            vec![
                TreeInput {
                    path: "x/backup.zip".into(),
                    content_hash: "Z".into(),
                    size_bytes: 1,
                    is_archive_root: true,
                },
                f("v", "x/backup.zip/Photos/a.jpg", "H1", 10),
            ],
        );
        let inner = nodes
            .iter()
            .find(|n| n.path == "x/backup.zip/Photos")
            .unwrap();
        assert_eq!(inner.archive_container(), Some("x/backup.zip"));

        // The archive ITSELF can be moved as one file, so it must not be flagged.
        let zip = nodes.iter().find(|n| n.path == "x/backup.zip").unwrap();
        assert_eq!(zip.archive_container(), None);

        // Nor may an ordinary folder be flagged, whatever it is called.
        let plain = nodes.iter().find(|n| n.path == "x").unwrap();
        assert_eq!(plain.archive_container(), None);
    }

    #[test]
    fn a_folder_merely_named_like_an_archive_is_not_flagged() {
        // Guards against classifying by extension: this is a real directory called "stuff.zip"
        // with no entry rows, so nothing about it needs a repack.
        let nodes = build_dir_hashes("v", vec![f("v", "stuff.zip/inner/a.txt", "H1", 1)]);
        let inner = nodes.iter().find(|n| n.path == "stuff.zip/inner").unwrap();
        assert_eq!(
            inner.archive_container(),
            None,
            "no entry rows means no archive, whatever the name suggests"
        );
    }

    #[test]
    fn groups_are_ranked_by_reclaimable_bytes() {
        let nodes = build_dir_hashes(
            "v",
            vec![
                f("v", "small1/s.bin", "S", 1),
                f("v", "small2/s.bin", "S", 1),
                f("v", "big1/b.bin", "B", 5000),
                f("v", "big2/b.bin", "B", 5000),
            ],
        );
        let groups = maximal_groups(&nodes);
        assert_eq!(groups[0].reclaimable_bytes, 5000, "biggest win first");
    }
}

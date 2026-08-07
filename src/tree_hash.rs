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

/// Receives directory nodes as the streaming fold finalises them.
///
/// The point of the trait is that nodes never have to be collected: a sink can write each one
/// straight to the database, so a 20 TB rebuild holds the current spine and nothing else.
pub trait DirSink {
    fn emit(&mut self, node: DirNode) -> anyhow::Result<()>;
}

impl DirSink for Vec<DirNode> {
    fn emit(&mut self, node: DirNode) -> anyhow::Result<()> {
        self.push(node);
        Ok(())
    }
}

/// One open directory on the spine.
struct Frame {
    path: String,
    /// `name\0kind\0hash` per child seen so far. Bounded by this directory's width, not by the
    /// corpus: a directory's hash simply cannot be computed before all its children are known.
    lines: Vec<String>,
    file_count: i64,
    total_bytes: i64,
    /// This frame is an archive whose entries form its contents.
    is_archive_root: bool,
}

impl Frame {
    fn new(path: String, is_archive_root: bool) -> Self {
        Frame {
            path,
            lines: Vec::new(),
            file_count: 0,
            total_bytes: 0,
            is_archive_root,
        }
    }
}

/// Fold path-ordered rows into directory hashes, emitting each node as it is finalised.
///
/// `rows` MUST be ordered by `path` under SQLite's BINARY collation. Two properties make this work,
/// both verified against every row of the live catalogue rather than assumed (see the design spec):
///
/// - a directory's descendants are contiguous, because every string with prefix `a/` lies in
///   `["a/", "a0")` and nothing else does;
/// - an archive's own row lands immediately before its entries, because an entry's path extends the
///   archive's own and `/` sorts below any ordinary name character. That is what lets a single row
///   of lookahead replace a full pre-pass over the corpus.
///
/// Returns the number of directories emitted.
pub fn stream_dir_hashes<I, S>(volume_id: &str, rows: I, sink: &mut S) -> anyhow::Result<usize>
where
    I: IntoIterator<Item = anyhow::Result<TreeInput>>,
    S: DirSink,
{
    let mut stack: Vec<Frame> = Vec::new();
    let mut emitted = 0usize;
    let mut prev_path: Option<String> = None;
    let mut pending: Option<TreeInput> = None;
    let mut iter = rows.into_iter();

    loop {
        // A row error must abort: hashing the rest would describe a tree that is missing files.
        // `pending` holds the already-unwrapped lookahead row from the previous iteration.
        let row = match pending.take() {
            Some(r) => r,
            None => match iter.next() {
                Some(r) => r?,
                None => break,
            },
        };
        // The fold is silently WRONG on unordered input -- it would close a directory early and
        // hash a fragment of it, yielding plausible-looking duplicate groups the user would then
        // act on. One comparison per row is cheap insurance against that.
        if let Some(prev) = &prev_path {
            if row.path.as_str() <= prev.as_str() {
                anyhow::bail!(
                    "rows are not in ascending path order: {:?} came after {:?}; \
                     the directory hash fold requires ORDER BY path (BINARY collation)",
                    row.path,
                    prev
                );
            }
        }
        prev_path = Some(row.path.clone());

        // One row of lookahead: if the next path is inside this one, this row is an archive whose
        // entries are its contents, so its own content hash is discarded and it becomes a directory.
        let next = iter.next().transpose()?;
        let is_archive = row.is_archive_root
            && next.as_ref().is_some_and(|n| {
                n.path.len() > row.path.len() + 1 && n.path.starts_with(&format!("{}/", row.path))
            });
        pending = next;

        let parts: Vec<&str> = row.path.split('/').collect();
        // Directories this row lives in. For an archive root that includes the archive itself.
        let dir_depth = if is_archive {
            parts.len()
        } else {
            parts.len() - 1
        };

        // How much of the open spine this row still shares. Frame `i` (ignoring the implicit root
        // frame at index 0) holds the directory `parts[..=i]`, so the shared depth is however many
        // leading components still match.
        let root_offset = usize::from(stack.first().is_some_and(|f| f.path.is_empty()));
        let mut shared = 0usize;
        while shared < dir_depth
            && shared + root_offset < stack.len()
            && stack[shared + root_offset].path == parts[..=shared].join("/")
        {
            shared += 1;
        }

        // Close everything below the shared prefix, deepest first.
        while stack.len() > shared + root_offset {
            let f = stack.pop().expect("len checked above");
            close_frame(volume_id, f, &mut stack, sink, &mut emitted)?;
        }
        // Open frames for the components this row newly enters.
        for i in shared..dir_depth {
            if stack.is_empty() {
                stack.push(Frame::new(String::new(), false)); // implicit volume root
            }
            stack.push(Frame::new(
                parts[..=i].join("/"),
                is_archive && i + 1 == dir_depth,
            ));
        }

        if !is_archive {
            let name = parts[parts.len() - 1];
            let top = top_frame(&mut stack);
            top.lines
                .push(format!("{name}\u{0}f\u{0}{}", row.content_hash));
            top.file_count += 1;
            top.total_bytes += row.size_bytes;
        }
    }

    while let Some(f) = stack.pop() {
        close_frame(volume_id, f, &mut stack, sink, &mut emitted)?;
    }
    Ok(emitted)
}

/// The frame files are added to. The volume root is implicit and created on demand, so a file at
/// the top level has somewhere to go.
fn top_frame(stack: &mut Vec<Frame>) -> &mut Frame {
    if stack.is_empty() {
        stack.push(Frame::new(String::new(), false));
    }
    stack.last_mut().expect("just ensured non-empty")
}

/// Hash a finished directory, emit it, and fold it into its parent.
fn close_frame<S: DirSink>(
    volume_id: &str,
    f: Frame,
    stack: &mut [Frame],
    sink: &mut S,
    emitted: &mut usize,
) -> anyhow::Result<()> {
    if f.file_count == 0 {
        return Ok(()); // empty tree: they would all hash alike and mean nothing
    }
    // Children are sorted here rather than kept sorted, so the hash does not depend on the order
    // rows arrived in -- byte order over full paths is not the same as byte order over base names.
    let mut lines = f.lines;
    lines.sort();
    let dir_hash = blake3::hash(lines.join("\n").as_bytes())
        .to_hex()
        .to_string();

    let archive_root = stack
        .iter()
        .rev()
        .find(|p| p.is_archive_root)
        .map(|p| p.path.clone());

    if let Some(parent) = stack.last_mut() {
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        parent.lines.push(format!("{name}\u{0}d\u{0}{dir_hash}"));
        parent.file_count += f.file_count;
        parent.total_bytes += f.total_bytes;
    }

    sink.emit(DirNode {
        volume_id: volume_id.to_string(),
        path: f.path,
        dir_hash,
        file_count: f.file_count,
        total_bytes: f.total_bytes,
        archive_root,
    })?;
    *emitted += 1;
    Ok(())
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

    /// Run both implementations over the same rows and require identical output.
    ///
    /// This is the real safety net for the streaming rewrite: the hash definition must not move, or
    /// every published figure and every stored dir_hash silently changes meaning.
    fn assert_same(rows: Vec<TreeInput>) {
        let mut expected = build_dir_hashes("v", rows.clone());
        let mut sorted = rows;
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let mut actual: Vec<DirNode> = Vec::new();
        stream_dir_hashes("v", sorted.into_iter().map(Ok), &mut actual).unwrap();
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        actual.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            expected.len(),
            actual.len(),
            "different node counts
expected: {:?}
actual:   {:?}",
            expected.iter().map(|n| &n.path).collect::<Vec<_>>(),
            actual.iter().map(|n| &n.path).collect::<Vec<_>>()
        );
        for (e, a) in expected.iter().zip(actual.iter()) {
            assert_eq!(e, a, "node mismatch at {:?}", e.path);
        }
    }

    #[test]
    fn streaming_matches_the_in_memory_build_for_a_plain_tree() {
        assert_same(vec![
            f("v", "top/one.txt", "H1", 10),
            f("v", "top/sub/two.txt", "H2", 20),
            f("v", "top/sub/deep/three.txt", "H3", 30),
            f("v", "other/x.txt", "H4", 40),
            f("v", "root.txt", "H5", 50),
        ]);
    }

    #[test]
    fn streaming_matches_for_identical_trees_and_odd_names() {
        // Includes names that stress byte ordering around '/': '.' (0x2E) sorts below it and '0'
        // (0x30) above, so these interleave with directory contents in a way a naive fold breaks on.
        assert_same(vec![
            f("v", "Photos2019/a.jpg", "H1", 10),
            f("v", "Photos2019/b.jpg", "H2", 20),
            f("v", "Photos 2019 copy/a.jpg", "H1", 10),
            f("v", "Photos 2019 copy/b.jpg", "H2", 20),
            f("v", "a.txt", "H6", 1),
            f("v", "a/inner.txt", "H7", 2),
            f("v", "a0.txt", "H8", 3),
        ]);
    }

    #[test]
    fn streaming_matches_when_archives_are_descended() {
        assert_same(vec![
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
            // Deeply nested entries, the shape that broke the first implementation.
            TreeInput {
                path: "z/deep.zip".into(),
                content_hash: "ZIP_C".into(),
                size_bytes: 5,
                is_archive_root: true,
            },
            f("v", "z/deep.zip/Project/.git/config", "H9", 7),
            f("v", "z/deep.zip/Project/src/main.rs", "HA", 8),
            // An archive with no entries stays an ordinary file.
            TreeInput {
                path: "d/report.docx".into(),
                content_hash: "DOCX".into(),
                size_bytes: 5,
                is_archive_root: true,
            },
        ]);
    }

    #[test]
    fn unordered_input_is_refused_rather_than_hashed_wrong() {
        // The fold would close a directory early and hash a fragment of it, producing duplicate
        // groups that look plausible and are wrong. That must be an error, not a silent result.
        let rows = vec![
            f("v", "b/second.txt", "H2", 1),
            f("v", "a/first.txt", "H1", 1),
        ];
        let mut out: Vec<DirNode> = Vec::new();
        let err = stream_dir_hashes("v", rows.into_iter().map(Ok), &mut out).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ascending path order"), "got: {msg}");
        assert!(
            msg.contains("a/first.txt") && msg.contains("b/second.txt"),
            "the error must name both paths; got: {msg}"
        );
    }

    #[test]
    fn a_duplicate_path_is_refused_too() {
        // Equal paths mean the caller lost a uniqueness guarantee somewhere; folding them would
        // double-count the file into its directory.
        let rows = vec![f("v", "a/x.txt", "H1", 1), f("v", "a/x.txt", "H1", 1)];
        let mut out: Vec<DirNode> = Vec::new();
        assert!(stream_dir_hashes("v", rows.into_iter().map(Ok), &mut out).is_err());
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

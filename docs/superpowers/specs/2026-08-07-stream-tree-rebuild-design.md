# Streaming the directory-tree rebuild — design

**Status:** approved
**Date:** 2026-08-07
**Closes:** #50 (stream the directory-tree rebuild; ~9.9 GB projected at 20 TB)
**Epic:** #27 (make duplicate review usable at millions-of-files scale)

## Why

`Catalog::rebuild_directory_trees` materialises every active row for a volume, builds the whole tree
in maps, and returns every node as a `Vec`. Measured peak working set on the live catalogue copy
(808,588 rows): **160 MB**, or roughly **198 bytes per row**.

The write-path spec puts the 20 TB corpus at *"on the order of 50 million"* files:

| files | projected peak |
| --- | --- |
| 20 M | ~4.0 GB |
| 40 M | ~7.9 GB |
| 50 M | **~9.9 GB** |

This runs **automatically after every completed scan**. So the identical-tree feature, which exists
precisely to make a 20 TB corpus reviewable, currently cannot run on one. That is the same
"built for a sandbox, run against 20 TB" failure as the rest of epic #27.

## The idea

Rows come back ordered by path. A directory can be finalised the moment the walk leaves it, so only
the **current root-to-leaf spine** ever needs to be resident — a function of tree depth and
directory width, not of corpus size. Nodes are written to the database as they are finalised rather
than accumulated and returned.

## The two assumptions, verified against real data before designing

Both were checked over all 808,588 active rows of the live catalogue, not reasoned about.

**1. A directory's descendants are contiguous under byte ordering.** Every string with prefix `a/`
lies in `["a/", "a0")`, and nothing else does — `/` is `0x2F` and `0` is `0x30`. **0 contiguity
failures** across every directory in the catalogue.

**2. An archive's own row immediately precedes its entries.** An entry's path is
`relative_path + "/" + container_chain`, and the archive's own row is `relative_path`. A prefix
sorts before any extension of it, so `backup.zip` lands directly before `backup.zip/...`; and
`backup.zip/x` < `backup.zipX` because `/` < any ordinary name character. **770 archives checked, 0
adjacency failures.**

Assumption 2 is what removes the pre-pass. The current code scans every row once to discover which
paths have children, purely to know whether a loose row is an archive. With ordering, **one row of
lookahead** answers the same question.

## Architecture

```rust
pub trait DirSink {
    fn emit(&mut self, node: DirNode) -> anyhow::Result<()>;
}

/// Fold path-ordered rows into directory hashes, emitting each node as it is finalised.
/// `rows` MUST be ordered by `path` under SQLite's BINARY collation.
pub fn stream_dir_hashes<I, S>(volume_id: &str, rows: I, sink: &mut S) -> anyhow::Result<usize>
where I: Iterator<Item = anyhow::Result<TreeInput>>, S: DirSink;
```

A stack of open directories, one frame per path component:

```rust
struct Frame {
    path: String,
    lines: Vec<String>,   // "name\0kind\0hash" for each child, in insertion order
    file_count: i64,
    total_bytes: i64,
    is_archive_root: bool,
}
```

For each row, in order:

1. **Close** every frame that is not a prefix of this row's parent. Closing hashes the frame and
   emits it, then folds its `(name, "d", hash)` line, count and bytes into its parent.
2. **Open** frames for any new path components.
3. **Add** the file as a `(name, "f", content_hash)` line on the top frame — unless the lookahead
   says the next row starts with `this path + "/"`, in which case this row is an **archive root**:
   its own hash is discarded and the frame opened for it is marked `is_archive_root`.

At the end, close the remaining spine.

`archive_root` for an emitted node is the innermost frame on the stack marked `is_archive_root` —
already to hand, no scan of a separate set.

### Ordering must be enforced, not assumed

The fold is silently wrong on unordered input: it would close a directory early and hash a fragment
of it. Since the correctness of every hash depends on it, `stream_dir_hashes` **asserts** each path
is `>` the previous one and returns an error naming both paths otherwise. Cheap (one comparison per
row) against a defect that would otherwise surface as wrong duplicate groups.

`ORDER BY` uses the default BINARY collation. It must never be given `COLLATE NOCASE`, which would
break assumption 1.

### Sorting cost

SQLite sorts the result set. That is disk-backed via a temp store, so it does not reintroduce the
memory problem, but it is real work on 50 M rows. The existing indexes do not cover the computed
path (it concatenates two columns), so this will be a sort, not an index walk. Accepted: a sort of
that size is minutes at worst against a five-day scan, and the alternative — an index on a computed
column — costs storage on every row for a once-per-scan query. If it proves slow it can be
revisited with a measurement.

### Line accumulation is bounded by directory width, not corpus size

An open frame holds one line per child seen so far, so a single directory with a million files holds
a million lines. That is inherent — a directory's hash cannot be computed before all its children
are known — and it is bounded by the widest directory rather than by the whole corpus. The live
catalogue's widest directory should be recorded when this is measured.

## What this does not change

- The hash definition, so **every published figure must come out identical**: 80,202 nodes, 1,458
  maximal groups, 4,251 folders, 53.2 GB reclaimable, 1,911 in-archive.
- The `directory_trees` schema, the rebuild triggers, quarantine, or the review UI.
- `tree_duplicate_groups`, which loads every *twinned* node to compute maximal groups. That is a
  smaller problem — it grows with directory count rather than file count, and it runs only when the
  user opens the review page rather than automatically after every scan. **Measure it separately
  and file it if it warrants work; do not fix it here on a guess.**

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **The fold is silently wrong on unordered input** — wrong hashes, wrong duplicate groups, and the user acts on them | Order asserted per row, with an error naming both paths. Not a debug assertion |
| The rewrite changes behaviour subtly | Every existing `tree_hash` test must pass unmodified, and the aggregate figures on the live catalogue must be **identical**, not merely close |
| Memory improves less than hoped | Measured before and after with `examples/validate_trees.rs`, the same harness that produced the 194 → 160 MB figures. A null result is recorded, not hidden |
| A collation change elsewhere breaks the ordering | The query pins the default BINARY collation, and the order assertion catches it at runtime regardless |

## Success criteria

1. `rebuild_directory_trees` holds no per-file collection: peak memory is a function of tree shape,
   not row count.
2. Peak working set on the live catalogue copy is **measured** before and after and recorded in
   `docs/benchmarking-scans.md`, with the projection to 50 M files updated.
3. The aggregates are byte-identical: 80,202 / 1,458 / 4,251 / 53.2 GB / 1,911.
4. Unordered input produces an error naming both paths, not a wrong hash. Tested.
5. Every existing `tree_hash` and `tree_quarantine` test passes unmodified.

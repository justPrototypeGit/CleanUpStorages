# Identical-tree collapse — design

**Status:** approved
**Date:** 2026-08-06
**Closes:** #38 (collapse identical folder trees into a single review item)
**Epic:** #27 (make duplicate review usable at millions-of-files scale)

## Why

The live catalogue holds **125,977 duplicate groups** across 535,313 loose files. Reviewed one at a
time at ten seconds a decision that is roughly 350 hours. It is not a task a human finishes, so the
duplicates half of this project is currently unusable — which is the actual gap between where the
tool is and where it needs to be.

Most of those groups are not independent decisions. When a folder was copied — an old backup, a
duplicated course folder, a drive cloned onto another — every file inside becomes its own duplicate
group, though there is really **one** question: *is this whole copy redundant?*

This was measured against the live catalogue before designing, not assumed.

| | archives as leaves | **archives descended (chosen)** |
| --- | --- | --- |
| maximal identical-tree groups | 1,773 | **1,458** |
| duplicate groups explained by them | 109,023 (86.5%) | 84,449 (58.5%) |
| groups left needing per-file review | 16,954 | 59,924 |
| reclaimable by collapsing | 29.7 GB | **53.2 GB** |
| collapsed folders sitting inside an archive | 0 | 1,966 |

Total reclaimable across all duplicates is 72.1 GB, so collapsing is a **decision-count** win first
and a space win second. The decision to descend into archives was taken deliberately: it nearly
doubles the space collapsing recovers, at the cost of complications this design has to absorb rather
than hide.

Caveats on those numbers, stated so nobody over-reads them: this is the partial catalogue (3 volumes,
808,588 rows), not the full 20 TB, so the ratio is directional and not a promise. The "maximal" rule
used to measure drops a folder whenever its parent is duplicated, which can miss a folder paired with
a different partner than its parent's — rare, and it makes 1,458/1,773 a slight **under**count.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Identity | **Merkle `dir_hash` built from stored content hashes** | Needs no extra I/O — every BLAKE3 hash is already in the catalogue. Composes: a folder whose children all match is automatically a match |
| Folder names | **Not part of the hash** | A copied folder is usually renamed (`backupApple/…` in the live data). Requiring the name to match would miss the main case |
| Skipped files | **Hash only what was catalogued** | The hash then describes exactly what the UI can show. Hashing invisible files would describe something the user cannot check |
| Archives | **Descended: an archive is a directory of its entries** | Nearly doubles reclaimable space (29.7 → 53.2 GB) |
| Confirm action | **Follows what is physically safe, per case** | A directory rename cannot apply inside a zip. See below |
| Empty folders | **Excluded** | They all hash alike and would report thousands of meaningless matches |
| Reporting | **Maximal subtree only** | Reporting every identical subfolder would restore the problem this solves |

## Architecture

### The directory hash

Bottom-up, the way git builds tree objects:

```
dir_hash(D) = BLAKE3( sorted list of (child_name, kind, child_hash) )
                where child_hash = content_hash  for files
                                   dir_hash      for subdirectories
```

Two directories are the same tree iff their `dir_hash` matches. The folder's **own** name is not an
input — only its children's names are. `Photos2019/` and `Photos 2019 copy/` therefore match when
their contents match, which is the case that matters, and the review UI compensates by always showing
both full paths.

Only rows with `status='active'` participate. A folder whose twin has been quarantined is no longer a
duplicate, and must stop being reported as one.

### Archives are directories

An archive row is **replaced** by the tree of its entries, not listed alongside it. This is the one
subtlety that is easy to get wrong: an archive is both a file row (`relative_path`, with its own
`content_hash`) and a set of entry rows (`container_chain` holding the path inside it). If both were
fed to the hash, an archive would appear as its own sibling and every containing folder's hash would
be wrong.

So when computing `dir_hash`, a path that has entry rows contributes `dir_hash(entries)` and its
`content_hash` is ignored. A useful consequence: two archives holding identical content but compressed
differently have different `content_hash` yet the same `dir_hash`, and are correctly matched.

`container_chain` is a `/`-separated path within the archive, so the entry tree is built exactly like
the loose tree — the two are the same code path over `relative_path + '/' + container_chain`.

### Reporting only maximal subtrees

A duplicated tree would otherwise report every subfolder inside it. Only the **highest** node whose
`dir_hash` has a twin is reported; nothing is said about its descendants.

A folder is maximal when its parent is not itself part of a duplicate group. Because `dir_hash(parent)`
includes every child hash, a duplicated parent guarantees the child's twin sits inside the parent's
twin — so suppressing the child loses nothing. The exception noted above (a folder paired with a
different partner than its parent's) is accepted: it under-reports rather than over-reports, which is
the right direction for a destructive-action UI.

### What confirming does — per case, because one action cannot cover all three

This is where the decision to descend into archives has to be paid for honestly.

| the redundant side is… | action | why |
| --- | --- | --- |
| a **loose directory** | **one rename** into `_ToDelete`, preserving internal structure | Cheapest and most reversible; the whole tree can be put back by hand. Every file inside still gets its catalogue row updated, or catalogue and disk disagree |
| a **whole archive** whose entry tree is redundant | quarantine the `.zip` **as a single file** | Still a rename. The archive is one object on disk |
| a folder **inside** an archive | **reported, labelled "needs repack"**, never offered as a delete | A file inside a zip cannot be moved. The only correct remedy is the existing verified repack path (Case 4 of the main design spec), which builds a temp copy and re-hashes every retained entry before swapping |

1,966 of the collapsed folders fall in the third row. Presenting them as ordinary deletable items
would be offering an action the tool cannot safely perform, so they are visibly a different kind of
item.

The tree quarantine is **not** a bypass of the per-file bookkeeping: it performs one directory rename
and then updates the catalogue row of every file beneath it, exactly as N individual quarantines
would have. The rename is the optimisation; the bookkeeping is unchanged. If the rename succeeds and
the bookkeeping then fails, the catalogue and disk disagree — so the rename is recorded first, and a
failed update leaves a scan error rather than a silent mismatch.

### Archive-internal duplicates stay out of the per-file queue

Descending adds 18,396 duplicate groups that exist only *inside* archives. Those are not independently
actionable — deleting one entry requires repacking its container. Adding 43,000 such rows to the
review queue would be handing the user decisions with no safe action attached, which is the opposite
of this issue's purpose.

They are therefore excluded from the per-file duplicate queue by default, and reachable only through
the archive that contains them. They still participate fully in tree matching, which is where their
value is.

### Storage

```sql
CREATE TABLE directory_trees (
    volume_id   TEXT NOT NULL REFERENCES volumes(volume_id),
    path        TEXT NOT NULL,         -- '' is the volume root
    dir_hash    TEXT NOT NULL,
    file_count  INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL,
    computed_at INTEGER NOT NULL,
    PRIMARY KEY (volume_id, path)
);
CREATE INDEX idx_directory_trees_hash ON directory_trees(dir_hash);
```

Derived data, rebuilt per volume from the catalogue — never authoritative, and safe to drop and
recompute. 48,541 rows for the current catalogue (80,202 with archives descended), so the table is
small next to `files`.

Computed **after** a scan completes for that volume, and after quarantine or purge changes what is
active. It is a read over one volume's rows plus a sort, so it costs seconds, not minutes — and it is
skipped for a stopped scan, whose picture of the volume is by definition incomplete.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| **One wrong click discards a whole tree instead of one file** — the blast radius grows | Quarantine is still only a rename, `purge` is still the only real delete, and the user still empties it manually. The confirm UI must show file count, total bytes, and the **full path of both sides** before acting |
| The user picks the wrong survivor because names differ | Both full paths are always shown; neither side is labelled "the copy" by position. The existing per-file survivor heuristic suggests, never decides |
| A tree is reported as identical when it is not | The hash is derived from BLAKE3 content hashes already verified by the scanner. A match is exact by construction — this design adds no new comparison logic |
| `dir_hash` goes stale after quarantine and offers a tree that is already gone | Rebuilt whenever what is active changes; the confirm path re-checks that every file is still `active` before renaming, and refuses otherwise |
| Empty or near-empty folders flood the report | Empty trees excluded entirely |
| Descending archives inflates the per-file queue with unactionable rows | Archive-internal duplicates excluded from that queue by default |
| The rename succeeds but the catalogue update fails | The rename is recorded first; a failed update logs a scan error, so the disagreement is visible rather than silent |

## Non-goals

- **Near-miss folders** (99% identical, one extra file). The more interesting case, and deliberately
  a follow-up — a pure hash match cannot surface it, and the design does not preclude adding it.
- No change to hashing, to the scanner's write path, or to what gets catalogued.
- No change to `purge`, or to quarantine being a rename the user empties manually.
- Not a replacement for the per-file duplicate view: 16,954 loose groups still need per-file review,
  and they hold most of the remaining reclaimable bytes.

## Success criteria

1. Two folders with identical contents and different names are reported as one item.
2. Only the **maximal** matching folder is reported; its subfolders are not.
3. Empty folders are never reported.
4. Two archives with identical contents but different compression are matched.
5. A folder inside an archive is reported as "needs repack" and offers no delete action.
6. Confirming a loose tree performs **one** directory rename and updates every catalogue row beneath
   it; a subsequent scan sees the files as quarantined, not missing.
7. The confirm UI shows file count, total bytes, and both full paths before acting.
8. A tree whose files are no longer all `active` is refused rather than renamed.
9. Rebuilding `directory_trees` for the live catalogue completes in seconds and yields the measured
   counts (≈1,458 maximal groups with archives descended).

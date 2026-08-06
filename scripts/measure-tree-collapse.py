"""#38 premise, measured two ways: archives as leaves, and archives descended as directories.

The subtlety the first draft got wrong: an archive is BOTH a file row (its own content_hash) and a
directory of entry rows. When descending, it must become a directory -- its file identity is
replaced by the hash of its entry tree, not kept alongside it.
"""
import sqlite3, hashlib, collections, sys

DESCEND_ARCHIVES = sys.argv[1] == 'descend'
c = sqlite3.connect('file:cat-copy.db?mode=ro', uri=True)

loose = c.execute("""SELECT volume_id, relative_path, content_hash, size_bytes
                       FROM files WHERE status='active' AND container_chain IS NULL""").fetchall()
entries = c.execute("""SELECT volume_id, relative_path, container_chain, content_hash, size_bytes
                         FROM files WHERE status='active' AND container_chain IS NOT NULL""").fetchall()

# every path that is an archive (has entries) -- these become directories when descending
archives = {(v, r) for v, r, _, _, _ in entries}

files = []          # (vol, path, hash, size) -- leaf files only
for v, r, h, s in loose:
    if DESCEND_ARCHIVES and (v, r) in archives:
        continue     # replaced by its entry tree below
    files.append((v, r, h, s))
if DESCEND_ARCHIVES:
    for v, r, cc, h, s in entries:
        files.append((v, r + '/' + cc, h, s))

print(f"leaf files: {len(files):,}  (archives {'descended' if DESCEND_ARCHIVES else 'as leaves'})",
      flush=True)

children = collections.defaultdict(dict)   # dir -> {name: ('f',hash,size) or ('d',None,None)}
all_dirs = {(v, '') for v, _, _, _ in files}
for vol, rel, h, size in files:
    parts = rel.split('/')
    children[(vol, '/'.join(parts[:-1]))][parts[-1]] = ('f', h, size)
    for i in range(len(parts) - 1, 0, -1):
        me = (vol, '/'.join(parts[:i]))
        parent = (vol, '/'.join(parts[:i - 1]))
        all_dirs.add(me)
        if parts[i - 1] not in children[parent] or children[parent][parts[i - 1]][0] == 'd':
            children[parent][parts[i - 1]] = ('d', None, None)

depth = lambda d: 0 if d[1] == '' else d[1].count('/') + 1
dir_hash, dir_files, dir_bytes = {}, {}, {}
for d in sorted(all_dirs, key=depth, reverse=True):
    ents, nf, nb = [], 0, 0
    for name, (kind, h, size) in children[d].items():
        if kind == 'f':
            ents.append((name, 'f', h)); nf += 1; nb += size
        else:
            sub = (d[0], (d[1] + '/' + name) if d[1] else name)
            if sub in dir_hash:
                ents.append((name, 'd', dir_hash[sub])); nf += dir_files[sub]; nb += dir_bytes[sub]
    if nf == 0:
        continue
    blob = '\n'.join(f"{k}\x00{t}\x00{h}" for k, t, h in sorted(ents))
    dir_hash[d] = hashlib.blake2b(blob.encode('utf-8', 'surrogatepass'), digest_size=32).hexdigest()
    dir_files[d], dir_bytes[d] = nf, nb

by_hash = collections.defaultdict(list)
for d, h in dir_hash.items():
    by_hash[h].append(d)
dupe = {h: ds for h, ds in by_hash.items() if len(ds) > 1}
dup_set = {d for ds in dupe.values() for d in ds}
parent = lambda d: None if d[1] == '' else (d[0], d[1].rsplit('/', 1)[0] if '/' in d[1] else '')

maximal = collections.defaultdict(list)
for h, ds in dupe.items():
    for d in ds:
        p = parent(d)
        if p is None or p not in dup_set:
            maximal[h].append(d)
maximal = {h: ds for h, ds in maximal.items() if len(ds) > 1}

reclaim = sum(dir_bytes[ds[0]] * (len(ds) - 1) for ds in maximal.values())
print(f"non-empty dirs: {len(dir_hash):,}   twinned: {len(dup_set):,}")
print(f"MAXIMAL identical-tree groups: {len(maximal):,}  "
      f"folders {sum(len(v) for v in maximal.values()):,}  reclaimable {reclaim/1e9:.1f} GB")

hash_of = collections.defaultdict(set)
for v, r, h, s in files:
    hash_of[h].add((v, r))
groups = {h: l for h, l in hash_of.items() if len(l) > 1}
prefixes = collections.defaultdict(list)
for h, ds in maximal.items():
    for v, p in ds:
        prefixes[v].append(p + '/' if p else '')
inside = lambda v, r: any(r.startswith(p) for p in prefixes.get(v, ()))
full = sum(1 for h, locs in groups.items() if all(inside(v, r) for v, r in locs))
total_recl = sum(max(s for _, _, hh, s in files if hh == h) * (len(l) - 1)
                 for h, l in list(groups.items())[:0]) or None
print(f"file-level dup groups: {len(groups):,}   explained: {full:,} ({100*full/len(groups):.1f}%)"
      f"   left per-file: {len(groups)-full:,}")

# how many collapsed trees live (partly) inside an archive? those cannot be a directory rename
in_arch = sum(1 for ds in maximal.values() for d in ds
              if any(d[1] == r or d[1].startswith(r + '/') for (v, r) in archives if v == d[0]))
print(f"collapsed folders that sit inside an archive: {in_arch:,}")

# Benchmarking scans

Scan timings are easy to measure and easy to measure *wrongly*. Every number below should state
which of the three conditions it was taken under, or it cannot be compared to anything.

## Reading the breakdown

`cleanupstorages scan <path>` prints a phase split after the summary, and the Scan page keeps the
last runs. The two numbers that decide epic #21's ordering:

- **`hash` vs `walk` + `skip_check`.** If hashing is a small slice and the walk dominates, the scan
  is seek-bound: faster hashing (#24) will buy almost nothing, and concurrency (#23) must be tuned
  carefully because more parallel readers on a spinning disk means more seeking, not less.
- **MB/s while hashing vs MB/s overall.** A large gap means time is going somewhere other than
  reading bytes.

`accounted` is the sum of the phases. While the pipeline is sequential it should be close to wall
clock; untimed glue (loop overhead, path and category string work) makes up the rest. After #23
parallelises the pipeline, `accounted` will *exceed* wall clock, and the ratio is the overlap
achieved — that is the point of the number.

## The three traps

### 1. Windows Defender

Defender scans every file we open. On a corpus that is 88.3% files under 64 KB, that per-open tax
can rival seek time — and from inside our process it is indistinguishable from slow I/O.

Run the A/B once:

1. Scan a representative subtree, note files/s and MB/s.
2. Add that subtree to Defender's exclusions
   (Windows Security → Virus & threat protection → Manage settings → Exclusions).
3. Scan the same subtree again the same way, and compare.
4. **Remove the exclusion afterwards** if it is not somewhere you want permanently excluded.

If this alone moves throughput materially, the fix is a documentation note, not code.

### 2. Cold vs warm OS file cache

The second scan of the same subtree reads from the OS cache and will look faster for reasons that
have nothing to do with our code. Either reboot between runs, use a subtree far larger than RAM, or
label the number "warm" and only compare it to other warm numbers.

### 3. First pass vs rescan

The incremental skip means a second scan of already-catalogued files exercises `skip_check`, not
`hash`. These measure different code paths and must never be compared to each other. Use
`--force` to make a rescan take the hashing path, or compare first-pass to first-pass.

## Recording a result

Runs are persisted in the `scan_runs` table and survive restarts, so a multi-day scan's numbers are
not lost. Note in the issue which condition each figure was taken under.

# Web Scanning — Design Spec

**Date:** 2026-07-07
**Status:** Approved design (pre-implementation)

## 1. Problem & goal

Scanning a drive into the catalog is currently **CLI-only** (`cleanupstorages scan <path>`). The web UI
(`browse`) can search, browse, review duplicates, quarantine, and repack — but there is no way to *add* a drive
to the catalog from the browser. This forces the user back to the terminal for the very first step of every
session.

**Goal:** add a "Scan a drive" experience to the local web UI: pick or type a folder (with a native folder
picker and a list of currently-connected drives), start the scan in the background, and watch live progress —
without leaving the browser or blocking on a long-running scan.

## 2. Overriding constraints (unchanged from the project)

- **Reliability: nothing lost or corrupted.** Scans go through the same `scan_volume` + snapshot path the CLI
  uses; the only scanner change is a purely-additive, opt-in progress hook that is a no-op for existing callers.
- **Localhost only.** The server still binds `127.0.0.1`. The scan endpoint (which mutates the catalog and writes
  a drive's identity marker) and the folder-picker endpoint are **CSRF-guarded** with the existing per-run token.
- **No terminal prompts.** The web UI cannot ask interactive questions mid-scan, so a read-only drive always
  falls back to fingerprint identification (the CLI's own recommended default).

## 3. Architecture

A new in-memory, single-worker **scan queue** lives in `AppState` alongside the mount resolver and CSRF token.
A dedicated background task (spawned at server start) runs queued scans one at a time; a set of atomic counters
exposes live progress; finished results move into a small capped history.

New web surface:
- Page `GET /scan` — the scan form + live status area.
- `GET /api/detected-drives` — connected drives, cross-referenced against the catalog (new vs. already known).
- `POST /api/scan` — enqueue a scan (CSRF-guarded). Body `{ path, force }`.
- `GET /api/scan/status` — poll: currently-running job with live counts, queued paths, recent results.
- `POST /api/pick-folder` — open the native OS folder dialog (CSRF-guarded), return the chosen path or none.

New dependency: `rfd` (native file dialogs), used only by `/api/pick-folder`, run via `spawn_blocking`.

## 4. Scanner progress hook (the only change to the scanning core)

Add an optional progress sink to `scan_volume`:

```rust
pub trait Progress: Send + Sync {
    fn on_hashed(&self);
    fn on_skipped(&self);
    fn on_error(&self);
    fn on_archive_entry(&self);
}

pub fn scan_volume(cat, root, identity, force, now, progress: Option<&dyn Progress>) -> Result<ScanSummary>
```

Each method is called at the existing count site (where `summary.hashed += 1`, `summary.skipped += 1`,
`summary.errors += 1`, `summary.archive_entries += 1` already happen, including inside `descend_archive`). The
CLI passes `None` — behavior and all existing tests are byte-for-byte unchanged. The web worker passes a struct
backed by `AtomicUsize` counters the status endpoint reads. This is purely additive (a no-op when `None`); it is
the only honest way to surface real per-file counts.

## 5. The scan queue, worker, and status

**`ScanQueue`** (in `src/scan_queue.rs`), held as `Arc<ScanQueue>` in `AppState`, internally a `Mutex` over:
- `pending: VecDeque<ScanRequest>`
- `running: Option<RunningScan>` — the current job's path + live `Arc` of atomic counters
- `recent: VecDeque<ScanResult>` — bounded to the last 20 finished results

A `ScanRequest` is `{ path: PathBuf, force: bool }` (read-only fallback is always fingerprint; stored implicitly).

**Worker:** one background task spawned at server start loops forever: take the next `pending` request (waiting
on a notify/channel when idle) → move it to `running` with fresh counters → run the scan on `spawn_blocking` →
record a `ScanResult` into `recent`, clear `running` → repeat. **One scan at a time; extras wait in the queue.**
Enqueue returns the queue position so the UI can show "queued (N ahead)".

**Running a scan** mirrors `cmd_scan` exactly, driven by the queue: resolve the volume identity
(`volume::resolve` with the fingerprint fallback), `upsert_volume`, `scan_volume(..., Some(&counters))`, then
`backup::snapshot`. Reusing that logic keeps one definition of "how a scan works." (`cmd_scan` and the worker
should share a small helper so they can't drift.)

**`GET /api/scan/status`** returns:
```json
{
  "running": { "path": "...", "hashed": 1204, "skipped": 380, "errors": 3, "archive_entries": 2 } | null,
  "queued": ["D:\\", "E:\\photos"],
  "recent": [
    { "path": "...", "hashed": 1204, "skipped": 380, "errors": 3, "archive_entries": 2,
      "marked_missing": 0, "error_message": null }
  ]
}
```
The page polls this every ~1.5s while anything is running or queued, and stops when idle.

**Failure handling.** A failed scan (bad path, drive vanished, marker unwritable and un-fingerprintable) is
caught by the worker, recorded as a `recent` entry with `error_message`, and the worker moves to the next job —
one bad scan never wedges the queue or crashes the server. The catalog is only touched through the safe
`scan_volume`/snapshot path, so a failed scan cannot corrupt it.

## 6. Detected drives

`GET /api/detected-drives` lists currently-mounted drives (via the same `sysinfo`/`mounts` logic used elsewhere)
and, for each, whether it already carries our marker / is already catalogued (join against `volumes` by the
marker's `volume_id` when present). Each entry: `{ mount_path, label, catalogued: bool, volume_label? }`. The
page renders them as clickable rows that fill the path field; already-catalogued drives are labelled so the user
knows a scan will be incremental.

## 7. The `/scan` page

Self-contained HTML/CSS/JS (no external requests), XSS-safe (all dynamic strings via `esc()`/`textContent`),
CSRF token in a `<meta>` tag. Elements:
- **Detected drives** list (fetched on load) — click to fill the path field.
- **Path field** — type/paste an absolute path.
- **"Browse…"** button — POSTs `/api/pick-folder`; the native OS dialog opens on the user's desktop; the chosen
  path fills the field; cancel does nothing.
- **"Force full rescan"** checkbox — unchecked by default; when checked, POSTs `force: true`.
- **Scan** button — POSTs `{ path, force }` to `/api/scan` with the token.
- **Live status area** — polls `/api/scan/status`; shows the running job's ticking counts, queued paths, and a
  short list of recent finished scans (including any `error_message`).
- A **link to `/scan`** is added to the browse and review page headers.

## 8. The folder picker

`rfd::AsyncFileDialog`/`FileDialog::pick_folder` run via `spawn_blocking` so it never blocks the async runtime.
The server is localhost-only and the dialog opens on the same machine as the browser, so a native desktop dialog
is coherent. `/api/pick-folder` is CSRF-guarded so no other site can pop a dialog on the user's desktop. Returns
`{ path: "..." }` or `{ path: null }` on cancel.

## 9. Testing

- **Scanner hook:** a test asserting the progress callback counts equal the returned `ScanSummary` fields.
- **Queue/worker:** unit tests — enqueue → worker runs → counters tick → result lands in `recent`; a failing job
  records an `error_message` and the worker continues to the next; queue ordering/position.
- **Endpoints:** `oneshot` tests — `/api/detected-drives` shape; `/api/scan` requires the CSRF token (403
  without); `/api/scan/status` JSON shape.
- **Folder dialog:** the native dialog itself is not unit-tested (needs a desktop); the handler around it is.
- **E2e (real TCP):** enqueue a scan of a temp folder, poll `/api/scan/status` until it reports the scan in
  `recent` with the right counts.

## 10. Reliability / safety summary

- Only the additive `Option<&dyn Progress>` touches the scanning core; `None` = today's behavior exactly.
- Catalog mutations go only through `scan_volume` + `backup::snapshot` (same as the CLI).
- `/api/scan` and `/api/pick-folder` are CSRF-guarded; server stays `127.0.0.1`.
- One scan at a time; failures are isolated to their `recent` entry; the worker/server never crash on a bad scan.
- Read-only drives → fingerprint (no impossible mid-scan prompt).

## 11. Out of scope (deferred)

- Concurrent/parallel scans (kept to one-at-a-time + queue).
- Cancelling a running scan mid-flight (a queued-but-not-started job could be cancellable later; not now).
- Per-file live path display (only aggregate counts) and ETA estimation.
- Scheduling/auto-rescan.

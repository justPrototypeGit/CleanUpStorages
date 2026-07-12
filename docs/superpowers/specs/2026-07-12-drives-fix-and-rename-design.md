# Drives: fix "offline" detection + rename/description — design

Sub-project 1 of the UI-improvement roadmap (2 = design-system/Stitch overhaul, 3 = Windows
packaging). This one is backend + a light Drives-page touch; it corrects the data the visual pass
will later display.

## Goal

1. **Fix the bug where folder-based "drives" always show as offline** (and consequently can't be
   quarantined/purged/rescanned from the web).
2. **Let the user give each drive a custom name and a short description**, shown everywhere a drive
   name appears, so drives are easy to recognize.

## Problem 1 — folder-drives read as offline

`MountResolver::Live` detects a connected volume by scanning **real OS disk mount points** (C:\, D:\,
…) for the `.cleanupstorages_id` marker (`mounts::live_mounts` → `sysinfo::Disks`). But when the user
scans a **folder** (e.g. `…\cleanup-sandbox\DriveA`), the marker is written *inside that folder* — a
subpath of C:\, never itself a disk root. So the folder-volume's marker is never found, the volume is
never in the mount map, and `/api/drives` reports it `connected:false`. This also breaks every
web action that resolves a mount (quarantine/purge/repack/rescan) for folder-drives.

### Fix — remember each volume's last-scanned path and check the marker there

- **Schema (additive, via `ensure_column`):** `volumes.last_scanned_path TEXT`.
- **Record it on scan:** `run_scan` calls a new `Catalog::set_volume_path(volume_id, path, now)` with
  the absolute mount root it just scanned (loose files' root), right after `upsert_volume`.
- **Read it back:** `Catalog::volume_paths() -> Vec<(String, String)>` — `(volume_id,
  last_scanned_path)` for volumes that have one.
- **Merge into mount resolution.** `MountResolver::Live` gains the catalog path:
  `Live { catalog_path: PathBuf }`. Its `snapshot()` becomes the union of:
  - the existing disk-root marker scan (`live_mounts`) — handles real removable drives whose letter
    changed, and
  - each remembered `(volume_id, path)` whose marker **still matches** (`volume::read_volume_id(path)
    == Some(volume_id)`) — handles folder-drives and any drive still at its last path.

  The pure merge is factored into a testable free function
  `mounts::resolve_live(disk_roots: HashMap<String,PathBuf>, remembered: &[(String, PathBuf)]) ->
  HashMap<String,PathBuf>` so it can be unit-tested without real disks. `resolve()` stays
  `snapshot().get(id).cloned()`. The `Fixed` variant (tests) is unchanged.
- `AppState::new_live` passes `catalog_path` into `Live { .. }`; the `app_state_new_live` test's
  `matches!(…, MountResolver::Live { .. })` is updated. CLI commands don't use `MountResolver` (they
  take an explicit path + `read_volume_id`), so they're unaffected.

**Safety:** the marker must equal the volume_id, so a folder that was moved/renamed away (marker gone
or different) correctly reads offline, and a different volume at a remembered path is never
mis-attributed. This changes only *connectivity detection*, never a destructive path — the existing
per-action marker re-checks in quarantine/purge/repack stay as they are.

## Problem 2 — custom drive name + description

- **Schema (additive, via `ensure_column`):** `volumes.display_name TEXT`, `volumes.description TEXT`
  (both nullable; absent = use the detected `label`).
- **Store:**
  - `Catalog::set_volume_meta(volume_id, display_name: Option<&str>, description: Option<&str>, now)`
    — sets/clears both (empty string → `NULL` → falls back to label). Logs a `rename` audit action.
  - `Catalog::volume_meta(volume_id) -> (Option<String>, Option<String>)` (display_name, description).
  - "Effective name" = `display_name` (non-empty) else `label`, computed at each display site.
- **Endpoint:** `POST /api/rename-drive {volume_id, name, description}` (CSRF-first, `Catalog::open`).
  A blank `name` clears the custom name. Returns the saved effective name.
- **CLI parity:** `cus rename <mount> [--name X] [--description Y]` (matches the every-action-has-a-CLI
  convention).
- **Shown everywhere a drive name appears:**
  - `/api/drives` (`DriveDto`) gains `display_name`, `description`; the Drives card shows the effective
    name + the description line, plus a small **Edit** control (pencil) that reveals inline
    name/description fields and POSTs `/api/rename-drive`.
  - `/api/volumes` (`VolumeDto`) and the search hit `volume_label` prefer the effective name, so the
    Browse dropdown, Browse tree drive nodes, and Overview all show the custom name (fallback label).

## Out of scope (this sub-project)
- The visual/Stitch restyle, theme toggle, dropdown theming, scan-status rebuild (sub-project 2).
- Windows DPI manifest / app icon (sub-project 3).
- Any change to scanning/dedup/quarantine/purge engines beyond recording the scanned path and reading
  the two new columns.

## Testing
- `mounts::resolve_live` pure-function unit tests: a remembered folder path whose marker matches is
  included; a remembered path whose marker is gone/mismatched is excluded; disk-root entries still
  win/merge.
- `Catalog::set_volume_path` / `volume_paths` and `set_volume_meta` / `volume_meta` store unit tests
  (round-trip; empty name clears to NULL → effective name falls back to label).
- `/api/drives` returns `display_name`/`description`; `/api/rename-drive` requires the token, persists,
  and the effective name then appears in `/api/volumes` and `/api/search` `volume_label`.
- `cus rename` integration test (name + description persist; visible in `status`/`/api/drives`).
- The offline-detection fix is validated end-to-end against the sandbox in the testing guide (a scanned
  folder-drive now shows **connected** on the Drives page).

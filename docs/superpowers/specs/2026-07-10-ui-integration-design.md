# CleanUpStorages UI integration — design

## Goal

Turn the Google Stitch mockups in `StitchExport/` into the app's real UI: six pages sharing one
macOS-glass design system, fully wired to the live API, self-contained (no CDN, no external
fonts/icons, no build step) per the existing web-security posture in `src/web.rs`. Three of the
six screens (Overview, Drives, Console) don't exist yet — the first two need small, targeted
backend additions; Console needs none (see §7).

The Stitch export is the **visual reference**, not usable code — it loads Tailwind from a CDN,
Google Fonts, and a Material Symbols icon font (all external network calls), and every number/row
in it is fake. This design hand-builds the same look from the tokens in
`StitchExport/stitch_cleanupstorages_ui/precision_file_architect/DESIGN.md`, wired to real data.

## Pages and routes

One shared shell (sidebar + toolbar) renders behind all six pages, with the active nav item
highlighted. Nav order: Overview, Browse, Duplicates, Drives, Scan, Console.

| Route | Page | Status |
|---|---|---|
| `/` | **Overview** — hero stat card, duplicates card, reclaimable-space-by-drive card, recent activity list | New |
| `/browse` | **Browse** — search/filter table (moves off `/`, where it lives today) | Restyle of existing `INDEX_HTML` |
| `/review` | **Duplicates** — Tinder-style duplicate review | Restyle of existing `REVIEW_HTML` |
| `/drives` | **Drives** — per-drive cards (capacity bar, last scan, actions), global purge-all | New |
| `/scan` | **Scan** — detected drives + folder picker + live progress | Restyle of existing `SCAN_HTML` |
| `/console` | **Console** — terminal-style panel for running the app's own commands | New |

Browse, Duplicates, and Scan keep their existing behavior exactly (search/filter, quarantine,
repack, live scan progress via `scan_queue`) — this is a re-skin of those three, not a rewrite of
their logic.

## New backend capabilities

### 1. Recent activity feed
`actions_log` is already written to (`quarantine`, `quarantine_skip`, `quarantine_error`, `purge`,
`repack`) but nothing reads it back. Add:
- `Catalog::recent_actions(limit) -> Vec<(action, details_json, occurred_at)>` — a plain read of
  the existing table, newest first.
- One more write: scan completion, logged once from the shared `run_scan` helper (single source
  for CLI + web) so "Scanned Old Backup HDD — 1,204 new" shows up in the feed too.
- `GET /api/activity` returns a small `ActivityDto { kind, summary, occurred_at }` list, with
  `summary` formatted server-side per action kind (so the page stays a dumb renderer, consistent
  with the existing `esc()`/`textContent` XSS-safety pattern — no client-side string building of
  DB content).

### 2. Disk capacity
`sysinfo` is already a dependency. Its `Disks` API gives real total/available bytes per mount
point — no new crate needed. A new `GET /api/drives` endpoint (distinct from the existing
`/api/detected-drives`, which the Scan page keeps using) returns, per mounted volume: capacity
(total/free), catalogued status, label, last-scan time, and reclaimable bytes (see below). If
capacity can't be read for a path, the field is omitted and the UI hides that bar rather than
erroring.

### 3. Forget drive
Removing a volume's catalog awareness without touching any file on disk — the mockup's "Eject"
button, reinterpreted per your direction, since a local cataloging tool has no real eject action.
- `Catalog::forget_volume(volume_id)` deletes the volume's `files` rows and its `volumes` row (the
  existing FTS triggers clean up `files_fts` automatically as a side effect of the row deletes).
  A rescan fully rebuilds the volume's catalog entries later — nothing is destructive to actual
  files.
- `cus forget <path>` (CLI) and `POST /api/forget-drive {volume_id}` (web), both requiring the
  path/volume to resolve like other volume-scoped commands. Web UI shows a confirm step before
  calling it, and the copy is explicit that files on disk are untouched.

### 4. Purge all
The mockup's "Purge quarantine" / "Purge All" actions, generalized to every drive at once.
- A thin wrapper that iterates every **currently-mounted** volume with reclaimable
  `_ToDelete` space and calls the existing `purge_volume` on each. Unmounted volumes are skipped
  and reported back — mirrors the existing `QuarantineResultDto` structured-partial-failure
  pattern (`purged: [...]`, `skipped_unmounted: [...]`, `errors: [...]`), so one bad volume can't
  hide that others succeeded.
- `cus purge --all` (CLI) and `POST /api/purge-all` (web).

### 5. "Repair"
The mockup's "Repair" button on an errored drive card is just the existing rescan action
(`api_scan`/`cus scan`), relabeled in the UI when that drive has rows in `scan_errors`. No new
engine code.

### 6. Reclaimable space per drive
Needed for the Overview and Drives cards. Defined as: total bytes of a volume's **active** files
that are duplicate-group members but not that group's `suggested_keep` — i.e. exactly the set of
files `/api/duplicates` would suggest quarantining, aggregated by volume instead of returned as
full groups. New store method reuses the same grouping query `/api/duplicates` already runs.

### 7. Console — terminal-style panel, no new backend surface

A user request to add "a terminal to execute CLI commands directly in the GUI," scoped down
deliberately: not an arbitrary shell (that would be a real remote-code-execution surface on a
localhost tool managing irreplaceable data — directly against the project's "nothing may ever be
lost or corrupted" constraint), but a **constrained command console**.

Every action already has a safe, CSRF-protected `/api/*` endpoint (search, status/duplicates,
quarantine, purge/purge-all, repack, scan, forget). The Console is a client-side REPL only:

- A small fixed grammar (`scan <path> [--force]`, `search <query> [--category X]`, `status`,
  `duplicates`, `quarantine <id>...`, `purge <path>` / `purge --all`, `repack <id>`,
  `forget <path>`, `help`) parsed entirely in the page's JS.
- Each recognized line becomes exactly one `fetch()` call against an existing endpoint — the same
  ones the buttons on other pages call, with the same CSRF token. No new Rust code, no subprocess
  spawn, no shell parsing, no arbitrary command execution.
- Unrecognized input is rejected client-side with a usage hint before any request is made.
- Responses are the same JSON other pages already receive, rendered as scrollback text through the
  existing `esc()`-safe rendering path (never raw `innerHTML` of server data).

Net new attack surface: **zero**. This is a different way to trigger the same fixed set of
already-reviewed actions, not a new capability.

## Visual/technical implementation

- **Fonts**: system stacks, not Google Fonts — `-apple-system, "Segoe UI", sans-serif` for UI
  text, `ui-monospace, "Cascadia Code", "SF Mono", Consolas, monospace` for paths/hashes/sizes.
  This is actually closer to the *original* Stitch prompt brief (`docs/design/stitch-prompt.md`,
  which asked for "San Francisco/system-ui") than what Stitch itself substituted.
- **Icons**: a small inline SVG set (~16 stroke-based glyphs — dashboard, folder, duplicate,
  drive, scan, search, warning, etc.) embedded in the shared shell, replacing the Material Symbols
  web font.
- **No fake data, no placeholder avatar images.** Every number, row, and status pill in the
  mockups is replaced with real data from the API; the mockups' fake "Alex Rivera / Premium Plan"
  profile block and Google-hosted stock photos are dropped entirely.
- **Light + dark mode** via CSS custom properties + `prefers-color-scheme`, using the light/dark
  token pairs already specified in `docs/design/stitch-prompt.md` and `DESIGN.md` (both call for
  both modes; the Stitch exports themselves are light-only screenshots).
- **Shared shell**: one Rust helper renders the sidebar+toolbar HTML with the active nav item
  highlighted; one shared `<style>` constant holds the design-system CSS (colors, spacing, type
  scale, card/button/badge styles), included by all six page templates — avoids duplicating
  ~150 lines of shell markup six times and keeps each page's own const focused on its content.
- Existing CSRF-token-in-header pattern, `esc()`/`textContent` XSS-safety, and
  `Catalog::open_readonly` for read-only handlers all carry over unchanged.

## Testing

- Unit tests for new store methods: `recent_actions`, `forget_volume` (row + FTS cleanup),
  `reclaimable_bytes_by_volume`.
- Integration tests (existing `tests/*.rs` + inline `#[cfg(test)]` patterns) for each new endpoint:
  `/api/activity`, `/api/drives`, `/api/forget-drive`, `/api/purge-all`, plus the new `/`, `/browse`,
  `/drives` page handlers (asserting they fetch the right endpoints, same style as existing page
  tests).
- CLI integration tests for `cus forget` and `cus purge --all`, following the existing
  `CARGO_BIN_EXE_cleanupstorages` pattern.
- Console has no server-side surface to unit test; cover its command grammar with a page-level JS
  smoke check (e.g. a headless test asserting known inputs map to the right endpoint + method) if
  the project's existing page tests support that pattern, otherwise a documented manual check.
- Manual pass against `scripts/make-test-sandbox.ps1` + `docs/TESTING-GUIDE.md`'s sandbox once
  implemented, and the guide gets a short new section for Overview/Drives/Console/forget/purge-all.

## Out of scope

- Any real OS-level "eject" (unmounting a physical drive) — not attempted; "Forget drive" is a
  catalog-only action.
- Actual corruption "repair" beyond rescanning — the existing corrupt-catalog snapshot-guard is
  unchanged and out of scope here.
- Changing the underlying scan/quarantine/purge/repack engines — this pass is UI + the backend
  additions in items 1–6 above (Console adds none — see §7), not new dedup logic.

# Browse tree + duplicate highlighting + theme colors — design

## Goal

Make the Browse page answer "what do I have and where does it live" at a glance: replace the flat,
hard-to-read results table with a **drive → folder tree**, **color-highlight duplicate files** (same
content = same color, click a copy to highlight all its siblings), give **each drive a stable auto
color** used everywhere it appears, and **fix the dark-theme status colors** (currently low-contrast)
plus soften the pure-black background.

Two concrete defects motivate this: (1) the Browse "Drive" column prints the raw `volume_id` marker
string instead of the drive's name, so provenance is unreadable; (2) the amber/red/green status pill
colors are defined only for light mode and never overridden for dark, so they render as dark
brown/muddy on a dark background.

## Design principles (PoSD)

Each change is a small, well-bounded unit with a clear interface, so the whole stays simple:
- **One store query** answers "which of these hashes are duplicated, and how many copies" — the
  handler stays a thin mapper.
- **The search DTO gains exactly three fields** (drive name, content hash, copy count); no schema
  change, no new endpoint.
- **One shared `driveColor()` helper** owns the drive→color mapping, reused by every page (no
  per-page divergence).
- **The tree is two focused client functions** — `buildTree(hits)` (data → nested model) and
  `renderTree(node)` (model → DOM) — each testable in isolation by shape.
- **All theme tokens live in one `:root` block** (light) + one `@media(dark)` override; adding the
  missing dark status colors is a localized edit, not scattered.

## Backend changes (minimal, no schema change)

### 1. `Catalog::duplicate_counts(hashes) -> HashMap<String, i64>`
New store method (`src/catalog/store.rs`): given a slice of content hashes, return those that have
**more than one active copy in the whole catalog**, mapped to their active copy count. Bounded by an
`IN (…)` list of the passed hashes (fast; indexed on `content_hash`). "Duplicated" here is global
(a file counts as duplicated even if its other copy isn't in the current search result), which is the
correct meaning for provenance.

### 2. Enrich `HitDto` (`src/web.rs`) with three fields
- `volume_label: String` — the drive's friendly name (from the `volume_id → label` map the handler
  already builds elsewhere; falls back to the id if somehow absent).
- `content_hash: String` — so the client can group same-content files and color them together.
- `copies: Option<i64>` — the global active-copy count when the file is duplicated, else `null`.
  `is_duplicate` on the client is simply `copies != null`.

`api_search` builds the label map (`cat.volume_stats()`), collects the result hashes, calls
`duplicate_counts` once, and fills the three fields. The Browse page requests a higher `limit`
(e.g. 3000, still ≤ the existing 5000 cap) so the tree is useful; the "showing first N — refine your
search to see more" note appears when the cap is hit. The console `search` command just prints the
richer JSON — no change needed there.

## Frontend changes

### 3. `driveColor(id)` in `SHARED_JS`
A pure helper: hash the `volume_id` to a hue and return a stable HSL color (mid saturation/lightness
so it reads on both themes). Used for every drive dot/chip across Browse, Drives, and Overview so a
drive is the same color everywhere. A matching `dupColor(hash)` derives a distinct hue from a content
hash for the duplicate marker/tint.

### 4. Browse page becomes a tree (`web_ui::browse_page`)
Search box + the three filters (drive/type/status) stay on top and narrow the tree live (same
`/api/search` call as today). The results area renders a collapsible tree:
- **Drive nodes** (top level, expanded by default): a `driveColor` dot + drive name + roll-up size.
- **Folder nodes**: collapsible (native `<details>`/`<summary>` for zero-JS, accessible collapse),
  showing the folder name and, when collapsed, a small "N duplicates inside" count if any.
- **File leaves**: filename (primary), size, a status pill only when not active. A **duplicated file**
  is tinted with its `dupColor` and shows a `◆N` marker (N = `copies`). Archives (`container_chain`)
  render as a node whose children are the in-archive entries.
- **Click a duplicated file → highlight every visible file sharing its hash** (toggle a highlight
  class on all leaves with the same `data-hash`). This is the "see the connection" a graph would
  give, without the graph.
- Empty/`0 results` and error states are calm one-liners (reusing the existing pattern).

`buildTree(hits)` groups hits by `volume_id`, splits each `relative_path` into segments to build the
nested folder model, and attaches archive entries under their archive file. `renderTree` walks the
model to HTML via `esc()`/`textContent` (no raw innerHTML of server strings). Both are small and
independently reasoned-about.

### 5. Theme colors (`STYLE`)
- Add dark-mode overrides for the status colors: orange `#ff9f0a`, red `#ff453a`, green `#30d158`
  (Apple system dark), each with a low-alpha background tint, so pills have proper contrast in dark.
- Soften dark background: `--bg` `#000` → `#1c1c1e`, `--content` `#1d1d1f` → `#2c2c2e` (elevated
  cards read as surfaces). Light mode unchanged.
- New tree-specific classes (tree indentation, node rows, the `◆` duplicate marker, the highlight
  state) live in `STYLE` with the rest of the design system.

## Out of scope
- Lazy per-folder server-side tree loading (the tree is built from the capped search result set; a
  very large catalog would want lazy expansion — noted for later, not built now).
- Any change to the Duplicates/Review flow, the catalog schema, scanning, or the reliability engines.
- A force-directed "graph" visualization — the duplicate coloring + click-to-highlight delivers the
  same insight far more simply.

## Testing
- Unit test `duplicate_counts`: unique hash absent from the map; a 2-copy hash present with count 2;
  a hash duplicated only across `missing` rows excluded (active-only).
- Endpoint test: `/api/search` results carry `volume_label` (real name, not the id), `content_hash`,
  and `copies` set for a duplicated file / `null` for a unique one.
- Browse page test: self-contained (no `http(s)://`), renders the tree container, references
  `/api/search` + `/api/volumes`, and no SHARED_JS const is re-declared.
- The existing suite (behavior of search/quarantine/etc.) stays green; the moved-off table is
  replaced, and the browse page test is updated to assert the tree structure instead of the old
  `<table>`/`id="results"` (kept meaningful, not weakened).

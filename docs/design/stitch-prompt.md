# Google Stitch prompts — CleanUpStorages UI

Paste the **Design system brief** first (or prepend it to each screen). Then generate **one screen at a
time** using the per-screen prompts. Aesthetic: minimal, modern, pro, Apple/macOS-like.

---

## Design system brief (prepend to every screen)

> Design a **desktop web app** styled like a native **macOS** application — minimal, modern, precise, and
> calm. This is a local single-user tool that helps someone organize and de-duplicate thousands of
> irreplaceable personal + academic files spread across external hard drives. The mood is **trustworthy and
> in control**: it moves precious data, so the UI should feel safe, quiet, and deliberate — never flashy.
>
> **Layout:** a macOS-style app window with a **left sidebar** and a large content area. The top of the
> content has a translucent, blurred toolbar (vibrancy) with a hairline bottom border. 8-pt spacing grid,
> generous whitespace, large corner radii (10–16px), hairline 1px separators instead of heavy borders, very
> soft shadows only where needed.
>
> **Sidebar:** a light translucent panel. Top section = primary navigation with small SF-style symbols:
> **Overview**, **Browse**, **Duplicates**, **Scan**. Below, a **Drives** section listing the connected
> drives (e.g. “Photos HDD”, “Old Backup HDD”) each with a small disk icon and a subtle dot showing
> connected/offline. Selected item has a soft blue-tinted rounded highlight.
>
> **Typography:** San Francisco / system-ui. Large, semibold but light page titles; secondary text in muted
> gray; **SF Mono / monospaced** for file paths, hashes, and sizes (tabular numerals). Clear hierarchy, no
> visual noise.
>
> **Color:** near-neutral surfaces. Light mode: content `#ffffff`, window/sidebar `#f5f5f7`, text `#1d1d1f`,
> secondary `#6e6e73`. Dark mode: content `#1d1d1f`, window `#000000`, text `#f5f5f7`. **One** accent —
> system blue (`#0071e3` light / `#0A84FF` dark). Status colors used sparingly as small tinted pill badges:
> amber = quarantined, red = missing, gray = purged, green = active/kept. Support **both light and dark
> mode**.
>
> **Controls:** rounded search field with a magnifier glyph; pill **segmented controls** for filters; a
> filled blue **primary** button and a tinted/quiet **secondary**; small toggles and checkboxes; unobtrusive
> icon buttons. Everything rounded, restrained, high-contrast text, comfortable hit targets.

---

## Screen 1 — Overview (dashboard)

> Using the design system above, design the **Overview** screen. Toolbar title “Overview”. The content area
> is a set of calm summary **cards** on a subtle grid:
> - A hero card: **“2,914,203 files catalogued”** across **“4 drives”**, with a small line “Catalog stored
>   safely on this Mac”.
> - A card **“38 duplicate groups”** with a subtitle “~12.4 GB reclaimable” and a blue **“Review
>   duplicates”** button.
> - A card **“Reclaimable space”** showing a slim horizontal bar per drive (Photos HDD, Old Backup HDD…)
>   with a monospaced GB figure, and a quiet **“Purge quarantine”** secondary action.
> - A **“Recent activity”** list card: a few rows like “Quarantined trip-2019.jpg”, “Repacked bundle.zip”,
>   “Scanned Old Backup HDD — 1,204 new”, each with a small icon and a relative time (“2m ago”).
> Keep it airy, aligned to the grid, hairline separators inside cards, monospaced numbers. No clutter.

---

## Screen 2 — Browse & Search

> Using the design system above, design the **Browse** screen — a searchable inventory of every catalogued
> file (including files that live *inside* zip archives, and files on drives that are currently
> disconnected).
> - Toolbar: a wide rounded **search field** (“Search filename or path…”), and to its right three pill
>   **segmented filters**: Drive (All drives ▾), Type (All types ▾ — Photo/Video/Document/Academic/Other),
>   Status (Any ▾).
> - A results **list** (Finder-like, hairline row separators, comfortable rows). Columns: **Location**
>   (monospaced path; for an archived file show it as `bundle.zip › 2019/thesis.pdf` with the “›” chevrons),
>   **Drive** (small label like “Photos HDD”), **Type** (tiny category chip), **Size** (right-aligned
>   monospaced, e.g. “12.4 MB”), **Status** (small tinted pill — mostly empty for Active; show one
>   `Missing` in red and one `Quarantined` in amber to demonstrate).
> - Sample rows: `2019/thesis_final.pdf` (Photos HDD, Document, 12 MB), `bundle.zip › report.txt` (Old
>   Backup HDD, Document, 67 B), `Trip 2019/beach.jpg` (Photos HDD, Photo, 4.2 MB, thumbnail-less is fine),
>   `misc/notes.txt` (Old Backup HDD — Missing).
> - A quiet result count above the list (“1,204 results”). Empty, calm, precise.

---

## Screen 3 — Scan a drive

> Using the design system above, design the **Scan a drive** screen — where the user adds a drive to the
> catalog and watches progress.
> - Section **“Detected drives”**: 2–3 selectable cards/rows for connected drives, each showing the mount
>   path (monospaced, e.g. `D:\` or `/Volumes/Photos HDD`), a friendly label, and a small tag —
>   “new” (blue) or “catalogued · rescan” (gray).
> - Section **“Or choose a folder”**: a rounded path text field prefilled `…/Old Backup HDD`, a quiet
>   **“Browse…”** button (opens a native folder picker), and a small toggle **“Force full rescan”** with a
>   one-line caption. A note in tiny muted text: “Scanning writes a tiny hidden marker so the drive is
>   recognised next time.”
> - A filled blue **“Scan”** button.
> - A **live status** panel below: a currently-running scan showing an indeterminate progress feel and
>   ticking counts — “Scanning Photos HDD — **1,204** hashed · **380** unchanged · **3** errors · **2**
>   archive entries”. Below it a **“Recent scans”** list with 2 finished rows (drive name + the same counts,
>   one with a small red error note “permission denied on 1 folder”).
> Calm, one primary action, monospaced counts, generous spacing.

---

## Screen 4 — Review duplicates (the hero screen)

> Using the design system above, design the **Review duplicates** screen — a focused, Tinder-style review of
> one duplicate group at a time, so the user confirms which copy to keep and sends the rest to a reversible
> quarantine.
> - Toolbar title “Review duplicates”, with a subtle progress caption on the right: “Group 3 of 38 · 4
>   copies”.
> - Centered, a row of **comparison cards** (2–4) — one per identical copy. Each card:
>   - A **photo thumbnail** at top (for a photo group, show the same sunset/beach image on each card; for a
>     non-photo group show a subtle file-type glyph placeholder “no preview”).
>   - The **location** (monospaced, wraps; archived copies show `bundle.zip › trip/beach.jpg`).
>   - Small metadata lines: **Drive** (semibold, e.g. “Photos HDD”), **size** (“4.2 MB”), **created** date,
>     and a tiny status pill.
>   - The **suggested keep** card is highlighted with a blue ring + a small “✓ Keep this” badge; other cards
>     are quietly de-emphasized. Cards are clickable to change which one is kept.
>   - Archived copies show a small **“Remove from archive”** button instead of the normal remove (rebuilds
>     the zip without that entry). One card can show “inside archive — drive not connected” disabled state.
> - Bottom action bar: a filled blue **“Keep selected, quarantine the rest”** primary button and a quiet
>   **“Skip”** secondary. A tiny reassuring line: “Nothing is deleted — copies move to a recoverable
>   _ToDelete folder until you purge.”
> - Show one alternate state faintly if space allows: a **“All duplicates reviewed 🎉”** empty/finished
>   state (calm, centered).
> This is the emotional core: make it feel safe, confident, and effortless — one decision at a time.

---

## Optional Screen 5 — First-run / empty state

> Using the design system above, design a **first-run empty state** for when no drives have been scanned
> yet: a centered, calm hero with a short line “Let’s catalogue your drives”, one sentence of reassurance
> (“Everything stays on this Mac. Nothing is ever deleted without your say-so.”), and a single blue **“Scan
> a drive”** button. Minimal, spacious, inviting.

---

### Tips for using these in Stitch
- Generate **Screen 4 (Review duplicates)** first — it’s the signature screen and sets the tone.
- If Stitch drifts busy/colorful, re-emphasize: *“minimal, mostly monochrome, one blue accent, lots of
  whitespace, hairline separators, macOS-native feel.”*
- Ask Stitch for **both light and dark** variants of each screen.
- Keep the **sidebar identical** across screens for consistency; iterate the content area per screen.

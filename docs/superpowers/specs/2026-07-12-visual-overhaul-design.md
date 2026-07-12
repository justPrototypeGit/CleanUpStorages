# Visual overhaul (Stitch fidelity) — design

Sub-project 2 of the UI-improvement roadmap. A top-to-bottom visual refinement of the web UI to get
close to the Google Stitch mockups' look, plus the two things the user asked for directly: an in-app
**Auto/Light/Dark theme toggle** and **themed dropdowns**, and a **rebuilt scan live-status panel**.

## Constraints & decisions

- **100% self-contained** (unchanged rule): no external requests — no CDN, no web fonts, no icon
  fonts. Every page still passes "no `http(s)://`".
- **Typeface: system stack** (Segoe UI on Windows / system-ui), per the user ("don't care about the
  font, care about the graphics"). No Inter/JetBrains-Mono embedding. All effort goes into layout,
  color, depth, spacing, and components — where the Stitch gap actually lives.
- **Icons: a richer hand-authored inline-SVG set** (Material Symbols is a font we can't fetch). Add
  glyphs for the actions that currently have none (edit, rescan, forget, purge, eject-ish, add,
  sun/moon/auto for the theme toggle, chevrons, status marks).
- **Reference:** the Stitch export in `StitchExport/` and its `precision_file_architect/DESIGN.md`
  tokens (macOS-native utility, glassmorphism/vibrancy, hairline borders, large radii, 8-pt grid,
  system-blue as the sole accent, status as small tinted pills).
- **Honesty:** the result is built/tested headlessly; color/spacing is subjective and will need a
  round of user fine-tuning after they run it.

## Scope

### A. Theme system + toggle + themed form controls
- Keep `@media (prefers-color-scheme)` as the **Auto** default. Add explicit
  `:root[data-theme="light"]{…}` and `:root[data-theme="dark"]{…}` blocks (same variables) that
  **win** over the media default (attribute selector outside a media query has higher precedence), so
  the user can force a theme regardless of OS.
- A **theme control** in the sidebar footer: a 3-way segmented control (Auto / Light / Dark) with
  sun/moon/auto icons. It writes `localStorage.theme` and sets/clears `document.documentElement`'s
  `data-theme`. A tiny inline script in `<head>` applies the saved choice **before first paint**
  (no flash of the wrong theme).
- **Themed form controls:** `select`, `option`, `input`, `textarea` get explicit
  `background:var(--content); color:var(--fg)` and `color-scheme` so the native dropdown *popup*
  follows the theme (the current bug: option lists render light in dark mode). Refined focus rings.

### B. Core design-system refresh (shared — benefits every page)
Rework `STYLE` and the shell for a more polished, Stitch-like feel, keeping the existing CSS-variable
architecture so pages don't need rewrites:
- **Depth & glass:** layered, softer card shadows with a subtle hover lift; stronger but tasteful
  sidebar/toolbar vibrancy (backdrop blur + saturate); hairline separators at low opacity.
- **Buttons:** primary = filled accent with a subtle top inner-highlight + soft shadow (the macOS
  "slightly raised" look) and a gradient-free but crisp hover; secondary = tinted; danger; **icon
  buttons** (no chrome, hover halo). Consistent radii (8–10px).
- **Sidebar:** refined active state (accent-tinted rounded block, accent text + icon), tighter
  rhythm, a small brand mark, section labels, and the drive list with colored dots (reusing
  `driveColor`). Theme control pinned in the footer.
- **Toolbar:** page title + a right-aligned slot for page actions; hairline bottom, vibrancy.
- **Cards / stat tiles / pills / tables / tree / review cards / console:** unified radii, spacing on
  the 8-pt grid, refined status pill colors (already fixed for dark), nicer empty/hover states. The
  Browse tree, Review comparison cards, and Console terminal all get their shared-class refinements
  here (no per-page rewrite needed).
- **Richer icon set:** expand `icon()` and add action glyphs used across pages.

### C. Scan live-status rebuild
Rebuild the Scan page's status area to match the Stitch scan screen:
- **Detected-drive cards** with drive icon, label/path, and a "new / catalogued" tag.
- The path input + **Browse…** + a proper **toggle switch** for "force full rescan".
- A **live status panel** while a scan runs: an (indeterminate) progress bar + **count tiles**
  (Hashed / Unchanged / Errors / Archive entries) that tick, and a clear "Scanning <drive>…" header;
  collapses to "No scan running" when idle.
- A **Recent scans** list with a status icon per row (done / error), the counts, and the error note
  when present.
- Same polling/behavior as today (`/api/scan`, `/api/scan/status`) — only the presentation changes.

### D. Overview + Drives refinement
- **Overview:** a hero stat card with a subtle accent glow, a bento arrangement of the
  duplicates / reclaimable-space / activity cards, refined typography and the drive-colored bars.
- **Drives:** refined cards (capacity bar, status, last-scan, reclaimable), action buttons with
  icons, and **upgrade the drive Edit from `window.prompt` to a proper inline themed form**
  (name + description fields, Save/Cancel) — same `/api/rename-drive` call.

## Out of scope
- Any backend/behavior change (this is presentation only; all endpoints/handlers unchanged).
- Embedding fonts or icon fonts; a real OS-level file dialog restyle (that's sub-project 3: DPI
  manifest + app icon).
- New features beyond the theme toggle and the scan-panel/edit-form presentation.

## Testing
- The existing page tests (self-contained; each page fetches its endpoints; `id="q"`/`id="results"`
  etc.) must stay green — this is a restyle, behavior-preserving. Assertions that check for specific
  wiring strings are preserved.
- New/updated assertions: pages include the theme-toggle control and the `data-theme` init; the Scan
  page renders the count-tiles/progress structure; the Drives page still references
  `/api/rename-drive` (now via the inline form).
- Self-contained check (no `http(s)://`) on every page after the overhaul.
- Manual pass (the user runs it) is the real visual gate; a short "what to look at" note goes in the
  testing guide.

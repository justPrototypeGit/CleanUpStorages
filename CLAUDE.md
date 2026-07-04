# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Design phase complete; no application code exists yet. The approved design lives in
[docs/superpowers/specs/2026-07-04-cleanupstorages-design.md](docs/superpowers/specs/2026-07-04-cleanupstorages-design.md)
— **read it before writing any code**; it is the source of truth for architecture and behavior. The next step
is turning it into an implementation plan (Phase 1). Update this file with real build/test/run commands once the
Rust project is scaffolded.

## Project goal

CleanUpStorages helps the user bring order to thousands of GB of important, irreplaceable data (mixed personal +
academic) spread across multiple near-full external HDDs. Three phases:

1. **Catalog + search** — crawl each drive, hash every file, build a persistent searchable register (works even
   for drives not currently plugged in).
2. **Deduplicate** — visual review GUI ("Tinder-style" swipe + WinMerge-style compare) to confirm duplicates
   one-by-one; confirmed duplicates are soft-deleted to quarantine.
3. **Reorganize** — later, into a clean structure (target taxonomy deliberately undecided).

## Overriding constraint

**Reliability: nothing may ever be lost or corrupted.** This dominates every decision. The tool never performs
an irreversible destructive action on its own — duplicates move to a same-drive `_ToDelete` quarantine (a
rename, ~zero space), and the user empties it manually. Archive repacks (Case 4 in the spec) build a verified
temp copy and only swap after re-hashing every retained entry; the original is preserved in quarantine too. When
in doubt, prefer the option that cannot lose data, even if slower or more manual.

## Tech stack (decided)

- **Rust**, compiled to a single static binary per platform (Windows `.exe`, macOS arm64). No runtime/interpreter.
- **SQLite** catalog (`catalog.db`) stored on the computer, never on the HDDs. WAL mode + auto-snapshots.
- **BLAKE3** for content hashing (streamed, parallel).
- **`axum`** local web server (`127.0.0.1` only) for the review + search GUI; plain HTML/CSS/JS, no frontend
  build system. A CLI covers scan/search/status/purge.

## Git conventions

See [CONTRIBUTING.md](CONTRIBUTING.md). Trunk-based on `main`; feature work on `<type>/<kebab-desc>` branches
(types: `feat`/`fix`/`docs`/`refactor`/`test`/`chore`/`perf`/`build`/`ci`/`style`; scopes: `scanner`,
`catalog`, `dedup`, `archive`, `review`, `storage`, `cli`). Commits follow Conventional Commits.

## Documentation map

- Approved design spec: `docs/superpowers/specs/2026-07-04-cleanupstorages-design.md`
- Deferred next-version ideas (do not implement without a new spec): `docs/future-ideas.md`

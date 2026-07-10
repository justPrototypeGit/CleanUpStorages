# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Phases 1 and 2 are implemented and the web UI is fully built out. The approved design lives in
[docs/superpowers/specs/2026-07-04-cleanupstorages-design.md](docs/superpowers/specs/2026-07-04-cleanupstorages-design.md)
— it remains the source of truth for architecture and behavior; read it before changing core behavior.
Phase 3 (reorganize into a clean taxonomy) is still deferred.

## Build / test / run

- Build: `cargo build --release` → `target/release/cleanupstorages(.exe)`
- Test: `cargo test`
- Web UI: `cleanupstorages browse` (serves 127.0.0.1, opens a browser) — six pages (Overview, Browse,
  Duplicates, Drives, Scan, Console), all self-contained (no CDN/fonts/build step).
- CLI verbs: `scan`, `search`, `status`, `duplicates`, `quarantine`, `purge` (`--all`), `repack`, `forget`,
  `browse`. Global `-v/--verbose`; `RUST_LOG` overrides.
- Safe end-to-end walkthrough: [docs/TESTING-GUIDE.md](docs/TESTING-GUIDE.md) (+ `scripts/make-test-sandbox.ps1`).

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

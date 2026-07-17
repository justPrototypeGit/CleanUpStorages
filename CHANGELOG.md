# Changelog

All notable changes to CleanUpStorages are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-07-17

First published release, with downloadable Windows and macOS (Apple Silicon) binaries.

### Added
- **Catalog + search.** Crawl a drive, hash every file with BLAKE3, and build a persistent SQLite
  catalog stored on your computer — so search keeps working when the drive is unplugged. Loose files
  and files inside (nested) zip archives are both catalogued.
- **Deduplicate.** Find duplicate groups by content hash and review them one at a time in a
  visual GUI. Confirmed duplicates are *moved* to a same-drive `_ToDelete` quarantine (a rename, not
  a copy); `purge` is the only real delete and is user-initiated.
- **Archive repack.** Remove a duplicate from inside a zip by rebuilding a verified copy, keeping the
  original in quarantine.
- **Local web UI** (`browse`) — Overview, Browse (tree view with duplicate highlighting), Duplicates,
  Drives, Scan and Console pages, in light and dark themes. Binds to `127.0.0.1` only, CSRF-guarded,
  and makes zero external requests (fonts are vendored and compiled into the binary).
- **CLI** — `scan`, `search`, `status`, `duplicates`, `quarantine`, `purge`, `repack`, `forget`,
  `rename`, `browse`.
- **Release automation** — tagging `vX.Y.Z` builds and checksums the binaries and opens a draft
  GitHub Release.

### Safety
- Nothing is ever deleted automatically. The catalog lives on the computer, never on the drives, so
  it survives a drive failing.

[Unreleased]: https://github.com/justPrototypeGit/CleanUpStorages/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/justPrototypeGit/CleanUpStorages/releases/tag/v0.2.0

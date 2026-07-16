# Git conventions

This project follows a standard trunk-based workflow with Conventional Commits.

## Branches

- **`main`** — the default, always-releasable branch. No direct commits for feature work; changes land via
  short-lived branches.
- **Working branches** — named `<type>/<short-kebab-description>`, where `<type>` matches the commit types
  below. Examples:
  - `feat/scanner-recursive-archives`
  - `fix/volume-marker-readonly-drive`
  - `docs/dedup-workflow-spec`
  - `chore/ci-setup`

## Commits — Conventional Commits

Format: `<type>(<optional scope>): <summary>`

- Summary in imperative mood, lowercase, no trailing period, ≤ ~72 chars.
- Body (optional) explains the *why*, wrapped at ~72 chars.
- Footer (optional) for `BREAKING CHANGE:` notes or issue references.

**Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `build`, `ci`, `style`.

**Scopes** used in this project: `scanner`, `catalog`, `dedup`, `archive`, `review`, `storage`, `cli`.

Examples:

```
feat(scanner): recognize drives across sessions via hidden marker file
fix(archive): stop descending when max nesting depth is exceeded
docs: record CleanUpStorages design spec
```

## Merging

- Prefer small, reviewable branches merged into `main`.
- Keep history readable; squash noisy work-in-progress commits when merging.

## Building, testing, CI

```bash
cargo build --release      # -> target/release/cleanupstorages(.exe)
cargo test                 # full suite
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs exactly these on every push to `main` and every PR, across **Windows** and **macOS**.
Run them locally before opening a PR and CI will not surprise you.

## How this project is designed

Every feature starts as a **design spec** in `docs/superpowers/specs/`, becomes an
**implementation plan** in `docs/superpowers/plans/`, and only then gets written. Both are
committed alongside the code, so you can read *why* a thing looks the way it does before
proposing a change. See [docs/ai-sdlc.md](docs/ai-sdlc.md) for how that loop works.

If you're proposing something substantial, open an issue first — a short spec beats a large
surprise PR.

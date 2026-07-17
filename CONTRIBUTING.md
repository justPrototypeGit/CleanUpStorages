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

For day-to-day iteration, plain debug builds are fine and faster:

```bash
cargo build                # -> target/debug/cleanupstorages(.exe)
cargo test                 # full suite
```

Before opening a PR, run the same commands CI runs, so nothing surprises you:

```bash
cargo build --release --locked
cargo test --release --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

CI is split across two jobs:

- A **build/test/clippy** job runs the `--release --locked` build, test, and clippy commands
  above on a matrix of **Windows** and **macOS** (`windows-latest`, `macos-latest`).
- A separate **`rustfmt`** job runs only `cargo fmt --check`, on **Linux** (`ubuntu-latest`).

Both jobs run on every push to `main` and every PR; all of them must pass.

## How this project is designed

Every feature starts as a **design spec** in `docs/superpowers/specs/`, becomes an
**implementation plan** in `docs/superpowers/plans/`, and only then gets written. Both are
committed alongside the code, so you can read *why* a thing looks the way it does before
proposing a change. See [docs/ai-sdlc.md](docs/ai-sdlc.md) for how that loop works.

If you're proposing something substantial, open an issue first — a short spec beats a large
surprise PR.

## Cutting a release

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed. To cut one:

1. Bump `version` in `Cargo.toml`, and run `cargo build` so `Cargo.lock` updates.
2. Add a `## [X.Y.Z] - YYYY-MM-DD` section to `CHANGELOG.md` describing the release. The workflow
   uses this section verbatim as the release notes, and **refuses to release if it's missing**.
3. Commit both, and merge to `main`.
4. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.
5. The workflow builds both binaries and opens a **draft** release. Review it on the Releases page —
   check both archives and `SHA256SUMS` are attached and the notes read well — then click **Publish**.

The `guard` job fails fast if the tag doesn't match `Cargo.toml`, the changelog section is missing, or
tests don't pass — so a bad tag never produces a release.

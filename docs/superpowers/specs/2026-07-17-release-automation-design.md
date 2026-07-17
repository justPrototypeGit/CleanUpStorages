# Release automation (C1) — design

**Status:** approved
**Date:** 2026-07-17
**Scope:** sub-project C1 of the AI-driven-SDLC roadmap — the *release* half of the loop.
**Sibling (separate spec, later):** C2 = issue → spec → plan automation. **Deferred to D:** the LinkedIn post.

## Goal

Make the SDLC claim honest at the release end. Today the loop runs spec → plan → implement → review
→ merge, and `docs/ai-sdlc.md` admits *"there is no release automation yet."* This closes that gap:
**push a version tag → the pipeline builds Windows and macOS binaries, checksums them, and opens a
DRAFT GitHub Release with human-written notes → a human clicks Publish.** The machine does the
mechanics; the human keeps the judgement and the final gate.

Concretely, a stranger should be able to go to the Releases page, download a binary for their OS,
verify its checksum, and run it — without a Rust toolchain.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Trigger | Push of a `v*` tag | Tags are the natural release record; provenance lives in git, not a text box. |
| Human gate | **Draft** release the user publishes | The machine builds; the human reviews and publishes. A broken build never reaches users. Matches the loop's thesis. |
| Release notes | **Hand-written `CHANGELOG.md`**, section per version | Strongest signal of care; cashes in the Conventional-Commits discipline. In this project "hand-written" = AI-drafted, human-approved — the loop working as designed. |
| Targets | `windows-latest` (x86_64-msvc) + `macos-latest` (aarch64 / M-series) | Exactly what CLAUDE.md commits to. Same platforms as CI. macOS Intel is out — user confirmed M-series only. |
| First release | **`v0.2.0`** | Marks "now with releases"; also serves as the first live test of the pipeline. Requires bumping `Cargo.toml` `0.1.0` → `0.2.0` before tagging (the version-match guard enforces the two agree). |
| Signing | **Out of scope** | Notarization/code-signing needs paid Apple + Windows certs. Instead, the release notes tell users how to get past Gatekeeper/SmartScreen. |

## The release flow

```
git tag v0.1.0 && git push origin v0.1.0
        │
        ▼
  [guard job]  version + changelog + tests   ──fail──▶  no release, red run, nothing built
        │ pass
        ▼
  [build matrix]  windows-latest, macos-latest  →  archive per OS
        │
        ▼
  [release job]  gather archives + SHA256SUMS  →  DRAFT release (notes = CHANGELOG section)
        │
        ▼
  human reviews the draft → clicks Publish     ◀── the gate
```

## Components

### 1. `.github/workflows/release.yml`

Three jobs. The workflow has **no side effects until the release job**, so a failed guard or build
leaves nothing half-published.

- **`guard`** (ubuntu-latest, runs first; the build matrix `needs: guard`):
  - Derive `VERSION` from the tag: `v0.1.0` → `0.1.0` (strip leading `v`).
  - **Version match:** read `version` from `Cargo.toml`; if it ≠ `VERSION`, fail with a message
    naming both. Kills "tagged v0.2.0, shipped 0.1.0".
  - **Changelog present:** `CHANGELOG.md` must contain a section header for `VERSION` (format fixed
    below). If absent, fail — never ship empty notes.
  - Export `VERSION` as a job output for downstream jobs.
  - **The guard does NOT compile.** It stays on ubuntu-latest for the cheap metadata checks only.
    (Superseded during execution: the guard originally ran `cargo test` here, but the project does
    not build on Linux without system libs — `rfd` pulls `wayland-sys`, whose build script needs
    libwayland/GTK — which is exactly why A+B's CI excluded Linux. The first live `v0.2.0` run caught
    this; test re-proving moved to the `build` jobs, on the platforms we actually ship.)

- **`build`** (matrix: `windows-latest`, `macos-latest`; `needs: guard`):
  - `cargo test --release --locked`, then `cargo build --release --locked` — re-prove the tests on
    the exact tagged commit on a platform we actually ship, then build. `--locked` so the binary is
    built from the exact committed `Cargo.lock`; a lockfile drift fails the build rather than silently
    resolving new dependency versions into a release artifact. A test failure fails the job, so
    `release` (needs: build) is skipped and nothing is published.
  - Package:
    - Windows → `cleanupstorages-<VERSION>-x86_64-pc-windows-msvc.zip` containing
      `cleanupstorages.exe`, `README.md`, `LICENSE`, `assets/LICENSES.md`.
    - macOS → `cleanupstorages-<VERSION>-aarch64-apple-darwin.tar.gz` containing the same set with
      the unsuffixed `cleanupstorages` binary.
  - Bundling `LICENSE` + `assets/LICENSES.md` in the archive keeps the binary distribution compliant
    with AGPL and the vendored-font notices — the same obligation the repo satisfies, now travelling
    with the downloadable artifact.
  - Upload each archive as a build artifact for the release job.

- **`release`** (ubuntu-latest; `needs: build`):
  - Download both archives.
  - Generate `SHA256SUMS` over the two archive files.
  - Extract the `CHANGELOG.md` section for `VERSION` → release body, appended with a fixed
    **unsigned-binary note** (see below).
  - Create a **draft** release for the tag with the two archives + `SHA256SUMS` attached.
  - Permissions: the job needs `contents: write`; set it at job scope, not workflow-wide.

### 2. `CHANGELOG.md` (new, repo root)

[Keep a Changelog](https://keepachangelog.com) format. The workflow parses it, so the header format
is a contract, not cosmetic:

- Version sections are `## [X.Y.Z] - YYYY-MM-DD`.
- The extractor pulls everything from `## [VERSION]` up to (not including) the next `## [` line.
- Seed it with **`## [0.2.0]`** — the first published release. Since nothing was ever released as
  0.1.0, this section documents the whole tool to date: catalog + search (BLAKE3, SQLite,
  persistent/offline), deduplicate (visual review, quarantine-as-rename, purge), the six-page web UI,
  the safety model, and — new in this release — automated Windows + macOS binaries. An
  `## [Unreleased]` section sits on top for ongoing work.

### 3. Doc updates

- **`README.md` — Quick install:** currently only "build from source". Add a **Download a release**
  route above it (grab the archive for your OS from Releases, verify against `SHA256SUMS`, unzip, run),
  and a short "verify the download" snippet. Keep the from-source route.
- **`docs/ai-sdlc.md` — "What's next":** it currently says *"there is no release automation yet."*
  Rewrite to describe the release loop as real, and move the remaining gap (issue → spec → plan, C2)
  into the "next" slot.
- **`CONTRIBUTING.md` — a short "Cutting a release" section:** bump `Cargo.toml`, add the
  `CHANGELOG.md` section, `git tag vX.Y.Z && git push origin vX.Y.Z`, then review and publish the
  draft. So the process is repeatable by a human who isn't the author.

## The unsigned-binary note (fixed release-body footer)

Because we don't sign, the note must give the *actual* steps, not a hand-wave:

- **macOS:** downloaded binaries are quarantined by Gatekeeper. After extracting, either
  `xattr -d com.apple.quarantine ./cleanupstorages`, or right-click → Open the first time and confirm.
- **Windows:** SmartScreen may warn on first run → "More info" → "Run anyway". The `SHA256SUMS` file
  lets you confirm the download matches before trusting it.
- A line pointing at `SHA256SUMS` and how to check it (`sha256sum -c` / `Get-FileHash`).

## Testing / verification

CI-workflow changes can't be unit-tested, so verification is staged:

1. **Static:** the workflow YAML parses; a local dry-run of the guard logic (version-compare and
   changelog-extract) against the real `Cargo.toml`/`CHANGELOG.md` produces the expected `0.1.0`
   string and non-empty notes. The extract/compare logic lives in small shell steps that can be run
   by hand.
2. **Live, low-stakes:** tag `v0.1.0`, watch the run, inspect the produced **draft** (archives present,
   both platforms, `SHA256SUMS` correct, notes = the changelog section + footer). Because it's a
   draft, nothing is public until reviewed — the test *is* the first real release.
3. **Negative checks (once, by hand):** a tag whose version disagrees with `Cargo.toml`, and a tag
   whose version is missing from `CHANGELOG.md`, must both fail the guard and produce no release.

## Non-goals

- Code signing / notarization (paid certs).
- Linux and macOS-Intel binaries (CLAUDE.md commits to Windows + macOS-arm64 only).
- Publishing to crates.io.
- Auto-bumping `Cargo.toml`/changelog or auto-tagging — the human writes the changelog and pushes the
  tag; that *is* the human gate at the front of the release.
- Auto-publishing (draft-only is deliberate).
- C2 (issue → spec → plan) — separate spec.

## Success criteria

1. Pushing `v0.2.0` (after bumping `Cargo.toml` to `0.2.0`) produces a **draft** GitHub Release with a
   Windows `.zip`, a macOS-arm64 `.tar.gz`, and a `SHA256SUMS`, notes = the `CHANGELOG.md` `[0.2.0]`
   section + the unsigned-binary footer.
2. A tag whose version ≠ `Cargo.toml`, or that has no `CHANGELOG.md` section, **fails and releases
   nothing**.
3. Each archive carries `LICENSE` and `assets/LICENSES.md`, so the binary distribution is
   licence-compliant on its own.
4. A stranger can download, checksum-verify, and run a binary with no Rust toolchain, and the release
   notes tell them how to get past Gatekeeper/SmartScreen.
5. `docs/ai-sdlc.md` no longer claims release automation is missing.
6. The whole thing is reproducible by a human via the CONTRIBUTING "Cutting a release" steps.

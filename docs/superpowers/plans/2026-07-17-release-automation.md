# Release Automation (C1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pushing a `v*` tag builds Windows + macOS-arm64 binaries, checksums them, and opens a DRAFT GitHub Release whose notes come from `CHANGELOG.md`, which a human then publishes.

**Architecture:** One workflow (`.github/workflows/release.yml`) with three jobs — `guard` (version/changelog/tests must agree, else nothing builds), `build` (matrix of the two OSes → archive each), `release` (gather archives + `SHA256SUMS` → draft release, notes = the changelog section + an unsigned-binary footer). Supporting files: a `CHANGELOG.md` the workflow parses, a `Cargo.toml` version bump, and doc updates.

**Tech Stack:** GitHub Actions, `gh` CLI (release creation), bash/awk (version + changelog extraction), the existing Rust/cargo build.

**Spec:** [docs/superpowers/specs/2026-07-17-release-automation-design.md](../specs/2026-07-17-release-automation-design.md)

## Global Constraints

- First release is **`v0.2.0`**; `Cargo.toml` must be bumped `0.1.0` → `0.2.0` before tagging. The `guard` job enforces tag == `Cargo.toml` version.
- Targets: **`windows-latest` (x86_64-pc-windows-msvc)** and **`macos-latest` (aarch64-apple-darwin / M-series)**. No Linux, no macOS-Intel.
- Release is created as a **draft**; a human publishes it. Never auto-publish.
- The `CHANGELOG.md` version-header format `## [X.Y.Z] - YYYY-MM-DD` is a **parsing contract** — the workflow extracts `## [VERSION]` up to the next `## [`. A missing section must fail the release.
- Every archive bundles `LICENSE` and `assets/LICENSES.md` so the binary distribution is licence-compliant on its own.
- Builds use `cargo build --release --locked` / `cargo test --release --locked` — the committed lockfile, same as CI.
- Signing, crates.io, Linux/Intel, and auto-publish are out of scope.
- Conventional Commits; commit trailers:
  `Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>`
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`
- Work on a feature branch off `main`; do NOT tag until the branch is merged to `main` (the tag must sit on a commit that already contains `release.yml`). Tagging/pushing the tag is human-gated (Task 5).

---

### Task 1: `CHANGELOG.md`

**Files:**
- Create: `CHANGELOG.md`

**Interfaces:**
- Produces: a file the `release` job (Task 3) parses. The header format `## [X.Y.Z] - YYYY-MM-DD` and the presence of a `## [0.2.0]` section are the contract.

**Context:** [Keep a Changelog](https://keepachangelog.com) format. Nothing was ever released as 0.1.0, so `[0.2.0]` documents the whole tool to date plus the new release automation.

- [ ] **Step 1: Write the changelog**

Create `CHANGELOG.md`:

```markdown
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
```

- [ ] **Step 2: Verify the extractor pulls exactly this section**

This is the same awk the workflow will use. Run it and confirm it returns the `[0.2.0]` body (not the header line, not `[Unreleased]`, not the link footer):

```bash
awk -v ver="0.2.0" '
  $0 ~ "^## \\[" ver "\\]" {grab=1; next}
  grab && /^## \[/ {grab=0}
  grab {print}
' CHANGELOG.md
```

Expected: prints from "First published release…" through the "### Safety" block and its bullet, then stops. It must NOT include the `## [0.2.0] - ...` header line, the `## [Unreleased]` section, or the `[Unreleased]:`/`[0.2.0]:` link lines (those come after the next-section boundary is… note: the link lines are NOT preceded by `## [`, so verify they are excluded — see Step 3).

- [ ] **Step 3: Confirm the link-reference lines don't leak into the notes**

The `[0.2.0]:` link-definition lines sit after the last `## [` section, so the awk keeps printing them. Verify:

```bash
awk -v ver="0.2.0" '$0 ~ "^## \\[" ver "\\]" {grab=1; next} grab && /^## \[/ {grab=0} grab {print}' CHANGELOG.md | tail -5
```

If the output ends with the `[Unreleased]: https://...` / `[0.2.0]: https://...` lines, they WILL leak into release notes. Fix by putting a trailing sentinel section so the extractor stops: add this as the LAST line-group of the file, AFTER the link definitions is wrong — instead, move the link definitions ABOVE `## [Unreleased]`? No. Simplest robust fix: the workflow's awk also stops at a line matching `^\[` (a link-reference definition). Update the local check to the workflow's final form:

```bash
awk -v ver="0.2.0" '
  $0 ~ "^## \\[" ver "\\]" {grab=1; next}
  grab && (/^## \[/ || /^\[[^]]+\]: /) {grab=0}
  grab {print}
' CHANGELOG.md | tail -5
```

Expected now: ends with the "### Safety" bullet, no link lines. (Task 3 uses this exact awk.)

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: add CHANGELOG.md (Keep a Changelog), seed [0.2.0]

Documents the tool to date as the first published release and is the
source the release workflow parses for release notes.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Bump version to 0.2.0

**Files:**
- Modify: `Cargo.toml` (`version = "0.1.0"` → `"0.2.0"`)
- Modify: `Cargo.lock` (the crate's own version entry)

**Interfaces:**
- Produces: `Cargo.toml` version `0.2.0`, which the `guard` job (Task 3) compares against the tag.

- [ ] **Step 1: Bump the version**

In `Cargo.toml`, change the `[package]` line `version = "0.1.0"` to:

```toml
version = "0.2.0"
```

- [ ] **Step 2: Update the lockfile without changing dependencies**

A plain build refreshes the crate's own version entry in `Cargo.lock` and nothing else:

```bash
cargo build --release
git diff Cargo.lock
```

Expected: the ONLY change in `Cargo.lock` is the `name = "cleanupstorages"` package's
`version = "0.2.0"`. If any third-party dependency version changed, run
`git checkout Cargo.lock && cargo build --release` again — a version bump must not drag in dependency
updates.

- [ ] **Step 3: Verify the version-match logic the guard will use**

```bash
cargo_version="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
echo "Cargo.toml version: $cargo_version"
test "$cargo_version" = "0.2.0" && echo "MATCHES v0.2.0" || echo "MISMATCH"
```

Expected: `Cargo.toml version: 0.2.0` and `MATCHES v0.2.0`.

- [ ] **Step 4: Full build + tests green at the new version**

```bash
cargo test --release --locked 2>&1 | grep "test result"
```

Expected: the lib suite shows `122 passed` and every `test result:` line says `ok`. (Windows: if "Access is denied" removing the exe, run `Get-Process cleanupstorages -ErrorAction SilentlyContinue | Stop-Process -Force` first.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.2.0

First release cut via the new release workflow. The guard job checks the
tag matches this version.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `release.yml` workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `Cargo.toml` version (Task 2), `CHANGELOG.md` `## [0.2.0]` section (Task 1).
- Produces: on a `v*` tag push, a draft GitHub Release with two archives + `SHA256SUMS`.

**Context:** The existing `ci.yml` uses `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2` — reuse the same actions for consistency. GitHub-hosted `windows-latest` has `7z` on PATH; `macos-latest` is arm64. `GITHUB_REF_NAME` on a tag push is the tag (e.g. `v0.2.0`). The default `GITHUB_TOKEN` with `contents: write` can create releases.

- [ ] **Step 1: Create the workflow file**

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags: ['v*']

# Read-only by default; only the release job may write.
permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  guard:
    name: guard (version, changelog, tests)
    runs-on: ubuntu-latest
    outputs:
      version: ${{ steps.derive.outputs.version }}
    steps:
      - uses: actions/checkout@v4

      - name: Derive version and check it matches Cargo.toml
        id: derive
        shell: bash
        run: |
          tag="${GITHUB_REF_NAME}"
          version="${tag#v}"
          echo "version=$version" >> "$GITHUB_OUTPUT"
          cargo_version="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
          echo "tag=$tag version=$version cargo=$cargo_version"
          if [ "$version" != "$cargo_version" ]; then
            echo "::error::tag $tag implies version '$version' but Cargo.toml is '$cargo_version'"
            exit 1
          fi

      - name: Check CHANGELOG has a section for this version
        shell: bash
        run: |
          version="${{ steps.derive.outputs.version }}"
          if ! grep -qE "^## \[${version}\]" CHANGELOG.md; then
            echo "::error::CHANGELOG.md has no '## [${version}]' section"
            exit 1
          fi

      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Tests
        run: cargo test --release --locked

  build:
    name: build (${{ matrix.target }})
    needs: guard
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: windows-latest
            target: x86_64-pc-windows-msvc
          - os: macos-latest
            target: aarch64-apple-darwin
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --release --locked

      - name: Package (Windows)
        if: matrix.os == 'windows-latest'
        shell: bash
        run: |
          version="${{ needs.guard.outputs.version }}"
          name="cleanupstorages-${version}-${{ matrix.target }}"
          mkdir -p "$name/assets"
          cp target/release/cleanupstorages.exe "$name/"
          cp README.md LICENSE "$name/"
          cp assets/LICENSES.md "$name/assets/"
          7z a "${name}.zip" "$name" >/dev/null

      - name: Package (macOS)
        if: matrix.os == 'macos-latest'
        shell: bash
        run: |
          version="${{ needs.guard.outputs.version }}"
          name="cleanupstorages-${version}-${{ matrix.target }}"
          mkdir -p "$name/assets"
          cp target/release/cleanupstorages "$name/"
          cp README.md LICENSE "$name/"
          cp assets/LICENSES.md "$name/assets/"
          tar czf "${name}.tar.gz" "$name"

      - uses: actions/upload-artifact@v4
        with:
          name: dist-${{ matrix.target }}
          path: |
            *.zip
            *.tar.gz
          if-no-files-found: error

  release:
    name: draft release
    needs: [guard, build]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - name: Checksums
        shell: bash
        run: |
          cd dist
          sha256sum * > SHA256SUMS
          echo "----- SHA256SUMS -----"
          cat SHA256SUMS

      - name: Build release notes from CHANGELOG + unsigned-binary footer
        shell: bash
        run: |
          version="${{ needs.guard.outputs.version }}"
          # index()/substr() instead of a dynamic regex: escaping [ and ] inside an awk regex string
          # is implementation-dependent (gawk warns and mis-parses "\[" as a bracket expression,
          # yielding EMPTY output). Literal string matching is robust across gawk/mawk.
          awk -v ver="$version" '
            index($0, "## [" ver "]") == 1 {grab=1; next}
            grab && (index($0, "## [") == 1 || substr($0,1,1) == "[") {grab=0}
            grab {print}
          ' CHANGELOG.md > notes.md
          if [ ! -s notes.md ]; then
            echo "::error::extracted release notes are empty for $version"
            exit 1
          fi
          cat >> notes.md <<'FOOTER'

          ---

          ### Downloads are unsigned

          These binaries are not code-signed, so your OS warns on first run.

          - **macOS (Apple Silicon):** after extracting, clear the quarantine flag —
            `xattr -d com.apple.quarantine ./cleanupstorages` — or right-click the binary → Open, then confirm.
          - **Windows:** if SmartScreen warns, click **More info** → **Run anyway**.
          - **Verify your download** against `SHA256SUMS`:
            `sha256sum -c SHA256SUMS` (macOS) or `Get-FileHash <file>` (PowerShell).
          FOOTER

      - name: Create draft release
        env:
          GH_TOKEN: ${{ github.token }}
        shell: bash
        run: |
          version="${{ needs.guard.outputs.version }}"
          tag="${GITHUB_REF_NAME}"
          # Idempotent re-runs: replace an existing *draft* for this tag; never touch a published one.
          if [ "$(gh release view "$tag" --json isDraft --jq .isDraft 2>/dev/null)" = "true" ]; then
            gh release delete "$tag" --yes
          fi
          gh release create "$tag" \
            --draft \
            --title "v${version}" \
            --notes-file notes.md \
            dist/*.zip dist/*.tar.gz dist/SHA256SUMS
```

- [ ] **Step 2: Verify the YAML parses**

```bash
python -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('yaml ok')"
```

Expected: `yaml ok`. (If python/yaml is unavailable, note it and rely on Step 3.)

- [ ] **Step 3: Dry-run the guard + notes logic locally against the real files**

Run the exact shell the workflow will run, with `GITHUB_REF_NAME` faked to `v0.2.0`:

```bash
GITHUB_REF_NAME=v0.2.0
version="${GITHUB_REF_NAME#v}"
cargo_version="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
echo "version=$version cargo=$cargo_version"
test "$version" = "$cargo_version" && echo "GUARD version: PASS" || echo "GUARD version: FAIL"
grep -qE "^## \[${version}\]" CHANGELOG.md && echo "GUARD changelog: PASS" || echo "GUARD changelog: FAIL"
awk -v ver="$version" 'index($0,"## ["ver"]")==1{grab=1;next} grab&&(index($0,"## [")==1||substr($0,1,1)=="["){grab=0} grab{print}' CHANGELOG.md > /tmp/notes.md
test -s /tmp/notes.md && echo "NOTES non-empty: PASS" || echo "NOTES: FAIL"
echo "----- extracted notes -----"; cat /tmp/notes.md
```

Expected: `GUARD version: PASS`, `GUARD changelog: PASS`, `NOTES non-empty: PASS`, and the printed notes are the `[0.2.0]` body with NO `## [` header and NO `[0.2.0]:` link lines.

- [ ] **Step 4: Dry-run the negative guards (must both FAIL the checks)**

```bash
# wrong version
v=9.9.9; grep -qE "^## \[${v}\]" CHANGELOG.md && echo "changelog 9.9.9: unexpectedly PASS" || echo "changelog 9.9.9: correctly FAIL"
# version mismatch simulation
test "1.0.0" = "$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')" && echo "mismatch: unexpectedly PASS" || echo "mismatch: correctly FAIL"
```

Expected: both print `correctly FAIL`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release workflow — tag v* -> draft release with binaries

guard (tag==Cargo.toml, CHANGELOG section, tests) -> build Windows +
macOS-arm64 archives (incl. LICENSE + font notices) with SHA256SUMS ->
draft GitHub Release, notes from CHANGELOG + unsigned-binary footer.
Draft-only: a human publishes.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Docs — install route, SDLC status, release recipe

**Files:**
- Modify: `README.md` (Quick install section)
- Modify: `docs/ai-sdlc.md` ("What's next" section)
- Modify: `CONTRIBUTING.md` (add "Cutting a release")

**Interfaces:**
- Consumes: the release the workflow produces (naming, `SHA256SUMS`).

- [ ] **Step 1: Add a "Download a release" route to `README.md`**

In `README.md`, the `## Quick install` section currently starts with `**From source**`. Insert this ABOVE the "From source" block, right after the `## Quick install` heading:

```markdown
**Download a binary** (no Rust needed) — grab the archive for your OS from the
[Releases page](https://github.com/justPrototypeGit/CleanUpStorages/releases):

- **Windows:** `cleanupstorages-<version>-x86_64-pc-windows-msvc.zip`
- **macOS (Apple Silicon):** `cleanupstorages-<version>-aarch64-apple-darwin.tar.gz`

Unzip it and run `cleanupstorages`. The binaries are unsigned, so your OS warns on first run — the
release notes explain how to get past Gatekeeper / SmartScreen, and every release ships a
`SHA256SUMS` file you can verify against (`sha256sum -c SHA256SUMS` on macOS, `Get-FileHash` in
PowerShell).

```

Leave the existing "From source" block and everything below it unchanged.

- [ ] **Step 2: Update "What's next" in `docs/ai-sdlc.md`**

Replace the `## What's next` section body (currently: *"…there is no release automation yet. Closing that gap … is the next phase."*) with:

```markdown
## What's next

The release end of the loop is now automated: pushing a version tag builds the Windows and macOS
binaries, checksums them, and opens a **draft** GitHub Release with notes drawn from the changelog —
a human reviews and publishes. So the loop reaches an actual downloadable artifact, with the human on
the gate rather than the mechanics.

What's still hand-driven is the *front* of the loop: turning an incoming issue into a spec and a plan.
Automating that — safely, on a public repo where anyone can open an issue — is the next phase.
```

- [ ] **Step 3: Add "Cutting a release" to `CONTRIBUTING.md`**

Append to `CONTRIBUTING.md`:

```markdown
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
```

- [ ] **Step 4: Verify links and headings resolve**

```bash
grep -q "Download a binary" README.md && echo "README: download route added"
grep -q "release end of the loop is now automated" docs/ai-sdlc.md && echo "ai-sdlc: updated"
grep -q "## Cutting a release" CONTRIBUTING.md && echo "CONTRIBUTING: recipe added"
grep -c '```' README.md CONTRIBUTING.md docs/ai-sdlc.md | grep -v ':0' || true
```

Expected: the three confirmation lines print, and each file's fence count (the `grep -c` lines) is even.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/ai-sdlc.md CONTRIBUTING.md
git commit -m "docs: document binary downloads and the release recipe

README gains a 'Download a binary' route; ai-sdlc's 'what's next' now
reflects that the release half is automated (front-of-loop issue->plan is
the remaining gap); CONTRIBUTING gains a 'Cutting a release' checklist.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Cut v0.2.0 and verify the draft (human-gated)

**Files:** none — this task merges, tags, and inspects a live run.

**Interfaces:**
- Consumes: everything above, merged to `main`.
- Produces: a **draft** `v0.2.0` release for a human to publish.

**Context:** The workflow triggers on a tag push, and the tag must sit on a commit that already contains `release.yml` — so the branch must be merged to `main` first, then `main` tagged. Publishing is the human gate; do not publish on the user's behalf without explicit confirmation.

- [ ] **Step 1: Final local gate on the feature branch**

```bash
cargo test --release --locked 2>&1 | grep -c "test result: ok"
cargo clippy --all-targets --locked -- -D warnings && echo "clippy clean"
cargo fmt --check && echo "fmt clean"
```

Expected: a non-zero count of `ok` result lines, `clippy clean`, `fmt clean`.

- [ ] **Step 2: Merge the branch to `main`**

`git merge -F -` (stdin) is not supported and process substitution is fragile on Windows git-bash —
write the message to a temp file first:

```bash
git checkout main
cat > /tmp/mergemsg.txt <<'EOF'
Merge: release automation (C1)

Tag v* -> draft GitHub Release with Windows + macOS-arm64 binaries.

Co-Authored-By: justPrototypeGit <217975680+justPrototypeGit@users.noreply.github.com>
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
git merge --no-ff <feature-branch> -F /tmp/mergemsg.txt
git push origin main
```

(Replace `<feature-branch>` with the branch this plan was executed on.)

Wait for the `CI` workflow on `main` to pass (`gh run list --workflow=ci.yml --limit 1`), so the tagged commit is known-green.

- [ ] **Step 3: Tag and push (HUMAN-CONFIRMED)**

Confirm with the user that `v0.2.0` should be cut. Then:

```bash
git tag v0.2.0
git push origin v0.2.0
```

- [ ] **Step 4: Watch the release run**

```bash
gh run list --workflow=release.yml --limit 1
# then, using the run id:
gh run watch <run-id> --exit-status && echo "release run: SUCCESS"
```

Expected: `guard`, both `build` jobs, and `release` all succeed. If `guard` fails, the version/changelog disagree — fix and re-tag (delete the tag first: `git push origin :refs/tags/v0.2.0 && git tag -d v0.2.0`).

- [ ] **Step 5: Inspect the draft**

```bash
gh release view v0.2.0 --json isDraft,name,assets --jq '"draft: \(.isDraft)", "title: \(.name)", (.assets[] | "asset: \(.name) (\(.size) bytes)")'
```

Expected: `draft: true`, `title: v0.2.0`, and three assets — the Windows `.zip`, the macOS `.tar.gz`, and `SHA256SUMS`. Also `gh release view v0.2.0` and read the notes: they must be the `[0.2.0]` changelog body followed by the unsigned-binary footer, with no stray `## [` header or link-reference lines.

- [ ] **Step 6: Publish (HUMAN decision)**

Present the draft (URL from `gh release view v0.2.0 --web`) to the user. Only on their explicit go-ahead:

```bash
gh release edit v0.2.0 --draft=false --latest
```

Then verify it's public and marked latest:

```bash
gh release view v0.2.0 --json isDraft,isLatest --jq '"draft: \(.isDraft)", "latest: \(.isLatest)"'
```

Expected: `draft: false`, `latest: true`. Do NOT run this step without the user's confirmation.

---

## Notes for later (not this plan)

- **C2** (issue → spec → plan automation) — separate spec; needs a security design because the repo is public.
- **Signing/notarization** — remove the "unsigned" footer once real Apple + Windows certs exist.
- **git-cliff / auto-changelog** — if hand-writing the changelog ever becomes the bottleneck.

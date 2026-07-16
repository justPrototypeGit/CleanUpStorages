# Publish + Collaboration-Ready Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take CleanUpStorages from a private, never-pushed repo to a public GitHub repo a stranger can clone, build with no extra flags, understand, and contribute to — with CI green on the first push and every vendored font's licence obligation satisfied.

**Architecture:** Six local tasks land in dependency order — code hygiene first (rustfmt, clippy) so CI can be green the moment it exists, then licensing, CI, collaboration scaffolding, and docs. A seventh, human-gated task creates the remote and pushes. No task depends on network access except the licence-text fetches (Task 3) and the push (Task 7).

**Tech Stack:** Rust 1.88 / cargo, GitHub Actions, rustfmt, clippy, existing PowerShell screenshot loop (`UI/shoot.ps1`).

**Spec:** [docs/superpowers/specs/2026-07-16-publish-and-collaboration-design.md](../specs/2026-07-16-publish-and-collaboration-design.md)

## Global Constraints

- Licence: **AGPL-3.0-only** for project code. Vendored fonts keep their own licences and are redistributed **unmodified**.
- Fonts stay **vendored** in `assets/`. Do not add submodules. Do not subset (OFL Reserved Font Name applies to modified fonts).
- The build must succeed from a plain `git clone` + `cargo build --release` with **no extra flags**.
- Self-contained property is non-negotiable: no `http://` / `https://` references in served UI. Existing tests assert this.
- The 122-test suite (`cargo test`) must pass at the end of every task.
- Commits follow Conventional Commits (see CONTRIBUTING.md) and end with the `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` trailer.
- GitHub owner assumed to be **`justPrototypeGit`**; Task 7 verifies before it is baked in anywhere user-visible.
- Do **not** create the GitHub repo or push without explicit user confirmation (Task 7).

---

### Task 1: Apply rustfmt and make formatting enforceable

**Files:**
- Modify: every `src/**/*.rs` and `tests/**/*.rs` (mechanical reformat)
- Verify: `cargo test`

**Interfaces:**
- Consumes: nothing.
- Produces: a tree where `cargo fmt --check` exits 0. Task 4's `fmt` job depends on this.

**Context:** Baseline measured on this tree: `cargo fmt --check` reports **398 diff hunks**. rustfmt is semantics-preserving and does **not** reformat the contents of raw string literals, so the CSS/JS inside `web_ui.rs` (`r##"..."##`) is untouched. The 122 tests are the safety net.

- [ ] **Step 1: Record the pre-state so the diff is explainable**

```bash
cargo fmt --check 2>&1 | grep -c "^Diff in"
```

Expected: a non-zero count (≈398).

- [ ] **Step 2: Confirm tests are green before touching anything**

```bash
cargo test --release
```

Expected: `test result: ok. 122 passed` for the lib suite, and all integration suites ok.

- [ ] **Step 3: Apply rustfmt**

```bash
cargo fmt
```

Expected: no output, exit 0.

- [ ] **Step 4: Verify the gate now passes**

```bash
cargo fmt --check
```

Expected: no output, exit 0.

- [ ] **Step 5: Verify rustfmt changed no behaviour**

```bash
cargo test --release
```

Expected: `test result: ok. 122 passed` — identical counts to Step 2. If any test fails, STOP: rustfmt is semantics-preserving, so a failure means something else is wrong.

- [ ] **Step 6: Sanity-check the UI is still self-contained (raw strings intact)**

```bash
cargo test --release --lib web:: 2>&1 | grep "test result"
```

Expected: `test result: ok. 39 passed` — these include the `no external http resources` assertions.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "style: apply rustfmt across the codebase

Mechanical reformat only, no behaviour change, so the rustfmt CI gate can
be enforced from here on. rustfmt does not touch raw-string contents, so
the inline CSS/JS in web_ui.rs is unchanged. 122 tests green before and
after.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Fix clippy warnings so `-D warnings` can be enforced

**Files:**
- Modify: `src/catalog/store.rs` (`manual_repeat_n`), `src/scanner.rs` (`too_many_arguments`), `src/repack.rs`, `src/web.rs` (`uninlined_format_args`), plus any other file clippy names.
- Verify: `cargo clippy --all-targets -- -D warnings`

**Interfaces:**
- Consumes: Task 1's formatted tree.
- Produces: a tree where `cargo clippy --all-targets -- -D warnings` exits 0. Task 4's clippy step depends on this.

**Context:** Baseline measured: **13 warnings** across `--all-targets`. They are pre-existing style lints, not bugs. Prefer a real fix; only use a targeted `#[allow(...)]` **with a `reason`** where the fix would genuinely hurt clarity (e.g. `too_many_arguments` on a function whose parameters are all meaningful).

- [ ] **Step 1: Enumerate the exact warnings**

```bash
cargo clippy --all-targets 2>&1 | grep -E "^warning|^  -->"
```

Expected: ~13 warnings with file:line locations. Record them.

- [ ] **Step 2: Fix `manual_repeat_n` in `src/catalog/store.rs`**

The `push_in_clause` helper builds SQL placeholders. Replace the `repeat().take()` form:

```rust
// before
let holders = std::iter::repeat("?").take(values.len()).collect::<Vec<_>>().join(",");
// after
let holders = std::iter::repeat_n("?", values.len()).collect::<Vec<_>>().join(",");
```

If `repeat_n` is not stable on the toolchain in use, use this instead (also clippy-clean):

```rust
let holders = vec!["?"; values.len()].join(",");
```

- [ ] **Step 3: Fix `uninlined_format_args` occurrences**

Inline the variable into the format string wherever clippy points:

```rust
// before
format!("{}: {}", volume_id, e)
// after
format!("{volume_id}: {e}")
```

Apply to every location clippy listed. Do **not** inline where the expression is not a bare identifier (e.g. `format!("{}", x.y())` must stay).

- [ ] **Step 4: Decide `too_many_arguments` in `src/scanner.rs`**

If the function's parameters are cohesive enough to group, extract a struct. If not, add the allow **with a reason** directly above the function:

```rust
#[allow(clippy::too_many_arguments, reason = "each parameter is an independent scan input; \
    grouping them into a struct would add indirection without reducing real complexity")]
```

- [ ] **Step 5: Verify the gate passes**

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: no output, exit 0.

- [ ] **Step 6: Verify no behaviour changed**

```bash
cargo test --release
```

Expected: `test result: ok. 122 passed`.

- [ ] **Step 7: Verify formatting still clean (fixes may have broken it)**

```bash
cargo fmt --check
```

Expected: exit 0. If it fails, run `cargo fmt` and re-run Step 6.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: resolve clippy warnings so CI can gate on -D warnings

Mechanical lint fixes (uninlined format args, manual repeat_n) plus one
justified allow. No behaviour change; 122 tests green.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Licensing, third-party notices, and package metadata

**Files:**
- Create: `LICENSE` (AGPL-3.0-only, full text)
- Create: `assets/LICENSES.md` (third-party font notices)
- Modify: `Cargo.toml` (`[package]` metadata)
- Modify: `.gitignore` (ignore non-shipping local dirs)

**Interfaces:**
- Consumes: nothing.
- Produces: `LICENSE` and `assets/LICENSES.md` at the paths the README (Task 6) links to.

**Context:** This is the blocking legal task. OFL-1.1 permits embedding and does not impose itself on the embedding software; Apache-2.0 is one-way compatible with AGPL-3.0. Both require that the notice ships with the distribution — that is exactly what `assets/LICENSES.md` discharges. **Copyright lines must be copied verbatim from the upstream sources, never written from memory.**

- [ ] **Step 1: Fetch the AGPL-3.0 text**

```bash
curl -sSL https://www.gnu.org/licenses/agpl-3.0.txt -o LICENSE
```

- [ ] **Step 2: Verify the licence file is the real thing**

```bash
head -n 3 LICENSE && wc -l LICENSE
```

Expected: first lines contain `GNU AFFERO GENERAL PUBLIC LICENSE` and `Version 3, 19 November 2007`; ~660 lines. If the file is short or contains HTML, the fetch failed — do not proceed.

- [ ] **Step 3: Fetch the three upstream font licences into a scratch dir**

```bash
mkdir -p /tmp/fontlic
curl -sSL https://raw.githubusercontent.com/rsms/inter/master/LICENSE.txt -o /tmp/fontlic/inter.txt
curl -sSL https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/OFL.txt -o /tmp/fontlic/jetbrains.txt
curl -sSL https://raw.githubusercontent.com/google/material-design-icons/master/LICENSE -o /tmp/fontlic/material.txt
head -n 5 /tmp/fontlic/inter.txt /tmp/fontlic/jetbrains.txt /tmp/fontlic/material.txt
```

Expected: Inter and JetBrains show a `Copyright ...` line followed by SIL Open Font License wording; Material shows `Apache License Version 2.0`. **Record the exact copyright lines** — they go into the next step verbatim. If a URL 404s, find the correct path in that repo rather than inventing a notice.

- [ ] **Step 4: Write `assets/LICENSES.md`**

Use the copyright lines captured in Step 3 in place of the `<verbatim …>` markers, and paste each licence's full text into its section.

```markdown
# Third-party assets

The font files in this directory are **not** covered by the project's AGPL-3.0 licence.
They are redistributed here **unmodified**, each under its own licence, so that
CleanUpStorages can render its UI with no network access at runtime.

If you modify any of these fonts, note that the SIL Open Font License's **Reserved Font
Name** clause forbids the modified font from keeping the original name.

---

## Inter — `InterVariable.woff2`

- Upstream: https://github.com/rsms/inter
- Licence: SIL Open Font License 1.1

<verbatim copyright line from /tmp/fontlic/inter.txt>

<full OFL-1.1 text from /tmp/fontlic/inter.txt>

---

## JetBrains Mono — `JetBrainsMono-Regular.woff2`, `JetBrainsMono-Medium.woff2`

- Upstream: https://github.com/JetBrains/JetBrainsMono
- Licence: SIL Open Font License 1.1

<verbatim copyright line from /tmp/fontlic/jetbrains.txt>

<full OFL-1.1 text from /tmp/fontlic/jetbrains.txt>

---

## Material Symbols Outlined — `MaterialSymbolsOutlined.woff2`

- Upstream: https://github.com/google/material-design-icons
- Licence: Apache License 2.0

<verbatim notice from /tmp/fontlic/material.txt>

<full Apache-2.0 text from /tmp/fontlic/material.txt>
```

- [ ] **Step 5: Verify no placeholder markers survived**

```bash
grep -n "<verbatim\|<full" assets/LICENSES.md
```

Expected: **no output**. Any hit means a section was left unfilled — fix before committing.

- [ ] **Step 6: Update `Cargo.toml` `[package]`**

```toml
[package]
name = "cleanupstorages"
version = "0.1.0"
edition = "2021"
description = "Reliable catalog + deduplication tool for messy external drives"
license = "AGPL-3.0-only"
repository = "https://github.com/justPrototypeGit/CleanUpStorages"
readme = "README.md"
keywords = ["deduplication", "catalog", "backup", "storage", "blake3"]
categories = ["command-line-utilities", "filesystem"]
```

- [x] **Step 7: Add the non-shipping dirs to `.gitignore`** — **ALREADY DONE** (commit `4160558`)

Pulled forward during execution: Task 1's `git add -A` swept the then-untracked `StitchExport/` and
`.user/` into a commit, which had to be undone. The ignore rules landed early so that every later
`git add -A` is safe. Nothing to do here; verify with Step 8.

For reference, the appended rules were:

```gitignore
# Design reference input (Google Stitch export) — not part of the build
StitchExport/
# Local agent/session scratch
.user/
.superpowers/
```

- [ ] **Step 8: Verify the manifest still parses and nothing new is tracked**

```bash
cargo metadata --no-deps --format-version 1 > /dev/null && echo "manifest ok"
git status --porcelain --untracked-files=all | grep -E "StitchExport|\.user/|\.superpowers/" || echo "scratch dirs correctly ignored"
```

Expected: `manifest ok` then `scratch dirs correctly ignored`.

- [ ] **Step 9: Commit**

```bash
git add LICENSE assets/LICENSES.md Cargo.toml .gitignore
git commit -m "docs: AGPL-3.0 licence, third-party font notices, package metadata

Add the project licence (AGPL-3.0-only) and assets/LICENSES.md carrying the
verbatim upstream notices for the vendored fonts: Inter and JetBrains Mono
(SIL OFL-1.1) and Material Symbols (Apache-2.0). The fonts are redistributed
unmodified and keep their own licences; OFL permits embedding and does not
impose itself on the embedding software, and Apache-2.0 is one-way compatible
with AGPL-3.0. Shipping these notices is what makes redistribution lawful.

Also fill in Cargo.toml metadata and ignore local scratch dirs.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: green `cargo fmt --check` (Task 1) and `cargo clippy -- -D warnings` (Task 2).
- Produces: a workflow named `CI` whose badge URL Task 6's README embeds:
  `https://github.com/justPrototypeGit/CleanUpStorages/actions/workflows/ci.yml/badge.svg`

**Context:** Matrix is Windows + macOS per the spec — the two platforms CLAUDE.md promises. Linux is excluded because `rfd` needs GTK dev packages and we don't ship Linux. `cargo fmt --check` needs no compilation, so it runs once on `ubuntu-latest` (cheapest runner) rather than per-OS — this satisfies the spec's "run it once" intent at lower cost.

- [ ] **Step 1: Create `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  test:
    name: test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy

      - uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --release --locked

      - name: Test
        run: cargo test --release --locked

      - name: Clippy
        run: cargo clippy --all-targets --locked -- -D warnings

  fmt:
    name: rustfmt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt

      - name: Check formatting
        run: cargo fmt --check
```

- [ ] **Step 2: Verify the YAML parses**

```bash
python -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
```

Expected: `yaml ok`. (If python/yaml is unavailable, skip — Step 3 is the real check.)

- [ ] **Step 3: Reproduce every CI step locally so the first push is green**

```bash
cargo build --release --locked
cargo test --release --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Expected: all four exit 0. `--locked` additionally proves `Cargo.lock` is committed and current — if it errors with "the lock file needs to be updated", run `cargo build` once and commit the updated lock.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml Cargo.lock
git commit -m "ci: build, test, clippy on Windows + macOS; rustfmt once

Gate every push to main and every PR. Matrix covers the two platforms the
project ships. clippy runs with -D warnings and fmt --check runs once on
Linux (formatting is platform-independent, and it needs no compilation).
All four steps verified locally before this landed, so the first run is green.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Collaboration scaffolding

**Files:**
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Create: `.github/ISSUE_TEMPLATE/feature_request.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `.github/PULL_REQUEST_TEMPLATE.md`
- Create: `CODE_OF_CONDUCT.md`
- Modify: `CONTRIBUTING.md` (append a "Building, testing, CI" section)

**Interfaces:**
- Consumes: the CI job names from Task 4 (`test`, `rustfmt`).
- Produces: templates the README (Task 6) links to.

**Context:** The bug template asks explicitly whether data was at risk — this project's overriding constraint is that nothing is ever lost, so a data-loss report must be triaged differently from a cosmetic bug.

- [ ] **Step 1: Create `.github/ISSUE_TEMPLATE/bug_report.yml`**

```yaml
name: Bug report
description: Something behaved incorrectly
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for reporting. If this involves **actual or potential data loss**, please say so
        clearly below — that is this project's highest-severity category.

  - type: textarea
    id: what-happened
    attributes:
      label: What happened?
      description: What did you expect, and what happened instead?
    validations:
      required: true

  - type: textarea
    id: repro
    attributes:
      label: Steps to reproduce
      placeholder: |
        1. cleanupstorages scan D:\
        2. Open Duplicates
        3. ...
    validations:
      required: true

  - type: dropdown
    id: data-risk
    attributes:
      label: Was any data lost, moved, or put at risk?
      options:
        - "No — cosmetic or read-only issue"
        - "Unsure"
        - "Yes — files were moved/deleted unexpectedly"
    validations:
      required: true

  - type: input
    id: version
    attributes:
      label: Version / commit
      placeholder: "v0.1.0 or a commit SHA"
    validations:
      required: true

  - type: dropdown
    id: os
    attributes:
      label: Operating system
      options:
        - Windows
        - macOS
        - Other
    validations:
      required: true

  - type: textarea
    id: logs
    attributes:
      label: Logs
      description: "Output of the command, ideally with -v. Please redact personal paths."
      render: shell
```

- [ ] **Step 2: Create `.github/ISSUE_TEMPLATE/feature_request.yml`**

```yaml
name: Feature request
description: Suggest an improvement
labels: ["enhancement"]
body:
  - type: markdown
    attributes:
      value: |
        Please describe the **problem** first. Solutions are easier to evaluate against a
        clearly-stated problem, and there may be an option you haven't considered.

  - type: textarea
    id: problem
    attributes:
      label: What problem are you trying to solve?
    validations:
      required: true

  - type: textarea
    id: proposal
    attributes:
      label: What would you like to happen?
    validations:
      required: false

  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives you've considered
    validations:
      required: false
```

- [ ] **Step 3: Create `.github/ISSUE_TEMPLATE/config.yml`**

```yaml
blank_issues_enabled: true
contact_links:
  - name: Design specs and implementation plans
    url: https://github.com/justPrototypeGit/CleanUpStorages/tree/main/docs/superpowers
    about: Every feature has a committed spec and plan — check there before proposing a redesign.
```

- [ ] **Step 4: Create `.github/PULL_REQUEST_TEMPLATE.md`**

```markdown
## What and why

<!-- What does this change, and what problem does it solve? Link an issue if there is one. -->

## Checklist

- [ ] **Reliability:** this change cannot lose or corrupt user data. Deletes remain
      user-initiated (quarantine is a rename; `purge` is the only real delete).
- [ ] Tests added or updated, and `cargo test` passes.
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
- [ ] `cargo fmt --check` passes.
- [ ] If the web UI changed: it is still self-contained (no `http://` / `https://` references).
- [ ] Commits follow Conventional Commits (see CONTRIBUTING.md).

## Notes for the reviewer

<!-- Anything you want a second pair of eyes on. -->
```

- [ ] **Step 5: Fetch the Contributor Covenant 2.1**

```bash
curl -sSL https://www.contributor-covenant.org/version/2/1/code_of_conduct/code_of_conduct.md -o CODE_OF_CONDUCT.md
head -n 3 CODE_OF_CONDUCT.md
```

Expected: begins with the Contributor Covenant Code of Conduct heading. If it returns HTML, the fetch failed — do not commit it.

- [ ] **Step 6: Set the enforcement contact in `CODE_OF_CONDUCT.md`**

The template contains a `[INSERT CONTACT METHOD]` placeholder. Replace it with:

```
justprototypeemail@gmail.com
```

Then verify none remain:

```bash
grep -n "INSERT CONTACT METHOD" CODE_OF_CONDUCT.md || echo "no placeholder left"
```

Expected: `no placeholder left`.

- [ ] **Step 7: Append a "Building, testing, CI" section to `CONTRIBUTING.md`**

```markdown
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
```

- [ ] **Step 8: Verify the issue templates parse as YAML**

```bash
python -c "import yaml; [yaml.safe_load(open(f)) for f in ['.github/ISSUE_TEMPLATE/bug_report.yml','.github/ISSUE_TEMPLATE/feature_request.yml','.github/ISSUE_TEMPLATE/config.yml']]; print('templates ok')"
```

Expected: `templates ok`.

- [ ] **Step 9: Commit**

```bash
git add .github/ISSUE_TEMPLATE .github/PULL_REQUEST_TEMPLATE.md CODE_OF_CONDUCT.md CONTRIBUTING.md
git commit -m "docs: issue/PR templates, code of conduct, contributor build guide

The bug template asks explicitly whether data was at risk, since that is this
project's highest-severity category. The PR checklist makes the reliability
constraint and the self-contained-UI property reviewable rather than tribal.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Screenshots, README, and the AI-SDLC doc

**Files:**
- Create: `docs/screenshots/overview.png`, `duplicates.png`, `browse-dark.png`, `drives.png`
- Create: `README.md`
- Create: `docs/ai-sdlc.md`

**Interfaces:**
- Consumes: `LICENSE` + `assets/LICENSES.md` (Task 3), the CI badge URL (Task 4), templates (Task 5).
- Produces: the repo's landing page. Phase C extends `docs/ai-sdlc.md`.

**Context:** `UI/` is gitignored, so screenshots must be **copied** into a tracked `docs/screenshots/`. Regenerate them first so they match the merged UI.

- [ ] **Step 1: Regenerate screenshots from the current build**

```bash
pwsh -File UI/shoot.ps1 -Width 1600 -Height 950
```

Expected: `done - 12 shots` written to `UI/Screenshots/`.

- [ ] **Step 2: Copy the four chosen shots into a tracked directory**

```bash
mkdir -p docs/screenshots
cp UI/Screenshots/shot_overview_light.png  docs/screenshots/overview.png
cp UI/Screenshots/shot_duplicates_light.png docs/screenshots/duplicates.png
cp UI/Screenshots/shot_browse_dark.png      docs/screenshots/browse-dark.png
cp UI/Screenshots/shot_drives_light.png     docs/screenshots/drives.png
ls -l docs/screenshots
```

Expected: four PNGs present, each non-zero size.

- [ ] **Step 3: Confirm the screenshots contain no personal data**

Open each of the four PNGs and look at them. The sandbox drives are named `DriveA` / `DriveB`, but the Scan page renders **full filesystem paths** (e.g. `C:\Users\<name>\Documents\cleanup-sandbox\DriveA`). None of the four chosen pages should show a home-directory path — verify visually. If any does, re-shoot that page against a sandbox mounted at a neutral path, or crop the path out. Do not commit a screenshot showing a personal path.

- [ ] **Step 4: Write `README.md`**

```markdown
# CleanUpStorages

[![CI](https://github.com/justPrototypeGit/CleanUpStorages/actions/workflows/ci.yml/badge.svg)](https://github.com/justPrototypeGit/CleanUpStorages/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)

Catalog, search and de-duplicate thousands of GB spread across near-full external drives —
**without ever losing a file.**

![Overview](docs/screenshots/overview.png)

## What this is

Years of important, irreplaceable data — personal and academic — scattered across a pile of
external HDDs, most of them nearly full, most of them containing overlapping copies of each
other. You cannot plug them all in at once, and you cannot trust yourself to delete by hand.

CleanUpStorages crawls each drive, hashes every file with BLAKE3, and builds a **persistent
searchable catalog** that keeps working when the drive is unplugged. It then helps you review
duplicates one at a time and remove them — safely.

## Safety model

This is the whole point, so it comes before the feature list:

- **Nothing is ever deleted automatically.** Confirmed duplicates are *moved* to a `_ToDelete`
  folder on the same drive. That's a rename — near-instant, no copying, fully reversible.
- **`purge` is the only real delete**, and only you can trigger it.
- **The catalog lives on your computer, never on the drives**, so it survives a drive dying.
- **Archive repacks** build a verified temp copy and only swap after re-hashing every retained
  entry — the original is preserved in quarantine too.
- The web UI binds to `127.0.0.1` only, is CSRF-guarded, and ships **zero external requests** —
  no CDN, no fonts fetched at runtime, no telemetry. A test asserts this.

## Quick install

**From source** (needs [Rust](https://rustup.rs)):

```bash
git clone https://github.com/justPrototypeGit/CleanUpStorages.git
cd CleanUpStorages
cargo build --release
```

The binary lands at `target/release/cleanupstorages` (`.exe` on Windows). It's a single
self-contained executable — no runtime, no interpreter, no assets to copy.

## Usage

Catalog a drive, then open the UI:

```bash
cleanupstorages scan D:\        # crawl + hash + catalog
cleanupstorages browse          # opens the local web UI on 127.0.0.1
```

Other verbs:

| Command | What it does |
| --- | --- |
| `scan <path> [--force]` | Crawl a drive/folder, hash files, update the catalog |
| `search <query>` | Search the catalog (works for unplugged drives) |
| `status` | Catalog summary |
| `duplicates` | List duplicate groups |
| `quarantine <id>…` | Move duplicates to `_ToDelete` |
| `purge [--all]` | **The only real delete** — empty `_ToDelete` |
| `repack <id>` | Remove a duplicate from inside an archive, safely |
| `forget <volumeId>` | Drop a drive from the catalog (files untouched) |
| `browse` | Local web UI |

Add `-v` for verbose logs; `RUST_LOG` overrides.

![Duplicates review](docs/screenshots/duplicates.png)

The UI has six pages — Overview, Browse (tree view with duplicate highlighting), Duplicates,
Drives, Scan and Console — in light and dark themes.

![Browse](docs/screenshots/browse-dark.png)

## How this was built

Every feature in this repo started as a **design spec**, became an **implementation plan**, and
only then got written — with an AI doing the work and a human reviewing at each gate. Those
specs and plans are committed next to the code.

**→ [docs/ai-sdlc.md](docs/ai-sdlc.md)** — how the loop works, what it's good at, and where it
needed a human.

## Docs

- [docs/ai-sdlc.md](docs/ai-sdlc.md) — the AI-driven development loop
- [docs/superpowers/specs/](docs/superpowers/specs/) — design specs
- [docs/superpowers/plans/](docs/superpowers/plans/) — implementation plans
- [docs/TESTING-GUIDE.md](docs/TESTING-GUIDE.md) — safe end-to-end walkthrough
- [docs/future-ideas.md](docs/future-ideas.md) — deferred ideas
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to build, test and contribute

## Status

Phases 1 (catalog + search) and 2 (deduplicate) are implemented; the web UI is complete.
Phase 3 (reorganize into a clean taxonomy) is deliberately deferred.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Please open an issue before
large changes. By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Licence

[AGPL-3.0-only](LICENSE) © the CleanUpStorages authors.

The vendored fonts in `assets/` are **not** AGPL — they ship unmodified under their own
licences (Inter and JetBrains Mono under SIL OFL-1.1, Material Symbols under Apache-2.0).
See [assets/LICENSES.md](assets/LICENSES.md).
```

- [ ] **Step 5: Write `docs/ai-sdlc.md`**

```markdown
# Building this with AI, human in the loop

CleanUpStorages is an experiment as much as a tool: **can a real, reliability-critical
application be taken from idea to release with AI doing the work and a human holding the
gates?** This document is the honest answer so far.

## The loop

Every feature goes through the same cycle. Nothing is written before the first two steps exist.

```
idea ──▶ SPEC ──▶ PLAN ──▶ IMPLEMENT ──▶ REVIEW ──▶ MERGE
          │        │          │            │          │
        human    human      tests        human      human
        approves approves   must pass   approves   approves
```

1. **Spec** (`docs/superpowers/specs/`) — brainstorm the problem, weigh 2–3 approaches, record
   the decision *and the rejected alternatives with reasons*. A human approves it.
2. **Plan** (`docs/superpowers/plans/`) — decompose into bite-sized, independently testable
   tasks with exact files, code and verification commands. A human approves it.
3. **Implement** — task by task, test-first where there's behaviour to test.
4. **Review** — a fresh reviewer reads the diff against the spec.
5. **Merge** — conventional commits, CI green.

## Why the specs are committed

The specs are not ceremony. They're the reason this codebase can be handed to a stranger — or
to a fresh AI session with no memory of the last one. Each spec records **why**, including what
was rejected. That's what stops a decision being re-litigated every time context is lost.

Concrete example from this repo: publishing it needed a decision on vendoring fonts. The
suggestion was git submodules; the analysis rejected them (they break `include_bytes!` on a
normal clone, and pull ~1 GB of upstream history to obtain one 3.8 MB file) and recorded the
reasoning. Without that written down, the same idea resurfaces in three months.

## Where the human is load-bearing

The parts that AI did **not** get right on its own:

- **Taste.** The first UI redo was rejected outright — "it looks like a drawing of a baby
  compared to Stitch". Recovering meant giving the AI *eyes*: a headless-Chrome screenshot
  loop it could run and critique itself against a reference design. Without a human saying
  "this is not good enough", it would have shipped.
- **Judgement about what matters.** An AI will happily implement a bad idea well. Several
  decisions here were the human's: AGPL over BSL, keeping fonts vendored rather than
  submoduled, deferring phase 3 entirely.
- **Noticing what's missing.** "The purge button disappears after I quarantine a file" was a
  real bug found by a human using the tool — a bug that every test suite passed straight
  through, because the tests encoded the same wrong assumption the code did.

## Where it's genuinely strong

- **Reliability discipline.** The overriding constraint — *nothing may ever be lost* — is
  written into the spec, CLAUDE.md and the PR checklist, so it survives context loss.
- **Test coverage as a by-product.** 122 tests exist because the loop makes writing them the
  default, not a chore deferred to later.
- **Archaeology.** Every non-obvious decision has a paper trail.

## What's next

The loop above is real but still human-driven at the edges: issues are triaged by hand and
there is no release automation yet. Closing that gap — issue → spec → plan → implementation →
review → **released binary**, with the human on the gates rather than the mechanics — is the
next phase.

## Read the artefacts

- [Specs](superpowers/specs/) — one per feature, with rejected alternatives
- [Plans](superpowers/plans/) — the task-by-task breakdowns that produced the commits
- [CLAUDE.md](../CLAUDE.md) — the project constitution the AI is held to
```

- [ ] **Step 6: Verify every relative link/image in the README resolves**

README links are relative to the repo root, so check them from there:

```bash
grep -oE '\]\([^)]+\)' README.md | tr -d '](' | tr -d ')' | grep -v '^http' | while read -r p; do
  [ -e "$p" ] && echo "ok      $p" || echo "BROKEN  $p"
done
```

Expected: every line starts `ok`. Any `BROKEN` line is a dead link on the landing page — fix it.

Then the same for `docs/ai-sdlc.md`, whose links are relative to `docs/`:

```bash
cd docs && grep -oE '\]\([^)]+\)' ai-sdlc.md | tr -d '](' | tr -d ')' | grep -v '^http' | while read -r p; do
  [ -e "$p" ] && echo "ok      $p" || echo "BROKEN  $p"
done; cd ..
```

Expected: every line starts `ok`.

- [ ] **Step 7: Commit**

```bash
git add README.md docs/ai-sdlc.md docs/screenshots
git commit -m "docs: README, screenshots, and the AI-SDLC write-up

README is the product page: what it is, the safety model up front, quick
install, the CLI verbs and real screenshots. docs/ai-sdlc.md carries the
process story separately, including where the human was load-bearing and
where the AI got it wrong.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: Publish (human-gated)

**Files:** none — this task only touches git remotes.

**Interfaces:**
- Consumes: everything above.
- Produces: a public `origin` and a pushed `main`.

**Context:** `gh` CLI is **not installed** on this machine. This task is **irreversible in effect** — it makes the code, the full commit history, and the commit author email public. **Do not run any step here without explicit user confirmation.**

- [ ] **Step 1: Confirm with the user before doing anything**

Confirm all four:
1. The GitHub owner/username (the plan assumes `justPrototypeGit` — if wrong, every URL written in Tasks 3–6 must be corrected first).
2. Repo name `CleanUpStorages`.
3. Public visibility.
4. That the commit author email `justprototypeemail@gmail.com` becoming permanently public is accepted.

**Do not proceed until all four are confirmed.**

- [ ] **Step 2: Verify the tree is clean and green one final time**

```bash
git status --porcelain
cargo test --release 2>&1 | grep "test result"
cargo clippy --all-targets -- -D warnings && cargo fmt --check && echo "gates green"
```

Expected: no uncommitted changes, `122 passed`, `gates green`.

- [ ] **Step 3: Verify nothing private is about to be published**

```bash
git ls-files | xargs grep -lE "C:\\\\Users\\\\|/Users/[a-z]+/|@gmail\.com" 2>/dev/null || echo "no personal paths in tracked files"
git ls-files | grep -iE "\.db$|catalog|\.env|secret|token" || echo "no data/secret files tracked"
```

Expected: `no personal paths in tracked files` (the known-good exception is the `ProjectDirs::from("dev", "justPrototype", …)` namespace, which is intentional) and `no data/secret files tracked`.

- [ ] **Step 4: Create the empty repo**

Either install `gh` (`winget install --id GitHub.cli`) and run:

```bash
gh repo create CleanUpStorages --public --source=. --remote=origin --description "Reliable catalog + deduplication tool for messy external drives"
```

…or the user creates an empty **public** repo named `CleanUpStorages` at https://github.com/new (**no** README/licence/gitignore — the repo already has them), then:

```bash
git remote add origin https://github.com/justPrototypeGit/CleanUpStorages.git
```

- [ ] **Step 5: Verify the remote**

```bash
git remote -v
```

Expected: `origin` pointing at the correct owner/repo, fetch and push.

- [ ] **Step 6: Push**

```bash
git push -u origin main
```

Expected: branch `main` set up to track `origin/main`.

- [ ] **Step 7: Verify CI went green**

Open `https://github.com/justPrototypeGit/CleanUpStorages/actions` and confirm the `CI` run
passes on `test (windows-latest)`, `test (macos-latest)` and `rustfmt`.

If anything is red, fix it **before** telling anyone the repo exists — a red badge on the
landing page is worse than no badge. Note that Task 4 Step 3 ran all four gates locally, so a
failure here is most likely a caching/toolchain difference, not a real regression.

- [ ] **Step 8: Confirm the landing page renders**

Open `https://github.com/justPrototypeGit/CleanUpStorages` and check: screenshots load, CI badge
is green, licence shows as **AGPL-3.0**, and the "About" description is set.

---

## Notes for phase C (not this plan)

Deliberately out of scope here, and the natural next spec:

- Release automation: tag → build Windows + macOS binaries → GitHub Release with checksums.
- Issue → spec → plan automation, and a Claude GitHub Action for review.
- Font subsetting (~4.3 MB → ~200–500 KB) — a good first "AI-driven SDLC" demo issue, but note
  it is a **font modification**, so OFL's Reserved Font Name clause applies and the subset
  cannot keep the name "Inter"/"JetBrains Mono".

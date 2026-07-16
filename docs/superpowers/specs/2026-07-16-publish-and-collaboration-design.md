# Publish + collaboration-ready — design

**Status:** approved
**Date:** 2026-07-16
**Scope:** sub-project A+B of the "publish the project" roadmap (A = publishable, B = collaboration-ready).
**Deferred to C:** release automation and issue→spec→plan automation (the "to release" half of the
AI-driven SDLC). **Deferred to D:** the LinkedIn post.

## Goal

Take CleanUpStorages from a private, never-pushed local repo to a **public GitHub repo that a
stranger can land on, understand, build, and contribute to** — without breaking the project's
existing promises (self-contained binary, reliability-first) or its licensing obligations.

Two audiences, both served by the same repo:

1. Someone who wants the **tool** (what is it, is it safe, how do I run it).
2. Someone evaluating the **process** — the project was built end-to-end with AI, human in the loop.
   This is the differentiator, and it already has real artefacts: every feature has a design spec and
   an implementation plan committed under `docs/superpowers/`, alongside the commits that delivered it.

## Decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Visibility | Public | Showcase + collaboration. |
| Project licence | **AGPL-3.0-only** | OSI open source (GitHub renders a real badge), no CLA needed, strong copyleft so it can't be closed or SaaS-ified. BSL was rejected: it is source-available (not open source), normally needs a CLA, and protects against a cloud-reseller threat that does not exist for a `127.0.0.1` local tool. Sole copyright holder, so relicensing stays possible. |
| Vendored fonts | **Keep vendored, unmodified** | Vendoring *is* the self-contained design. Submodules were rejected (see below). Subsetting deferred (see below). |
| CI | **windows-latest + macos-latest**: build, test, clippy, fmt | Matches the platforms CLAUDE.md promises. Linux omitted: `rfd` needs GTK dev packages and we don't ship Linux. |
| Formatting | **Apply rustfmt once, then gate** | rustfmt is table stakes in Rust; without the gate every contributor's format-on-save produces noise. |
| README | **Product page + quick install**, AI story in its own doc | A landing visitor wants to know what it is; the process story gets `docs/ai-sdlc.md`, which C expands. |

### Why not submodules for `assets/`

Considered and rejected — it would break the two things we care about most:

- **It breaks the build on a normal clone.** `web.rs` uses `include_bytes!("../assets/…woff2")` at
  compile time. Without `--recurse-submodules` the files are absent and `cargo build` fails with a
  confusing missing-file error. That is the opposite of contributor-friendly.
- **It makes cloning dramatically worse.** Git cannot submodule a *file*, only a repo. Pinning
  `google/material-design-icons` (~1 GB) to obtain one 3.8 MB `.woff2` trades 4.3 MB of assets for
  a gigabyte of upstream history.
- **It contradicts the design.** CLAUDE.md: *"single static binary … no CDN/fonts/build step."*
  Submodules add a network dependency to the build.
- **It does not simplify licensing.** OFL still requires shipping the notice, which would then live
  in a possibly-unchecked-out submodule.

### Why not subset the fonts (yet)

`MaterialSymbolsOutlined.woff2` is 3,869 KB of the 4,394 KB total and carries ~3,700 icons; the app
uses **32**. Subsetting would cut assets to roughly 200–500 KB. Deferred, not rejected, because:

- 4.3 MB is not a problem today (`.git` is already ~47 MB); this is an optimisation, not a rescue.
- Subsetting is **modification**, which triggers OFL's **Reserved Font Name** clause — a subset
  font may not keep the name "Inter" / "JetBrains Mono". That needs deliberate handling, not a
  drive-by change.
- It must be verified glyph-by-glyph (ligature `liga` feature must survive) via the screenshot loop.

Good candidate for a first AI-driven-SDLC demo issue in phase C.

## Deliverables

### 1. Licensing & compliance (blocking — this is what makes redistribution legal)

- `LICENSE` — AGPL-3.0-only, full text.
- `assets/LICENSES.md` — attribution + full licence text for each vendored font:
  - Inter — SIL OFL-1.1
  - JetBrains Mono — SIL OFL-1.1
  - Material Symbols Outlined — Apache-2.0

  Copyright lines are to be copied **verbatim from the upstream `LICENSE` files**, not from memory.
  The file must state plainly that these files remain under their own licences and are **not**
  covered by the project's AGPL-3.0, and that they are redistributed **unmodified**.
- `Cargo.toml` `[package]`: add `license = "AGPL-3.0-only"`, `repository`, `readme`, `keywords`,
  `categories`.

Rationale, recorded so it is not re-litigated: OFL-1.1 explicitly permits embedding and does not
impose itself on the embedding software; Apache-2.0 is one-way compatible with AGPL-3.0. Both
require that notices ship with the binary/repo — `assets/LICENSES.md` discharges that.

### 2. Docs

- **`README.md`** — the product page:
  - one-line description + CI badge + licence badge
  - screenshots (light + dark)
  - what it is / why it exists (thousands of GB of irreplaceable data across near-full HDDs)
  - **Quick install** — download a binary, or `cargo build --release`
  - Usage — CLI verbs (`scan`, `search`, `status`, `duplicates`, `quarantine`, `purge`, `repack`,
    `forget`, `browse`) and the local web UI
  - **Safety model** — the overriding constraint: nothing is ever lost; duplicates move to a
    same-drive `_ToDelete` quarantine (a rename); `purge` is the only real delete and is
    user-initiated
  - Docs map + prominent link to `docs/ai-sdlc.md`
  - Contributing, licence, third-party notices
- **`docs/ai-sdlc.md`** — the process story: spec → plan → implement → review → merge, the
  human-in-the-loop gates, an index linking each spec to its plan and shipped commits, and an honest
  "what worked / what didn't". C expands this into the automated loop.
- **`docs/screenshots/`** — 4 tracked PNGs (overview, duplicates, browse dark, drives), sourced from
  the existing `UI/Screenshots` output.

### 3. Green gates (must land before CI, or CI is red on day one)

Verified current state: `cargo fmt --check` reports **398 diff hunks**; `cargo clippy --all-targets`
reports **13 warnings**. Both gates would fail today.

- `style: apply rustfmt` — one clearly-labelled commit. rustfmt is semantics-preserving and does not
  touch the contents of the raw-string CSS/JS in `web_ui.rs`; the 122 tests are the safety net.
- `fix: clippy` — resolve all 13 warnings (`uninlined_format_args`, `manual_repeat_n`,
  `too_many_arguments`). Prefer real fixes; use a targeted `#[allow]` with a reason only where a
  fix would hurt clarity.

### 4. CI — `.github/workflows/ci.yml`

- Triggers: push to `main`, and pull requests.
- Matrix: `windows-latest`, `macos-latest`.
- Steps: checkout → Rust stable (+ `clippy`, `rustfmt`) → cargo cache → `cargo build` →
  `cargo test` → `cargo clippy --all-targets -- -D warnings`.
- `cargo fmt --check` runs **once** (Windows only) — formatting is platform-independent.
- Badge in the README.

### 5. Collaboration scaffolding — `.github/`

- `ISSUE_TEMPLATE/bug_report.yml` — repro, expected/actual, OS, drive layout, **whether any data
  was at risk**.
- `ISSUE_TEMPLATE/feature_request.yml` — problem first, not solution.
- `ISSUE_TEMPLATE/config.yml`.
- `PULL_REQUEST_TEMPLATE.md` — checklist mirroring CONTRIBUTING, and explicitly: *"does this change
  preserve the reliability constraint (nothing lost/corrupted)?"* and *"tests added/updated?"*.
- `CODE_OF_CONDUCT.md` — Contributor Covenant 2.1.
- Extend `CONTRIBUTING.md` — build/test commands, the CI gates, where specs and plans live, and how
  the AI-driven workflow fits.

### 6. Hygiene

- `.gitignore`: add `StitchExport/` (design reference input), `.user/`, `.superpowers/`.

### 7. Publish

- Create the public GitHub repo **`CleanUpStorages`**, add the remote, push `main`.
- `gh` CLI is **not installed**. Either install it, or the user creates an empty repo on github.com
  and we add the remote and push. No auto-created repo without explicit confirmation.

## Non-goals

- Release automation / binary artefacts (phase C).
- Issue→spec→plan automation, Claude GitHub Action (phase C).
- The LinkedIn post (phase D).
- Publishing to crates.io.
- Font subsetting (deferred, see above).
- Rewriting commit-author history. The author email `justprototypeemail@gmail.com` is a deliberate
  project alias matching the `justPrototype` handle and will become public. Flagged and accepted.

## Success criteria

1. A stranger can clone and `cargo build --release` with **no extra flags** and no missing files.
2. CI is **green on the first push**, on both Windows and macOS.
3. Every vendored font ships its required notice; the repo states what is AGPL and what is not.
4. `git ls-files` contains no personal paths, secrets, or catalog data. (Verified: the only match for
   personal-looking strings is `ProjectDirs::from("dev", "justPrototype", "CleanUpStorages")`, which
   is the intentional app-data namespace.)
5. The README answers "what is it / is it safe / how do I run it" above the fold, and links to the
   AI-SDLC story.
6. The 122-test suite still passes after the rustfmt and clippy commits.

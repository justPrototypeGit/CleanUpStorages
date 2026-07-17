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

The release end of the loop is now automated: pushing a version tag builds the Windows and macOS
binaries, checksums them, and opens a **draft** GitHub Release with notes drawn from the changelog —
a human reviews and publishes. So the loop reaches an actual downloadable artifact, with the human on
the gate rather than the mechanics.

What's still hand-driven is the *front* of the loop: turning an incoming issue into a spec and a plan.
Automating that — safely, on a public repo where anyone can open an issue — is the next phase.

## Read the artefacts

- [Specs](superpowers/specs/) — one per feature, with rejected alternatives
- [Plans](superpowers/plans/) — the task-by-task breakdowns that produced the commits
- [CLAUDE.md](../CLAUDE.md) — the project constitution the AI is held to

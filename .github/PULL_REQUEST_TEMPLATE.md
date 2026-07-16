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

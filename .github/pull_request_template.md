## What and why

<!-- What does this pull request change, and which problem does it solve? Link the issue, if there is one. -->

## Checks

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
      and `sh tests/workspace/layout-test.sh` pass.
- [ ] New or changed behavior has a test.
- [ ] If a kit changed: its test under `tests/kit-hooks/` passes on Linux.
- [ ] If the behavior that users see changed: `README.md` and the pages in `docs/` are updated.

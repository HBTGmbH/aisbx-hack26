# Agent notes for aisbx

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the checks every change must pass, and [docs/kits.md](docs/kits.md) for
how kits are written. These notes add what is easy to miss.

## Which kits belong here

`kits/` holds only kits that are useful in many projects: toolchains, caches, token savers, agent wiring. A kit for
one project, team or company (an internal CLI, a pinned database client, an MCP server for one service) is a user
kit. It lives in the user's own folder, which aisbx reads from `AISBX_KITS_PATH` or `~/.config/aisbx/kits`. See
[Your own kits](docs/kits.md#your-own-kits). When someone asks for a project-specific kit, write it there (or into a
folder they name), not into `kits/`, and don't touch the docs' kit tables or the catalog tests.

User kits must be mixins and must not reuse a shipped or generated kit's name. `aisbx kits` shows what was loaded
and what was skipped. Tests never read user folders: `kits::user_kit_dirs` returns nothing under the test guard.

## Add a kit

Copy the closest existing kit. A mixin with no network and no host folders (`jdk25`, `rtk`) can be added to a live
sandbox. Network rules or host folders make the kit create-time only (`rust`, `web`).

Touch every one of these, or a test or a doc table goes stale:

- `kits/<name>/`: `<name>.yaml`, `<name>.dockerfile`, `changes.txt`, `files/<name>/` for scripts, and
  `harnesses.txt` if the kit works only with some agents. Set the marker `ENV SBX_KIT_<NAME>=1`.
- `core/src/kits.rs`, `the_shipped_kits_report_why_they_are_create_time_only`: add the kit either to the
  create-time list with its reason, or to the add-able list.
- `README.md`: the addon table.
- `docs/kits.md`: the kit table.
- `docs/addons.md`: a section for the kit, and the add-able list at the top if the kit can be added live.
- `docs/network.md`: the allow-list table. Kits that allow nothing go in the `nothing` row.
- `docs/sandboxes.md`: the table of what `e` (add a kit) does.
- An add-able kit adds a row to the TUI's kit list, which can push rows off screen in `tui/src/ui/mod.rs` tests.

## Test a kit's image

- `sbx kit validate kits/<name>` builds through the sbx daemon. It leaves no image in the local Docker engine.
- To check the image itself, build it with `docker build -t <tag> -f kits/<name>/<name>.dockerfile kits/<name>`, copy
  it onto a base image in a throwaway Dockerfile (`COPY --from=<tag> / /`), and run the kit's hooks there. Delete the
  test images afterwards.
- Usually only `sbx/shell-image` is pulled locally. Say so when you have not tried a kit on the other workload images
  (`docker/sandbox-templates:claude-code-docker`, `sbx/shell-docker-image`, `sbx/antigravity-image`).

## The step primitive

`core/src/step.rs` is `aisbx step`: one sandboxed unit of work per process, for pipelines that must never trust a
sandbox. [docs/step-protocol.md](docs/step-protocol.md) is its contract.

- The lifecycle order is fixed and security-relevant: refuse, create (with every deny at create time), prove, log,
  work, log, remove, report. Never reorder it, never read the workspace for the caller, never skip the removal, and
  never let an exit code of 0 mean the agent succeeded. A removal that fails is exit 125 whatever happened before.
- `tests/step/` holds golden events, specs and reports from real runs. Another repository reads them as contract
  tests. A change to an event, a report field or an exit code updates the fixtures, and an incompatible one bumps
  `PROTOCOL` and the version table in the protocol page. The plant repository pins the protocol version through
  `aisbx step --protocol`, so a silent change breaks it.
- `step` creates through `up_inner` and removes through `rm_inner`, so a sandbox made by `step` is the same sandbox
  `up` makes. Keep it that way: the dashboard then shows and rebuilds it like any other.
- Run `sh tests/step-test.sh` after a change; it needs a signed-in sbx and makes one sandbox.

## Before you commit

Run all four CONTRIBUTING checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` and `sh tests/workspace/layout-test.sh`. Don't stop at `cargo test`.

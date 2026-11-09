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

## Before you commit

Run all four CONTRIBUTING checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` and `sh tests/workspace/layout-test.sh`. Don't stop at `cargo test`.

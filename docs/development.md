# Development

[← Back to the README](../README.md)

For contributors: the repository layout, how to build and test, and the sbx version the dashboard needs.

## Layout

```text
bin/unix/, bin/windows/   # the launchers aisbx and aisbx-ui: rebuild if stale, then run
bin/<platform>/build/     # the built binaries (gitignored)
Cargo.toml, Cargo.lock    # one Cargo workspace: core/, tui/, gui/; builds into target/
core/src/                 # aisbx-core: everything but the drawing, no UI library
core/assets/              # files the dashboard sends into sandboxes
core/tests/docs.rs        # checks every link and anchor in README.md and docs/*.md
tui/src/                  # aisbx, the terminal dashboard (Ratatui)
gui/src/                  # aisbx-ui, the desktop window (eframe)
kits/<name>/              # <name>.yaml, <name>.dockerfile, scripts under files/<topic>/
tests/                    # shell tests: kit-hooks/, repo-skills-test.sh, workspace/layout-test.sh
scripts/package.py        # packs a release archive; its test is scripts/test_package.py
.github/                  # CI and release workflows, Dependabot, issue templates
```

The modules in `core/src/`:

| Area                 | Modules                                                                                  |
|----------------------|------------------------------------------------------------------------------------------|
| Screens and input    | `app`, `actions`, `create`, `find`, `roster`, `insight`, `stats`, `repo_skills_ui`, `input`, `geom` |
| sbx and state        | `sbx` (every sbx call), `state` (`registry.json`), `bindings`, `job`                      |
| Kits and accounts    | `kits`, `tokens`, `litellm`, `models`, `model_fetch`, `model_picks`, `tmpfs`               |
| Sandboxes            | `model`, `folders`, `startdir`, `remote`, `terminal`, `skill_source`, `skill_repos`, `skill_select` |
| Scripts and tests    | `headless`, `testkit`                                                                    |

Turn on the `test-support` feature of `aisbx-core` only in the `[dev-dependencies]` of a frontend.

### Build and test

Run all commands in the repository root:

```console
$ cargo build --workspace
$ cargo test --workspace
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ sh tests/workspace/layout-test.sh
$ sh tests/kit-hooks/claude-workloads-test.sh      # one script per kit topic
$ sh tests/repo-skills-test.sh
```

- The launchers run `cargo build --release -p aisbx` and `cargo build --release -p aisbx-ui`.
- If `cargo fmt --all --check` still fails after `cargo fmt`, run `cargo fmt` again.
- Run `tests/kit-hooks/headroom-test.sh` and `tests/kit-hooks/host-skills-test.sh` on Linux. They fail on macOS.
- `cargo test --workspace -- --ignored --nocapture preview` prints the TUI screens as text.
- CI (`.github/workflows/ci.yml`) runs these checks on every push to `main` and every pull request: lint and the
  kit hook tests on Linux, `cargo test` on Linux and macOS, a build on Windows. Lint also runs
  `python3 -m unittest scripts/test_package.py`. It skips
  `tests/kit-hooks/host-overlay-test.sh`, which needs uid 1000.

## sbx version

The dashboard needs **sbx 0.50.1** or later and refuses an older sbx. The kits, the JSON it reads and the commands it
runs are measured on sbx 0.50. 0.50.1 fixes an issue that prevented sandboxes from starting. 0.50 brings security fixes
and HTTP rules with query parameters, which a rebuild carries over.

The 0.50 security fixes: host Git commands do not run programs that a sandbox set in the `.git` folder of the
workspace. sbx composes a local kit, as every kit here is, only from a build with a matching digest. The host
browser helper for a sign-in runs only from trusted system locations.

### aisbx version

The aisbx version is the sbx version it needs plus a counter: aisbx **0.50.1.0** needs sbx 0.50.1. Count the last
number up for each aisbx release on the same sbx version. Set it back to 0 when aisbx moves to a newer sbx.

Cargo accepts only three numbers, so the root `Cargo.toml` writes 0.50.1.0 as `version = "0.50.1+0"`.
`aisbx --version` and `aisbx-ui --version` print 0.50.1.0. A test in `core/src/lib.rs` fails when the first three
numbers differ from `sbx::REQUIRED_VERSION`.

### Rules for sbx calls

- Put every `sbx` call in `core/src/sbx.rs`.
- Pass `--force` / `-f` on every removal. Without a terminal, a confirmation prompt exits 1.
- Do not rebuild a sandbox whose workspace folder is gone. sbx refuses to create it without a terminal.

### Deliberately not adopted

aisbx does not use these sbx features. Test the reason again before you add one.

- The v3 capabilities `network-policy@2`, capability groups, `kind: set`, `agent-skill@1`, `port@1`,
  `resources@1`, `privileged@1`, `git-identity@1`, `ssh-agent@1` and `long-running@1`.
- `sbx run --model` with `model.providers`, `sbx run --rm`, kit arguments (`--kit-arg`) and `-e` / `--env`.
- `sbx mount`, `host-mount@1` and `sbx create --mount HOST:SANDBOX_PATH`.
- `sbx mcp load` / `unload` (see [MCP servers](sandboxes.md#mcp-servers)) and `sbx setup ssh --auto-create`.
- Cloud sandboxes (`--cloud`).

### On an upgrade

1. Run the live test. It reads every sbx output on this machine and prints the JSON:

   ```console
   $ cargo test --workspace -- --ignored --nocapture live
   ```

   Set `AISBX_PLAN_SANDBOX=<name>` to print the rebuild steps of a real sandbox without running them.

2. Check by hand what no command reports:

   - **Setup-token swap:** store an invalid token for a `claude-token` sandbox and run
     `sbx exec <sandbox> sh -c 'cd /tmp && claude -p hi'`. Expect `401 OAuth access token is invalid.`
   - **Skills mounts**, one sandbox per backend: run `grep skill /proc/mounts`. Expect the store at
     `/home/agent/.local/share/aisbx/skills-store`, `ro` for `readonly`, `rw` for `readwrite`, no mount for `off`.
   - **Install hooks:** `sbx policy log <sandbox> --json` of a new `claude-litellm` sandbox shows no request to
     `api.anthropic.com`.
   - **OpenCode config:** do the `opencode debug config` check under [OpenCode](agents.md#opencode).
   - **Telemetry:** do the `strings` check under [Telemetry](network.md#telemetry-and-non-essential-traffic).

3. Set `sbx::REQUIRED_VERSION` to the new sbx version and the root `Cargo.toml` `version` to `<new>+0`
   ([aisbx version](#aisbx-version)).

## Release

A tag `v<aisbx version>` builds the release. `.github/workflows/release.yml` then publishes one archive per
platform on the GitHub release page.

1. Set the root `Cargo.toml` `version` ([aisbx version](#aisbx-version)) and merge it into `main`.
2. Tag the merge commit and push the tag:

   ```console
   $ python3 scripts/package.py version
   0.50.1.0
   $ git tag v0.50.1.0
   $ git push origin v0.50.1.0
   ```

3. The workflow stops when the tag is not `v` plus the `Cargo.toml` version. Otherwise it makes a draft
   release, builds `aisbx` and `aisbx-ui` for each platform, uploads the archives and publishes the release.

Each archive holds `bin/` with both binaries, `kits/`, `docs/`, `README.md` and `LICENSE`.
`scripts/package.py build --target <triple>` writes it from `target/<triple>/release/` and the files git tracks.
The platforms: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`,
`x86_64-apple-darwin` and `x86_64-pc-windows-msvc`.

To build an archive locally:

```console
$ cargo build --release --locked --target aarch64-apple-darwin -p aisbx -p aisbx-ui
$ python3 scripts/package.py build --target aarch64-apple-darwin
$ python3 -m unittest scripts/test_package.py
```

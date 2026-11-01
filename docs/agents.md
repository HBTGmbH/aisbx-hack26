# Agents

[← Back to the README](../README.md)

What is special about OpenCode, Codex, Pi, the Antigravity CLI and Claude Code's plugin marketplace. For Claude
Code's accounts, see [Accounts and secrets](accounts.md).

## OpenCode

`opencode-litellm` runs **OpenCode 2.x** on a [LiteLLM connection](litellm.md#litellm-connections). The connection's
generated kit writes the OpenCode config before every start.

To see the config sources, run `sbx exec <sandbox> sh -c 'cd /tmp && opencode debug config'`. To see the models the
picker offers, run `sbx exec <sandbox> opencode models`.

### The config files

| File | Written by | Holds |
| --- | --- | --- |
| `~/.config/opencode/opencode-litellm-<name>.json` | the connection's generated kit, before every start | the settings from `core/assets/opencode-litellm.json` and the provider `litellm-<name>` |
| `opencode.json` | the workload's install hook | only the MCP gateway entry, `mcp.servers["mcp-gateway"]` |

`OPENCODE_CONFIG` points at the connection's file, and OpenCode merges it over the global one. **Do not put the
provider in `opencode.json`.**

### Which OpenCode

The kit installs the npm package `@opencode/cli` in the range `OPENCODE_LITELLM_VERSION=2`
(`kits/opencode-litellm/opencode-litellm.dockerfile`). Every 2.x release arrives; a 3.x never arrives by itself. The
image build, the install hook and the dashboard's pre-start update install it.

To bump the major version:

1. Change the variable *and* the fallback in `files/opencode/update.sh`. `sh tests/kit-hooks/opencode-litellm-test.sh`
   checks that they agree.
2. Check `core/assets/opencode-litellm.json` against the new schema.
3. Rebuild.

### The 2.x config

The config uses only 2.x keys. The model for session titles is `agents.title.model`. The config is written from the
2.x docs and schema and is not yet measured in a sandbox.

### The gateway's models

The provider lists the models you left on in the wizard's Models step, with the limits, capabilities and cost the
gateway reports. Your picks set `model` and `agents.title.model`. aisbx rewrites the file before every start, so a new
list or pick reaches the sandbox at its next session.

### Privacy and permissions

- **Sharing is off** (`"share": "disabled"`). Else `/share` uploads the conversation and its data to `opencode.ai`.
- **No update check** (`"update": "disable"`, `OPENCODE_DISABLE_AUTOUPDATE=true`).
- **No telemetry switch exists** in 2.x. `models.dev` stays allowed: it is the model catalog.
- **Only the gateway's models.** A `provider.use` deny policy hides `github-copilot` and the other built-in providers.
- **No permission prompts.** One allow rule approves every prompt, because the sandbox is the boundary. A `deny` rule
  you add after it still holds.
- **The `plan` agent is read-only.** Switch to `build` for edits and bash.

## Codex

`codex-openai` runs Codex on your OpenAI account: the ChatGPT sign-in or an API key.

1. Sign in on the host, once. No other backend needs a command:

   ```console
   $ sbx secret set openai --oauth     # the ChatGPT sign-in, in your browser
   $ sbx secret set openai             # or: an API key
   ```

2. Create the sandbox. Every `codex-openai` sandbox uses that account, also after a rebuild.

- The tokens stay in the host keychain.
- Without the secret the sandbox starts logged out. The wizard's Harness step warns you and prints the command.
- **Do not run `codex login` inside a sandbox.** That sign-in stays in that container.
- The sign-in is not yet measured with this kit.

### What the kit sets

- **Current release.** The install hook and the dashboard's pre-start step run `codex update` (about 15 s).
- **No analytics, no feedback upload, no startup update check.** The kit sets them off in `/etc/codex/config.toml`.
- **Your settings** are in `~/.codex/config.toml`: approval policy, sandbox mode, the MCP gateway, the sign-in mode.
  The MCP gateway works.
- **No GitHub.** The kit allows no GitHub host. To use GitHub, store a `github` secret on the host and open GitHub,
  for example with the `balanced` policy or the `web` addon.

## Pi

`pi-litellm` runs [Pi](https://pi.dev), a minimal open-source (MIT) coding agent, on a
[LiteLLM connection](litellm.md#litellm-connections). Pi has four tools (read, bash, edit, write), no sub-agents and
no approval prompts.

### Gateway only

**Pi uses only the gateway of a LiteLLM connection.** The connection's generated kit (`litellm-<name>-pi`) writes two
files:

| File | Holds |
| --- | --- |
| `~/.pi/agent/models.json` | the OpenAI-compatible provider `litellm-<name>` on `<URL>/v1`, with the connection's placeholder as its key |
| `~/.pi/agent/settings.json` | the name of that provider; `defaultModel` where you picked one |

- Use `/model` to switch the model. The list is the [connection's](litellm.md#the-gateways-model-list), minus the
  models you switched off.
- Pi's `/login` and API-key providers do not work: the kit opens no model provider's host.
- Not yet measured: that the proxy swaps Pi's placeholder for the real key.

### Install

The image installs the latest `@earendil-works/pi-coding-agent`. The install hook and the pre-start update update it
(about 10 s). **Pi needs Node.js 22.19 or later.** The build fails with the reason when Node is missing or too old.

### No phoning home

The image sets `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0` with `enableInstallTelemetry: false`, and `PI_OFFLINE=1`.
Requests to your gateway still work. The image has `fd` and `rg`, so Pi downloads nothing from GitHub.

### What Pi does not do

- **No sbx MCP gateway.** Pi reads its own `~/.pi/agent/mcp.json`, so `--static-mcp` has no effect.
- **No shared skills store.**
- **No Claude Code or OpenCode addons.** `statusline`, `ponytail` and `rtk`'s agent hooks do nothing for Pi.
- **`AGENTS.md`:** not measured that Pi reads the one sbx writes beside the workspace.
- **`/share` uploads a session as a GitHub gist** wherever GitHub is open, for example under the `balanced` policy.

## Antigravity (agy)

`agy` runs Google's Antigravity CLI on one Google account per machine, or on a Gemini API key.

**This is the one exception to "no secret in the sandbox":** the container's token file holds a real Google
`id_token` that names your account.

### The image and the allow-list

The kit uses Docker's community image `docker.io/sbx/antigravity-image` from
[sbx-kits-contrib](https://github.com/docker/sbx-kits-contrib), rebuilt every day, with this repo's own allow-list:

- It denies agy's log sink.
- It allows `lh3.googleusercontent.com`, the account picture: without it the sign-in fails. It allows
  `www.gstatic.com`, agy's plugin marketplace.

### Sign in

**Sign in once, in any agy sandbox:**

1. Start the sandbox: select it and press `↵`, or click **Start**.
2. Follow agy's sign-in. The URL opens in your host browser. If sbx's `browser.enabled` setting is off, open the URL
   that agy prints.

- The host proxy keeps the real tokens on the host. Every agy sandbox then starts signed in (not yet measured).
- It is one Google account per machine.
- **The proxy adds the token on `www.googleapis.com`, a broad Google API host.** Anything in the sandbox can use it
  there, within its scopes.
- The sign-in is adapted from [shelajev/agy-sbx-kit](https://github.com/shelajev/agy-sbx-kit) (commit 026a3a5,
  Apache-2.0).

### Accounts, bindings and existing sandboxes

- **Switch accounts:** in `S`, press `d` on the Antigravity row (or run `sbx secret rm antigravity -f`). Then sign in
  again in any agy sandbox.
- **`S`** shows the Antigravity row as *signed in* or *not signed in*. `sbx secret ls` does not list it.
- **The binding:** aisbx writes it to `~/.config/sbx/credentials.yaml` before it creates, rebuilds or starts an agy
  sandbox ([Credentials](accounts.md#credentials)). If agy reports *"OAuth refresh denied (no authorizing
  binding)"*, start the sandbox through aisbx once.

### Or a Gemini API key

Run `sbx secret set google` before you create the sandbox. The proxy adds the key on
`generativelanguage.googleapis.com` only. An API key bills differently from the subscription.

### Telemetry and updates

- agy has no telemetry switch. The kit denies its log sink, `play.googleapis.com`.
- The dashboard runs `agy update` before each start. It downloads from `storage.googleapis.com`, a general-purpose
  host. To give up the self-update, deny that host:

  ```console
  $ sbx policy deny network --sandbox <sandbox> storage.googleapis.com
  ```

### Still unmeasured

Not yet run on a host: the first sign-in (agy must refresh its token through the proxy at every start), that a
second fresh sandbox starts signed in, that the token file holds sentinels in place of both tokens, that API-key mode
works, and the scopes in the sign-in URL.

## Plugins and the official marketplace

Claude Code registers Anthropic's official plugin marketplace on its first interactive start. Every Claude kit leaves
`CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL` unset on purpose: it is not telemetry, and with it `/plugin`
offers no plugins. The kits already allow its download host; do not add `github.com` for it.

**Install a plugin only if you trust it.** A plugin can bring skills, hooks and MCP servers into the sandbox.

### If the marketplace is missing

A sandbox that once ran with the variable set never retries. Rebuild it (`R`), or add the marketplace by hand (this
needs `github.com`):

```console
$ sbx exec <sandbox> claude plugin marketplace add anthropics/claude-plugins-official
```

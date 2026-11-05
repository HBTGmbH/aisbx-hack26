# Network and privacy

[← Back to the README](../README.md)

This page tells where a sandbox may connect, how to answer a blocked request, and which telemetry the kits switch off.

## Network policy

The network policy of the host applies to every sandbox on the machine. Set it to `deny-all` once, so that only the
kits' allow-lists open hosts:

```console
$ sbx policy init deny-all
$ sbx policy reset --force && sbx policy init deny-all   # if sbx already has a policy
```

**`sbx policy reset` stops every sandbox.** Until you set a policy, sbx refuses every create; the create wizard
warns you first. Under the `balanced` preset, GitHub, npm, Maven Central and `**.gradle.org` are open to every
sandbox, whatever the kits say.

### Check a host

Select a sandbox and press `c`, then type the host. The dashboard tells whether the policy allows it. This opens no
connection. Without the dashboard, run `sbx policy check network --sandbox <name> <host>`.

The *5 Network* tab shows every rule that applies to the sandbox and where it came from. The last row, marked
`fallback`, is the policy's own `deny **`: it decides every request that no other rule matches.

### A blocked host asks first

Under `deny-all` and `balanced`, sbx holds a request that no rule allows. The agent gets
`403 Approval required for <host>`. Press `y` to allow or dismiss it for that sandbox
([Requests held for approval](sandboxes.md#requests-held-for-approval-y)).

![A rule allows the host: the proxy forwards the request and adds a bound key. No rule allows it: the request is held and the agent gets 403. You press y: Allow adds a rule for this sandbox only, so the next attempt goes through; Dismiss keeps the host blocked.](images/network-decision.png)

### Rules for part of an API

A rule can name HTTP methods, paths and query parameters. Add one with `sbx policy`:

```console
$ sbx policy allow network <host> --method GET --path '/v1/**'
$ sbx policy deny network <host> --method GET --path '/v1/**' --query team=platform
```

## Egress

A kit's allow-list can only open hosts, never close one. Under `deny-all`, these lists are all a sandbox can reach.
*Install* hosts are open only while the kits install at creation; the others are open while the sandbox runs.

| Kit | Allows (at run time unless marked) |
| --- | --- |
| `claude-litellm` | `downloads.claude.ai` (updates; also at install), `code.claude.com` (docs), `api.anthropic.com` (WebFetch's domain check only); the gateway host |
| `claude-anthropic` | `api.anthropic.com`, `claude.ai`, `platform.claude.com`, `claude.com`, `mcp-proxy.anthropic.com`, `bridge.claudeusercontent.com` (`/remote-control`), `downloads.claude.ai` (also at install), `code.claude.com` |
| `claude-token` | `api.anthropic.com`, `downloads.claude.ai` (also at install), `code.claude.com` |
| `opencode-litellm` | the npm registry (updates; also at install), `models.dev`, `github.com`, `*.github.com`, `*.githubusercontent.com`; the gateway host |
| `codex-openai` | `api.openai.com`, `openai.com`, `auth.openai.com`, `chatgpt.com`, `files.openai.com`, the npm registry (also at install) ([Codex](agents.md#codex)) |
| `agy` | Google's sign-in, model, picture, plugin and updater hosts, `storage.googleapis.com`; **denies** `play.googleapis.com`, agy's log sink ([Antigravity](agents.md#antigravity-agy)) |
| `pi-litellm` | the npm registry (updates; also at install); the gateway host ([Pi](agents.md#pi)) |
| `rust` | at install only: Ubuntu's mirrors and `download.docker.com` |
| `headroom` | at install only: `pypi.org`, `files.pythonhosted.org` |
| `ponytail` | at install only: `github.com` |
| `web` | `**`: every host, see [Web access](addons.md#web-access-websearch--webfetch) |
| `remote-dev` | the editors' server download hosts, see [Editor and app integrations](sandboxes.md#editor-and-app-integrations-vs-code-intellij-claude-desktop) |
| `jdk25`, `rtk`, `host-gradle`, `host-maven`, `host-npm`, `host-pip`, `host-skills`, `pkg-cache`, `statusline` | nothing |

- A connection's kit allows exactly `<host>:443` of its URL.
- Every backend also allows `plugins.gradle.org`, `plugins-artifacts.gradle.org`, the Ubuntu apt mirrors and
  `download.docker.com`.
- The image build downloads the tools (JDK, rtk, Rust, OpenCode, Pi, ponytail) on your host's Docker, outside the
  sandbox policy.

### What the lists guarantee

- **A credential goes only to its own host.** `web`'s `**` widens where traffic goes, never which host gets a key.
- **A backend inherits nothing** from sbx's built-in agents: its list is all it opens.
- **No kit allows a telemetry host.**
- A host that is not allowed cannot be resolved by DNS. ICMP is blocked.

## Telemetry and non-essential traffic

The kits configure each agent not to send telemetry at all. This matters because under `balanced` the telemetry
hosts may be open, and Anthropic metrics can go through `api.anthropic.com`.

### sbx's own telemetry (`T`)

sbx's CLI and daemon send usage data to Docker unless you turn it off. The dashboard turns it off when it starts,
and `aisbx up`, `run`, `status`, `stop` and `rm` before they run, while the setting still has sbx's default.
Each says so once (the dashboard in its status line, a headless run on stderr). A value you set yourself stays. Press `T`
(or *Settings* in the window's header) and choose *telemetry*: `↵` turns it on or off again. A running daemon follows the change after
`sbx daemon restart`. Without the dashboard:

```console
$ sbx settings set telemetry.disabled true
$ sbx daemon restart
```

`SBX_NO_TELEMETRY=1` in sbx's environment turns it off too, but the setting does not show that. `aisbx` and
`aisbx-ui` set it for every `sbx` they start, unless it already has a value; set `SBX_NO_TELEMETRY=0` before you
start them to leave it to the setting.

### The Claude kits' switches

All three Claude kits set these variables in their image:

| Variable | Turns off |
| --- | --- |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` | the umbrella: metrics, error reports, `/feedback`, surveys |
| `DISABLE_TELEMETRY=1` | usage metrics |
| `DISABLE_ERROR_REPORTING=1` | error reports to a third-party tracker |
| `DISABLE_FEEDBACK_COMMAND=1` | `/feedback`, which uploads the conversation **including code** |
| `DISABLE_BUG_COMMAND=1` | `/bug` and `/share` |
| `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` | the session-rating prompt |
| `DISABLE_GROWTHBOOK=1` | remote feature flags from `cdn.growthbook.io` |
| `DO_NOT_TRACK=1` | the cross-tool convention |
| `DISABLE_AUTOUPDATER=1` | the background updater only; `claude update` still works |

**Keep every variable, not only the umbrella:** a release can narrow what the umbrella covers.

#### Install hooks

Each install hook that runs `claude` sets the same variables, so a create sends no telemetry.

#### Re-derive the list

After a Claude Code upgrade, list the switches the binary knows:

```console
$ strings "$(readlink -f "$(command -v claude)")" \
    | grep -oE '\b(DISABLE_[A-Z0-9_]+|CLAUDE_CODE_[A-Z0-9_]+|DO_NOT_TRACK)\b' | sort -u
```

`DISABLE_NON_ESSENTIAL_MODEL_CALLS`, which hardening guides recommend, does not exist.

### Deliberately left on

- **The official plugin marketplace.** See [Plugins](agents.md#plugins-and-the-official-marketplace).
- **WebFetch's domain safety check.** It sends only the target hostname to `api.anthropic.com`.
- **Local transcripts** in `~/.claude/projects/`. They stay on the machine. Set `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1`
  to keep no history.

### Other agents

- OpenCode: config settings, see [OpenCode](agents.md#opencode).
- Codex: config keys, see [Codex](agents.md#codex).
- agy has no switch, so its kit denies its log sink. See [Antigravity](agents.md#antigravity-agy).

## Blocked hosts

Keep one sandbox off hosts that the policy or a kit would open. Type them in the create wizard's *Blocked hosts*
field under *More options*, separated by spaces or commas. Wildcards are allowed. A deny beats every allow, also
`web`'s `**`, and a rebuild keeps it.

To add or remove one later, use `sbx policy`:

```console
$ sbx policy deny network --sandbox my-sandbox ads.example.com
$ sbx policy rm network --sandbox my-sandbox --resource ads.example.com
```

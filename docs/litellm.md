# LiteLLM connections

[← Back to the README](../README.md)

Connect Claude Code, OpenCode and Pi to any LiteLLM gateway, and choose the models the agent starts on.

The backends `claude-litellm`, `opencode-litellm` and `pi-litellm` use a *connection*: a name, an `https://` URL
(port 443 only) and a key. Keep as many connections as you need. aisbx reads the model list of each gateway itself
and keeps your model picks per connection. You set up everything in the dashboard.

## Adding a connection

Add a connection in the create wizard (*new connection…* on the Connection step) or in `S` with `n`. Type a name
(lower-case letters, digits and dashes, at most 32), the URL and the key. aisbx then reads the gateway's model list.

### In scripts

Write a connection file and give the key on stdin, never as an argument:

```json
{
  "name": "acme",
  "url": "https://litellm.example.com"
}
```

```console
$ aisbx litellm add acme.json --key-stdin < key.txt   # refuses a name that exists
$ aisbx litellm add acme.json --key-stdin --replace   # replaces the connection and its key
$ aisbx litellm ls [--json]                           # name, url, models, key stored, last used
$ aisbx litellm fetch acme                            # reads the gateway's model list now
$ aisbx litellm rm acme [--force]                     # refused while a sandbox uses it, unless --force
```

A `--replace` onto another host needs the key again.

## Where the key lives

The key stays in the host keychain as an sbx secret. Inside the sandbox the agent sees only a placeholder, and the
host proxy puts in the key on requests to the connection's host only. The rest of the connection is in
`~/.local/state/aisbx/litellm/<name>/connection.json`.

- **Rotate a key:** store it again (`↵` in `S`). Every sandbox on the connection uses it, with no rebuild.
- **Remove a connection:** `d` in `S`, or `aisbx litellm rm`. **Its sandboxes stop signing in at once.**

## Choosing a connection

- **In the create wizard**, pick the connection on the Connection step: `←→`, then `↵` to go on, or a click. It
  opens on the one you used last. A connection without a stored key shows a warning.
- **With `aisbx up`**, give `--litellm <name>`. With one connection, `aisbx up` takes it. With several and no
  `--litellm`, it exits 2.
- **On a sandbox**, `A` [moves it to another connection](accounts.md#accounts-a). This is a rebuild.

The sandbox's row shows its connection (*Claude Code · LiteLLM acme*). After you change a connection's URL, press
`R` on each of its sandboxes. `R` refuses a sandbox whose connection is gone; use `A`.

## The generated kit

For each agent on a connection, aisbx writes a small kit to
`~/.local/state/aisbx/litellm/<name>/litellm-<name>-<agent>/` (`<agent>` is `claude`, `opencode` or `pi`). It
allows only the gateway's host and points the agent at the gateway:

| Agent | What the kit sets |
| --- | --- |
| Claude Code | `ANTHROPIC_BASE_URL` and the key's placeholder, plus the [model picker](#claude-codes-model-picker-on-the-gateway) |
| OpenCode | the config file `~/.config/opencode/opencode-litellm-<name>.json` ([OpenCode](agents.md#opencode)) |
| Pi | `~/.pi/agent/models.json` and `~/.pi/agent/settings.json` ([Pi](agents.md#pi)) |

aisbx writes these files again before every start, so a model change reaches the next session with no rebuild.

**Without the dashboard**, put the kit after the backend kit:

```console
$ sbx run --kit ~/.local/state/aisbx/litellm/acme/litellm-acme-claude ./kits/claude-litellm
```

## Seeing and editing the config files

The wizard's Models step lists the files the connection writes for the agent, under *Config files*. To change one:

1. Open *Config files*, select the file and press `↵` (*Open* in `aisbx-ui`).
2. Edit the copy in your editor (`$VISUAL`, `$EDITOR`, else `vi` or Notepad) and save it.

The edited copy then replaces the generated file for every sandbox of that agent on that connection.

- **Model changes no longer reach an edited copy.**
- aisbx skips a copy that is not valid JSON and shows a warning on its row.
- Press `Delete` (*Reset*) to drop the copy and use the generated file again.

## The gateway's model list

aisbx reads each connection's model list through a hidden sandbox, `aisbx-models-<name>` (the model fetcher). It
runs no agent and reaches only the gateway's host.

The fetcher runs when you store a key, once a day when the dashboard starts, and when the wizard needs a list older
than a day. To fetch now, press `L` on a sandbox, choose *Refresh the list* on the Models step, or run
`aisbx litellm fetch <name>`. The first fetch takes about 15 s.

### Limits

aisbx is not yet tested against the answer of a real gateway.

## Claude Code's model picker on the gateway

On **Claude Code · LiteLLM**, `/model` offers only the gateway models you left on in the Models step, Claude models
first. Each row shows the model's name and, where the gateway gives them, its route, context and price:
`Bedrock EU · 1M ctx · $4.40 / $22.00 per MTok`. To write your own labels,
[edit the file](#seeing-and-editing-the-config-files).

## Pinning Claude Code's models on the gateway

Claude Code maps `opus`, `sonnet` and `haiku` (background work) to Anthropic model names, which the gateway may not
serve. Pin each alias to one of the [connection's models](#the-gateways-model-list):

1. Open *Pick models* on the wizard's Models step, or the kits editor (`e`) on a sandbox.
2. Press `↵` on an alias and pick a model. All pins are optional.
3. Pick *default* to remove a pin.

The wizard opens on the pins you used last on that connection. The first pin is added in place. **A change or a
removal rebuilds the sandbox**; the session history stays. Without the dashboard, add the generated kit
`~/.local/state/aisbx/models/models-<sandbox>` with one more `--kit`.

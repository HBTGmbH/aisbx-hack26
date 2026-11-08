# Local inference engines

[← Back to the README](../README.md)

Run Pi, Claude Code and OpenCode against a model that runs on this computer: llama.cpp, Ollama, LM Studio, MLX or
vLLM.

The backends `pi-local`, `claude-bare-local`, `opencode-local` and `claude-local` use an *engine*: a name, the port
the engine listens on and, if the engine has one, its key. aisbx finds the engines that run on this computer, reads
their model lists and keeps your model picks per engine. The agent stays in its sandbox: it reaches this computer
only on the engine's port.

## Adding an engine

1. Start the engine on this computer, for example `llama-server -m model.gguf --port 8080`.
2. Press `n` in `aisbx`, or click **New sandbox** in `aisbx-ui`, and pick the provider **Local engine**.
3. The Engine step offers the engines you added, then those it found, marked *(found)*. Pick one, or pick
   *new engine…* and type the engine's port.
4. Press Next. aisbx stores the engine's key, if it has one, and reads its model list.

An engine needs no key. If the Engine step says *it wants a key* and the key field is empty, type the key.

### What aisbx finds

aisbx asks for `/v1/models` on these ports of this computer:

- the `--port` of each engine process that runs (its default port where it has none),
- and 8080, 11434, 1234, 8000 and 11234: the default ports of llama-server and mlx-lm, Ollama, LM Studio, vLLM and
  mlx-serve.

On Windows, aisbx asks only the default ports. It reads the key of an engine that runs with `--api-key-file` or
`--api-key`, and the context window where the engine gives it: llama-server, vLLM, LM Studio, and Ollama for a loaded
model.

### In scripts

```console
$ aisbx local discover [--json]                         # the engines that answer on this computer
$ aisbx local add                                       # the only engine found that is not added yet
$ aisbx local add k2 --port 8095                        # the engine on port 8095, named k2
$ aisbx local add k2 --port 8095 --key-stdin < key.txt  # with its key on stdin
$ aisbx local ls [--json]                               # name, URL, engine, models, key stored
$ aisbx local fetch k2                                  # reads the engine's model list now
$ aisbx local rm k2 [--force]                           # refused while a sandbox uses it, unless --force
$ aisbx up --backend pi-local --local k2 ~/my-project   # a sandbox on engine k2
```

`local add` prints the engine's name. With one engine, `aisbx up` takes it. With several and no `--local`, it exits
2.

## Engines

| Engine | Pi and OpenCode (OpenAI chat API) | Claude Code (Anthropic Messages API) | aisbx finds it by |
| --- | --- | --- | --- |
| llama.cpp `llama-server` | yes | yes | port 8080, its process |
| Ollama | yes | yes | port 11434, its process |
| LM Studio | yes | yes, from 0.4.1 | port 1234 |
| vLLM, vLLM Metal | yes | yes | port 8000, its process |
| mlx-lm `mlx_lm.server` | yes | no | port 8080, its process |
| mlx-serve | yes | yes | port 11234, its process |
| oMLX, Rapid-MLX, vMLX, MTPLX, vllm-mlx | yes | yes | its process |

Any other server that answers OpenAI's `/v1/models` on this computer works too, as an *OpenAI-compatible server*.
MetalRT has no server, so no agent can use it.

## Which harness

Before your task, every agent sends the model its instructions and the descriptions of its tools. A local model
reads all of it before it answers, and keeps it in its context, which takes memory. So a light agent needs less
memory and answers sooner. The Harness step says how light each agent is:

| Harness | Backend | Weight | Sent before your task | Tools | Use it |
| --- | --- | --- | --- | --- | --- |
| Pi | `pi-local` | light | 1.2k to 1.5k tokens | 4 | first: the first answer comes in seconds |
| Claude Code bare | `claude-bare-local` | light | 0.9k to 1.3k tokens | 3 | for Claude Code's tools at Pi's speed |
| OpenCode | `opencode-local` | medium | 4.8k to 5.8k tokens, plus a short request for the session's title | 12 | with 16k context or more |
| Claude Code | `claude-local` | heavy | 10.6k to 12.2k tokens | 14 | with 32k context or more, and time |

- At about 200 tokens per second of prompt processing, the model reads Pi's part in about 7 s and Claude Code's in
  about 60 s. Later requests reuse the engine's prompt cache.
- Short instructions also give a small model less help. With Qwen3.5-9B, Pi answered without calling its tool in 5
  of 5 runs of one task; with K2 Horizon 7B it solved the task. When Pi does not call its tools, try Claude Code bare.
- The Harness step warns when the engine's context is too small for the agent, or when the engine has no Anthropic
  API for Claude Code.

### Not offered

- **Codex**: it speaks only OpenAI's Responses API, and refuses the chat API (`wire_api = "chat"`). llama.cpp serves
  the Responses API only in part, mlx-lm and oMLX not at all. Before your task it sends about 40 kB with 9 tools.
- **Antigravity** (`agy`): it uses Google's models only.

## Claude Code bare

`claude-bare-local` runs Claude Code as `claude --bare`: its image sets `CLAUDE_CODE_SIMPLE=1`. Claude Code then
loads no hooks, no plugins and no CLAUDE.md, and has three tools: Bash, Edit and Read. The kits' instructions for
the agent, which sbx writes into a CLAUDE.md, do not reach it either.

- The status line, RTK, Ponytail and headroom addons do nothing in bare mode, so the wizard does not offer them.
- To give it instructions, pass them in the prompt, or run `claude --append-system-prompt-file <file>` in its shell.

## Models and the context window

- aisbx reads each engine's model list on this computer: when you add the engine, once a day when the dashboard
  starts, and when you ask (`L` on a sandbox, *Refresh the list* on the Models step, `aisbx local fetch`).
- Before every start, aisbx writes the list and your picks into the agent's config again.
- Without a pick, the agent starts on the engine's first model. Ollama, LM Studio and vLLM answer only to the names
  of the models they serve, so pick on the Models step when the engine serves more than one. llama-server with one
  model answers to any name.
- aisbx tells Claude Code the engine's context window (`CLAUDE_CODE_MAX_CONTEXT_TOKENS`, with
  `CLAUDE_CODE_DISABLE_1M_CONTEXT=1`), so Claude Code compacts the conversation before the engine's limit. A new
  window reaches Claude Code at the next rebuild (`R`), Pi and OpenCode at their next start.
- Picks and pins work as on a gateway: [Pinning Claude Code's models](litellm.md#pinning-claude-codes-models-on-the-gateway),
  [Seeing and editing the config files](litellm.md#seeing-and-editing-the-config-files).

## The key and the network

- If the engine has a key, aisbx stores it as an sbx secret for `localhost`:
  `sbx secret set-custom --host localhost --placeholder sbx-cs-aisbx-llm-<name>`. The agent sees only the
  placeholder. The host proxy puts in the key on the agent's requests to this computer.
- The sandbox reaches the engine as `host.docker.internal:<port>`. sbx checks that request against
  `localhost:<port>`, which the engine's kit allows while the sandbox runs. No other port of this computer is open
  to it.
- Check it: `sbx policy check network --sandbox <name> host.docker.internal:<port>`.
- Let the engine listen on this computer only, for example with `--host 127.0.0.1`. The sandbox needs nothing else.

## Memory

The model and the sandboxes share this computer's memory. On a 16 GB Mac, an 8 GB model and one sandbox at the
default 4 GiB already take 12 GB. Give more sandboxes less memory:
[CPU and memory limits](sandboxes.md#cpu-and-memory-limits).

## The generated kit

For each agent on an engine, aisbx writes a small kit to
`~/.local/state/aisbx/litellm/<name>/local-<name>-<agent>/` (`<agent>` is `claude`, `opencode` or `pi`). It sets
the engine's URL in the sandbox and the key's placeholder, and allows only the engine's port.

**Without the dashboard**, put the kit after the backend kit:

```console
$ sbx run --kit ~/.local/state/aisbx/litellm/k2/local-k2-pi ./kits/pi-local
```

# Shared agent skills

[← Back to the README](../README.md)

Give the agents your skills: the sbx skills store, aisbx's own skill repositories, and the skills each sandbox
takes from them.

aisbx gives skills to a sandbox in three ways:

- the sbx skills store, below: one store on the host, shared by the sandboxes;
- the [`host-skills`](addons.md#host-skills-read-only) addon: the host's skills folders, live and read-only, on every
  backend;
- [skill repositories](#skill-repositories-aisbx-skill-repo): Git repositories that aisbx mirrors, with
  [skills picked per sandbox](#skills-from-skill-repos-aisbx-skill).

## The sbx skills store

sbx keeps one skills store on the host and mounts it into each new sandbox. The store survives sandbox deletion.

### The skills screen (`K`)

Press `K` to see the store: its path, each skill, and how many sandboxes share it.

| Key | Does |
| --- | --- |
| `i` | imports your skill folders (`~/.agents/skills`, `~/.claude/skills`, `~/.config/opencode/skills`, ...) |
| `a` | adds skills from a Git repository, from one line such as `anthropics/skills pdf docx` |
| `u` | updates the skills that `a` added |
| `d` | removes a skill |

An import over a repository skill drops that skill's link to its repository.

Without the dashboard, use `sbx skills import`, `sbx skills add <repo> --skill <name>`, `sbx skills update` and
`sbx skills ls`.

### Share modes

Choose the mode on the create wizard's *share skills* row. It starts on the host's `skills.defaultMode`, which you
change on the settings screen (`T`, *skills default*) or with `sbx settings set skills.defaultMode <mode>`. The
default also applies to `aisbx up` and a plain `sbx create` without `--skills`. You cannot change a sandbox's mode
after creation; a rebuild keeps it.

| Mode | The sandbox |
| --- | --- |
| `readonly` (default) | reads the shared skills, but cannot change them |
| `readwrite` | can also change the skills that every other sharing sandbox loads |
| `off` | gets no store |

**`readwrite` puts every sharing sandbox behind one trust boundary.**

### Which backends get the store

| Backend | Skills linked into |
| --- | --- |
| `claude-litellm`, `claude-anthropic`, `claude-token` | `/home/agent/.claude/skills` |
| `opencode-litellm` | `/home/agent/.config/opencode/skills` |
| `codex-openai` | `/home/agent/.agents/skills` |
| `agy`, `pi-litellm` | nothing |

### How the store reaches the agent

The kits link each store skill into the agent's skills folder at creation and at every start. A skill added to the
store reaches a running sandbox at its next start. The agent's skills folder stays writable.

## Skill repositories (`aisbx skill-repo`)

aisbx keeps a list of skill repositories, public or private, and a local mirror of each. Sandboxes take skills from
the mirrors, never from the remote. Manage the list from the command line:

```console
$ aisbx skill-repo add acme/agent-skills                  # GitHub shorthand
$ aisbx skill-repo add git@github.com:acme/private-skills.git --ref v2
$ aisbx skill-repo fetch                                  # clone or update every repo
$ aisbx skill-repo ls                                     # id, source, ref, method, cached commit
$ aisbx skill-repo auto-update off                        # starts keep installed skills as they are
$ aisbx skill-repo rm acme/agent-skills                   # also deletes its mirror
```

Run `aisbx skill-repo fetch` to get a newer commit. Then pick the skills per sandbox
([Skills from skill repos](#skills-from-skill-repos-aisbx-skill)).

### Files

The list is `skill-repos.json` and the mirrors are `skill-cache/<id>/`, in the aisbx state folder
(`~/.local/state/aisbx`; on Windows `%LOCALAPPDATA%\aisbx\state`).

### Sources and methods

A source is `owner/repo` (GitHub), `https://host/path`, `ssh://[user@]host/path` or `user@host:path`. `--ref` takes a
branch or tag. **Never paste a token into a source.**

| `--via` | How aisbx fetches |
| --- | --- |
| `ssh` | git over SSH with your keys |
| `gh` | the GitHub CLI; GitHub only |
| `token` | HTTPS to github.com with `GITHUB_TOKEN` |
| `git` | plain git with your own credential helper |
| `auto` (default) | tries `ssh`, `token`, `gh`, then `git` |

### The token rule

aisbx gives `GITHUB_TOKEN` to git only for the fetch. The token never goes into the list, the mirror or a message.

### Unattended by design

A fetch never prompts for credentials: without them it fails. ssh can still ask for a key passphrase when no agent
holds the key.

### `fetch`

A failed update keeps the previous commit. `fetch` exits 1 if a repo failed.

### The fetch report

`fetch` prints the skills that each repo added (`+`), removed (`-`) and changed (`~`). Then it tells, per sandbox,
what its next start does. `--json` prints the same report on stdout.

```text
acme-agent-skills-1a2b3c4d: 0123abcd4567 → 89abcdef0123: +pdf  -old (skipped: too-big)  ~docx
acme-agent-skills-1a2b3c4d:   probe: next start installs pdf; updates docx; removes old
```

### Auto-update

`auto-update on|off` is one switch for every sandbox. It is on by default.

- **On:** the next start of each sandbox brings its installed skills to the fetched commit.
- **Off:** a start keeps each installed skill as it is. New picks still arrive and removed picks still go.
  `aisbx skill sync NAME` always updates.

To pin one repo instead, add it with `--ref <tag>`.

## Skills from skill repos (`aisbx skill`)

Choose per sandbox which skills of the mirrored repos it gets. Global picks go to every sandbox. You need no kit and
no rebuild.

### In the dashboard

- Press `i` on a sandbox to pick its skills. Global picks show ticked and locked.
- Press `K`, then *Repo skills for all sandboxes*, to edit the global picks.
- Press `space` to tick a repo (*all skills*) or one skill. Then choose *Save*.
- Choose *Sync now* to copy the skills in at once. Otherwise they arrive at the next start.

### The selection

On the command line, a spec is `REPO` (every skill of the repo) or `REPO#SKILL` (one skill):

```console
$ aisbx up --backend claude-anthropic --skill acme/agent-skills#pdf --name probe ~/src/app
$ aisbx skill add probe acme/agent-skills                 # all of its skills, now and later
$ aisbx skill rm probe acme/agent-skills#pdf
$ aisbx skill add --global acme/agent-skills#review       # every sandbox
$ aisbx skill ls
$ aisbx skill sync probe                                  # copy them in now
```

aisbx refuses a skill name that two repos would give to one sandbox. The picks are in `skill-selection.json` in the
aisbx state folder.

### Global picks

Global picks reach every sandbox, also one made with plain `sbx`, at its next start. A sandbox cannot drop a global
pick.

### What counts as a skill

A skill is a folder with a `SKILL.md`, at most four levels below the repo root. aisbx skips a folder with a bad name,
a symlink, a submodule or more than 20 MiB, and names each skip. **A skill's scripts are code: they run in the
sandbox when the agent uses them.**

### When it syncs

aisbx syncs the picks before each agent start (`↵`, `aisbx run`), on *Sync now* and on `aisbx skill sync`. A failed
sync does not stop the start. A rebuild keeps the picks; deleting the sandbox with `d` or `aisbx rm` drops them.

### In the sandbox

The skills are copied to `~/.local/share/aisbx-skills/` and linked into the agent's skills folders. The agent can
change the copies; the next sync overwrites them. No token or host path reaches the sandbox.

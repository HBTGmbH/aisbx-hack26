//! `aisbx up | run | status | stop | rm`, `litellm`, `skill-repo` and `skill`: the dashboard's
//! kits and sandboxes, driven by parameters, for scripts.
//!
//! Scripts depend on this contract: stdout carries only the payload (`up`: the sandbox name,
//! `run`: the agent's own output, `status --json`: one JSON document), and everything else goes to
//! stderr. `run` exits with the agent's own exit code or with one of the `EXIT_*` codes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::kits::{self, Catalog, HeadlessRun, OutputFormat};
use crate::{
    bindings, folders, litellm, model, models, sbx, skill_repos, skill_select, skill_source, state,
    tmpfs, tokens,
};

/// Usage error: nothing ran.
pub const EXIT_USAGE: u8 = 2;
/// `--timeout` hit, as `timeout(1)`: the agent was stopped.
pub const EXIT_TIMEOUT: u8 = 124;
/// sbx or the sandbox failed, as `docker run`; for `run`, the agent never started.
pub const EXIT_SANDBOX: u8 = 125;
/// Interrupted by Ctrl-C: the agent was stopped.
pub const EXIT_INTERRUPTED: u8 = 130;

pub const HELP: &str = "\
HEADLESS USE — the same kits and names as the dashboard, for scripts:

    aisbx up --backend <kit> [options] [WORKSPACE]
        Create the sandbox unless it exists, and print its name.
        --name NAME            sbx name (default: <backend>-<folder>, as the dashboard derives it)
        --kit ADDON            stack an addon kit (repeatable)
        --token NAME           the named setup token, for --backend claude-token
        --litellm NAME         the LiteLLM connection, for the LiteLLM backends (default: the
                               only one; with several, required — never the dashboard's last)
        --local NAME           the local engine, for the *-local backends (the same rule)
        --clone                clone mode: the agent works on its own clone, your repo is read-only
        --skills MODE          off | readonly | readwrite (default: the host's skills.defaultMode)
        --deny-network HOST    keep this sandbox off a host (repeatable; deny beats every allow)
        --mount DIR[:ro]       an extra host folder at its own path, read-only with :ro (repeatable;
                               never your home folder, a folder holding it, or one with credentials)
        --cpus N, --memory SIZE    resource limits (default: 4 CPUs, 4g; `auto` for sbx's own:
                               every host CPU, half the host's memory)
        --template REF         refused: every backend is a v3 kit workload, and sbx
                               builds no sandbox from a saved template under one
        --tmpfs-volumes        keep the kits' volumes in RAM: nothing of the sandbox stays on
                               disk after `aisbx rm` (sbx keeps a block volume, and no sbx command
                               deletes it), and their contents go at every stop
        --label TEXT           the label the dashboard shows
        --skill REPO[#SKILL]   skills from a cached skill repo: all of it, or one skill
                               (repeatable; see `aisbx skill`)
        --ssh-agent            forward the host SSH agent (off by default)
        --git-readonly         mount the workspace's .git folder read-only as well: the agent
                               reads the history but cannot commit or change hooks
        --no-host-folders      mount neither the folders of `aisbx folders` nor those the
                               workspace's companion.json names (mounted by default)
        --no-mcp               no MCP servers and no tools to find the host's; applied at
                               creation only (an existing sandbox keeps what it has)

    aisbx run NAME (--prompt TEXT | --prompt-file FILE|-) [options] [-- AGENT_ARGS]
        Run the agent once, non-interactively, and exit with its exit code.
        --model M, --effort E  passed to the agent
        --format F             json (default) | stream-json | text  — the agent's own output
        --timeout DUR          stop the agent after DUR (90s, 45m, 4h, 1h30m); exit 124
        --workdir DIR          start in DIR instead of the workspace root
        --no-update            skip the agent update that runs first
        --approvals MODE       dismiss (default) | keep — requests held for approval
        --report FILE          write a JSON summary of the run to FILE (on the host)
        --ssh-agent            forward the host SSH agent (off by default)
        Agents: Claude Code, Codex, Antigravity (agy), Pi and OpenCode.

    aisbx ls [--json]              every sandbox: name, status, agent, workspace, label
    aisbx status NAME [--json]     state, agent, kits, held requests (exit 1 if unknown)
    aisbx stop NAME                stop it
    aisbx rm NAME                  remove it (the workspace is never touched)

    aisbx litellm ls [--json]      the LiteLLM connections: name, URL, models, key stored
    aisbx litellm add FILE.json [--key-stdin] [--replace]
        Add a connection ({\"name\", \"url\": \"https://…\"}). --key-stdin reads its key from
        stdin and stores it with sbx (never in a file or argv); the connection is written only
        once sbx stored the key, and its model list is fetched then. An existing name is
        refused unless --replace.
    aisbx litellm fetch NAME
        Fetch the connection's model list now, through its own small sandbox
        (aisbx-models-NAME), which reaches the gateway's host only.
    aisbx litellm rm NAME [--force]
        Remove a connection and its key; refused (exit 2) while a sandbox uses it, unless --force.

    aisbx local discover [--json]  the inference engines answering on this computer: the ports
                                   engine processes name, and 8080, 11434, 1234, 8000, 11234
    aisbx local add [NAME] [--port N] [--key-stdin] [--replace]
        Add the engine found on port N (without --port: the only one found that no engine
        connection is on yet) and print its name. The key: --key-stdin reads it from stdin,
        else the one the engine's process was started with (--api-key, --api-key-file), if
        any; sbx stores it for localhost. Its model list is read then.
    aisbx local ls [--json]        the local engines: name, URL, engine, models, key stored
    aisbx local fetch NAME [--key-stdin]
        Read the engine's model list now, on the host (no sandbox).
    aisbx local rm NAME [--force]  as litellm rm

    aisbx folders ls [--json]      the host folders every sandbox mounts beside its workspace
    aisbx folders add DIR [--write] [--purpose TEXT]
        Add a folder, or change one listed: read-only unless --write. The purpose goes into the
        agent's instructions with the folder's path. A sandbox mounts it from its next create
        or rebuild, as it mounts the folders its workspace's companion.json names.
    aisbx folders rm DIR           drop it from the list

    aisbx skill-repo add SOURCE [--ref REF] [--via auto|gh|ssh|token|git]
        Keep a skill repository in aisbx's list and print its id. SOURCE is owner/repo
        (GitHub), https://host/path, ssh://[user@]host/path or user@host:path — never a token
        or a URL with a user or password. --via token reads GITHUB_TOKEN at fetch time only.
    aisbx skill-repo rm ID|SOURCE      drop it from the list and delete its local cache
    aisbx skill-repo ls [--json]       the list, with the cached commit as git reports it,
                                       and the auto-update switch (last line on stderr)
    aisbx skill-repo fetch [ID|SOURCE …] [--json]
        Clone or update the local cache of each (all of them without arguments); exit 1 if
        any failed. Sandboxes take skills from the cache only (`aisbx skill`), never the remote.
        Reports, per repo, the skills added (+), removed (-) and changed (~) since the previous
        cache, and each sandbox whose selection they touch with what its next start does.
        --json prints that report as one JSON document on stdout.
    aisbx skill-repo auto-update [on|off]
        Without an argument, print on or off. On (the default): each sandbox's start brings its
        installed skills to the cached commit. Off: a start keeps installed skills as they are
        (newly selected ones still arrive, deselected ones still go); `aisbx skill sync NAME`
        updates them.

    aisbx skill add NAME REPO[#SKILL] …   select skills of a cached repo for a sandbox: REPO
                                          (an id or source) is all of its skills, REPO#SKILL one
    aisbx skill rm NAME REPO[#SKILL] …    drop them from the selection
    aisbx skill add|rm --global REPO[#SKILL] …
                                          the same for every sandbox, existing and new: global
                                          picks are merged with each sandbox's own at its sync
    aisbx skill ls [NAME] [--json]        the selection, global entries included, with each
                                          repo's cached commit
    aisbx skill sync NAME                 copy the selected skills from the cache into the
                                          sandbox and link them (also before every aisbx start,
                                          as `skill sync --start NAME`, which follows the
                                          auto-update switch)
        The selection is skill-selection.json in aisbx's state folder; add and rm only edit
        it. Exit codes: 2 a refused spec, 125 no such sandbox, 1 a failed step.

    Exit codes of run: the agent's own; 2 usage, 124 timeout, 125 sandbox failure (the agent
    never started), 130 interrupted. stdout carries only the agent's output (or, for up, the
    sandbox name); everything else goes to stderr.
";

pub fn is_subcommand(arg: &str) -> bool {
    matches!(
        arg,
        "up" | "run"
            | "ls"
            | "status"
            | "stop"
            | "rm"
            | "litellm"
            | "local"
            | "folders"
            | "skill-repo"
            | "skill"
    )
}

/// The command lines core's own jobs run as this very binary ([`job_argv`]), by their first
/// words. `aisbx` takes every subcommand; `aisbx-ui` hands only these to [`exit`].
pub const JOB_COMMANDS: [&[&str]; 2] = [&["skill"], &["local", "fetch"]];

/// Whether `args` start with one of [`JOB_COMMANDS`].
pub fn is_job_command<S: AsRef<str>>(args: &[S]) -> bool {
    JOB_COMMANDS.iter().any(|words| {
        args.len() >= words.len() && words.iter().zip(args).all(|(w, a)| a.as_ref() == *w)
    })
}

/// A job step's command line that runs this very binary with `args`, one of [`JOB_COMMANDS`].
pub fn job_argv(args: &[&str]) -> Vec<String> {
    debug_assert!(
        is_job_command(args),
        "{args:?} is not in JOB_COMMANDS: aisbx-ui would take it for its FOLDER"
    );
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "aisbx".to_string());
    std::iter::once(exe)
        .chain(args.iter().map(|a| a.to_string()))
        .collect()
}

/// Run [`main`], then exit the process with its code.
pub fn exit(args: &[String]) -> ! {
    std::process::exit(main(args).into())
}

/// Run one subcommand and hand back the process exit code.
pub fn main(args: &[String]) -> u8 {
    let (cmd, rest) = args.split_first().expect("dispatched on a subcommand");
    if runs_sandboxes(cmd) {
        telemetry_off_by_default();
    }
    let result = match cmd.as_str() {
        "up" => up(rest),
        "run" => run(rest),
        "ls" => ls(rest),
        "status" => status(rest),
        "stop" => stop(rest),
        "rm" => rm(rest),
        "litellm" => litellm_cmd(rest),
        "local" => local_cmd(rest),
        "folders" => folders_cmd(&state::state_dir(), rest, &mut std::io::stdout()),
        "skill-repo" => skill_repo(rest),
        "skill" => skill(rest),
        other => Err(Fail::usage(format!("unknown subcommand: {other}"))),
    };
    match result {
        Ok(code) => code,
        Err(fail) => {
            // One line, not the help: the help would bury the reason in the last lines of stderr.
            eprintln!("aisbx: {}", fail.message);
            if fail.code == EXIT_USAGE {
                eprintln!("aisbx: usage: `aisbx --help`");
            }
            fail.code
        }
    }
}

/// The subcommands that drive sandboxes, before which sbx's telemetry is turned off where nobody
/// chose a value.
fn runs_sandboxes(cmd: &str) -> bool {
    matches!(cmd, "up" | "run" | "ls" | "status" | "stop" | "rm")
}

/// [`sbx::disable_telemetry_by_default`], said on stderr; a failure only warns.
fn telemetry_off_by_default() {
    match sbx::disable_telemetry_by_default() {
        Ok(true) => eprintln!("aisbx: {}", crate::app::TELEMETRY_TURNED_OFF_HEADLESS),
        Ok(false) => {}
        Err(e) => eprintln!("aisbx: warning: could not turn sbx's telemetry off ({e:#})"),
    }
}

#[derive(Debug)]
struct Fail {
    code: u8,
    message: String,
}

impl Fail {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
        }
    }

    fn sandbox(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_SANDBOX,
            message: message.into(),
        }
    }

    /// Exit 1: the command ran and did not work out.
    fn failed(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

type Outcome = Result<u8, Fail>;

// --- argument parsing ------------------------------------------------------------------------------

/// A minimal `--flag value` / `--flag=value` / positional parser; `--` ends the options, and what
/// follows is kept verbatim (for `run`, the agent's own arguments).
#[derive(Debug, Default)]
struct Args {
    flags: Vec<(String, Option<String>)>,
    positional: Vec<String>,
    trailing: Vec<String>,
}

impl Args {
    fn parse(raw: &[String], switches: &[&str], valued: &[&str]) -> Result<Self, Fail> {
        let mut out = Self::default();
        let mut it = raw.iter();
        while let Some(arg) = it.next() {
            if arg == "--" {
                out.trailing = it.by_ref().cloned().collect();
                break;
            }
            let Some(body) = arg.strip_prefix("--") else {
                if arg.starts_with('-') && arg.len() > 1 {
                    return Err(Fail::usage(format!("unknown option: {arg}")));
                }
                out.positional.push(arg.clone());
                continue;
            };
            let (key, inline) = match body.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (body, None),
            };
            if switches.contains(&key) {
                if inline.is_some() {
                    return Err(Fail::usage(format!("--{key} takes no value")));
                }
                out.flags.push((key.to_string(), None));
            } else if valued.contains(&key) {
                let value = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| Fail::usage(format!("--{key} needs a value")))?,
                };
                out.flags.push((key.to_string(), Some(value)));
            } else {
                return Err(Fail::usage(format!("unknown option: --{key}")));
            }
        }
        Ok(out)
    }

    fn has(&self, key: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == key)
    }

    fn one(&self, key: &str) -> Result<Option<String>, Fail> {
        let mut values = self.all(key);
        match values.len() {
            0 => Ok(None),
            1 => Ok(values.pop()),
            _ => Err(Fail::usage(format!("--{key} given more than once"))),
        }
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(k, _)| k == key)
            .filter_map(|(_, v)| v.clone())
            .collect()
    }

    /// The single positional `NAME` the sandbox commands take.
    fn name(&self) -> Result<String, Fail> {
        match self.positional.as_slice() {
            [name] => Ok(name.clone()),
            [] => Err(Fail::usage("which sandbox? Pass its name")),
            more => Err(Fail::usage(format!(
                "one sandbox name, not {}",
                more.join(" ")
            ))),
        }
    }
}

/// `sbx` that never asks ([`sbx::no_input`]: no person answers a headless run), without the host
/// SSH agent unless `ssh_agent`: sbx forwards the agent of every client that creates, starts or
/// joins a sandbox.
fn sbx_command(argv: &[String], ssh_agent: bool) -> Command {
    let argv = sbx::no_input(argv);
    let (program, args) = argv.split_first().expect("an argv names its program");
    let mut cmd = Command::new(program);
    cmd.args(args);
    if !ssh_agent {
        cmd.env_remove("SSH_AUTH_SOCK");
    }
    cmd
}

/// The model fetcher's steps ([`crate::model_fetch::steps`]) as a headless fetch runs them: never
/// asking, like every headless sbx call.
fn fetch_steps(conn: &litellm::Connection, kit: &Path, exists: bool) -> Vec<crate::job::Step> {
    crate::model_fetch::steps(conn, kit, exists)
        .into_iter()
        .map(|mut step| {
            step.argv = sbx::no_input(&step.argv);
            step
        })
        .collect()
}

/// A child's stdout routed to our stderr, so stdout stays what the contract says it is.
fn stderr_stdio() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() {
            return Stdio::from(fd);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        if let Ok(handle) = std::io::stderr().as_handle().try_clone_to_owned() {
            return Stdio::from(handle);
        }
    }
    Stdio::null()
}

/// Run an sbx step whose output belongs on stderr, and say whether it succeeded.
fn step(argv: &[String], ssh_agent: bool) -> std::io::Result<ExitStatus> {
    sbx_command(argv, ssh_agent)
        .stdin(Stdio::null())
        .stdout(stderr_stdio())
        .stderr(Stdio::inherit())
        .status()
}

/// Run an sbx step quietly; `true` when it exited 0.
fn quiet(argv: &[String]) -> bool {
    sbx_command(argv, false)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The agent's headless command line for `run`, or the usage error that stops `run` before it
/// starts anything.
fn agent_argv(name: &str, agent: &str, run: &HeadlessRun<'_>) -> Result<Vec<String>, Fail> {
    if run.effort.is_some() && !kits::accepts_effort(agent) {
        return Err(Fail::usage(format!(
            "{name} runs {agent:?}; OpenCode has no effort flag: drop --effort"
        )));
    }
    kits::headless_argv(agent, run).ok_or_else(|| {
        Fail::usage(format!(
            "{name} runs {agent:?}; headless runs know Claude Code, Codex, agy, Pi and OpenCode"
        ))
    })
}

fn find(name: &str) -> Result<Option<sbx::LsEntry>, Fail> {
    let list = sbx::list().map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    Ok(list.into_iter().find(|e| e.name == name))
}

// --- up --------------------------------------------------------------------------------------------

fn up(raw: &[String]) -> Outcome {
    let args = Args::parse(
        raw,
        &[
            "clone",
            "ssh-agent",
            "tmpfs-volumes",
            "git-readonly",
            "no-host-folders",
            "no-mcp",
        ],
        &[
            "backend",
            "name",
            "kit",
            "token",
            "litellm",
            "local",
            "skills",
            "deny-network",
            "mount",
            "cpus",
            "memory",
            "label",
            "template",
            "skill",
        ],
    )?;
    if !args.trailing.is_empty() {
        return Err(Fail::usage("up takes no agent arguments"));
    }
    let skill_specs = args.all("skill");
    parse_specs(&skill_specs)?;
    let catalog = Catalog::load().map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let backend_name = args.one("backend")?.ok_or_else(|| {
        Fail::usage("--backend is required (a kit in kits/, e.g. claude-anthropic)")
    })?;
    let backend = catalog.backend(&backend_name).ok_or_else(|| {
        let known: Vec<&str> = catalog
            .backends
            .iter()
            .map(|b| b.kit.name.as_str())
            .collect();
        Fail::usage(format!(
            "no backend kit {backend_name:?} — one of: {}",
            known.join(", ")
        ))
    })?;

    let mut addons: Vec<String> = Vec::new();
    for name in args.all("kit") {
        if !addons.contains(&name) {
            addons.push(name);
        }
    }
    for addon in &addons {
        if catalog.addon(addon).is_none() {
            let known: Vec<&str> = catalog.all_addons().map(|a| a.kit.name.as_str()).collect();
            return Err(Fail::usage(format!(
                "no addon kit {addon:?} — one of: {}",
                known.join(", ")
            )));
        }
    }
    let template = template_arg(args.one("template")?, &backend_name)?;

    let token = args.one("token")?;
    let token_backend = backend.account() == Some(kits::Account::Token);
    match (&token, token_backend) {
        (None, true) => {
            return Err(Fail::usage(format!(
                "{backend_name} authenticates with a named setup token: pass --token NAME"
            )))
        }
        (Some(_), false) => {
            return Err(Fail::usage(format!(
                "--token is only for the setup-token backend, not {backend_name}"
            )))
        }
        (Some(t), true) if !sbx::token_name_ok(t) => {
            return Err(Fail::usage(
                "token names are lower-case letters, digits and dashes",
            ))
        }
        _ => {}
    }
    let kind = litellm::kind_of_backend(&backend.kit.name);
    let wanted = connection_flag(
        kind,
        args.one("litellm")?,
        args.one("local")?,
        &backend_name,
    )?;
    let connection = match (litellm::Agent::of_backend(&backend.kit.name), kind) {
        (Some(agent), Some(kind)) => {
            let all: Vec<litellm::Connection> = (litellm::load_all().into_iter())
                .filter(|c| c.kind() == kind)
                .collect();
            // Read only to name it in the "pick one" message.
            let registry = (all.len() > 1).then(state::Registry::load);
            let last = registry.as_ref().and_then(|r| r.last_litellm());
            let conn = pick_connection(&all, wanted.as_deref(), last, kind)?;
            eprintln!(
                "aisbx: {}: {} ({})",
                kind.noun(),
                conn.name,
                litellm::place(&conn.url)
            );
            Some((conn, agent))
        }
        _ => None,
    };

    let workspace = match args.positional.as_slice() {
        [] => {
            std::env::current_dir().map_err(|e| Fail::usage(format!("current directory: {e}")))?
        }
        [path] => PathBuf::from(path),
        more => {
            return Err(Fail::usage(format!(
                "one workspace, not {}",
                more.join(" ")
            )))
        }
    };
    let workspace = workspace
        .canonicalize()
        .map_err(|e| Fail::usage(format!("workspace {}: {e}", workspace.display())))?;

    let name = match args.one("name")? {
        Some(n) if valid_sandbox_name(&n) => n,
        Some(n) => {
            return Err(Fail::usage(format!(
                "{n:?} is not an sbx name: letters, digits, '.' and '-', at most 63, ending in a \
                 letter or digit"
            )))
        }
        None => model::unique_sandbox_name(&backend.kit.name, &workspace, &[]),
    };
    let ssh_agent = args.has("ssh-agent");
    let tmpfs_volumes = args.has("tmpfs-volumes");

    if let Some(existing) = find(&name)? {
        template_reuse(&name, template.as_deref())?;
        if let Some(refusal) = reuse_refusal(&name, &existing.agent, &backend_name, &backend.agent)
        {
            return Err(Fail::usage(refusal));
        }
        let inspected = sbx::inspect(&name);
        if let (Some((conn, _)), Err(e)) = (&connection, &inspected) {
            return Err(Fail::sandbox(format!(
                "could not read {name}'s kit list ({e:#}) — cannot tell whether it is on \
                 connection {}",
                conn.name
            )));
        }
        if let (Some((conn, _)), Ok(i)) = (&connection, &inspected) {
            let on = connection_of_kits(&i.kits).map(|(c, _)| c);
            if on.as_deref() != Some(conn.name.as_str()) {
                return Err(Fail::usage(format!(
                    "a sandbox named {name} exists, but it is on {} — not connection {}; pick \
                     another --name, or `aisbx rm {name}`",
                    on.map(|c| format!("connection {c}"))
                        .unwrap_or_else(|| "no LiteLLM connection".into()),
                    conn.name
                )));
            }
        }
        match &inspected {
            Ok(i) => {
                if let Some(refusal) = volumes_refusal(&name, &i.kits, tmpfs_volumes) {
                    return Err(Fail::usage(refusal));
                }
            }
            Err(e) if tmpfs_volumes => {
                return Err(Fail::sandbox(format!(
                    "could not read {name}'s kit list ({e:#}) - cannot tell whether its volumes \
                     are in RAM"
                )))
            }
            Err(_) => {}
        }
        if args.has("no-mcp") {
            eprintln!(
                "aisbx: warning: --no-mcp only applies when a sandbox is created; it cannot be \
                 checked or changed on the existing {name}"
            );
        }
        eprintln!("aisbx: {name} exists ({}) — reusing it", existing.status);
        match inspected {
            Ok(inspected) => approve_credentials(&inspected.kits),
            Err(e) => eprintln!("aisbx: could not read the kit list ({e:#}) — no credential check"),
        }
        if !skill_specs.is_empty() {
            let notes =
                skill_select::add(&state::state_dir(), &name, &skill_specs).map_err(select_fail)?;
            warn_all(&notes);
            sync_after_up(&name);
        }
        println!("{name}");
        return Ok(0);
    }
    // Checked before anything is created; recorded once the sandbox exists.
    let (skill_picks, skill_notes) =
        skill_select::prepare(&state::state_dir(), &skill_specs).map_err(select_fail)?;

    let skills =
        match args.one("skills")? {
            Some(s) => Some(sbx::SkillsMode::parse(&s).ok_or_else(|| {
                Fail::usage(format!("--skills {s:?}: off, readonly or readwrite"))
            })?),
            None => backend.shares_skills().then(sbx::skills_default_mode),
        };
    let (cpus, memory) = limit_args(args.one("cpus")?, args.one("memory")?)?;
    let deny_network = args.all("deny-network");
    let home = model::home_dir();
    let extra_mounts = args
        .all("mount")
        .iter()
        .map(|raw| mount_arg(raw, home.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;

    if sbx::network_policy_needs_init() == Some(true) {
        return Err(Fail::sandbox(sbx::POLICY_INIT_HINT));
    }
    warn_about_host(
        backend,
        token.as_deref(),
        connection.as_ref().map(|(c, _)| c),
    );

    let mut kit_dirs: Vec<PathBuf> = vec![backend.kit.dir.clone()];
    // The connection's generated kit goes right after the backend kit it configures.
    if let Some((conn, agent)) = &connection {
        let dir = litellm::ensure_kit(conn, *agent).map_err(|e| Fail::sandbox(format!("{e:#}")))?;
        kit_dirs.push(dir);
        // Claude Code's saved picks ride a kit; OpenCode and Pi have theirs in the files above.
        if let Some(dir) = claude_pins_kit(&litellm::dir(), &name, conn, *agent)? {
            kit_dirs.push(dir);
        }
    }
    for addon in catalog.all_addons() {
        if addons.iter().any(|a| a == &addon.kit.name) {
            kit_dirs.push(addon.kit.dir.clone());
        }
    }
    if let Some(token) = &token {
        let dir = tokens::ensure_kit(token).map_err(|e| Fail::sandbox(format!("{e:#}")))?;
        kit_dirs.push(dir);
    }
    // A refused package cache only warns: the sandbox works without it, only slower.
    let kit_mounts = model::kit_mounts(&addons);
    for note in &kit_mounts.notes {
        eprintln!("aisbx: warning: {note}");
    }
    let mut mounts = kit_mounts.mounts;
    if args.has("git-readonly") {
        match folders::git_guard(&workspace) {
            Some(spec) => mounts.insert(0, spec),
            None => eprintln!("aisbx: warning: --git-readonly: the workspace has no .git folder"),
        }
    }
    mounts.extend(extra_mounts);
    let folder_plan = match args.has("no-host-folders") {
        true => folders::Plan::default(),
        false => host_folders(&state::state_dir(), &workspace)?,
    };
    for note in &folder_plan.notes {
        eprintln!("aisbx: warning: {note}");
    }
    for spec in folder_plan.sbx_specs() {
        if !mounts.contains(&spec) {
            mounts.push(spec);
        }
    }
    let git_guard = args.has("git-readonly") && folders::git_guard(&workspace).is_some();
    if let Some(dir) = folders::ensure_kit(&name, &folder_plan.folders, git_guard)
        .map_err(|e| Fail::sandbox(format!("{e:#}")))?
    {
        kit_dirs.push(dir);
    }
    // This swaps the backend kit too: its copy keeps the folder name, so the agent stays the same.
    let kit_dirs = match tmpfs_volumes {
        false => kit_dirs,
        true => {
            let copies = tmpfs::copies(&kit_dirs).map_err(|e| Fail::sandbox(format!("{e:#}")))?;
            eprintln!("aisbx: {}", ram_note(&kit_dirs, &copies));
            copies
        }
    };
    approve_credentials(&kit_dirs);
    let kit_refs: Vec<&Path> = kit_dirs.iter().map(PathBuf::as_path).collect();
    let static_mcp = static_mcp_for(&args);
    let argv = sbx::create_argv(sbx::Create {
        name: &name,
        agent: &backend.agent,
        // The backend kit is the first of them.
        agent_kit: backend.kit.is_workload.then_some(kit_dirs[0].as_path()),
        kit_dirs: &kit_refs,
        workspace: &workspace,
        mounts: &mounts,
        clone: args.has("clone"),
        ports: &[],
        skills,
        deny_network: &deny_network,
        static_mcp: &static_mcp,
        cpus,
        memory: memory.as_deref(),
        template: template.as_deref(),
    });
    eprintln!("aisbx: {}", argv.join(" "));
    match step(&argv, ssh_agent) {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return Err(Fail::sandbox(format!(
                "sbx create {name} failed ({status})"
            )));
        }
        Err(e) => return Err(Fail::sandbox(format!("could not run sbx: {e}"))),
    }

    // What sbx bakes in and reports nowhere — the same split the dashboard keeps.
    let mut registry = state::Registry::load();
    let account = model::label_account(
        backend,
        token.as_deref(),
        connection.as_ref().map(|(c, _)| c.name.as_str()),
    );
    let label = args.one("label")?.unwrap_or_else(|| {
        model::auto_label(&catalog, &backend.kit.name, account.as_deref(), &addons)
    });
    registry.set_display_name(&name, Some(label));
    registry.set_clone(&name, args.has("clone"));
    registry.set_skills(&name, skills);
    if !static_mcp.is_empty() {
        // The same record the dashboard keeps, so a rebuild repeats the choice.
        registry.set_static_mcp(&name, static_mcp);
    }
    if let Some(token) = &token {
        registry.set_last_token(token);
    }
    if let Some((conn, _)) = &connection {
        registry.set_last_litellm(&conn.name);
    }
    if let Err(e) = registry.save() {
        eprintln!("aisbx: could not record the sandbox's label ({e:#}) — harmless");
    }
    // A selection left under this name by an earlier sandbox is replaced, never inherited.
    let selected = !skill_picks.is_empty();
    match skill_select::replace(&state::state_dir(), &name, skill_picks) {
        Ok(()) => {
            warn_all(&skill_notes);
            if selected {
                sync_after_up(&name);
            }
        }
        Err(e) => eprintln!("aisbx: warning: the skill selection was not recorded ({e:#})"),
    }
    println!("{name}");
    Ok(0)
}

/// The fixed MCP set `up` asks sbx for: none at all with `--no-mcp`, else sbx's default (no flag).
fn static_mcp_for(args: &Args) -> Vec<String> {
    match args.has("no-mcp") {
        true => vec![sbx::STATIC_MCP_NONE.to_string()],
        false => Vec::new(),
    }
}

/// `up`'s sync: its status lines go to stderr (stdout is the name), and a failure is a warning.
fn sync_after_up(name: &str) {
    match skill_select::sync(
        &state::state_dir(),
        name,
        skill_select::SyncMode::Full,
        &mut std::io::stderr(),
    ) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "aisbx: warning: the skill sync into {name} did not fully work; \
             `aisbx skill sync {name}` retries it"
        ),
        Err(e) => eprintln!("aisbx: warning: no skill sync into {name} ({e:#})"),
    }
}

/// What `up` says before it creates anything: a missing secret, and a local policy that lets more
/// out than the kits do.
fn warn_about_host(
    backend: &kits::Backend,
    token: Option<&str>,
    connection: Option<&litellm::Connection>,
) {
    match sbx::secrets() {
        Ok(secrets) => {
            if let Some(conn) = connection.filter(|c| !c.is_local()) {
                if key_stored(Some(&secrets), conn) != Some(true) {
                    eprintln!(
                        "aisbx: warning: no key is stored for connection {} on {}, so the agent \
                         will have no access — aisbx litellm add <file> --key-stdin --replace",
                        conn.name,
                        litellm::host(&conn.url)
                    );
                }
            }
            for service in backend.required_services() {
                if !secrets.has_service(&service) {
                    eprintln!(
                        "aisbx: warning: {service} is not stored on this host, so the agent will \
                         have no access — {}",
                        kits::secret_set_hint(&service)
                    );
                }
            }
            if let Some(token) = token {
                if !secrets.holds(&sbx::named_token_placeholder(token)) {
                    eprintln!(
                        "aisbx: warning: no setup token named {token:?} is stored — add it in the \
                         dashboard (S), or: sbx secret set-custom --host api.anthropic.com \
                         --placeholder {}",
                        sbx::named_token_placeholder(token)
                    );
                }
            }
        }
        Err(e) => eprintln!("aisbx: warning: could not read the stored secrets ({e:#})"),
    }
    match sbx::global_network_allows() {
        Ok(hosts) if !hosts.is_empty() => eprintln!(
            "aisbx: warning: the local network policy allows {} host pattern(s) to every sandbox \
             ({}{}) — the kits' allow-lists are the whole story only under `sbx policy init deny-all`",
            hosts.len(),
            hosts.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
            if hosts.len() > 4 { ", …" } else { "" }
        ),
        Ok(_) => {}
        Err(e) => eprintln!("aisbx: warning: could not read the local network policy ({e:#})"),
    }
}

/// `--mount DIR[:ro]`: a host folder as an extra workspace at its own absolute path, read-write
/// unless `:ro`. Refuses home, a folder holding it, and credential folders.
fn mount_arg(raw: &str, home: Option<&Path>) -> Result<String, Fail> {
    let (path, read_only) = match raw.strip_suffix(":ro") {
        Some(p) => (p, true),
        None => (raw, false),
    };
    let given = PathBuf::from(path);
    if !given.is_absolute() {
        return Err(Fail::usage(format!(
            "--mount {raw:?}: an absolute path, please"
        )));
    }
    let real = given
        .canonicalize()
        .map_err(|e| Fail::usage(format!("--mount {raw:?}: {e}")))?;
    if !real.is_dir() {
        return Err(Fail::usage(format!("--mount {raw:?}: not a directory")));
    }
    if let Some(home) = home.and_then(|h| h.canonicalize().ok()) {
        if let Some(why) = model::mount_refusal(&real, &home) {
            return Err(Fail::usage(format!("--mount {raw:?}: {why}")));
        }
    }
    let text = real
        .to_str()
        .ok_or_else(|| Fail::usage(format!("--mount {raw:?}: not UTF-8")))?;
    if text.contains(':') || text.contains(',') {
        return Err(Fail::usage(format!(
            "--mount {raw:?}: sbx cannot take a path with ':' or ','"
        )));
    }
    Ok(if read_only {
        format!("{text}:ro")
    } else {
        text.to_string()
    })
}

/// `--cpus` / `--memory`: absent is aisbx's default, `auto` is sbx's own, which
/// `sbx::create_argv` spells as no flag. sbx judges the memory size.
fn limit_args(
    cpus: Option<String>,
    memory: Option<String>,
) -> Result<(Option<u32>, Option<String>), Fail> {
    let cpus = match cpus.as_deref() {
        None => None,
        Some("auto") => Some(0),
        Some(c) => Some(
            c.parse::<u32>()
                .map_err(|_| Fail::usage(format!("--cpus {c:?}: a whole number or auto")))?,
        ),
    };
    let memory = memory.map(|m| if m == "auto" { String::new() } else { m });
    Ok((cpus, memory))
}

/// Why an existing sandbox cannot be handed back for this backend: its agent is another one.
fn reuse_refusal(
    name: &str,
    agent: &str,
    backend_name: &str,
    backend_agent: &str,
) -> Option<String> {
    if agent == backend_agent {
        return None;
    }
    Some(format!(
        "a sandbox named {name} exists, but its agent is {agent} — not {backend_name}'s \
         {backend_agent}; pick another --name, or `aisbx rm {name}`"
    ))
}

/// Why an existing sandbox cannot be handed back to an `up` with (`want_ram`) or without
/// `--tmpfs-volumes`: its kit volumes are of the other kind. A kit list with no volume suits both.
fn volumes_refusal(name: &str, kits: &[String], want_ram: bool) -> Option<String> {
    let in_ram = kits.iter().any(|k| tmpfs::is_copy(Path::new(k)));
    let on_disk = kits.iter().any(|k| {
        !tmpfs::is_copy(Path::new(k)) && tmpfs::declares_disk_volumes(Path::new(k)).unwrap_or(false)
    });
    match want_ram {
        true if on_disk => Some(format!(
            "a sandbox named {name} exists with its kit volumes on disk, not in RAM; pick another \
             --name, or `aisbx rm {name}`"
        )),
        false if in_ram => Some(format!(
            "a sandbox named {name} exists with its kit volumes in RAM; pass --tmpfs-volumes, \
             pick another --name, or `aisbx rm {name}`"
        )),
        _ => None,
    }
}

/// What `up --tmpfs-volumes` says about the kits it swapped for their copies (`dirs` → `copies`).
fn ram_note(dirs: &[PathBuf], copies: &[PathBuf]) -> String {
    let moved: Vec<String> = dirs
        .iter()
        .zip(copies)
        .filter(|(dir, copy)| dir != copy)
        .filter_map(|(dir, _)| dir.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    match moved.is_empty() {
        true => "no kit here keeps a volume on disk: nothing to move to RAM".into(),
        false => format!(
            "volumes in RAM: {} (copies in {})",
            moved.join(", "),
            tmpfs::kits_dir().display()
        ),
    }
}

/// `--template REF` (a saved sbx template): always a usage error, before anything is created. sbx
/// builds no sandbox from a saved template under a v3 workload, and every backend is one.
fn template_arg(raw: Option<String>, backend: &str) -> Result<Option<String>, Fail> {
    match raw {
        None => Ok(None),
        Some(reference) => Err(Fail::usage(format!(
            "--template {reference:?}: {backend} is a v3 kit workload, and sbx builds no \
             sandbox from a saved template under one (\"saved image carries no kit descriptor\")"
        ))),
    }
}

/// `--template` only ever creates a sandbox: one that already has the name is refused, not reused.
fn template_reuse(name: &str, template: Option<&str>) -> Result<(), Fail> {
    match template {
        Some(_) => Err(Fail::usage(format!(
            "a sandbox named {name} exists — --template only ever creates a new one; pick another \
             --name, or `aisbx rm {name}`"
        ))),
        None => Ok(()),
    }
}

/// sbx's naming rule, as `model::unique_sandbox_name` produces it: `^[a-zA-Z0-9][a-zA-Z0-9.-]+$`,
/// at most 63 characters, ending in a letter or digit, never `default`.
fn valid_sandbox_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (2..=63).contains(&bytes.len())
        && name != "default"
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'-')
}

// --- run -------------------------------------------------------------------------------------------

/// What a headless run reports with `--report`, besides its exit code.
#[derive(Debug, Default)]
struct Report {
    started: bool,
    timed_out: bool,
    interrupted: bool,
    agent_exit_code: Option<i32>,
    blocked: Vec<String>,
}

fn run(raw: &[String]) -> Outcome {
    let args = Args::parse(
        raw,
        &["no-update", "ssh-agent"],
        &[
            "prompt",
            "prompt-file",
            "model",
            "effort",
            "format",
            "timeout",
            "workdir",
            "approvals",
            "report",
        ],
    )?;
    let name = args.name()?;
    let prompt = match (args.one("prompt")?, args.one("prompt-file")?) {
        (Some(p), None) => p,
        (None, Some(f)) if f == "-" => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                .map_err(|e| Fail::usage(format!("reading the prompt from stdin: {e}")))?;
            text
        }
        (None, Some(f)) => std::fs::read_to_string(&f)
            .map_err(|e| Fail::usage(format!("reading the prompt from {f}: {e}")))?,
        (Some(_), Some(_)) => return Err(Fail::usage("--prompt or --prompt-file, not both")),
        (None, None) => {
            return Err(Fail::usage(
                "no prompt: pass --prompt TEXT or --prompt-file FILE",
            ))
        }
    };
    if prompt.trim().is_empty() {
        return Err(Fail::usage("the prompt is empty"));
    }
    let format = match args.one("format")? {
        Some(f) => OutputFormat::parse(&f)
            .ok_or_else(|| Fail::usage(format!("--format {f:?}: json, stream-json or text")))?,
        None => OutputFormat::Json,
    };
    let timeout = match args.one("timeout")? {
        Some(t) => Some((
            parse_duration(&t)
                .ok_or_else(|| Fail::usage(format!("--timeout {t:?}: e.g. 90s, 45m, 4h, 1h30m")))?,
            t,
        )),
        None => None,
    };
    let dismiss = match args.one("approvals")?.as_deref() {
        None | Some("dismiss") => true,
        Some("keep") => false,
        Some(other) => {
            return Err(Fail::usage(format!(
                "--approvals {other:?}: dismiss or keep"
            )))
        }
    };
    let ssh_agent = args.has("ssh-agent");
    let workdir = args.one("workdir")?.map(PathBuf::from);
    let report_path = args.one("report")?.map(PathBuf::from);

    let entry = find(&name)?.ok_or_else(|| Fail::sandbox(format!("no sandbox named {name}")))?;
    let model = args.one("model")?;
    let effort = args.one("effort")?;
    let print_timeout = timeout.as_ref().map_or("0".to_string(), |(_, t)| t.clone());
    let agent_argv = agent_argv(
        &name,
        &entry.agent,
        &HeadlessRun {
            model: model.as_deref(),
            effort: effort.as_deref(),
            format,
            print_timeout: &print_timeout,
            extra: &args.trailing,
        },
    )?;

    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = Arc::clone(&interrupted);
        // Ctrl-C ends the `sbx exec` client too, but the agent in the sandbox keeps going, so
        // `watch` stops it.
        let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    }

    // Link the kits' skills before the agent starts: a kit's startup hook races the agent's first
    // skill scan. Never fatal, and `--no-update` does not skip it.
    // The skill-repo sync needs the selection, not the kits: it runs also when `inspect` fails.
    let mut connection = None;
    let addons = match sbx::inspect(&name) {
        Ok(inspected) => {
            approve_credentials(&inspected.kits);
            connection = connection_of_kits(&inspected.kits);
            skill_source::addons_from_kit_dirs(&inspected.kits)
        }
        Err(e) => {
            eprintln!(
                "aisbx: could not read the kit list ({e:#}) — no host-skills sync, no credential \
                 check"
            );
            Vec::new()
        }
    };
    for source in skill_source::sources_for(&name, &addons) {
        let what = source.step_label();
        match step(&source.install_argv(&name), ssh_agent) {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("aisbx: {what} exited {s} — running anyway"),
            Err(e) => eprintln!("aisbx: {what} did not run ({e}) — running anyway"),
        }
    }

    // Put the connection's model list and picks into the agent's config before it starts. Local
    // work, so `--no-update` does not skip it.
    if let Some(argv) = connection.and_then(|(name_, agent)| {
        push_argv_in(&litellm::dir(), &name, &litellm::load(&name_)?, agent)
    }) {
        match step(&argv, ssh_agent) {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("aisbx: writing the model list exited {s} — running anyway"),
            Err(e) => eprintln!("aisbx: the model list was not written ({e}) — running anyway"),
        }
    }

    // The agent applies a self-update only at its next launch, so update it before the run.
    if !args.has("no-update") {
        if let Some(update) = kits::agent_update(&entry.agent) {
            let argv = sbx::exec_argv(&name, update);
            match step(&argv, ssh_agent) {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    eprintln!("aisbx: the agent update exited {s} — running on what is installed")
                }
                Err(e) => eprintln!("aisbx: could not run the agent update ({e}) — running anyway"),
            }
        }
    }

    let run_dir = format!(
        "/tmp/aisbx-run-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default(),
        std::process::id()
    );
    upload_prompt(&name, &run_dir, &prompt, ssh_agent)?;

    let started_at = Instant::now();
    let launch = sbx::headless_exec_argv(&name, workdir.as_deref(), &run_dir, &agent_argv);
    let mut child = sbx_command(&launch, ssh_agent)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Fail::sandbox(format!("could not run sbx: {e}")))?;

    let mut report = Report::default();
    let deadline = timeout.as_ref().map(|(d, _)| started_at + *d);
    let status = watch(
        &mut child,
        deadline,
        &interrupted,
        &mut report,
        &name,
        &run_dir,
    );

    let stopped = report.timed_out || report.interrupted;
    report.started = stopped || quiet(&sbx::headless_started_argv(&name, &run_dir));
    if !stopped {
        quiet(&sbx::headless_cleanup_argv(&name, &run_dir));
    }
    report.agent_exit_code = status.and_then(|s| s.code());
    report.blocked = held_requests(&name, dismiss);

    let code = if report.interrupted {
        EXIT_INTERRUPTED
    } else if report.timed_out {
        EXIT_TIMEOUT
    } else if !report.started {
        EXIT_SANDBOX
    } else {
        report
            .agent_exit_code
            .map_or(EXIT_SANDBOX, |c| u8::try_from(c).unwrap_or(1))
    };
    match code {
        EXIT_TIMEOUT => eprintln!(
            "aisbx: {name}: stopped the agent after {}",
            timeout.map(|(_, t)| t).unwrap_or_default()
        ),
        EXIT_INTERRUPTED => eprintln!("aisbx: {name}: interrupted — the agent was stopped"),
        EXIT_SANDBOX => eprintln!("aisbx: {name}: the agent never started — the sandbox failed"),
        _ => {}
    }
    if let Some(path) = report_path {
        write_report(
            &path,
            &name,
            &entry.agent,
            code,
            started_at.elapsed(),
            &report,
        );
    }
    Ok(code)
}

/// Approve the kits' credentials in sbx's bindings file (see [`crate::bindings`]). Reported on
/// stderr and never fatal: without it the sandbox only runs with those credentials withheld.
fn approve_credentials<P: AsRef<Path>>(dirs: &[P]) {
    if let Some((err, text)) = bindings::note(&bindings::ensure(dirs)) {
        eprintln!("aisbx: {}{text}", if err { "warning: " } else { "" });
    }
}

/// Copy the prompt into the sandbox's run dir, through `sbx exec -i`'s stdin.
fn upload_prompt(name: &str, run_dir: &str, prompt: &str, ssh_agent: bool) -> Result<(), Fail> {
    let mut child = sbx_command(&sbx::prompt_upload_argv(name, run_dir), ssh_agent)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Fail::sandbox(format!("could not run sbx: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|e| Fail::sandbox(format!("handing the prompt to {name}: {e}")))?;
    }
    match child.wait() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Fail::sandbox(format!(
            "could not put the prompt into {name} ({s})"
        ))),
        Err(e) => Err(Fail::sandbox(format!("handing the prompt to {name}: {e}"))),
    }
}

/// Wait for the agent, enforcing the deadline and Ctrl-C. On either, stop the agent inside the
/// sandbox (killing the `sbx exec` client leaves it running), then the sandbox if it stays.
fn watch(
    child: &mut Child,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
    report: &mut Report,
    name: &str,
    run_dir: &str,
) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        let timed_out = deadline.is_some_and(|d| Instant::now() >= d);
        let stop = interrupted.load(Ordering::SeqCst);
        if timed_out || stop {
            report.timed_out = timed_out && !stop;
            report.interrupted = stop;
            quiet(&sbx::headless_kill_argv(name, run_dir));
            let grace = Instant::now() + Duration::from_secs(15);
            while Instant::now() < grace {
                if let Ok(Some(status)) = child.try_wait() {
                    return Some(status);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            eprintln!("aisbx: {name}: the agent did not stop — stopping the sandbox");
            let _ = child.kill();
            quiet(&sbx::stop_argv(name));
            return child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The requests `name` holds, out of one read of every pending request, or the warning to print
/// when the read failed. Never a quiet empty list: a report would then claim nothing was blocked.
fn held_by(
    name: &str,
    read: anyhow::Result<Vec<sbx::Approval>>,
) -> Result<Vec<sbx::Approval>, String> {
    match read {
        Ok(all) => Ok(all.into_iter().filter(|a| a.sandbox == name).collect()),
        Err(e) => Err(format!(
            "aisbx: warning: could not read the requests {name} holds for approval ({e:#})"
        )),
    }
}

/// The requests this sandbox is holding for approval, reported on stderr — and dismissed unless
/// the caller asked to keep them for the dashboard.
fn held_requests(name: &str, dismiss: bool) -> Vec<String> {
    let mine = match held_by(name, sbx::approvals()) {
        Ok(mine) => mine,
        Err(warning) => {
            eprintln!("{warning}");
            return Vec::new();
        }
    };
    for request in &mine {
        eprintln!(
            "aisbx: {name}: blocked {} ({}){}",
            request.title,
            request.reason(),
            if dismiss {
                " — dismissed"
            } else {
                " — left for approval (aisbx: y)"
            }
        );
        if dismiss && request.offers("dismiss") {
            quiet(&sbx::approval_respond_argv(&request.id, "dismiss"));
        }
    }
    mine.into_iter().map(|a| a.title).collect()
}

fn write_report(
    path: &Path,
    name: &str,
    agent: &str,
    code: u8,
    elapsed: Duration,
    report: &Report,
) {
    let doc = serde_json::json!({
        "sandbox": name,
        "agent": agent,
        "exit_code": code,
        "agent_exit_code": report.agent_exit_code,
        "started": report.started,
        "timed_out": report.timed_out,
        "interrupted": report.interrupted,
        "duration_ms": elapsed.as_millis() as u64,
        "blocked_hosts": report.blocked,
    });
    let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    if let Err(e) = std::fs::write(path, text + "\n") {
        eprintln!(
            "aisbx: could not write the report to {}: {e}",
            path.display()
        );
    }
}

/// `90s`, `45m`, `4h`, `1h30m`: the Go duration subset, because agy's `--print-timeout` gets the
/// same text. Zero is refused.
fn parse_duration(text: &str) -> Option<Duration> {
    let mut total = 0u64;
    let mut number = String::new();
    let mut any = false;
    for c in text.trim().chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let value: u64 = number.parse().ok()?;
        number.clear();
        total += value
            * match c {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => return None,
            };
        any = true;
    }
    (any && number.is_empty() && total > 0).then(|| Duration::from_secs(total))
}

// --- ls, status, stop, rm --------------------------------------------------------------------------

fn ls(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    if let Some(extra) = args.positional.first().or(args.trailing.first()) {
        return Err(Fail::usage(format!("ls takes no argument: {extra}")));
    }
    let entries = sbx::list().map_err(|e| Fail::sandbox(one_line(&format!("{e:#}"))))?;
    let registry = state::Registry::load();
    let out = render_ls(&entries, &registry, args.has("json"));
    if !out.is_empty() {
        println!("{out}");
    }
    Ok(0)
}

fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_ls(entries: &[sbx::LsEntry], registry: &state::Registry, json: bool) -> String {
    if json {
        let mut rows = Vec::new();
        for e in entries {
            let label = registry.display_name(&e.name);
            rows.push(serde_json::json!({
                "name": e.name,
                "status": e.status,
                "agent": e.agent,
                "workspaces": e.workspaces,
                "label": label,
            }));
        }
        return serde_json::to_string_pretty(&rows).unwrap_or_default();
    }
    let cell = |text: &str| text.replace(['\t', '\n', '\r'], " ");
    let mut lines = Vec::new();
    for e in entries {
        let primary = e.workspaces.first().map_or("", String::as_str);
        let label = registry.display_name(&e.name).unwrap_or("");
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}",
            cell(&e.name),
            cell(&e.status),
            cell(&e.agent),
            cell(primary),
            cell(label)
        ));
    }
    lines.join("\n")
}

fn status(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    let name = args.name()?;
    let entry = find(&name)?;
    let pending: Vec<String> = match held_by(&name, sbx::approvals()) {
        Ok(mine) => mine.into_iter().map(|a| a.title).collect(),
        Err(warning) => {
            eprintln!("{warning}");
            Vec::new()
        }
    };
    let kits_list = entry
        .as_ref()
        .and_then(|_| sbx::inspect(&name).ok())
        .map(|i| i.kits)
        .unwrap_or_default();
    if args.has("json") {
        let doc = serde_json::json!({
            "name": name,
            "exists": entry.is_some(),
            "status": entry.as_ref().map(|e| e.status.clone()),
            // Kept running after its sessions end.
            "detached": entry.as_ref().map(|e| e.detached),
            "agent": entry.as_ref().map(|e| e.agent.clone()),
            "workspaces": entry.as_ref().map(|e| e.workspaces.clone()).unwrap_or_default(),
            "kits": kits_list,
            "pending_approvals": pending,
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else if let Some(e) = &entry {
        let detached = if e.detached { " (detached)" } else { "" };
        println!(
            "{name}: {}{detached} · agent {} · {}",
            e.status,
            e.agent,
            e.workspaces.join(" ")
        );
        for kit in &kits_list {
            println!("  kit  {kit}");
        }
        for title in &pending {
            println!("  held {title}");
        }
    } else {
        println!("{name}: no such sandbox");
    }
    Ok(if entry.is_some() { 0 } else { 1 })
}

fn stop(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &[], &[])?;
    let name = args.name()?;
    match step(&sbx::stop_argv(&name), false) {
        Ok(s) if s.success() => Ok(0),
        Ok(s) => Err(Fail::sandbox(format!("sbx stop {name} failed ({s})"))),
        Err(e) => Err(Fail::sandbox(format!("could not run sbx: {e}"))),
    }
}

fn rm(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &[], &[])?;
    let name = args.name()?;
    match step(&sbx::rm_argv(&name), false) {
        Ok(s) if s.success() => {}
        Ok(s) => return Err(Fail::sandbox(format!("sbx rm {name} failed ({s})"))),
        Err(e) => return Err(Fail::sandbox(format!("could not run sbx: {e}"))),
    }
    // As the dashboard's delete. A named token stays: other sandboxes may use it.
    let mut registry = state::Registry::load();
    registry.forget(&name);
    let _ = registry.save();
    let _ = models::remove_kit(&name);
    let _ = folders::remove_kit(&name);
    // Its skill selection too: a later sandbox of this name starts with none.
    if let Err(e) = skill_select::forget(&state::state_dir(), &name) {
        eprintln!("aisbx: warning: the skill selection of {name} was not dropped ({e:#})");
    }
    Ok(0)
}

// --- litellm -------------------------------------------------------------------------------------

/// `aisbx litellm ls | add | rm | fetch` — the LiteLLM connections ([`litellm`]). A key goes to sbx on
/// stdin, never in argv or a file, and `connection.json` is written only once sbx stored it.
/// The folders `up` mounts beside `workspace`: its companion.json's and the host's list. A list
/// that does not load stops `up` (exit 2), as a refused `--mount` does.
fn host_folders(state: &Path, workspace: &Path) -> Result<folders::Plan, Fail> {
    let config =
        folders::load(&folders::config_path(state)).map_err(|e| Fail::usage(format!("{e:#}")))?;
    Ok(folders::plan(
        workspace,
        &config.folders,
        model::home_dir().as_deref(),
    ))
}

// --- folders ---------------------------------------------------------------------------------------

fn folders_cmd(state: &Path, raw: &[String], out: &mut dyn Write) -> Outcome {
    let Some((sub, rest)) = raw.split_first() else {
        return Err(Fail::usage("folders needs ls, add or rm"));
    };
    let path = folders::config_path(state);
    let typed = |args: &Args| -> Result<PathBuf, Fail> {
        let [dir] = args.positional.as_slice() else {
            return Err(Fail::usage(format!("folders {sub} takes one folder")));
        };
        folders::typed_path(dir, model::home_dir().as_deref())
            .map_err(|e| Fail::usage(format!("folders {sub}: {e}")))
    };
    match sub.as_str() {
        "ls" => {
            let args = Args::parse(rest, &["json"], &[])?;
            if !args.positional.is_empty() {
                return Err(Fail::usage("folders ls takes no arguments"));
            }
            let config = folders::load(&path).map_err(|e| Fail::usage(format!("{e:#}")))?;
            let text = match args.has("json") {
                true => format!(
                    "{}\n",
                    serde_json::to_string_pretty(&config.folders).unwrap_or_default()
                ),
                false => config
                    .folders
                    .iter()
                    .map(|f| {
                        let purpose = match f.purpose.as_str() {
                            "" => String::new(),
                            p => format!("  {p}"),
                        };
                        format!("{}\t{}{purpose}\n", f.access.as_str(), f.path.display())
                    })
                    .collect(),
            };
            let _ = out.write_all(text.as_bytes());
            Ok(0)
        }
        "add" => {
            let args = Args::parse(rest, &["write"], &["purpose"])?;
            let entry = folders::Entry {
                path: typed(&args)?,
                access: match args.has("write") {
                    true => folders::Access::Write,
                    false => folders::Access::Read,
                },
                purpose: args.one("purpose")?.unwrap_or_default().trim().to_string(),
            };
            folders::check_entry(&entry).map_err(|e| Fail::usage(format!("folders add: {e}")))?;
            if !entry.path.is_dir() {
                eprintln!(
                    "aisbx: warning: {} is not a folder now; a sandbox mounts it once it is",
                    entry.path.display()
                );
            }
            folders::put(state, entry).map_err(|e| Fail::usage(format!("{e:#}")))?;
            eprintln!("aisbx: sandboxes mount it from their next create or rebuild");
            Ok(0)
        }
        "rm" => {
            let args = Args::parse(rest, &[], &[])?;
            let dir = typed(&args)?;
            match folders::remove(state, &dir).map_err(|e| Fail::usage(format!("{e:#}")))? {
                true => Ok(0),
                false => Err(Fail::failed(format!(
                    "{} is not in the list",
                    dir.display()
                ))),
            }
        }
        other => Err(Fail::usage(format!(
            "unknown folders command: {other} (ls, add or rm)"
        ))),
    }
}

fn litellm_cmd(raw: &[String]) -> Outcome {
    let Some((sub, rest)) = raw.split_first() else {
        return Err(Fail::usage("litellm needs ls, add, rm or fetch"));
    };
    match sub.as_str() {
        "ls" => litellm_ls(rest),
        "add" => litellm_add(rest),
        "rm" => litellm_rm(rest),
        "fetch" => litellm_fetch(rest),
        other => Err(Fail::usage(format!(
            "unknown litellm command: {other} (ls, add, rm or fetch)"
        ))),
    }
}

fn litellm_ls(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    if !args.positional.is_empty() {
        return Err(Fail::usage("litellm ls takes no arguments"));
    }
    let all: Vec<litellm::Connection> = (litellm::load_all().into_iter())
        .filter(|c| !c.is_local())
        .collect();
    // Nothing to look up without a connection, so an empty list needs no daemon.
    let secrets = match all.is_empty() {
        true => Ok(sbx::Secrets::default()),
        false => sbx::secrets(),
    };
    let registry = state::Registry::load();
    let json = args.has("json");
    let (out, outcome) = litellm_ls_report(&all, secrets, registry.last_litellm(), json);
    print!("{out}");
    if all.is_empty() && !json {
        eprintln!(
            "aisbx: no LiteLLM connection — add one: aisbx litellm add <file.json> --key-stdin"
        );
    }
    outcome
}

/// `litellm ls`'s stdout and exit: when the secrets cannot be read, the connections are still
/// printed (key state unknown) and the command fails like any failed sbx call (125).
fn litellm_ls_report(
    all: &[litellm::Connection],
    secrets: anyhow::Result<sbx::Secrets>,
    last: Option<&str>,
    json: bool,
) -> (String, Outcome) {
    match secrets {
        Ok(s) => (litellm_ls_output(all, Some(&s), last, json), Ok(0)),
        Err(e) => (
            litellm_ls_output(all, None, last, json),
            Err(Fail::sandbox(format!(
                "could not read the stored secrets ({e:#}) — key state unknown"
            ))),
        ),
    }
}

/// Is the connection's key stored — the global row under its placeholder, targeted at its host?
/// `None` when the secrets could not be read.
fn key_stored(secrets: Option<&sbx::Secrets>, conn: &litellm::Connection) -> Option<bool> {
    let host = litellm::host(&conn.url);
    secrets.map(|s| {
        s.litellm_key(&conn.name)
            .is_some_and(|row| row.targets.iter().any(|t| t.trim() == host))
    })
}

/// `litellm ls`'s stdout: one JSON array with `--json`, else one line per connection.
fn litellm_ls_output(
    all: &[litellm::Connection],
    secrets: Option<&sbx::Secrets>,
    last: Option<&str>,
    json: bool,
) -> String {
    if json {
        let rows: Vec<serde_json::Value> = all
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "url": c.url,
                    "models": litellm::harvested_models(&c.name),
                    "key_stored": key_stored(secrets, c),
                    "last_used": last == Some(c.name.as_str()),
                })
            })
            .collect();
        return format!(
            "{}\n",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    }
    let mut out = String::new();
    for c in all {
        let key = match key_stored(secrets, c) {
            Some(true) => "key stored",
            Some(false) => "key missing",
            None => "key ?",
        };
        let mark = match last == Some(c.name.as_str()) {
            true => "  (last used)",
            false => "",
        };
        out.push_str(&format!(
            "{}  {}  {} model(s)  {key}{mark}\n",
            c.name,
            c.url,
            litellm::harvested_models(&c.name)
        ));
    }
    out
}

/// A connection file: its name and URL. It reads `default_model` and `models`, which older files
/// carry, only to note that they are ignored.
#[derive(serde::Deserialize, Debug)]
struct ConnectionFile {
    name: String,
    url: String,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    default_model: Option<serde_json::Value>,
    #[serde(default)]
    models: Option<serde_json::Value>,
}

impl ConnectionFile {
    /// Does the file carry keys a connection no longer has?
    fn has_models(&self) -> bool {
        self.default_model.is_some() || self.models.is_some()
    }
}

/// A connection file, parsed and checked before anything is written or sbx is called, and
/// whether it carries the model keys a connection no longer has.
fn parse_connection(text: &str) -> Result<(litellm::Connection, bool), Fail> {
    let file: ConnectionFile = serde_json::from_str(text)
        .map_err(|e| Fail::usage(format!("not a connection file: {e}")))?;
    let models = file.has_models();
    let conn = litellm::Connection {
        name: file.name,
        url: litellm::normalize_url(&file.url),
        engine: file.engine,
    };
    litellm::validate(&conn).map_err(|e| Fail::usage(format!("connection refused: {e:#}")))?;
    Ok((conn, models))
}

/// A new connection never overwrites one of the same name; only `--replace` does.
fn add_refusal(name: &str, exists: bool, replace: bool) -> Result<(), Fail> {
    match exists && !replace {
        true => Err(Fail::usage(format!(
            "connection {name} exists — pass --replace to overwrite it"
        ))),
        false => Ok(()),
    }
}

/// A replaced connection on another host needs its key again: sbx cannot read the stored key back.
fn moved_host_refusal(
    old: Option<&litellm::Connection>,
    new: &litellm::Connection,
    key_given: bool,
) -> Result<(), Fail> {
    match old {
        Some(old) if !key_given && litellm::moves_host(old, new) => Err(Fail::usage(format!(
            "connection {} moves from {} to {} — pass its key again with --key-stdin",
            new.name,
            litellm::host(&old.url),
            litellm::host(&new.url)
        ))),
        _ => Ok(()),
    }
}

/// The key from stdin, without its trailing newline. Never printed.
fn read_key(input: &mut impl std::io::Read) -> Result<String, Fail> {
    let mut key = String::new();
    std::io::Read::read_to_string(input, &mut key)
        .map_err(|e| Fail::usage(format!("reading the key from stdin: {e}")))?;
    let key = key.trim_end_matches(['\n', '\r']).to_string();
    if key.trim().is_empty() {
        return Err(Fail::usage("--key-stdin: no key on stdin"));
    }
    Ok(key)
}

/// [`step`] with `input` written to the child's stdin (then closed) — how a key reaches sbx.
fn step_with_stdin(argv: &[String], input: &str) -> std::io::Result<ExitStatus> {
    let mut child = sbx_command(argv, false)
        .stdin(Stdio::piped())
        .stdout(stderr_stdio())
        .stderr(Stdio::inherit())
        .spawn()?;
    let written = match child.stdin.take() {
        Some(mut pipe) => writeln!(pipe, "{input}"),
        None => Ok(()),
    };
    let status = child.wait();
    written?;
    status
}

fn litellm_add(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["key-stdin", "replace"], &[])?;
    let file = match args.positional.as_slice() {
        [file] => file.clone(),
        [] => return Err(Fail::usage("which connection file? Pass a JSON file")),
        _ => return Err(Fail::usage("one connection file at a time")),
    };
    let text = std::fs::read_to_string(&file).map_err(|e| Fail::usage(format!("{file}: {e}")))?;
    let (conn, models) = parse_connection(&text)?;
    if conn.is_local() {
        return Err(Fail::usage(
            "that is an engine on this computer — add it with `aisbx local add --port PORT`",
        ));
    }
    if models {
        eprintln!(
            "aisbx: note: the file's \"models\" and \"default_model\" are ignored — every \
             sandbox reads the models from the gateway, and the agent picks one"
        );
    }
    let existed = litellm::exists(&conn.name);
    add_refusal(&conn.name, existed, args.has("replace"))?;
    moved_host_refusal(
        litellm::load(&conn.name).as_ref(),
        &conn,
        args.has("key-stdin"),
    )?;
    let key = match args.has("key-stdin") {
        true => Some(read_key(&mut std::io::stdin())?),
        false => None,
    };
    let host = litellm::host(&conn.url);
    if let Some(key) = key {
        let argv = sbx::secret_set_litellm_key_argv(host, &litellm::placeholder(&conn.name));
        eprintln!("aisbx: {} (the key on stdin)", argv.join(" "));
        // set-custom first: a connection is written only once its key is stored.
        match step_with_stdin(&argv, &key) {
            Ok(s) if s.success() => {}
            Ok(s) => {
                return Err(Fail::sandbox(format!(
                    "sbx secret set-custom failed ({s}) — connection {} not written",
                    conn.name
                )))
            }
            Err(e) => return Err(Fail::sandbox(format!("could not run sbx: {e}"))),
        }
    } else {
        eprintln!(
            "aisbx: note: no key stored for connection {} — `aisbx litellm add {file} \
             --key-stdin --replace` stores it",
            conn.name
        );
    }
    litellm::save(&conn).map_err(|e| Fail::failed(format!("{e:#}")))?;
    if !existed {
        let mut registry = state::Registry::load();
        registry.set_last_litellm(&conn.name);
        if let Err(e) = registry.save() {
            eprintln!("aisbx: could not record the last used connection ({e:#}) — harmless");
        }
    }
    eprintln!("aisbx: connection {} saved ({host})", conn.name);
    if args.has("key-stdin") {
        if let Err(e) = fetch_models(&conn) {
            eprintln!(
                "aisbx: {} — `aisbx litellm fetch {}` tries again",
                e.message, conn.name
            );
        }
    }
    Ok(0)
}

fn litellm_fetch(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &[], &[])?;
    let name = args.name()?;
    let conn =
        litellm::load(&name).ok_or_else(|| Fail::usage(format!("no LiteLLM connection {name}")))?;
    match conn.is_local() {
        true => fetch_local(&conn, None)?,
        false => fetch_models(&conn)?,
    }
    Ok(0)
}

// --- local engines ---------------------------------------------------------------------------

fn local_cmd(raw: &[String]) -> Outcome {
    let Some((sub, rest)) = raw.split_first() else {
        return Err(Fail::usage("local needs discover, ls, add, fetch or rm"));
    };
    match sub.as_str() {
        "discover" => local_discover(rest),
        "ls" => local_ls(rest),
        "add" => local_add(rest),
        "fetch" => local_fetch(rest),
        "rm" => litellm_rm(rest),
        other => Err(Fail::usage(format!(
            "unknown local command: {other} (discover, ls, add, fetch or rm)"
        ))),
    }
}

/// The local connection on `port`, among `all`.
fn engine_on(all: &[litellm::Connection], port: u16) -> Option<&litellm::Connection> {
    all.iter().find(|c| litellm::port(&c.url) == Some(port))
}

fn local_discover(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    if !args.positional.is_empty() {
        return Err(Fail::usage("local discover takes no arguments"));
    }
    let found = crate::local::discover();
    let all = litellm::load_all();
    print!("{}", discover_output(&found, &all, args.has("json")));
    if found.is_empty() && !args.has("json") {
        eprintln!(
            "aisbx: no engine answers on this computer — start one, or name its port: aisbx \
             local add --port PORT"
        );
    }
    Ok(0)
}

/// `local discover`'s stdout: one line per engine found, or one JSON array. Never a key.
fn discover_output(
    found: &[crate::local::Found],
    all: &[litellm::Connection],
    json: bool,
) -> String {
    if json {
        let rows: Vec<serde_json::Value> = (found.iter())
            .map(|f| {
                serde_json::json!({
                    "engine": f.engine.slug(),
                    "port": f.port,
                    "url": f.url(),
                    "models": f.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                    "context": f.models.iter().filter_map(|m| m.limits.context).min(),
                    "anthropic_api": f.engine.anthropic(),
                    "needs_key": f.needs_key,
                    "key_found": f.key.is_some(),
                    "connection": engine_on(all, f.port).map(|c| c.name.clone()),
                })
            })
            .collect();
        return format!(
            "{}\n",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    }
    let mut out = String::new();
    for f in found {
        let key = match (&f.key_source, f.needs_key) {
            (Some(source), _) => format!("  key from {source}"),
            (None, true) => "  wants a key".to_string(),
            (None, false) => String::new(),
        };
        let added = match engine_on(all, f.port) {
            Some(c) => format!("  (engine {})", c.name),
            None => String::new(),
        };
        out.push_str(&format!("{}{key}{added}\n", f.summary()));
    }
    out
}

fn local_ls(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    if !args.positional.is_empty() {
        return Err(Fail::usage("local ls takes no arguments"));
    }
    let all: Vec<litellm::Connection> = (litellm::load_all().into_iter())
        .filter(litellm::Connection::is_local)
        .collect();
    let secrets = match all.is_empty() {
        true => Ok(sbx::Secrets::default()),
        false => sbx::secrets(),
    };
    let (secrets, outcome) = match secrets {
        Ok(s) => (Some(s), Ok(0)),
        Err(e) => (
            None,
            Err(Fail::sandbox(format!(
                "could not read the stored secrets ({e:#}) — key state unknown"
            ))),
        ),
    };
    print!(
        "{}",
        local_ls_output(&all, secrets.as_ref(), args.has("json"))
    );
    if all.is_empty() && !args.has("json") {
        eprintln!("aisbx: no local engine — add one: aisbx local add");
    }
    outcome
}

/// `local ls`'s stdout: one line per engine, or one JSON array.
fn local_ls_output(
    all: &[litellm::Connection],
    secrets: Option<&sbx::Secrets>,
    json: bool,
) -> String {
    if json {
        let rows: Vec<serde_json::Value> = (all.iter())
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "url": c.url,
                    "engine": c.engine,
                    "models": litellm::harvested_models(&c.name),
                    "key_stored": key_stored(secrets, c),
                })
            })
            .collect();
        return format!(
            "{}\n",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    }
    let mut out = String::new();
    for c in all {
        let key = match key_stored(secrets, c) {
            Some(true) => "key stored",
            Some(false) => "no key",
            None => "key ?",
        };
        let engine = (c.engine.as_deref())
            .and_then(crate::local::Engine::parse)
            .map_or("engine ?", crate::local::Engine::label);
        out.push_str(&format!(
            "{}  {}  {engine}  {} model(s)  {key}\n",
            c.name,
            c.url,
            litellm::harvested_models(&c.name)
        ));
    }
    out
}

/// Store a connection's key with sbx, on stdin: the half that must succeed before
/// `connection.json` is written.
fn store_key(conn: &litellm::Connection, key: &str) -> Result<(), Fail> {
    let argv = sbx::secret_set_litellm_key_argv(
        litellm::host(&conn.url),
        &litellm::placeholder(&conn.name),
    );
    eprintln!("aisbx: {} (the key on stdin)", argv.join(" "));
    match step_with_stdin(&argv, key) {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Fail::sandbox(format!(
            "sbx secret set-custom failed ({s}) — {} not written",
            conn.name
        ))),
        Err(e) => Err(Fail::sandbox(format!("could not run sbx: {e}"))),
    }
}

fn local_add(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["key-stdin", "replace"], &["port"])?;
    let wanted = match args.positional.as_slice() {
        [] => None,
        [name] if litellm::valid_name(name) => Some(name.clone()),
        [_] => return Err(Fail::usage("a name is 1–32 of a–z, 0–9 and “-”")),
        _ => return Err(Fail::usage("one engine at a time")),
    };
    let port = match args.one("port")? {
        Some(p) => Some(
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| Fail::usage(format!("--port {p:?}: a port of 1–65535")))?,
        ),
        None => None,
    };
    let all = litellm::load_all();
    let found = crate::local::discover();
    let found = pick_found(&found, &all, port, args.has("replace"))?;
    let key = match args.has("key-stdin") {
        true => Some(read_key(&mut std::io::stdin())?),
        false => found.key.clone(),
    };
    if found.needs_key && key.is_none() {
        return Err(Fail::usage(format!(
            "the engine on localhost:{} wants a key that aisbx could not read — pass it with \
             --key-stdin",
            found.port
        )));
    }
    let taken = |n: &str| all.iter().any(|c| c.name == n);
    let name = wanted.unwrap_or_else(|| found.default_name(&taken));
    let existed = litellm::exists(&name);
    add_refusal(&name, existed, args.has("replace"))?;
    if let Some(old) = all
        .iter()
        .find(|c| c.name == name)
        .filter(|c| !c.is_local())
    {
        return Err(Fail::usage(format!(
            "{} is a LiteLLM connection — pick another name",
            old.name
        )));
    }
    let mut conn = litellm::Connection::new(&name, found.url());
    conn.engine = Some(found.engine.slug().to_string());
    litellm::validate(&conn).map_err(|e| Fail::usage(format!("{e:#}")))?;
    if let Some(key) = &key {
        if let Some(source) = found.key_source.as_ref().filter(|_| !args.has("key-stdin")) {
            eprintln!("aisbx: the key comes from {source}");
        }
        store_key(&conn, key)?;
    }
    litellm::save(&conn).map_err(|e| Fail::failed(format!("{e:#}")))?;
    eprintln!(
        "aisbx: engine {name} saved: {} on {}",
        found.engine.label(),
        litellm::place(&conn.url)
    );
    if let Err(e) = fetch_local(&conn, key.as_deref()) {
        eprintln!(
            "aisbx: {} — `aisbx local fetch {name}` tries again",
            e.message
        );
    }
    println!("{name}");
    Ok(0)
}

/// The engine `local add` adds: the one on `port`, else the only one found that no engine is on
/// yet (with `--replace`, also one already added).
fn pick_found(
    found: &[crate::local::Found],
    all: &[litellm::Connection],
    port: Option<u16>,
    replace: bool,
) -> Result<crate::local::Found, Fail> {
    if let Some(port) = port {
        if let Some(f) = found.iter().find(|f| f.port == port) {
            return Ok(f.clone());
        }
        let probed = crate::local::discover_with(&[], &[port], &|_| None);
        return probed.into_iter().next().ok_or_else(|| {
            Fail::usage(format!(
                "no engine answers OpenAI's /v1/models on localhost:{port}"
            ))
        });
    }
    let new: Vec<&crate::local::Found> = (found.iter())
        .filter(|f| replace || engine_on(all, f.port).is_none())
        .collect();
    match new.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(Fail::usage(
            "no engine found that is not added yet — name its port: --port PORT",
        )),
        more => Err(Fail::usage(format!(
            "{} engines found: {} — pick one: --port PORT",
            more.len(),
            more.iter()
                .map(|f| format!("{} on {}", f.engine.label(), f.port))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn local_fetch(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["key-stdin"], &[])?;
    let name = args.name()?;
    let conn = (litellm::load(&name).filter(litellm::Connection::is_local))
        .ok_or_else(|| Fail::usage(format!("no local engine {name}")))?;
    let key = match args.has("key-stdin") {
        true => Some(read_key(&mut std::io::stdin())?),
        false => None,
    };
    fetch_local(&conn, key.as_deref())?;
    Ok(0)
}

/// Read a local engine's model list on the host and keep it, as the dashboard's fetch does.
fn fetch_local(conn: &litellm::Connection, key: Option<&str>) -> Result<(), Fail> {
    let root = litellm::dir();
    let now = state::now_epoch();
    let _ = crate::model_fetch::mark_in(&root, &conn.name, now);
    let models = crate::local::fetch(conn, key).map_err(|e| Fail::failed(format!("{e:#}")))?;
    let count = models.len();
    crate::local::store(&root, conn, models, now as i64)
        .map_err(|e| Fail::failed(format!("{e:#}")))?;
    match count {
        1 => eprintln!("aisbx: engine {}: 1 model", conn.name),
        n => eprintln!("aisbx: engine {}: {n} models", conn.name),
    }
    Ok(())
}

/// Fetch a connection's model list through its fetcher sandbox ([`crate::model_fetch`]) and keep
/// it on the host, as the dashboard's fetch does.
fn fetch_models(conn: &litellm::Connection) -> Result<(), Fail> {
    use crate::model_fetch;
    let root = litellm::dir();
    let kit =
        model_fetch::ensure_kit_in(&root, conn).map_err(|e| Fail::failed(format!("{e:#}")))?;
    let fetcher = model_fetch::sandbox_name(&conn.name);
    let exists = find(&fetcher)?.is_some();
    let now = state::now_epoch();
    let _ = model_fetch::mark_in(&root, &conn.name, now);
    for step in fetch_steps(conn, &kit, exists) {
        eprintln!("aisbx: {}", step.label);
        if !crate::job::run_quiet(&step) {
            return Err(Fail::sandbox(format!("{} failed", step.label)));
        }
    }
    let text = sbx::read_models_cache(&fetcher)
        .ok_or_else(|| Fail::sandbox(format!("no model list to read from {fetcher}")))?;
    let harvest = crate::models::parse_sandbox_cache(&text, now as i64).ok_or_else(|| {
        Fail::sandbox(format!(
            "the model list from {fetcher} did not pass validation"
        ))
    })?;
    let path = litellm::harvest_file(&conn.name);
    crate::models::store_harvest(&path, &fetcher, &harvest, now as i64)
        .map_err(|e| Fail::failed(format!("{e:#}")))?;
    eprintln!(
        "aisbx: connection {}: {} models from the gateway",
        conn.name,
        harvest.models.len()
    );
    Ok(())
}

/// The `models-<sandbox>` kit that pins Claude Code's aliases to the picks saved for the
/// connection, where any are saved and still offered. `None` for the other agents.
fn claude_pins_kit(
    root: &Path,
    sandbox: &str,
    conn: &litellm::Connection,
    agent: litellm::Agent,
) -> Result<Option<PathBuf>, Fail> {
    if agent != litellm::Agent::Claude {
        return Ok(None);
    }
    let offer = litellm::Offer::read(root, conn, agent);
    crate::models::ensure_kit(sandbox, &offer.slots, &offer.models)
        .map_err(|e| Fail::sandbox(format!("{e:#}")))
}

/// The pre-run step of `aisbx run` for a sandbox on a connection that still exists: the
/// connection's model list and picks, into the agent's config ([`litellm::push_argv_in`]).
fn push_argv_in(
    root: &Path,
    sandbox: &str,
    conn: &litellm::Connection,
    agent: litellm::Agent,
) -> Option<Vec<String>> {
    litellm::push_argv_in(root, sandbox, conn, agent)
}

/// The connection a sandbox's kit list (`sbx inspect`'s absolute kit dirs) wires it to, and the
/// agent its kit is for.
fn connection_of_kits(kits: &[String]) -> Option<(String, litellm::Agent)> {
    kits.iter().find_map(|k| {
        let dir = Path::new(k).file_name()?.to_str()?;
        litellm::of_kit(dir).map(|(name, agent)| (name.to_string(), agent))
    })
}

/// The sandboxes on a connection, from `(sandbox, kit dirs)` pairs.
fn users_of(name: &str, sandboxes: &[(String, Vec<String>)]) -> Vec<String> {
    sandboxes
        .iter()
        .filter(|(_, kits)| connection_of_kits(kits).is_some_and(|(c, _)| c == name))
        .map(|(sandbox, _)| sandbox.clone())
        .collect()
}

/// `litellm rm` without `--force` while sandboxes use the connection.
fn rm_refusal(name: &str, users: &[String]) -> Fail {
    Fail::usage(format!(
        "connection {name} is used by {} — they stop authenticating once its key is gone; \
         remove or rebuild them first, or pass --force",
        users.join(", ")
    ))
}

/// Whether `litellm rm` has a key to remove. A keyless connection and an orphan key (no
/// `connection.json`) are both removed. A failed `secret ls` is an sbx failure (125).
fn rm_has_key(
    name: &str,
    exists: bool,
    secrets: anyhow::Result<sbx::Secrets>,
) -> Result<bool, Fail> {
    let secrets = secrets.map_err(|e| Fail::sandbox(format!("{e:#}")))?;
    let has_key = secrets.litellm_key(name).is_some();
    if !exists && !has_key {
        return Err(Fail::usage(format!("no LiteLLM connection {name}")));
    }
    Ok(has_key)
}

fn litellm_rm(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["force"], &[])?;
    let name = match args.positional.as_slice() {
        [name] => name.clone(),
        [] => return Err(Fail::usage("which connection? Pass its name")),
        _ => return Err(Fail::usage("one connection at a time")),
    };
    if !litellm::valid_name(&name) {
        return Err(Fail::usage("a connection name is 1–32 of a–z, 0–9 and “-”"));
    }
    // Never the host's keychain under `cargo test`, where no connection exists either.
    let secrets = match cfg!(any(test, feature = "test-support")) {
        true => Ok(sbx::Secrets::default()),
        false => sbx::secrets(),
    };
    let has_key = rm_has_key(&name, litellm::exists(&name), secrets)?;
    let force = args.has("force");
    let list = match sbx::list() {
        Ok(list) => list,
        Err(e) if force => {
            eprintln!("aisbx: warning: could not list the sandboxes ({e:#}) — removing anyway");
            Vec::new()
        }
        Err(e) => return Err(Fail::sandbox(format!("{e:#}"))),
    };
    let mut pairs = Vec::new();
    let mut unread = Vec::new();
    for entry in &list {
        match sbx::inspect(&entry.name) {
            Ok(i) => pairs.push((entry.name.clone(), i.kits)),
            Err(_) => unread.push(entry.name.clone()),
        }
    }
    let users = users_of(&name, &pairs);
    if !force {
        if !users.is_empty() {
            return Err(rm_refusal(&name, &users));
        }
        if !unread.is_empty() {
            return Err(Fail::sandbox(format!(
                "could not read the kits of {} — pass --force to remove {name} anyway",
                unread.join(", ")
            )));
        }
    } else if !users.is_empty() {
        eprintln!(
            "aisbx: warning: {} stop authenticating now, until rebuilt onto another connection",
            users.join(", ")
        );
    }
    if has_key {
        let argv = sbx::secret_rm_litellm_key_argv(&litellm::placeholder(&name));
        match step(&argv, false) {
            Ok(s) if s.success() => {}
            Ok(s) => return Err(Fail::sandbox(format!("removing the key failed ({s})"))),
            Err(e) => return Err(Fail::sandbox(format!("could not run sbx: {e}"))),
        }
    }
    // Its model fetcher goes with it; one that does not exist is no error.
    let fetcher = crate::model_fetch::sandbox_name(&name);
    if find(&fetcher).ok().flatten().is_some() {
        if let Err(e) = step(&sbx::rm_argv(&fetcher), false) {
            eprintln!("aisbx: could not remove {fetcher} ({e}) — `sbx rm {fetcher}` does");
        }
    }
    litellm::remove(&name).map_err(|e| Fail::failed(format!("{e:#}")))?;
    let mut registry = state::Registry::load();
    registry.clear_last_litellm(&name);
    if let Err(e) = registry.save() {
        eprintln!("aisbx: could not update the registry ({e:#}) — harmless");
    }
    eprintln!("aisbx: connection {name} removed");
    Ok(0)
}

/// `up`'s connection: the one named, else the only one. Several without `--litellm` is a usage
/// error naming them — `last` (the dashboard's last used) is only ever *named*, never chosen.
fn pick_connection(
    all: &[litellm::Connection],
    wanted: Option<&str>,
    last: Option<&str>,
    kind: litellm::Kind,
) -> Result<litellm::Connection, Fail> {
    let (what, flag, add) = match kind {
        litellm::Kind::Gateway => (
            "LiteLLM connection",
            "--litellm",
            "aisbx litellm add <file.json> --key-stdin",
        ),
        litellm::Kind::Local => ("local engine", "--local", "aisbx local add"),
    };
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    if let Some(name) = wanted {
        if !litellm::valid_name(name) {
            return Err(Fail::usage(format!(
                "{flag}: a {} name is 1–32 of a–z, 0–9 and “-”",
                kind.noun()
            )));
        }
        if let Some(c) = all.iter().find(|c| c.name == name) {
            return Ok(c.clone());
        }
        let hint = match names.is_empty() {
            true => format!("add one: {add}"),
            false => format!("one of: {}", names.join(", ")),
        };
        return Err(Fail::usage(format!("no {what} {name} — {hint}")));
    }
    match all {
        [] => Err(Fail::usage(format!(
            "no {what} on this host — add one: {add}"
        ))),
        [only] => Ok(only.clone()),
        _ => {
            let last = last
                .filter(|l| names.contains(l))
                .map(|l| format!(" (last used: {l})"))
                .unwrap_or_default();
            Err(Fail::usage(format!(
                "{} {what}s: {}{last} — pick one: {flag} <name>",
                names.len(),
                names.join(", ")
            )))
        }
    }
}

/// The connection `up` was asked for: `--litellm` on a gateway backend, `--local` on a local
/// one, neither on any other.
fn connection_flag(
    kind: Option<litellm::Kind>,
    litellm_flag: Option<String>,
    local_flag: Option<String>,
    backend: &str,
) -> Result<Option<String>, Fail> {
    use litellm::Kind;
    match (kind, litellm_flag, local_flag) {
        (Some(Kind::Gateway), wanted, None) | (Some(Kind::Local), None, wanted) => Ok(wanted),
        (None, None, None) => Ok(None),
        (Some(Kind::Gateway), _, Some(_)) => Err(Fail::usage(format!(
            "--local is only for the *-local backends, not {backend}"
        ))),
        (Some(Kind::Local), Some(_), _) => Err(Fail::usage(format!(
            "{backend} runs on a local engine: pass --local NAME, not --litellm"
        ))),
        (None, Some(_), _) => Err(Fail::usage(format!(
            "--litellm is only for the LiteLLM backends, not {backend}"
        ))),
        (None, None, Some(_)) => Err(Fail::usage(format!(
            "--local is only for the *-local backends, not {backend}"
        ))),
    }
}

// --- skill-repo ----------------------------------------------------------------------------------

/// `aisbx skill-repo add | rm | ls | fetch | auto-update`: the skill repositories, their cache
/// ([`skill_repos`]) and the auto-update switch. Nothing here reads or touches a sandbox.
fn skill_repo(raw: &[String]) -> Outcome {
    let Some((sub, rest)) = raw.split_first() else {
        return Err(Fail::usage(
            "skill-repo needs add, rm, ls, fetch or auto-update",
        ));
    };
    match sub.as_str() {
        "add" => skill_repo_add(rest),
        "rm" => skill_repo_rm(rest),
        "ls" => skill_repo_ls(rest),
        "fetch" => skill_repo_fetch(rest),
        "auto-update" => skill_repo_auto_update(rest),
        other => Err(Fail::usage(format!("unknown skill-repo command: {other}"))),
    }
}

/// Everything is checked before the list is opened, so a refusal (exit 2) writes nothing. The
/// messages never quote the source: a refused one may be a pasted token.
fn skill_repo_add(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &[], &["ref", "via"])?;
    let source = match args.positional.as_slice() {
        [source] => source.clone(),
        [] => {
            return Err(Fail::usage(
                "which repository? Pass owner/repo or a Git URL",
            ))
        }
        _ => return Err(Fail::usage("one repository at a time")),
    };
    let parsed = skill_repos::parse_source(&source)
        .map_err(|e| Fail::usage(format!("source refused: {e}")))?;
    let git_ref = args.one("ref")?;
    if git_ref
        .as_deref()
        .is_some_and(|r| !skill_repos::valid_ref(r))
    {
        return Err(Fail::usage(
            "--ref must be a branch or tag name: letters, digits, . _ / -, at most 200, \
             no leading - and no ..",
        ));
    }
    let via = match args.one("via")? {
        None => skill_repos::Method::Auto,
        Some(v) => skill_repos::Method::parse(&v)
            .ok_or_else(|| Fail::usage("--via is auto, gh, ssh, token or git"))?,
    };
    skill_repos::supports(&parsed, via)
        .map_err(|e| Fail::usage(format!("--via {}: {e}", via.as_str())))?;
    let id = skill_repos::add(&state::state_dir(), &source, git_ref.as_deref(), via)
        .map_err(|e| Fail::failed(format!("{e:#}")))?;
    println!("{id}");
    Ok(0)
}

fn skill_repo_rm(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &[], &[])?;
    let handle = match args.positional.as_slice() {
        [handle] => handle.clone(),
        [] => return Err(Fail::usage("which skill repo? Pass its id or source")),
        _ => return Err(Fail::usage("one skill repo at a time")),
    };
    match skill_repos::remove(&state::state_dir(), &handle)
        .map_err(|e| Fail::failed(format!("{e:#}")))?
    {
        Some(id) => {
            eprintln!("removed {id} and its cache");
            Ok(0)
        }
        None => Err(Fail::failed(format!(
            "no configured skill repo is {}",
            shown_handle(&handle)
        ))),
    }
}

/// A handle as it may be quoted back: an id or a source that passed the checks, never anything
/// else (it could be a pasted token).
fn shown_handle(handle: &str) -> String {
    if skill_repos::is_id(handle) || skill_repos::parse_source(handle).is_ok() {
        handle.to_string()
    } else {
        "that (neither an id nor an accepted source)".to_string()
    }
}

/// What a skill-repo command printed: stdout as one text, stderr as lines — so the tests can
/// hold the stdout contract without capturing the process's own streams.
#[derive(Debug, Default)]
struct Printed {
    stdout: String,
    stderr: Vec<String>,
}

impl Printed {
    fn emit(self) {
        for line in &self.stderr {
            eprintln!("{line}");
        }
        print!("{}", self.stdout);
        let _ = std::io::stdout().flush();
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// The global auto-update switch, as `skill-repos.json` holds it (absent: on).
fn auto_update_of(state_dir: &Path) -> Result<bool, Fail> {
    skill_repos::load(&skill_repos::config_path(state_dir))
        .map(|c| c.auto_update())
        .map_err(|e| Fail::failed(format!("{e:#}")))
}

fn skill_repo_ls(raw: &[String]) -> Outcome {
    skill_repo_ls_in(&state::state_dir(), raw)?.emit();
    Ok(0)
}

fn skill_repo_ls_in(state_dir: &Path, raw: &[String]) -> Result<Printed, Fail> {
    let args = Args::parse(raw, &["json"], &[])?;
    if !args.positional.is_empty() {
        return Err(Fail::usage("skill-repo ls takes no arguments"));
    }
    let listed = skill_repos::list(state_dir).map_err(|e| Fail::failed(format!("{e:#}")))?;
    let auto_update = auto_update_of(state_dir)?;
    let mut out = Printed::default();
    if args.has("json") {
        let doc = skill_repos::list_json(&listed, auto_update);
        out.stdout = format!(
            "{}\n",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        );
        return Ok(out);
    }
    for l in &listed {
        let cache = match &l.cache {
            Some((_, commit, at)) => format!(
                "{} from {}",
                skill_repos::short(commit),
                state::format_local(*at)
            ),
            None => "not cached".to_string(),
        };
        out.stdout.push_str(&format!(
            "{}  {}  {}  via {}  {cache}\n",
            l.id,
            l.repo.source,
            l.repo.git_ref.as_deref().unwrap_or("default branch"),
            l.repo.via.as_str()
        ));
    }
    if listed.is_empty() {
        out.stderr.push(
            "no skill repos configured; add one with `aisbx skill-repo add owner/repo`".into(),
        );
    }
    out.stderr
        .push(format!("auto-update: {}", on_off(auto_update)));
    Ok(out)
}

/// `skill-repo auto-update [on|off]`: print the switch, or set it.
fn skill_repo_auto_update(raw: &[String]) -> Outcome {
    skill_repo_auto_update_in(&state::state_dir(), raw)?.emit();
    Ok(0)
}

fn skill_repo_auto_update_in(state_dir: &Path, raw: &[String]) -> Result<Printed, Fail> {
    let args = Args::parse(raw, &[], &[])?;
    if !args.trailing.is_empty() {
        return Err(Fail::usage(
            "skill-repo auto-update takes on, off or nothing",
        ));
    }
    let mut out = Printed::default();
    match args.positional.as_slice() {
        [] => out.stdout = format!("{}\n", on_off(auto_update_of(state_dir)?)),
        [word] if word == "on" || word == "off" => {
            skill_repos::set_auto_update(state_dir, word == "on")
                .map_err(|e| Fail::failed(format!("{e:#}")))?;
            out.stderr.push(format!("auto-update: {word}"));
        }
        _ => {
            return Err(Fail::usage(
                "skill-repo auto-update takes on, off or nothing",
            ))
        }
    }
    Ok(out)
}

/// Fetch the named entries (all without arguments, then drop orphaned cache folders); one line
/// per entry on stderr plus its skill report, exit 1 when any failed.
fn skill_repo_fetch(raw: &[String]) -> Outcome {
    let env = skill_repos::Env::from_host();
    let mut fetch = |repo: &skill_repos::Repo, root: &Path| skill_repos::fetch(repo, root, &env);
    let mut say = |line: String| eprintln!("{line}");
    let (code, stdout) = skill_repo_fetch_in(&state::state_dir(), raw, &mut fetch, &mut say)?;
    print!("{stdout}");
    let _ = std::io::stdout().flush();
    Ok(code)
}

type FetchResult = Result<skill_repos::Fetched, Vec<String>>;

/// [`skill_repo_fetch`] with the fetch and stderr handed in, for the tests. Each human line goes
/// to `say` at once; returns the exit code and stdout (the `--json` document, or nothing).
fn skill_repo_fetch_in(
    state_dir: &Path,
    raw: &[String],
    fetch: &mut dyn FnMut(&skill_repos::Repo, &Path) -> FetchResult,
    say: &mut dyn FnMut(String),
) -> Result<(u8, String), Fail> {
    let args = Args::parse(raw, &["json"], &[])?;
    let json = args.has("json");
    let config_path = skill_repos::config_path(state_dir);
    let config = skill_repos::load(&config_path).map_err(|e| Fail::failed(format!("{e:#}")))?;
    let auto_update = config.auto_update();
    let root = skill_repos::cache_root(state_dir);
    let mut reports: Vec<skill_select::FetchReport> = Vec::new();
    let mut failed = false;
    let mut chosen: Vec<usize> = Vec::new();
    if args.positional.is_empty() {
        chosen.extend(0..config.repos.len());
    }
    for handle in &args.positional {
        let found = skill_repos::matching(&config, handle);
        if found.is_empty() {
            say(format!(
                "aisbx: no configured skill repo is {}",
                shown_handle(handle)
            ));
            failed = true;
        }
        for i in found {
            if !chosen.contains(&i) {
                chosen.push(i);
            }
        }
    }
    for i in chosen {
        let repo = &config.repos[i];
        let id = repo.id().unwrap_or_default();
        let before = skill_select::read_cache(state_dir, &id).ok();
        let fetched = match fetch(repo, &root) {
            Ok(fetched) => {
                for attempt in &fetched.failures {
                    say(format!("{id}: {attempt}"));
                }
                say(format!(
                    "{id}: {} ({})",
                    skill_repos::short(&fetched.commit),
                    fetched.method.as_str()
                ));
                true
            }
            Err(attempts) => {
                for attempt in &attempts {
                    say(format!("{id}: {attempt}"));
                }
                say(format!("{id}: failed"));
                failed = true;
                false
            }
        };
        let (report, warnings) =
            skill_select::fetch_report(state_dir, &id, before.as_ref(), fetched, auto_update);
        for warning in warnings {
            say(format!("aisbx: warning: {warning}"));
        }
        for line in report.lines() {
            say(line);
        }
        reports.push(report);
    }
    if args.positional.is_empty() {
        // Read the list again: an entry added while the fetches ran is not an orphan.
        if let Ok(now) = skill_repos::load(&config_path) {
            let keep: Vec<String> = now.repos.iter().filter_map(|r| r.id().ok()).collect();
            for orphan in skill_repos::prune_orphans(&root, &keep) {
                say(format!("removed {orphan}, a cache no configured repo owns"));
            }
        }
    }
    let stdout = if json {
        let doc = skill_select::fetch_json(auto_update, &reports);
        format!(
            "{}\n",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        )
    } else {
        String::new()
    };
    Ok((if failed { 1 } else { 0 }, stdout))
}

// --- skill ---------------------------------------------------------------------------------------

/// `aisbx skill add | rm | ls | sync`: which skills of the cached skill repos each sandbox gets
/// ([`skill_select`]). `rm` and `ls` only read and write the selection file.
fn skill(raw: &[String]) -> Outcome {
    let Some((sub, rest)) = raw.split_first() else {
        return Err(Fail::usage("skill needs add, rm, ls or sync"));
    };
    match sub.as_str() {
        "add" => skill_add(rest),
        "rm" => skill_rm(rest),
        "ls" => skill_ls(rest),
        "sync" => skill_sync(rest),
        other => Err(Fail::usage(format!("unknown skill command: {other}"))),
    }
}

/// Check every spec's spelling (exit 2), before anything else runs.
fn parse_specs(specs: &[String]) -> Result<(), Fail> {
    for spec in specs {
        skill_select::parse_spec(spec).map_err(|e| Fail::usage(format!("--skill: {e}")))?;
    }
    Ok(())
}

fn select_fail(e: skill_select::Error) -> Fail {
    match e {
        skill_select::Error::Refused(why) => Fail::usage(why),
        skill_select::Error::Failed(why) => Fail::failed(why),
    }
}

fn warn_all(notes: &[String]) {
    for note in notes {
        eprintln!("aisbx: warning: {note}");
    }
}

/// `NAME SPEC…` or `--global SPEC…`: the sandbox (`None` for the global list) and at least one
/// spec, each spelled right.
fn sandbox_and_specs(raw: &[String], what: &str) -> Result<(Option<String>, Vec<String>), Fail> {
    let args = Args::parse(raw, &["global"], &[])?;
    if args.has("global") {
        if args.positional.is_empty() {
            return Err(Fail::usage(format!(
                "which skills to {what} for every sandbox? Pass REPO or REPO#SKILL"
            )));
        }
        parse_specs(&args.positional)?;
        return Ok((None, args.positional.clone()));
    }
    match args.positional.as_slice() {
        [] => Err(Fail::usage("which sandbox? Pass its name, or --global")),
        [_] => Err(Fail::usage(format!(
            "which skills to {what}? Pass REPO or REPO#SKILL"
        ))),
        [name, specs @ ..] => {
            parse_specs(specs)?;
            Ok((Some(name.clone()), specs.to_vec()))
        }
    }
}

/// The sandbox, as `sbx ls` reports it (exit 125 when it does not).
fn known_sandbox(name: &str) -> Result<(), Fail> {
    match find(name)? {
        Some(_) => Ok(()),
        None => Err(Fail::sandbox(format!("no sandbox named {name}"))),
    }
}

fn skill_add(raw: &[String]) -> Outcome {
    let (name, specs) = sandbox_and_specs(raw, "add")?;
    let state_dir = state::state_dir();
    let Some(name) = name else {
        let notes = skill_select::add_global(&state_dir, &specs).map_err(select_fail)?;
        warn_all(&notes);
        eprintln!(
            "selected for every sandbox; each gets them at its next start (or `aisbx skill sync \
             NAME`)"
        );
        return Ok(0);
    };
    known_sandbox(&name)?;
    let notes = skill_select::add(&state_dir, &name, &specs).map_err(select_fail)?;
    warn_all(&notes);
    eprintln!("selected for {name}; `aisbx skill sync {name}` or its next start installs them");
    if matches!(auto_update_of(&state_dir), Ok(false)) {
        eprintln!(
            "auto-update is off: new selections still install at the next start; skills already \
             installed stay as they are until `aisbx skill sync {name}`"
        );
    }
    Ok(0)
}

fn skill_rm(raw: &[String]) -> Outcome {
    let (name, specs) = sandbox_and_specs(raw, "remove")?;
    let Some(name) = name else {
        skill_select::remove_global(&state::state_dir(), &specs).map_err(select_fail)?;
        eprintln!(
            "dropped from the global selection; each sandbox loses them at its next start, \
             unless its own selection keeps them"
        );
        return Ok(0);
    };
    skill_select::remove(&state::state_dir(), &name, &specs).map_err(select_fail)?;
    eprintln!("dropped from {name}'s selection; `aisbx skill sync {name}` removes them from it");
    Ok(0)
}

fn skill_ls(raw: &[String]) -> Outcome {
    let args = Args::parse(raw, &["json"], &[])?;
    let only = match args.positional.as_slice() {
        [] => None,
        [name] => Some(name.as_str()),
        _ => return Err(Fail::usage("skill ls takes at most one sandbox name")),
    };
    let doc = skill_select::list_json(&state::state_dir(), only)
        .map_err(|e| Fail::failed(format!("{e:#}")))?;
    if args.has("json") {
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return Ok(0);
    }
    print!("{}", skill_ls_text(&doc));
    let globals = doc["global"].as_array().is_some_and(|g| !g.is_empty());
    let sandboxes = doc["sandboxes"].as_object().is_some_and(|s| !s.is_empty());
    if !globals && !sandboxes {
        eprintln!("no skills selected; add some with `aisbx skill add NAME|--global REPO[#SKILL]`");
    }
    Ok(0)
}

/// `skill ls` without `--json`: one line per entry, `global` for the global list's, and a
/// sandbox's entries tagged `(global)` or `(global + own)` where a global pick reaches them.
fn skill_ls_text(doc: &serde_json::Value) -> String {
    let line = |who: &str, e: &serde_json::Value| -> String {
        let commit = e["commit"]
            .as_str()
            .map_or("-", skill_repos::short)
            .to_string();
        let wanted = match &e["skills"] {
            serde_json::Value::String(all) => all.clone(),
            names => names
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(","),
        };
        let resolves: Vec<&str> = e["resolves_to"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n.as_str())
            .collect();
        let mut line = format!(
            "{who}  {}  {commit}  {wanted}  → {}",
            e["repo"].as_str().unwrap_or_default(),
            if resolves.is_empty() {
                "nothing".to_string()
            } else {
                resolves.join(", ")
            }
        );
        match e["origin"].as_str() {
            Some("global") => line.push_str("  (global)"),
            Some("both") => line.push_str("  (global + own)"),
            _ => {}
        }
        if let Some(problem) = e["problem"].as_str() {
            line.push_str(&format!("  ({problem})"));
        }
        line.push('\n');
        line
    };
    let mut out = String::new();
    for e in doc["global"].as_array().into_iter().flatten() {
        out.push_str(&line("global", e));
    }
    for (sandbox, entries) in doc["sandboxes"].as_object().into_iter().flatten() {
        for e in entries.as_array().into_iter().flatten() {
            out.push_str(&line(sandbox.as_str(), e));
        }
    }
    out
}

/// `skill sync [--start] NAME`: without `--start` the owner's full sync, whatever the switch;
/// with it the pre-start sync, which holds installed skills while auto-update is off.
fn skill_sync(raw: &[String]) -> Outcome {
    let (name, start) = sync_args(raw)?;
    known_sandbox(&name)?;
    let state_dir = state::state_dir();
    let mode = if start {
        skill_select::SyncMode::Start {
            auto_update: auto_update_of(&state_dir)?,
        }
    } else {
        skill_select::SyncMode::Full
    };
    match skill_select::sync(&state_dir, &name, mode, &mut std::io::stdout()) {
        Ok(true) => Ok(0),
        Ok(false) => Ok(1),
        Err(e) => Err(Fail::failed(format!("{e:#}"))),
    }
}

fn sync_args(raw: &[String]) -> Result<(String, bool), Fail> {
    let args = Args::parse(raw, &["start"], &[])?;
    Ok((args.name()?, args.has("start")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls_entry(name: &str, workspaces: &[&str]) -> sbx::LsEntry {
        sbx::LsEntry {
            name: name.into(),
            agent: "claude".into(),
            status: "running".into(),
            workspaces: workspaces.iter().map(|w| (*w).to_string()).collect(),
            ..Default::default()
        }
    }

    fn ls_registry() -> state::Registry {
        let mut registry = state::Registry::empty();
        registry.set_display_name("a", Some("nightly".into()));
        registry.set_display_name("c", Some("two\tparts\nand\rmore".into()));
        registry
    }

    #[test]
    fn ls_prints_one_tab_separated_line_per_sandbox() {
        let entries = [
            ls_entry("a", &["/w/a", "/w/extra:ro"]),
            ls_entry("b", &[]),
            ls_entry("c", &["/w/c"]),
        ];
        let out = render_ls(&entries, &ls_registry(), false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "a\trunning\tclaude\t/w/a\tnightly");
        assert_eq!(lines[1], "b\trunning\tclaude\t\t");
        assert_eq!(lines[2], "c\trunning\tclaude\t/w/c\ttwo parts and more");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn ls_json_is_an_array_of_the_five_fields() {
        let entries = [ls_entry("a", &["/w/a", "/w/x:ro"]), ls_entry("b", &[])];
        let out = render_ls(&entries, &ls_registry(), true);
        let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
        let rows = doc.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let object = row.as_object().unwrap();
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, ["agent", "label", "name", "status", "workspaces"]);
        }
        assert_eq!(rows[0]["label"], "nightly");
        assert_eq!(
            rows[0]["workspaces"],
            serde_json::json!(["/w/a", "/w/x:ro"])
        );
        assert!(rows[1]["label"].is_null());
        assert_eq!(rows[1]["workspaces"], serde_json::json!([]));
    }

    #[test]
    fn ls_of_nothing_prints_nothing_or_an_empty_array() {
        let registry = state::Registry::empty();
        assert_eq!(render_ls(&[], &registry, false), "");
        let json = render_ls(&[], &registry, true);
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, serde_json::json!([]));
    }

    #[test]
    fn ls_takes_only_json() {
        assert!(is_subcommand("ls"));
        assert!(runs_sandboxes("ls"));
        assert!(HELP.contains("aisbx ls [--json]"));
        for raw in [
            &["x"][..],
            &["--bogus"],
            &["--json=1"],
            &["--", "NAME"],
            &["--json", "--", "NAME"],
        ] {
            assert_eq!(ls(&strings(raw)).unwrap_err().code, EXIT_USAGE, "{raw:?}");
        }
    }

    #[test]
    fn ls_accepts_the_json_switch() {
        let args = Args::parse(&strings(&["--json"]), &["json"], &[]).unwrap();
        assert!(args.has("json"));
        assert!(args.positional.is_empty() && args.trailing.is_empty());
        let args = Args::parse(&[], &["json"], &[]).unwrap();
        assert!(!args.has("json"));
    }

    #[test]
    fn ls_errors_fit_on_one_line() {
        assert_eq!(
            one_line("sbx ls failed: boom\r\n  second line\n\nthird"),
            "sbx ls failed: boom second line third"
        );
        assert_eq!(one_line("plain"), "plain");
    }

    #[test]
    fn telemetry_is_turned_off_before_the_commands_that_drive_sandboxes() {
        for cmd in ["up", "run", "ls", "status", "stop", "rm"] {
            assert!(runs_sandboxes(cmd), "{cmd}");
        }
        for cmd in ["litellm", "folders", "skill-repo", "skill", "nope"] {
            assert!(!runs_sandboxes(cmd), "{cmd}");
        }
    }

    #[test]
    fn every_headless_sbx_call_never_asks_and_leaves_the_ssh_agent_out() {
        let agent = vec!["claude".to_string(), "--no-input".to_string()];
        let launch = sbx::headless_exec_argv("box", None, "/tmp/r", &agent);
        let cmd = sbx_command(&launch, false);
        let args: Vec<String> = (cmd.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("sbx"));
        assert_eq!(args[0], sbx::NO_INPUT);
        assert_eq!(args[1..], launch[1..], "the agent's own argv is untouched");
        let sock = std::ffi::OsStr::new("SSH_AUTH_SOCK");
        assert!(cmd.get_envs().any(|(k, v)| k == sock && v.is_none()));
        let shared = sbx_command(&launch, true);
        assert!(!shared.get_envs().any(|(k, _)| k == sock));
        assert_eq!(
            shared.get_args().next(),
            Some(std::ffi::OsStr::new("--no-input"))
        );
    }

    #[test]
    fn a_headless_model_fetch_never_asks() {
        let conn = crate::testkit::acme();
        let steps = fetch_steps(&conn, Path::new("/kits/aisbx-models"), false);
        assert_eq!(steps.len(), 2, "create, then fetch");
        for step in &steps {
            assert_eq!(&step.argv[..2], ["sbx", "--no-input"], "{:?}", step.argv);
        }
        assert!(
            steps[1].stdin.is_some(),
            "the script still goes in on stdin"
        );
    }

    #[test]
    fn folders_add_ls_and_rm_edit_the_host_list() {
        let state =
            std::env::temp_dir().join(format!("aisbx-tests-folders-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let run = |args: &[&str]| {
            let mut out = Vec::new();
            let code = folders_cmd(&state, &strings(args), &mut out).map_err(|f| f.code);
            (code, String::from_utf8(out).unwrap())
        };
        assert_eq!(
            run(&[
                "add",
                "/srv/kb",
                "--write",
                "--purpose",
                "the knowledge base"
            ])
            .0,
            Ok(0)
        );
        assert_eq!(run(&["add", "/srv/docs"]).0, Ok(0));
        let (code, text) = run(&["ls"]);
        assert_eq!(code, Ok(0));
        assert_eq!(
            text,
            "write\t/srv/kb  the knowledge base\nread\t/srv/docs\n"
        );
        let (_, json) = run(&["ls", "--json"]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["access"], "write");
        assert_eq!(run(&["add", "relative"]).0, Err(EXIT_USAGE));
        assert_eq!(run(&["add"]).0, Err(EXIT_USAGE));
        assert_eq!(run(&["rm", "/srv/kb"]).0, Ok(0));
        assert_eq!(run(&["rm", "/srv/kb"]).0, Err(1));
        assert_eq!(run(&["ls"]).1, "read\t/srv/docs\n");
        assert_eq!(run(&["nope"]).0, Err(EXIT_USAGE));
        std::fs::remove_dir_all(&state).unwrap();
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn scratch_state(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aisbx-headless-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn skill_repo_auto_update_prints_and_sets_the_switch() {
        let state = scratch_state("auto-update");
        let read = || skill_repo_auto_update_in(&state, &[]).unwrap();
        let shown = read();
        assert_eq!(shown.stdout, "on\n", "absent means on");
        assert!(shown.stderr.is_empty());

        let set = skill_repo_auto_update_in(&state, &strings(&["off"])).unwrap();
        assert_eq!(set.stdout, "");
        assert_eq!(set.stderr, strings(&["auto-update: off"]));
        assert_eq!(read().stdout, "off\n");
        let set = skill_repo_auto_update_in(&state, &strings(&["on"])).unwrap();
        assert_eq!(set.stderr, strings(&["auto-update: on"]));
        assert_eq!(read().stdout, "on\n");

        let before = std::fs::read_to_string(skill_repos::config_path(&state)).unwrap();
        for raw in [
            strings(&["yes"]),
            strings(&["on", "off"]),
            strings(&["--frob"]),
            strings(&["-x"]),
            strings(&["on", "--", "off"]),
        ] {
            let fail = skill_repo_auto_update_in(&state, &raw).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{raw:?}");
            let fail = skill_repo(&[vec!["auto-update".to_string()], raw.clone()].concat())
                .expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{raw:?}");
        }
        assert_eq!(
            std::fs::read_to_string(skill_repos::config_path(&state)).unwrap(),
            before,
            "a refusal writes nothing"
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn skill_repo_ls_ends_with_the_switch_on_stderr() {
        let state = scratch_state("ls");
        let empty = skill_repo_ls_in(&state, &[]).unwrap();
        assert_eq!(empty.stdout, "");
        assert_eq!(
            empty.stderr.last().map(String::as_str),
            Some("auto-update: on")
        );

        let id = skill_repos::add(&state, "o/r", None, skill_repos::Method::Auto).unwrap();
        let on = skill_repo_ls_in(&state, &[]).unwrap();
        assert_eq!(
            on.stdout,
            format!("{id}  https://github.com/o/r  default branch  via auto  not cached\n")
        );
        assert_eq!(on.stderr, strings(&["auto-update: on"]));

        skill_repos::set_auto_update(&state, false).unwrap();
        let off = skill_repo_ls_in(&state, &[]).unwrap();
        assert_eq!(off.stdout, on.stdout, "stdout unchanged");
        assert_eq!(off.stderr, strings(&["auto-update: off"]));

        let json = skill_repo_ls_in(&state, &strings(&["--json"])).unwrap();
        assert!(json.stderr.is_empty());
        let doc: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
        assert_eq!(doc["auto_update"], false);
        assert_eq!(doc["repos"][0]["id"], id.as_str());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn skill_sync_takes_start() {
        assert_eq!(
            sync_args(&strings(&["--start", "box"])).unwrap(),
            ("box".to_string(), true)
        );
        assert_eq!(
            sync_args(&strings(&["box", "--start"])).unwrap(),
            ("box".to_string(), true)
        );
        assert_eq!(
            sync_args(&strings(&["box"])).unwrap(),
            ("box".to_string(), false)
        );
        for raw in [
            strings(&["--start"]),
            strings(&["--start=yes", "box"]),
            strings(&["--hold", "box"]),
        ] {
            assert_eq!(
                sync_args(&raw).expect_err("refused").code,
                EXIT_USAGE,
                "{raw:?}"
            );
        }
        // What the dashboard and `aisbx run` run before a start.
        let step = skill_source::SkillSource::RepoCache.install_argv("box");
        let (name, start) = sync_args(&step[3..]).unwrap();
        assert_eq!((name.as_str(), start), ("box", true));
    }

    /// Plain git for the fixture, away from the host's own configuration.
    fn fixture_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn fixture_skill(work: &Path, name: &str, body: &str) {
        let dir = work.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("name: {name}\n{body}\n")).unwrap();
    }

    /// Commit everything and push it to the bare repo; hands back the commit.
    fn push_all(work: &Path, message: &str) -> String {
        fixture_git(work, &["add", "-A"]);
        fixture_git(work, &["commit", "-qm", message]);
        fixture_git(work, &["push", "-q", "origin", "HEAD:main"]);
        fixture_git(work, &["rev-parse", "HEAD"])
    }

    /// One `skill-repo fetch` through the test seam: `url` is the remote, `None` a fetch that
    /// fails. Hands back the exit code, stdout and the stderr lines.
    fn fetch_run(state: &Path, raw: &[&str], url: Option<&str>) -> (u8, String, Vec<String>) {
        let mut lines = Vec::new();
        let mut fetch = |repo: &skill_repos::Repo, root: &Path| match url {
            Some(url) => skill_repos::fetch_routes(
                &repo.id().unwrap(),
                &repo.source,
                repo.git_ref.as_deref(),
                vec![(
                    skill_repos::Method::Git,
                    Ok(skill_repos::Route {
                        method: skill_repos::Method::Git,
                        url: url.to_string(),
                    }),
                )],
                root,
                &skill_repos::Env::default(),
            ),
            None => Err(vec!["git: the remote is unreachable".to_string()]),
        };
        let mut say = |line: String| lines.push(line);
        let (code, stdout) =
            skill_repo_fetch_in(state, &strings(raw), &mut fetch, &mut say).unwrap();
        (code, stdout, lines)
    }

    fn names_of(v: &serde_json::Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn skill_repo_fetch_reports_the_skill_changes_and_the_sandboxes_they_reach() {
        let git_ok = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !git_ok {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let base = scratch_state("fetch-report");
        fixture_git(&base, &["init", "-q", "--bare", "up.git"]);
        fixture_git(&base, &["init", "-q", "work"]);
        let work = base.join("work");
        let url = format!("file://{}", base.join("up.git").display());
        let up_path = base.join("up.git").to_string_lossy().into_owned();
        fixture_git(&work, &["remote", "add", "origin", up_path.as_str()]);
        for name in ["keep", "edit", "gone", "linky"] {
            fixture_skill(&work, name, "v1");
        }
        let c1 = push_all(&work, "one");

        let state = base.join("state");
        let id = skill_repos::add(
            &state,
            "https://example.com/team/skills",
            None,
            skill_repos::Method::Git,
        )
        .unwrap();
        let pick = |skills: skill_select::Skills| {
            vec![skill_select::Pick {
                repo: id.clone(),
                skills,
            }]
        };
        let named = |names: &[&str]| skill_select::Skills::Named(strings(names));
        skill_select::replace(&state, "box-all", pick(skill_select::Skills::All)).unwrap();
        skill_select::replace(&state, "box-edit", pick(named(&["edit"]))).unwrap();
        skill_select::replace(&state, "box-keep", pick(named(&["keep"]))).unwrap();

        // Cloned: no cache before, every skill added; without --json, stdout stays empty.
        let (code, stdout, _) = fetch_run(&state, &[], Some(&url));
        assert_eq!((code, stdout.as_str()), (0, ""));
        std::fs::remove_dir_all(skill_repos::cache_root(&state)).unwrap();
        let (code, stdout, lines) = fetch_run(&state, &["--json"], Some(&url));
        assert_eq!(code, 0);
        let doc: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is one document");
        assert_eq!(doc["auto_update"], true);
        let repo = &doc["repos"][0];
        assert_eq!(doc["repos"].as_array().unwrap().len(), 1);
        assert_eq!(repo["id"], id.as_str());
        assert_eq!(repo["status"], "cloned");
        assert_eq!(repo["old_commit"], serde_json::Value::Null);
        assert_eq!(repo["commit"], c1.as_str());
        assert_eq!(
            names_of(&repo["added"]),
            strings(&["edit", "gone", "keep", "linky"])
        );
        assert_eq!(repo["removed"], serde_json::json!([]));
        assert_eq!(repo["changed"], serde_json::json!([]));
        assert_eq!(repo["sandboxes"][0]["name"], "box-all");
        assert_eq!(
            names_of(&repo["sandboxes"][0]["installs"]),
            strings(&["edit", "gone", "keep", "linky"])
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("{id}: new cache at "))),
            "{lines:?}"
        );

        // Updated: add, delete, edit, and a skill that is now skipped.
        fixture_skill(&work, "edit", "v2");
        std::fs::remove_dir_all(work.join("skills/gone")).unwrap();
        std::os::unix::fs::symlink("../keep", work.join("skills/linky/up")).unwrap();
        fixture_skill(&work, "fresh", "v1");
        let c2 = push_all(&work, "two");
        let (code, stdout, lines) = fetch_run(&state, &["--json"], Some(&url));
        assert_eq!(code, 0);
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let repo = &doc["repos"][0];
        assert_eq!(repo["status"], "updated");
        assert_eq!(repo["old_commit"], c1.as_str());
        assert_eq!(repo["commit"], c2.as_str());
        assert_eq!(names_of(&repo["added"]), strings(&["fresh"]));
        assert_eq!(
            repo["removed"],
            serde_json::json!([
                {"name": "gone", "skipped": null},
                {"name": "linky", "skipped": "symlink"}
            ])
        );
        assert_eq!(names_of(&repo["changed"]), strings(&["edit"]));
        assert_eq!(
            repo["sandboxes"],
            serde_json::json!([
                {"name": "box-all", "installs": ["fresh"], "updates": ["edit"],
                 "removes": ["gone", "linky"], "held": []},
                {"name": "box-edit", "installs": [], "updates": ["edit"], "removes": [],
                 "held": []}
            ]),
            "box-keep's one skill is untouched, so it is not named"
        );
        let summary = format!(
            "{id}: {} → {}: +fresh  -gone,linky (skipped: symlink)  ~edit",
            skill_repos::short(&c1),
            skill_repos::short(&c2)
        );
        assert!(lines.contains(&summary), "{lines:?}");
        assert!(
            lines.contains(&format!(
                "{id}:   box-all: next start installs fresh; updates edit; removes gone,linky"
            )),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("box-keep")), "{lines:?}");

        // Unchanged: the same commit, empty arrays.
        let (code, stdout, lines) = fetch_run(&state, &["--json", id.as_str()], Some(&url));
        assert_eq!(code, 0);
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let repo = &doc["repos"][0];
        assert_eq!(repo["status"], "unchanged");
        assert_eq!(repo["commit"], repo["old_commit"]);
        for key in ["added", "removed", "changed", "sandboxes"] {
            assert_eq!(repo[key], serde_json::json!([]), "{key}");
        }
        assert!(lines.contains(&format!("{id}: unchanged")), "{lines:?}");

        // Failed, plus a handle that matches nothing: one element, exit 1, the cache kept.
        let (code, stdout, lines) = fetch_run(&state, &["--json", id.as_str(), "o/nope"], None);
        assert_eq!(code, 1);
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(doc["repos"].as_array().unwrap().len(), 1);
        let repo = &doc["repos"][0];
        assert_eq!(repo["status"], "failed");
        assert_eq!(repo["commit"], serde_json::Value::Null);
        assert_eq!(repo["old_commit"], c2.as_str());
        for key in ["added", "removed", "changed", "sandboxes"] {
            assert_eq!(repo[key], serde_json::json!([]), "{key}");
        }
        assert!(
            lines
                .iter()
                .any(|l| l.contains("no configured skill repo is o/nope")),
            "{lines:?}"
        );
        assert!(lines.contains(&format!("{id}: failed")), "{lines:?}");
        let cached = skill_repos::cached_commit(&skill_repos::cache_root(&state).join(&id));
        assert_eq!(
            cached.map(|c| c.0),
            Some(c2.clone()),
            "the previous cache stays"
        );

        // Auto-update off: new skills still arrive, the touched installed ones are held.
        skill_repos::set_auto_update(&state, false).unwrap();
        fixture_skill(&work, "edit", "v3");
        fixture_skill(&work, "newer", "v1");
        push_all(&work, "three");
        let (code, stdout, lines) = fetch_run(&state, &["--json"], Some(&url));
        assert_eq!(code, 0);
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(doc["auto_update"], false);
        assert_eq!(
            doc["repos"][0]["sandboxes"],
            serde_json::json!([
                {"name": "box-all", "installs": ["newer"], "updates": [], "removes": [],
                 "held": ["edit"]},
                {"name": "box-edit", "installs": [], "updates": [], "removes": [],
                 "held": ["edit"]}
            ])
        );
        assert!(
            lines.contains(&format!(
                "{id}:   box-all: next start installs newer; held edit \
                 (run `aisbx skill sync box-all` to update)"
            )),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn skill_repo_refusals_exit_2_without_quoting_the_source() {
        assert!(is_subcommand("skill-repo"));
        assert!(is_subcommand("folders"));
        let secretish = format!("https://user:{}@github.com/o/r", "pw-123");
        for raw in [
            vec![],
            vec!["frob".to_string()],
            strings(&["add"]),
            vec!["add".to_string(), secretish.clone()],
            strings(&["add", "o/r", "--ref", "a..b"]),
            strings(&["add", "o/r", "--via", "carrier-pigeon"]),
            strings(&["add", "https://example.com/o/r", "--via", "gh"]),
            strings(&["add", "o/r", "o/s"]),
            strings(&["ls", "extra"]),
        ] {
            let fail = skill_repo(&raw).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{raw:?}");
            assert!(!fail.message.contains("pw-123"), "{}", fail.message);
        }
        assert_eq!(
            shown_handle(&secretish),
            "that (neither an id nor an accepted source)"
        );
        assert_eq!(shown_handle("o/r"), "o/r");
    }

    #[test]
    fn skill_refusals_exit_2_before_anything_runs() {
        assert!(is_subcommand("skill"));
        let secretish = format!("https://user:{}@github.com/o/r", "pw-123");
        for raw in [
            vec![],
            strings(&["frob"]),
            strings(&["add"]),
            strings(&["add", "box"]),
            strings(&["add", "box", "o/r#Bad_Name"]),
            strings(&["add", "box", "#x"]),
            vec!["add".to_string(), "box".to_string(), secretish.clone()],
            strings(&["rm", "box"]),
            strings(&["rm", "box", "o/r#"]),
            strings(&["ls", "a", "b"]),
            strings(&["ls", "--bogus"]),
            strings(&["sync"]),
            strings(&["sync", "a", "b"]),
            strings(&["add", "--global"]),
            strings(&["add", "--global", "o/r#Bad_Name"]),
            strings(&["rm", "--global"]),
            strings(&["rm", "--global", "o/r#"]),
            strings(&["add", "--global=yes", "o/r"]),
        ] {
            let fail = skill(&raw).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{raw:?}");
            assert!(!fail.message.contains("pw-123"), "{}", fail.message);
        }
        let (name, specs) = sandbox_and_specs(&strings(&["box", "o/r", "o/s#one"]), "add").unwrap();
        assert_eq!(name.as_deref(), Some("box"));
        assert_eq!(specs, strings(&["o/r", "o/s#one"]));
        // `--global` takes every positional as a spec, wherever the flag stands.
        for raw in [
            strings(&["--global", "o/r", "o/s#one"]),
            strings(&["o/r", "--global", "o/s#one"]),
        ] {
            let (name, specs) = sandbox_and_specs(&raw, "remove").unwrap();
            assert_eq!(name, None, "{raw:?}");
            assert_eq!(specs, strings(&["o/r", "o/s#one"]), "{raw:?}");
        }
    }

    #[test]
    fn skill_ls_text_shows_the_global_entries() {
        let entry = |repo: &str, origin: Option<&str>| {
            let mut e = serde_json::json!({
                "repo": repo,
                "skills": ["a"],
                "commit": "0123456789abcdef",
                "resolves_to": ["a"],
                "problem": null,
            });
            if let Some(origin) = origin {
                e["origin"] = origin.into();
            }
            e
        };
        let doc = serde_json::json!({
            "global": [entry("g", None)],
            "sandboxes": {
                "box": [entry("g", Some("both")), entry("o", Some("sandbox"))],
                "new": [entry("g", Some("global"))],
            },
        });
        let text = skill_ls_text(&doc);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("global  g  "), "{text}");
        assert!(lines[1].starts_with("box  g  ") && lines[1].ends_with("(global + own)"));
        assert!(
            lines[2].starts_with("box  o  ") && lines[2].ends_with("→ a"),
            "{text}"
        );
        assert!(lines[3].starts_with("new  g  ") && lines[3].ends_with("(global)"));
    }

    #[test]
    fn up_checks_its_skill_specs_first() {
        let args = Args::parse(
            &strings(&["--skill", "o/r", "--skill=o/s#one"]),
            &[],
            &["skill"],
        )
        .unwrap();
        assert_eq!(args.all("skill"), strings(&["o/r", "o/s#one"]));
        for raw in [
            strings(&["--skill", "o/r#Bad"]),
            strings(&["--skill", "o/r", "--skill", "/local"]),
            strings(&["--skill"]),
        ] {
            assert_eq!(up(&raw).expect_err("refused").code, EXIT_USAGE, "{raw:?}");
        }
    }

    #[test]
    fn parses_flags_values_and_the_agent_tail() {
        let raw = strings(&[
            "box",
            "--prompt=hi",
            "--model",
            "fable",
            "--no-update",
            "--",
            "--x",
            "y",
        ]);
        let args = Args::parse(&raw, &["no-update"], &["prompt", "model"]).unwrap();
        assert_eq!(args.name().unwrap(), "box");
        assert_eq!(args.one("prompt").unwrap().as_deref(), Some("hi"));
        assert_eq!(args.one("model").unwrap().as_deref(), Some("fable"));
        assert!(args.has("no-update"));
        assert_eq!(args.trailing, strings(&["--x", "y"]));
    }

    #[test]
    fn held_requests_are_this_sandboxs_and_a_failed_read_says_so() {
        let held = |sandbox: &str| crate::testkit::held(sandbox, "example.org:443");
        let mine = held_by("box", Ok(vec![held("box"), held("other")])).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].sandbox, "box");
        let warning = held_by("box", Err(anyhow::anyhow!("unknown flag: --json"))).unwrap_err();
        assert!(warning.starts_with("aisbx: warning: "), "{warning}");
        assert!(
            warning.contains("box") && warning.contains("unknown flag: --json"),
            "{warning}"
        );
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        assert!(Args::parse(&strings(&["--bogus"]), &[], &[]).is_err());
        assert!(Args::parse(&strings(&["-x"]), &[], &[]).is_err());
        assert!(
            Args::parse(&strings(&["--model"]), &[], &["model"]).is_err(),
            "needs a value"
        );
        assert!(
            Args::parse(&strings(&["--json=1"]), &["json"], &[]).is_err(),
            "a switch"
        );
        let twice =
            Args::parse(&strings(&["--model", "a", "--model", "b"]), &[], &["model"]).unwrap();
        assert!(twice.one("model").is_err());
    }

    #[test]
    fn reads_go_style_durations() {
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("45m"), Some(Duration::from_secs(2700)));
        assert_eq!(parse_duration("4h"), Some(Duration::from_secs(14400)));
        assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
        for bad in ["", "0s", "10", "5x", "h", "1.5h"] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn mounts_are_absolute_existing_folders_away_from_home_and_credentials() {
        let base = std::env::temp_dir().join(format!("aisbx-mount-test-{}", std::process::id()));
        let home = base.join("home");
        let cache = base.join("cache");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join("work")).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let real = cache.canonicalize().unwrap();
        let ro = format!("{}:ro", cache.display());
        assert_eq!(
            mount_arg(&ro, Some(&home)).unwrap(),
            format!("{}:ro", real.display())
        );
        assert_eq!(
            mount_arg(cache.to_str().unwrap(), Some(&home)).unwrap(),
            real.display().to_string()
        );
        assert!(
            mount_arg(home.join("work").to_str().unwrap(), Some(&home)).is_ok(),
            "a folder below home"
        );
        for bad in ["relative/dir", "/definitely/not/here/aisbx"] {
            assert!(mount_arg(bad, Some(&home)).is_err(), "{bad}");
        }
        assert!(
            mount_arg(home.to_str().unwrap(), Some(&home)).is_err(),
            "the home folder"
        );
        assert!(
            mount_arg(base.to_str().unwrap(), Some(&home)).is_err(),
            "a folder holding it"
        );
        assert!(
            mount_arg(home.join(".ssh").to_str().unwrap(), Some(&home)).is_err(),
            "credentials"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[cfg(unix)]
    fn mount_refusals_keep_their_wording() {
        let base = std::env::temp_dir().join(format!("aisbx-mount-words-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let real_home = home.canonicalize().unwrap();
        let raw = home.to_str().unwrap();
        assert_eq!(
            mount_arg(raw, Some(&home)).unwrap_err().message,
            format!("--mount {raw:?}: that would mount your home folder")
        );
        let ssh = home.join(".ssh");
        let raw = format!("{}:ro", ssh.display());
        assert_eq!(
            mount_arg(&raw, Some(&home)).unwrap_err().message,
            format!(
                "--mount {raw:?}: credentials live there ({})",
                real_home.join(".ssh").display()
            )
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn limits_default_to_aisbx_and_auto_is_sbx_own() {
        let argv = |cpus: Option<&str>, memory: Option<&str>| {
            let (cpus, memory) =
                limit_args(cpus.map(String::from), memory.map(String::from)).unwrap();
            sbx::create_argv(sbx::Create {
                name: "n",
                agent: "claude",
                agent_kit: None,
                kit_dirs: &[],
                workspace: Path::new("/w"),
                mounts: &[],
                clone: false,
                ports: &[],
                skills: None,
                deny_network: &[],
                static_mcp: &[],
                cpus,
                memory: memory.as_deref(),
                template: None,
            })
            .join(" ")
        };
        assert!(
            argv(None, None).contains("--cpus 4 -m 4g"),
            "{}",
            argv(None, None)
        );
        assert!(argv(Some("2"), Some("6g")).contains("--cpus 2 -m 6g"));
        let auto = argv(Some("auto"), Some("auto"));
        assert!(!auto.contains("--cpus") && !auto.contains(" -m "), "{auto}");
        assert!(!argv(Some("0"), Some("auto")).contains("--cpus"));
        assert!(
            argv(Some("auto"), None).contains("-m 4g"),
            "each falls back alone"
        );
        let fail = limit_args(Some("two".into()), None).unwrap_err();
        assert_eq!(fail.code, EXIT_USAGE);
    }

    #[test]
    fn an_existing_sandbox_of_another_agent_is_refused_with_the_reason() {
        assert_eq!(
            reuse_refusal(
                "box",
                "claude-anthropic",
                "claude-anthropic",
                "claude-anthropic"
            ),
            None
        );
        let other = reuse_refusal(
            "box",
            "codex-openai",
            "claude-anthropic",
            "claude-anthropic",
        )
        .unwrap();
        assert!(
            other.contains("its agent is codex-openai — not claude-anthropic's"),
            "{other}"
        );
    }

    #[test]
    fn a_template_is_refused_under_a_v3_workload() {
        let reference = "my-template-claude-anthropic:20261020t030000z";
        assert_eq!(
            template_arg(None, "claude-token").unwrap(),
            None,
            "no flag, no check"
        );
        let refused = template_arg(Some(reference.to_string()), "claude-anthropic").unwrap_err();
        assert_eq!(refused.code, EXIT_USAGE);
        assert!(
            refused
                .message
                .contains("saved image carries no kit descriptor"),
            "{}",
            refused.message
        );
        assert!(!refused.message.contains("0.50"), "{}", refused.message);
    }

    #[test]
    fn a_template_never_reuses_a_sandbox_that_exists() {
        assert_eq!(
            template_reuse("box", Some("t:v1")).unwrap_err().code,
            EXIT_USAGE
        );
        assert!(
            template_reuse("box", None).is_ok(),
            "without --template: reused as before"
        );
        let raw = strings(&["--backend", "claude-anthropic", "--template", "t:v1", "/w"]);
        let args = Args::parse(&raw, &["clone", "ssh-agent"], &["backend", "template"]).unwrap();
        assert_eq!(args.one("template").unwrap().as_deref(), Some("t:v1"));
    }

    #[test]
    fn checks_sandbox_names_like_sbx_does() {
        assert!(valid_sandbox_name("claude-anthropic-knowledge-base"));
        assert!(valid_sandbox_name("kiz-claude.2"));
        for bad in [
            "",
            "a",
            "default",
            "-x",
            "x-",
            "x.",
            "my_box",
            "a b",
            &"x".repeat(64),
        ] {
            assert!(!valid_sandbox_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn builds_the_headless_launch_for_each_agent() {
        let run = HeadlessRun {
            model: Some("m"),
            effort: Some("low"),
            format: OutputFormat::Json,
            print_timeout: "4h",
            extra: &[],
        };
        let claude = kits::headless_argv("claude", &run).unwrap();
        assert_eq!(&claude[..4], ["claude", "-p", "--output-format", "json"]);
        assert!(claude.contains(&"--dangerously-skip-permissions".to_string()));
        assert_eq!(&claude[claude.len() - 2..], ["--", sbx::PROMPT_SLOT]);

        let codex = kits::headless_argv("codex", &run).unwrap();
        assert_eq!(&codex[..3], ["codex", "exec", "--json"]);
        assert!(codex.contains(&"model_reasoning_effort=\"low\"".to_string()));
        assert!(codex.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert_eq!(&codex[codex.len() - 2..], ["--", sbx::PROMPT_SLOT]);

        let agy = kits::headless_argv("agy", &run).unwrap();
        assert_eq!(&agy[..3], ["agy", "--print", sbx::PROMPT_SLOT]);
        let at = agy.iter().position(|a| a == "--print-timeout").unwrap();
        assert_eq!(agy[at + 1], "4h");

        let pi = kits::headless_argv("pi-litellm", &run).unwrap();
        assert_eq!(
            pi,
            [
                "pi",
                "-p",
                "--mode",
                "json",
                "--model",
                "m",
                "--thinking",
                "low",
                "--",
                sbx::PROMPT_SLOT
            ]
        );

        let stream = HeadlessRun {
            format: OutputFormat::StreamJson,
            ..run.clone()
        };
        assert!(kits::headless_argv("claude-token", &stream)
            .unwrap()
            .contains(&"--verbose".to_string()));
        let no_effort = HeadlessRun {
            effort: None,
            ..run.clone()
        };
        assert_eq!(
            kits::headless_argv("opencode-litellm", &no_effort).unwrap(),
            [
                "opencode",
                "run",
                "--format",
                "json",
                "--model",
                "m",
                "--",
                sbx::PROMPT_SLOT
            ]
        );
        assert!(kits::headless_argv("unknown", &run).is_none());

        let launch = sbx::headless_exec_argv("box", Some(Path::new("/w")), "/tmp/r", &claude);
        assert_eq!(&launch[..5], ["sbx", "exec", "-w", "/w", "box"]);
        assert_eq!(launch[launch.len() - claude.len() - 1], "/tmp/r");
    }

    #[test]
    #[cfg(unix)]
    fn pi_gets_one_stream_for_both_json_formats_and_its_prompt_after_the_dashes() {
        let extra = strings(&["--no-session"]);
        let run = |format| HeadlessRun {
            model: None,
            effort: None,
            format,
            print_timeout: "0",
            extra: &extra,
        };
        let text = kits::headless_argv("pi-litellm", &run(OutputFormat::Text)).unwrap();
        assert_eq!(text, ["pi", "-p", "--no-session", "--", sbx::PROMPT_SLOT]);
        let json = kits::headless_argv("pi-litellm", &run(OutputFormat::Json)).unwrap();
        let stream = kits::headless_argv("pi-litellm", &run(OutputFormat::StreamJson)).unwrap();
        assert_eq!(json, stream, "Pi has no single-document form");
        assert_eq!(
            json,
            [
                "pi",
                "-p",
                "--mode",
                "json",
                "--no-session",
                "--",
                sbx::PROMPT_SLOT
            ]
        );

        // Through the real wrapper: a prompt starting with `-` is the argument after `--`.
        let dir = std::env::temp_dir().join(format!("aisbx-pi-wrapper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prompt"), "- list the files\n").unwrap();
        let mut argv = strings(&["printf", "[%s]\\n"]);
        argv.extend(text[1..].iter().cloned());
        let launch = sbx::headless_exec_argv("box", None, dir.to_str().unwrap(), &argv);
        let inner = &launch[3..];
        let out = Command::new(&inner[0]).args(&inner[1..]).output().unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            printed, "[-p]\n[--no-session]\n[--]\n[- list the files]\n",
            "{printed}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_gets_one_stream_for_both_json_formats_and_its_prompt_after_the_dashes() {
        let extra = strings(&["--title", "t"]);
        let run = |format| HeadlessRun {
            model: Some("litellm-gw/m"),
            effort: None,
            format,
            print_timeout: "0",
            extra: &extra,
        };
        let text = kits::headless_argv("opencode-litellm", &run(OutputFormat::Text)).unwrap();
        assert_eq!(
            text,
            [
                "opencode",
                "run",
                "--model",
                "litellm-gw/m",
                "--title",
                "t",
                "--",
                sbx::PROMPT_SLOT
            ]
        );
        let json = kits::headless_argv("opencode-litellm", &run(OutputFormat::Json)).unwrap();
        let stream =
            kits::headless_argv("opencode-litellm", &run(OutputFormat::StreamJson)).unwrap();
        assert_eq!(json, stream, "OpenCode has no single-document form");
        assert_eq!(
            json,
            [
                "opencode",
                "run",
                "--format",
                "json",
                "--model",
                "litellm-gw/m",
                "--title",
                "t",
                "--",
                sbx::PROMPT_SLOT
            ]
        );
        for agent in ["opencode-local", "opencode"] {
            assert_eq!(
                kits::headless_argv(agent, &run(OutputFormat::Json)).unwrap(),
                json,
                "{agent}"
            );
        }
    }

    #[test]
    fn run_refuses_an_effort_on_opencode_and_names_every_headless_agent() {
        let run = HeadlessRun {
            model: None,
            effort: Some("high"),
            format: OutputFormat::Json,
            print_timeout: "0",
            extra: &[],
        };
        for agent in ["opencode-litellm", "opencode-local", "opencode"] {
            assert!(!kits::accepts_effort(agent), "{agent}");
            let fail = agent_argv("box", agent, &run).unwrap_err();
            assert_eq!(fail.code, EXIT_USAGE);
            assert!(fail.message.contains("OpenCode"), "{}", fail.message);
            assert!(fail.message.contains("--effort"), "{}", fail.message);
        }
        for agent in ["claude-litellm", "codex-openai", "agy", "pi-litellm"] {
            assert!(kits::accepts_effort(agent), "{agent}");
            assert!(agent_argv("box", agent, &run).is_ok(), "{agent}");
        }
        let without = HeadlessRun {
            effort: None,
            ..run.clone()
        };
        assert!(agent_argv("box", "opencode-litellm", &without).is_ok());

        let fail = agent_argv("box", "unknown", &without).unwrap_err();
        assert_eq!(fail.code, EXIT_USAGE);
        for name in ["Claude Code", "Codex", "agy", "Pi", "OpenCode"] {
            assert!(fail.message.contains(name), "{name}: {}", fail.message);
        }
        assert!(HELP.contains("Pi and OpenCode"));
    }

    #[test]
    #[cfg(unix)]
    fn the_wrapper_puts_the_prompt_where_the_slot_was() {
        let dir = std::env::temp_dir().join(format!("aisbx-wrapper-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("prompt"),
            "- a \"quoted\" $HOME prompt\nsecond line\n",
        )
        .unwrap();
        let launch = sbx::headless_exec_argv(
            "box",
            None,
            dir.to_str().unwrap(),
            &strings(&["printf", "[%s]\\n", "first arg", sbx::PROMPT_SLOT, "--"]),
        );
        // Everything after `sbx exec box` is what runs inside the sandbox.
        let inner = &launch[3..];
        let out = Command::new(&inner[0]).args(&inner[1..]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text, "[first arg]\n[- a \"quoted\" $HOME prompt\nsecond line]\n[--]\n",
            "{text}"
        );
        assert!(std::fs::read_to_string(dir.join("pid"))
            .unwrap()
            .trim()
            .parse::<u32>()
            .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn the_kill_script_takes_the_grandchildren_too() {
        let dir = std::env::temp_dir().join(format!("aisbx-kill-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // agent (sh) → tool (sh) → `sleep`, the shape of an agent's shell tool.
        let mut agent = Command::new("sh")
            .args(["-c", "sh -c 'sleep 300; true' & wait"])
            .spawn()
            .unwrap();
        std::fs::write(dir.join("pid"), agent.id().to_string()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let tree = |pid: u32| -> Vec<String> {
            let out = Command::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        let tool = tree(agent.id());
        assert_eq!(tool.len(), 1, "the tool shell is running");
        let sleeper = tree(tool[0].parse().unwrap());
        assert_eq!(sleeper.len(), 1, "and its sleep under it");

        let status = Command::new("sh")
            .args(["-c", sbx::HEADLESS_KILL, "sh", dir.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        let _ = agent.wait();
        let alive = Command::new("kill")
            .args(["-0", &sleeper[0]])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "the grandchild was stopped too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn connection(name: &str, url: &str) -> litellm::Connection {
        litellm::Connection {
            name: name.into(),
            url: url.into(),
            engine: None,
        }
    }

    #[test]
    fn the_litellm_commands_refuse_bad_arguments() {
        for raw in [
            strings(&[]),
            strings(&["nope"]),
            strings(&["ls", "extra"]),
            strings(&["ls", "--force"]),
            strings(&["add"]),
            strings(&["add", "a.json", "b.json"]),
            strings(&["add", "a.json", "--key", "x"]),
            strings(&["add", "/nonexistent/aisbx-connection.json"]),
            strings(&["rm"]),
            strings(&["rm", "Bad_Name"]),
            // No connection exists under `cargo test` (`litellm::exists` reads nothing there).
            strings(&["rm", "acme"]),
            strings(&["rm", "acme", "--force"]),
        ] {
            let fail = litellm_cmd(&raw).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{raw:?}: {}", fail.message);
        }
        assert!(is_subcommand("litellm"));
        let args = Args::parse(
            &strings(&["c.json", "--key-stdin", "--replace"]),
            &["key-stdin", "replace"],
            &[],
        )
        .unwrap();
        assert!(args.has("key-stdin") && args.has("replace"));
        assert_eq!(args.positional, strings(&["c.json"]));
    }

    #[test]
    fn a_connection_file_is_checked_before_anything_runs() {
        let good = r#"{"name": "acme", "url": "https://litellm.example.com"}"#;
        let (conn, models) = parse_connection(good).expect("a good file parses");
        assert_eq!(
            conn,
            litellm::Connection::new("acme", "https://litellm.example.com")
        );
        assert!(!models);
        let old = r#"{"name": "acme", "url": "https://h", "default_model": "m-1",
            "models": [{"id": "m-1", "context": 200000}]}"#;
        let (conn, models) = parse_connection(old).expect("an older file still parses");
        assert!(models, "its models are noted as ignored");
        assert_eq!(conn.url, "https://h");
        assert!(
            parse_connection(r#"{"name": "acme", "url": "https://h", "default_model": "x"}"#)
                .unwrap()
                .1
        );
        for bad in [
            "not json",
            r#"{"name": "acme"}"#,
            &good.replace("https://litellm.example.com", "http://x"),
            &good.replace("https://litellm.example.com", "https://h:4000"),
            &good.replace("\"acme\"", "\"Name_1\""),
        ] {
            let fail = parse_connection(bad).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE, "{bad}");
        }
    }

    #[test]
    fn a_run_writes_the_connections_model_list_first() {
        let root = crate::testkit::litellm_root();
        let conn = connection("acme", "https://litellm.example.com/");
        let argv = push_argv_in(&root, "opencode-proj", &conn, litellm::Agent::Opencode)
            .expect("OpenCode gets its config");
        assert_eq!(&argv[..3], ["sbx", "exec", "opencode-proj"]);
        assert!(
            argv.contains(&litellm::opencode_config_path("acme")),
            "{argv:?}"
        );
        let pi = push_argv_in(&root, "pi-proj", &conn, litellm::Agent::Pi).expect("Pi too");
        assert!(pi.iter().any(|a| a == litellm::PI_MODELS_PATH));
        assert!(pi.iter().any(|a| a == litellm::PI_SETTINGS_PATH));
        let claude = push_argv_in(&root, "claude-proj", &conn, litellm::Agent::Claude)
            .expect("Claude Code gets its model picker");
        assert!(claude.iter().any(|a| a == litellm::CLAUDE_SETTINGS_PATH));
    }

    #[test]
    fn adding_an_existing_connection_needs_replace() {
        let fail = add_refusal("acme", true, false).expect_err("refused");
        assert_eq!(fail.code, EXIT_USAGE);
        assert!(
            fail.message.contains("connection acme exists"),
            "{}",
            fail.message
        );
        assert!(add_refusal("acme", true, true).is_ok());
        assert!(add_refusal("acme", false, false).is_ok());
    }

    #[test]
    fn moving_a_connection_to_another_host_needs_its_key() {
        let old = crate::testkit::acme();
        let mut moved = old.clone();
        moved.url = "https://gw.example.org".into();
        let fail = moved_host_refusal(Some(&old), &moved, false).expect_err("refused");
        assert_eq!(fail.code, EXIT_USAGE);
        assert!(fail.message.contains("--key-stdin"), "{}", fail.message);
        assert!(moved_host_refusal(Some(&old), &moved, true).is_ok());
        // The same host, another path: the stored key still targets it.
        let mut same = old.clone();
        same.url = "https://litellm.example.com/v2".into();
        assert!(moved_host_refusal(Some(&old), &same, false).is_ok());
        assert!(moved_host_refusal(None, &moved, false).is_ok());
    }

    #[test]
    fn the_key_is_read_from_stdin_without_its_newline() {
        assert_eq!(
            read_key(&mut "value-from-stdin\r\n".as_bytes()).unwrap(),
            "value-from-stdin"
        );
        let empty = read_key(&mut "\n".as_bytes()).unwrap_err();
        assert_eq!(empty.code, EXIT_USAGE);
        assert_eq!(read_key(&mut "".as_bytes()).unwrap_err().code, EXIT_USAGE);
    }

    #[test]
    fn up_picks_the_named_or_the_only_connection_never_the_last_used() {
        let acme = connection("acme", "https://litellm.example.com");
        let beta = connection("beta", "https://gw.example.org");
        use litellm::Kind::Gateway as Gw;
        let none = pick_connection(&[], None, Some("acme"), Gw).expect_err("none");
        assert_eq!(none.code, EXIT_USAGE);
        assert!(
            none.message.contains("aisbx litellm add"),
            "{}",
            none.message
        );

        let one = pick_connection(std::slice::from_ref(&beta), None, Some("acme"), Gw).unwrap();
        assert_eq!(one.name, "beta");

        let two = [acme, beta.clone()];
        let several = pick_connection(&two, None, Some("beta"), Gw).expect_err("several");
        assert_eq!(several.code, EXIT_USAGE);
        for want in [
            "acme",
            "beta",
            "last used: beta",
            "pick one: --litellm <name>",
        ] {
            assert!(
                several.message.contains(want),
                "{want}: {}",
                several.message
            );
        }
        // A remembered name that is gone is not named.
        let stale = pick_connection(&two, None, Some("gone"), Gw).expect_err("several");
        assert!(!stale.message.contains("gone"), "{}", stale.message);

        assert_eq!(pick_connection(&two, Some("beta"), None, Gw).unwrap(), beta);
        let unknown = pick_connection(&two, Some("gamma"), Some("acme"), Gw).expect_err("unknown");
        assert_eq!(unknown.code, EXIT_USAGE);
        assert!(unknown.message.contains("gamma"), "{}", unknown.message);
        assert_eq!(
            pick_connection(&two, Some("Bad_Name"), None, Gw)
                .unwrap_err()
                .code,
            EXIT_USAGE
        );
    }

    #[test]
    fn a_job_command_is_a_subcommand_named_by_its_first_words() {
        for args in [
            &["skill"][..],
            &["skill", "sync", "box"],
            &["local", "fetch", "llama-server"],
            &["local", "fetch", "llama-server", "--key-stdin"],
        ] {
            assert!(is_job_command(args), "{args:?}");
            assert!(is_subcommand(args[0]), "{args:?}");
            assert_eq!(job_argv(args)[1..], *args);
        }
        for args in [
            &[][..],
            &["local"],
            &["local", "ls"],
            &["local", "--here"],
            &["locals", "fetch"],
            &["up", "x"],
        ] {
            assert!(!is_job_command(args), "{args:?}");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "not in JOB_COMMANDS")]
    fn a_job_runs_this_binary_only_with_a_job_command() {
        job_argv(&["up", "x"]);
    }

    /// On a local backend, `up` picks among the local engines and names `--local` and
    /// `aisbx local add`.
    #[test]
    fn up_picks_a_local_engine_by_its_own_flag() {
        use litellm::Kind::Local;
        let none = pick_connection(&[], None, None, Local).unwrap_err();
        assert!(none.message.contains("no local engine"), "{}", none.message);
        assert!(none.message.contains("aisbx local add"), "{}", none.message);
        let k2 = connection("k2", "http://localhost:8095");
        let ol = connection("ollama", "http://localhost:11434");
        let several = pick_connection(&[k2.clone(), ol], None, None, Local).unwrap_err();
        assert!(
            several.message.contains("--local <name>"),
            "{}",
            several.message
        );
        assert_eq!(
            pick_connection(std::slice::from_ref(&k2), None, None, Local).unwrap(),
            k2
        );

        let flag = |kind, litellm: Option<&str>, local: Option<&str>| {
            connection_flag(kind, litellm.map(Into::into), local.map(Into::into), "b")
        };
        use litellm::Kind::Gateway;
        assert_eq!(
            flag(Some(Local), None, Some("k2")).unwrap().as_deref(),
            Some("k2")
        );
        assert_eq!(
            flag(Some(Gateway), Some("acme"), None).unwrap().as_deref(),
            Some("acme")
        );
        assert_eq!(flag(None, None, None).unwrap(), None);
        let wrong = flag(Some(Local), Some("acme"), None).unwrap_err();
        assert!(
            wrong.message.contains("pass --local NAME"),
            "{}",
            wrong.message
        );
        assert!(flag(Some(Gateway), None, Some("k2")).is_err());
        assert!(flag(None, None, Some("k2")).is_err());
        assert!(flag(None, Some("acme"), None).is_err());
    }

    /// `local discover` prints what it found, and never a key; `local ls` names each engine.
    #[test]
    fn the_local_listings_say_what_answers_and_never_a_key() {
        let found = crate::local::Found {
            engine: crate::local::Engine::LlamaServer,
            port: 8095,
            key: Some("k-secret".into()),
            key_source: Some("its key file /x/api-keys".into()),
            needs_key: false,
            models: vec![crate::models::Model {
                id: "k2-horizon-7b".into(),
                name: "k2-horizon-7b".into(),
                limits: crate::models::Limits {
                    context: Some(32768),
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        let mut k2 = connection("k2", "http://localhost:8095");
        k2.engine = Some("llama-server".into());
        let text = discover_output(
            std::slice::from_ref(&found),
            std::slice::from_ref(&k2),
            false,
        );
        assert_eq!(
            text,
            "llama-server on localhost:8095 · 1 model · 32k context  key from its key file \
             /x/api-keys  (engine k2)\n"
        );
        let json = discover_output(std::slice::from_ref(&found), &[], true);
        assert!(!json.contains("k-secret") && !text.contains("k-secret"));
        let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(rows[0]["url"], "http://localhost:8095");
        assert_eq!(rows[0]["context"], 32768);
        assert_eq!(rows[0]["key_found"], true);
        assert_eq!(rows[0]["anthropic_api"], true);
        assert_eq!(rows[0]["connection"], serde_json::Value::Null);
        let ls = local_ls_output(
            std::slice::from_ref(&k2),
            Some(&sbx::Secrets::default()),
            false,
        );
        assert_eq!(
            ls,
            "k2  http://localhost:8095  llama-server  0 model(s)  no key\n"
        );
        let picked = pick_found(std::slice::from_ref(&found), &[], None, false).unwrap();
        assert_eq!(picked.port, 8095);
        let taken = pick_found(
            std::slice::from_ref(&found),
            std::slice::from_ref(&k2),
            None,
            false,
        );
        assert!(taken.unwrap_err().message.contains("not added yet"));
        assert!(pick_found(std::slice::from_ref(&found), &[k2], None, true).is_ok());
    }

    /// No connection exists under `cargo test`, so a LiteLLM backend is refused also without
    /// `--litellm`.
    #[test]
    fn up_refuses_litellm_where_it_does_not_apply() {
        let Ok(catalog) = Catalog::load() else {
            return;
        };
        if catalog.backend("claude-anthropic").is_some() {
            let raw = strings(&["--backend", "claude-anthropic", "--litellm", "acme"]);
            let fail = up(&raw).expect_err("refused");
            assert_eq!(fail.code, EXIT_USAGE);
            assert!(fail.message.contains("--litellm"), "{}", fail.message);
        }
        for backend in ["claude-litellm", "opencode-litellm"] {
            if catalog.backend(backend).is_none() {
                continue;
            }
            for raw in [
                strings(&["--backend", backend]),
                strings(&["--backend", backend, "--litellm", "acme"]),
            ] {
                let fail = up(&raw).expect_err("refused");
                assert_eq!(fail.code, EXIT_USAGE, "{raw:?}: {}", fail.message);
                assert!(fail.message.contains("connection"), "{}", fail.message);
            }
        }
    }

    #[test]
    fn the_sandboxes_on_a_connection_come_from_their_kit_lists() {
        let pairs = vec![
            (
                "claude-litellm-proj".to_string(),
                strings(&[
                    "/repo/kits/claude-litellm",
                    "/state/litellm/acme/litellm-acme-claude",
                    "/repo/kits/web",
                ]),
            ),
            (
                "opencode-proj".to_string(),
                strings(&[
                    "/repo/kits/opencode-litellm",
                    "/state/litellm/acme-2/litellm-acme-2-opencode",
                ]),
            ),
            (
                "claude-anthropic-proj".to_string(),
                strings(&["/repo/kits/claude-anthropic"]),
            ),
            ("old".to_string(), Vec::new()),
        ];
        assert_eq!(users_of("acme", &pairs), strings(&["claude-litellm-proj"]));
        assert_eq!(users_of("acme-2", &pairs), strings(&["opencode-proj"]));
        assert!(users_of("beta", &pairs).is_empty());
        assert_eq!(connection_of_kits(&pairs[2].1), None);
        let fail = rm_refusal("acme", &users_of("acme", &pairs));
        assert_eq!(fail.code, EXIT_USAGE);
        for want in ["claude-litellm-proj", "--force"] {
            assert!(fail.message.contains(want), "{}", fail.message);
        }
    }

    #[test]
    fn litellm_rm_removes_the_key_only_when_one_is_stored() {
        let stored = || {
            Ok(sbx::Secrets {
                services: Vec::new(),
                custom: vec![crate::testkit::litellm_key("acme", "litellm.example.com")],
            })
        };
        assert!(rm_has_key("acme", true, stored()).unwrap());
        assert!(rm_has_key("acme", false, stored()).unwrap(), "an orphan");
        assert!(!rm_has_key("beta", true, stored()).unwrap(), "keyless");
        let none = rm_has_key("beta", false, stored()).unwrap_err();
        assert_eq!(none.code, EXIT_USAGE);
        assert!(none.message.contains("no LiteLLM connection beta"));
        for exists in [true, false] {
            let fail = rm_has_key("acme", exists, Err(anyhow::anyhow!("daemon down"))).unwrap_err();
            assert_eq!(fail.code, EXIT_SANDBOX);
            assert!(fail.message.contains("daemon down"), "{}", fail.message);
        }
    }

    #[test]
    fn litellm_ls_reports_the_key_per_connection() {
        let acme = connection("acme", "https://litellm.example.com/v1");
        let beta = connection("beta", "https://gw.example.org");
        let gamma = connection("gamma", "https://gamma.example.net");
        let row = |name: &str, host: &str| sbx::CustomSecret {
            scope: sbx::GLOBAL_SCOPE.into(),
            targets: vec![host.to_string()],
            placeholder: litellm::placeholder(name),
            masked: "sk***".into(),
            ..sbx::CustomSecret::default()
        };
        let secrets = sbx::Secrets {
            custom: vec![
                row("acme", "litellm.example.com"),
                row("beta", "other.example.org"),
            ],
            ..sbx::Secrets::default()
        };
        assert_eq!(key_stored(Some(&secrets), &acme), Some(true));
        assert_eq!(
            key_stored(Some(&secrets), &beta),
            Some(false),
            "another host"
        );
        assert_eq!(key_stored(Some(&secrets), &gamma), Some(false));
        assert_eq!(key_stored(None, &acme), None);

        let all = [acme, beta, gamma];
        let json = litellm_ls_output(&all, Some(&secrets), Some("beta"), true);
        let rows: serde_json::Value = serde_json::from_str(&json).expect("one JSON document");
        assert_eq!(rows[0]["name"], "acme");
        assert_eq!(rows[0]["url"], "https://litellm.example.com/v1");
        // The count of the list a sandbox read last: none under `cargo test`.
        assert_eq!(rows[0]["models"], 0);
        assert_eq!(rows[0]["key_stored"], true);
        assert_eq!(rows[0]["last_used"], false);
        assert_eq!(rows[1]["key_stored"], false);
        assert_eq!(rows[1]["last_used"], true);
        let unknown = litellm_ls_output(&all, None, None, true);
        let rows: serde_json::Value = serde_json::from_str(&unknown).unwrap();
        assert_eq!(rows[0]["key_stored"], serde_json::Value::Null);
        // A failed `secret ls` still prints the connections, and exits 125.
        let (out, outcome) =
            litellm_ls_report(&all, Err(anyhow::anyhow!("daemon down")), None, true);
        assert_eq!(out, unknown);
        let fail = outcome.unwrap_err();
        assert_eq!(fail.code, EXIT_SANDBOX);
        assert!(fail.message.contains("daemon down"), "{}", fail.message);
        let (_, ok) = litellm_ls_report(&all, Ok(secrets.clone()), None, true);
        assert_eq!(ok.ok(), Some(0));

        let text = litellm_ls_output(&all, Some(&secrets), Some("beta"), false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with("acme  https://litellm.example.com/v1  0 model(s)  key stored"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].ends_with("key missing  (last used)"),
            "{}",
            lines[1]
        );
        assert_eq!(litellm_ls_output(&[], None, None, true), "[]\n");
    }

    #[test]
    fn up_takes_tmpfs_volumes_as_a_switch() {
        let fail = up(&strings(&[
            "--backend",
            "claude-anthropic",
            "--tmpfs-volumes=yes",
        ]))
        .expect_err("refused");
        assert_eq!(fail.code, EXIT_USAGE);
        assert!(
            fail.message.contains("--tmpfs-volumes takes no value"),
            "{}",
            fail.message
        );
        let fail = up(&strings(&["--backend", "nope", "--tmpfs-volumes"])).expect_err("refused");
        assert_eq!(fail.code, EXIT_USAGE);
        assert!(
            fail.message.contains("no backend kit \"nope\""),
            "{}",
            fail.message
        );
        // A calling script reads the flag off the help before it passes it.
        assert!(HELP
            .lines()
            .any(|l| l.trim_start().starts_with("--tmpfs-volumes ")));
    }

    fn parse_up(raw: &[&str]) -> Args {
        Args::parse(&strings(raw), &["no-mcp"], &[]).unwrap()
    }

    fn create_argv_for(args: &Args) -> Vec<String> {
        let static_mcp = static_mcp_for(args);
        let kit = Path::new("/kits/claude-anthropic");
        sbx::create_argv(sbx::Create {
            name: "box",
            agent: "claude-anthropic",
            agent_kit: Some(kit),
            kit_dirs: &[kit],
            workspace: Path::new("/work"),
            mounts: &[],
            clone: false,
            ports: &[],
            skills: None,
            deny_network: &[],
            static_mcp: &static_mcp,
            cpus: None,
            memory: None,
            template: None,
        })
    }

    #[test]
    fn no_mcp_passes_static_mcp_none_once() {
        let args = parse_up(&["--no-mcp"]);
        let argv = create_argv_for(&args);
        let at: Vec<usize> = (0..argv.len())
            .filter(|&i| argv[i] == "--static-mcp")
            .collect();
        assert_eq!(at.len(), 1, "{argv:?}");
        assert_eq!(argv[at[0] + 1], sbx::STATIC_MCP_NONE);
        assert_eq!(argv.iter().filter(|a| a.as_str() == "none").count(), 1);
    }

    #[test]
    fn without_no_mcp_the_argv_has_no_static_mcp() {
        let args = parse_up(&[]);
        assert!(static_mcp_for(&args).is_empty());
        let argv = create_argv_for(&args);
        let has_flag = argv.iter().any(|a| a == "--static-mcp");
        assert!(!has_flag, "{argv:?}");
    }

    #[test]
    fn no_mcp_is_a_switch() {
        let args = parse_up(&["--no-mcp", "dir"]);
        assert!(args.has("no-mcp"));
        assert_eq!(args.positional, vec!["dir".to_string()]);
        let raw = strings(&["--backend", "claude-anthropic", "--no-mcp=x"]);
        let fail = up(&raw).expect_err("refused");
        assert_eq!(fail.code, EXIT_USAGE);
        let message = &fail.message;
        assert!(message.contains("--no-mcp takes no value"), "{message}");
    }

    #[test]
    fn no_mcp_is_documented() {
        assert!(HELP.contains("\n        --no-mcp "));
        let docs = include_str!("../../docs/headless.md");
        assert!(docs.contains("`--no-mcp`"));
    }

    #[test]
    fn a_reused_sandbox_keeps_its_volumes_where_they_are() {
        let kits_dir = Catalog::load().unwrap().kits_dir;
        let at = |dir: &Path, kit: &str| dir.join(kit).display().to_string();
        let disk = vec![at(&kits_dir, "claude-anthropic"), at(&kits_dir, "host-npm")];
        let ram = vec![
            at(&tmpfs::kits_dir(), "claude-anthropic"),
            at(&kits_dir, "jdk25"),
        ];
        let none = vec![at(&kits_dir, "codex-openai")];
        let refused = volumes_refusal("box", &disk, true).unwrap();
        assert!(
            refused.contains("box exists with its kit volumes on disk, not in RAM"),
            "{refused}"
        );
        assert_eq!(volumes_refusal("box", &disk, false), None);
        assert_eq!(volumes_refusal("box", &ram, true), None);
        let refused = volumes_refusal("box", &ram, false).unwrap();
        assert!(
            refused.contains("in RAM; pass --tmpfs-volumes"),
            "{refused}"
        );
        for want_ram in [true, false] {
            assert_eq!(volumes_refusal("box", &none, want_ram), None);
        }
    }

    #[test]
    fn up_says_which_kits_it_moved_to_ram() {
        let kits_dir = Catalog::load().unwrap().kits_dir;
        let dirs: Vec<PathBuf> = ["claude-anthropic", "jdk25", "host-npm"]
            .iter()
            .map(|k| kits_dir.join(k))
            .collect();
        let copies = tmpfs::copies(&dirs).unwrap();
        assert_eq!(
            copies,
            [
                tmpfs::kits_dir().join("claude-anthropic"),
                dirs[1].clone(),
                tmpfs::kits_dir().join("host-npm")
            ]
        );
        let note = ram_note(&dirs, &copies);
        assert!(
            note.starts_with("volumes in RAM: claude-anthropic, host-npm (copies in "),
            "{note}"
        );
        let codex = vec![kits_dir.join("codex-openai")];
        assert_eq!(
            ram_note(&codex, &tmpfs::copies(&codex).unwrap()),
            "no kit here keeps a volume on disk: nothing to move to RAM"
        );
    }
}

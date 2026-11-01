//! Discovery and classification of this repo's `kits/` directory: a `kind: workload` kit is a
//! backend (the agent itself), a `kind: mixin` kit is a stackable addon.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use yaml_rust2::{Yaml, YamlLoader};

/// The parsed surface of a kit descriptor (`<folder>/<folder>.yaml`) that the dashboard cares
/// about.
#[derive(Debug, Clone)]
pub struct Kit {
    /// The folder's name: what sbx calls the kit (its handle) and, for a workload, the agent.
    pub name: String,
    pub dir: PathBuf,
    pub display_name: String,
    /// The descriptor's `description`, folded onto one line; empty where it has none.
    pub description: String,
    /// Why `sbx kit add` would not apply this kit to a live sandbox, if it would not — see
    /// [`kit_add_blocker`]. `None` means the kit can be added to a running v3 sandbox.
    pub kit_add_blocker: Option<&'static str>,
    /// Secret services the kit's `credential@1` capabilities name, minus the ones it marks
    /// `optional: true`. See also [`Backend::required_services`].
    pub credentials: Vec<String>,
    /// `kind: workload`: the kit is the agent, a backend. A `kind: mixin` is an addon.
    pub is_workload: bool,
    /// The kit declares where its agent reads skills (`agent-skills@1`), so sbx mounts the shared
    /// skills store for it (`--skills`) — see [`Backend::shares_skills`].
    pub skills: bool,
    /// The kit declares volumes (`volume@1`): what it keeps there outlives `sbx rm`, so a rebuild
    /// keeps it. On a workload, that is the agent's session history.
    pub volumes: bool,
    /// What changed in the kit, newest first: the lines of its [`CHANGES_FILE`].
    pub changes: Vec<String>,
    /// The harnesses ([`Backend::harness`]) a mixin works with: the lines of its
    /// [`HARNESSES_FILE`]. Empty means every harness.
    pub harnesses: Vec<String>,
}

/// A kit's list of changes, beside its descriptor: one line per change, newest first; blank lines
/// and `#` lines are skipped. It describes the kit and is not part of its hash.
pub const CHANGES_FILE: &str = "changes.txt";

/// The harnesses a mixin works with, beside its descriptor: one agent CLI per line
/// ([`Backend::harness`]: `claude`, `opencode`, …); blank lines and `#` lines are skipped. Without
/// the file the kit works with every harness. It describes the kit and is not part of its hash.
pub const HARNESSES_FILE: &str = "harnesses.txt";

/// The changes a kit folder lists in its [`CHANGES_FILE`], newest first; none without one.
pub fn read_changes(dir: &Path) -> Vec<String> {
    read_lines(dir, CHANGES_FILE)
}

/// The non-blank, non-`#` lines of `dir`'s `file`, trimmed; none without the file.
fn read_lines(dir: &Path, file: &str) -> Vec<String> {
    fs::read_to_string(dir.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// What a backend authenticates against an Anthropic subscription with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Account {
    /// The host-global `/login`, proxy-managed by sbx and shared by every `/login` sandbox on the
    /// machine.
    Login,
    /// A named `claude setup-token`, stored once on the host; every sandbox on the name uses its
    /// account.
    Token,
}

/// Who serves a backend's model: the create wizard's first step, before the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    Anthropic,
    Google,
    OpenAi,
    /// The user's own LiteLLM gateway, through a connection ([`crate::litellm`]).
    Litellm,
    /// A backend this build does not place with a provider.
    Other,
}

impl Provider {
    /// The name the wizard shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Google => "Google",
            Self::OpenAi => "OpenAI",
            Self::Litellm => "LiteLLM",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub kit: Kit,
    /// The agent sbx reports for a sandbox on this backend: the workload's folder name.
    /// `sbx create` takes the folder itself ([`crate::sbx::Create::agent_kit`]).
    pub agent: String,
}

#[derive(Debug, Clone)]
pub struct Addon {
    pub kit: Kit,
}

impl Backend {
    /// Base for auto-generated sandbox labels, e.g. `OpenCode` → "OpenCode (RTK, JDK25)".
    pub fn base_label(&self) -> String {
        match self.kit.name.as_str() {
            "claude-litellm" => "Claude Code LiteLLM".into(),
            "claude-anthropic" | "claude-token" => "Claude Code".into(),
            "opencode-litellm" => "OpenCode".into(),
            "codex-openai" => "Codex".into(),
            "agy" => "Antigravity".into(),
            "pi-litellm" => "Pi".into(),
            _ => self.kit.display_name.clone(),
        }
    }

    /// Compact backend description for the sandbox table's TYPE column.
    pub fn type_label(&self) -> String {
        match self.kit.name.as_str() {
            "claude-litellm" => "Claude Code · LiteLLM".into(),
            // No suffix: the account is the interesting half here, and `Sandbox::type_label`
            // appends it (`Claude Code · /login`, `Claude Code · token private`).
            "claude-anthropic" | "claude-token" => "Claude Code".into(),
            "opencode-litellm" => "OpenCode · LiteLLM".into(),
            "codex-openai" => "Codex · OpenAI".into(),
            "agy" => "Antigravity · Google".into(),
            "pi-litellm" => "Pi · LiteLLM".into(),
            _ => self.kit.display_name.clone(),
        }
    }

    /// One line on what this backend runs on: the difference the choice makes.
    pub fn setup_hint(&self) -> &'static str {
        match self.kit.name.as_str() {
            "claude-litellm" | "opencode-litellm" | "pi-litellm" => {
                "a LiteLLM gateway of your choice (a connection); no Anthropic account"
            }
            "claude-anthropic" => "your own Anthropic account (Pro, Max or Enterprise)",
            "claude-token" => "your own Anthropic account, via a setup token",
            "codex-openai" => {
                "your own OpenAI account — ChatGPT sign-in or API key, kept on the host"
            }
            "agy" => "your Google account — signed in once, kept on the host — or a Gemini API key",
            _ => "",
        }
    }

    /// The secret services this backend cannot work without: its kit's needed ones plus
    /// [`agent_credential`]. The create form checks them against `sbx secret ls`.
    pub fn required_services(&self) -> Vec<String> {
        let mut out = self.kit.credentials.clone();
        if let Some(service) = agent_credential(&self.agent) {
            if !out.iter().any(|s| s == service) {
                out.push(service.to_string());
            }
        }
        out
    }

    /// Where this backend sits in the create form. The first one is the default for a new sandbox.
    fn rank(&self) -> u8 {
        match self.kit.name.as_str() {
            "claude-litellm" => 0,
            "opencode-litellm" => 1,
            "claude-anthropic" => 2,
            _ => 3,
        }
    }

    /// Whether this backend's model aliases can be pinned to a gateway model ([`crate::models`]).
    pub fn model_overrides(&self) -> bool {
        self.kit.name == "claude-litellm"
    }

    /// The agent a LiteLLM backend wires a connection into — `Some` means every sandbox on this
    /// backend is built on a connection ([`crate::litellm`]).
    pub fn litellm_agent(&self) -> Option<crate::litellm::Agent> {
        crate::litellm::Agent::of_backend(&self.kit.name)
    }

    /// Which Anthropic subscription account this backend authenticates with, if any. Keyed by
    /// name, because no descriptor field states it: add each new Claude backend here.
    pub fn account(&self) -> Option<Account> {
        match self.kit.name.as_str() {
            "claude-anthropic" => Some(Account::Login),
            "claude-token" => Some(Account::Token),
            _ => None,
        }
    }

    /// Who serves this backend's model. Keyed by name, because no descriptor field states it: add
    /// each new backend here, or the wizard offers it under *Other*.
    pub fn provider(&self) -> Provider {
        if self.litellm_agent().is_some() {
            return Provider::Litellm;
        }
        if self.account().is_some() {
            return Provider::Anthropic;
        }
        match self.kit.name.as_str() {
            "codex-openai" => Provider::OpenAi,
            "agy" => Provider::Google,
            _ => Provider::Other,
        }
    }

    /// Whether a sandbox on this backend can share the host's agent-skills store (`--skills`).
    pub fn shares_skills(&self) -> bool {
        self.kit.skills
    }

    /// The agent CLI this backend runs, which the create wizard's Harness step offers: the
    /// binary's name ([`agent_command`]), else the agent's name.
    pub fn harness(&self) -> &str {
        agent_command(&self.agent).unwrap_or(&self.agent)
    }
}

/// The agent CLI a sandbox runs, read off the agent name sbx reports: one of sbx's built-in
/// agents or a workload's folder name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cli {
    Claude,
    OpenCode,
    Codex,
    Agy,
    Pi,
}

fn cli(agent: &str) -> Option<Cli> {
    match agent {
        "claude" | "claude-litellm" | "claude-anthropic" | "claude-token" => Some(Cli::Claude),
        "opencode" | "opencode-litellm" => Some(Cli::OpenCode),
        "codex" | "codex-openai" => Some(Cli::Codex),
        "agy" => Some(Cli::Agy),
        "pi-litellm" => Some(Cli::Pi),
        _ => None,
    }
}

/// The command that brings the agent inside a sandbox up to date, for an sbx agent name.
pub fn agent_update(agent: &str) -> Option<&'static [&'static str]> {
    match cli(agent)? {
        Cli::Claude => Some(&["claude", "update"]),
        Cli::OpenCode => Some(&["sh", "-c", OPENCODE_UPDATE]),
        Cli::Codex => Some(&["codex", "update"]),
        Cli::Agy => Some(&["agy", "update"]),
        // The kit's own install command, not `pi update`: the kit turns Pi's version check off.
        Cli::Pi => Some(&["npm", "install", "-g", "--prefix", PI_PREFIX, PI_PACKAGE]),
    }
}

/// Where the `pi-litellm` kit's install hook puts Pi (`<prefix>/bin/pi`): an agent-owned prefix,
/// so [`agent_update`] can write it as the `agent` user.
const PI_PREFIX: &str = "/home/agent/.local";

/// Pi's npm package.
const PI_PACKAGE: &str = "@earendil-works/pi-coding-agent";

/// The OpenCode update, decided inside the sandbox from the env its kit baked in: the kit's
/// `@opencode/cli` range, else `opencode upgrade`.
const OPENCODE_UPDATE: &str = r#"if [ -n "${OPENCODE_LITELLM_VERSION:-}" ]; then npm install -g --force "@opencode/cli@$OPENCODE_LITELLM_VERSION"; else opencode upgrade; fi"#;

/// The command that launches this agent, for a start other than the workspace root
/// ([`crate::startdir`]). `None` for an agent this repo has no facts for.
pub fn agent_command(agent: &str) -> Option<&'static str> {
    match cli(agent)? {
        Cli::Claude => Some("claude"),
        Cli::OpenCode => Some("opencode"),
        Cli::Codex => Some("codex"),
        Cli::Agy => Some("agy"),
        Cli::Pi => Some("pi"),
    }
}

/// The warning under a ticked `headroom` row: unmeasured where it routes Claude Code, idle
/// elsewhere.
pub fn headroom_warning(agent: &str) -> &'static str {
    match agent_command(agent) {
        Some("claude") => {
            "⚠ headroom is unmeasured: Claude Code's requests go through it and fail while it is \
             down."
        }
        _ => "⚠ headroom routes only Claude Code; here it starts an idle proxy.",
    }
}

/// The flags `sbx run` would have added, for the [`agent_command`] launch that replaces it.
/// `sbx exec` runs the exact argv: without them, Claude Code asks before every tool call.
pub fn agent_launch_args(agent_cmd: &str) -> &'static [&'static str] {
    match agent_cmd {
        "claude" => &["--dangerously-skip-permissions"],
        "codex" => &["--dangerously-bypass-approvals-and-sandbox"],
        "agy" => &["--dangerously-skip-permissions"],
        // Pi has no approval prompt to skip.
        "pi" => &[],
        _ => &[],
    }
}

/// What a headless run prints, `aisbx run --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// One JSON result document at the end — Claude Code's and agy's `--output-format json`. Codex
    /// has no single-document form; it gets `--json`, its JSONL event stream, for both JSON formats.
    Json,
    /// One JSON event per line while the agent works.
    StreamJson,
    /// The agent's final answer as plain text.
    Text,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "json" => Some(Self::Json),
            "stream-json" => Some(Self::StreamJson),
            "text" => Some(Self::Text),
            _ => None,
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::StreamJson => "stream-json",
            Self::Text => "text",
        }
    }
}

/// Everything about a headless run that turns into agent flags.
#[derive(Debug, Clone)]
pub struct HeadlessRun<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub format: OutputFormat,
    /// agy's own `--print-timeout`, as the Go duration the caller was given (`4h`, `90m`), or
    /// `0` for none.
    pub print_timeout: &'a str,
    /// Appended after aisbx's own flags, before the prompt.
    pub extra: &'a [String],
}

/// The non-interactive command line for one prompt, for an sbx agent name, with
/// [`crate::sbx::PROMPT_SLOT`] where the prompt goes. `None` for an agent without headless facts.
pub fn headless_argv(agent: &str, run: &HeadlessRun<'_>) -> Option<Vec<String>> {
    let slot = crate::sbx::PROMPT_SLOT.to_string();
    let mut argv: Vec<String> = Vec::new();
    let flags = |argv: &mut Vec<String>, cmd: &str| {
        argv.extend(agent_launch_args(cmd).iter().map(|a| (*a).to_string()));
    };
    match cli(agent) {
        Some(Cli::Claude) => {
            argv.extend(["claude".into(), "-p".into(), "--output-format".into()]);
            argv.push(run.format.flag().into());
            if run.format == OutputFormat::StreamJson {
                argv.push("--verbose".into());
            }
            if let Some(model) = run.model {
                argv.extend(["--model".into(), model.into()]);
            }
            if let Some(effort) = run.effort {
                argv.extend(["--effort".into(), effort.into()]);
            }
            flags(&mut argv, "claude");
            argv.extend(run.extra.iter().cloned());
            argv.extend(["--".into(), slot]);
        }
        Some(Cli::Codex) => {
            argv.extend(["codex".into(), "exec".into()]);
            if run.format != OutputFormat::Text {
                argv.push("--json".into());
            }
            if let Some(model) = run.model {
                argv.extend(["-m".into(), model.into()]);
            }
            if let Some(effort) = run.effort {
                argv.extend(["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]);
            }
            flags(&mut argv, "codex");
            argv.extend(run.extra.iter().cloned());
            argv.extend(["--".into(), slot]);
        }
        Some(Cli::Agy) => {
            argv.extend(["agy".into(), "--print".into(), slot]);
            argv.extend(["--output-format".into(), run.format.flag().into()]);
            if let Some(model) = run.model {
                argv.extend(["--model".into(), model.into()]);
            }
            if let Some(effort) = run.effort {
                argv.extend(["--effort".into(), effort.into()]);
            }
            flags(&mut argv, "agy");
            argv.extend(["--print-timeout".into(), run.print_timeout.into()]);
            argv.extend(run.extra.iter().cloned());
        }
        Some(Cli::Pi) => {
            argv.extend(["pi".into(), "-p".into()]);
            if run.format != OutputFormat::Text {
                argv.extend(["--mode".into(), "json".into()]);
            }
            if let Some(model) = run.model {
                argv.extend(["--model".into(), model.into()]);
            }
            if let Some(effort) = run.effort {
                argv.extend(["--thinking".into(), effort.into()]);
            }
            flags(&mut argv, "pi");
            argv.extend(run.extra.iter().cloned());
            argv.extend(["--".into(), slot]);
        }
        Some(Cli::OpenCode) | None => return None,
    }
    Some(argv)
}

/// The secret service an agent signs in with although its kit marks the credential optional,
/// for an sbx agent name. The create form checks it ([`Backend::required_services`]).
pub fn agent_credential(agent: &str) -> Option<&'static str> {
    match cli(agent)? {
        Cli::Codex => Some("openai"),
        _ => None,
    }
}

/// The command that stores a missing service secret, as the create form and `aisbx up` print it.
pub fn secret_set_hint(service: &str) -> String {
    match service {
        "openai" => "sbx secret set openai --oauth   (or without --oauth, for an API key)".into(),
        other => format!("sbx secret set {other}"),
    }
}

impl Addon {
    /// Whether the addon works with `harness` ([`Backend::harness`]), as its [`HARNESSES_FILE`]
    /// says.
    pub fn applies_to(&self, harness: &str) -> bool {
        self.kit.harnesses.is_empty() || self.kit.harnesses.iter().any(|h| h == harness)
    }

    /// Short badge text: `rtk` → RTK, `jdk25` → JDK25, `host-gradle` → Host Gradle.
    pub fn short(&self) -> String {
        short_kit_label(&self.kit.name)
    }

    /// Can `sbx kit add` apply this to a live sandbox, or must it be baked in at creation?
    pub fn create_time_only(&self) -> bool {
        self.kit.kit_add_blocker.is_some() || self.needs_mounts()
    }

    /// Why — what the kit declares that `sbx kit add` does not apply ([`kit_add_blocker`]), or
    /// `extra workspaces` for the one reason that is not in the descriptor at all.
    pub fn create_time_reason(&self) -> Option<&'static str> {
        self.kit
            .kit_add_blocker
            .or_else(|| self.needs_mounts().then_some("extra workspaces"))
    }

    /// Needs extra read-only workspaces, which are fixed at sandbox creation.
    pub fn needs_mounts(&self) -> bool {
        crate::model::HOST_JVM_KITS.contains(&self.kit.name.as_str())
            || crate::model::HOST_FOLDER_KITS.contains(&self.kit.name.as_str())
            || crate::skill_source::needs_mounts(&self.kit.name)
    }
}

pub fn short_kit_label(name: &str) -> String {
    match name {
        "rtk" => "RTK".into(),
        "web" => "Web".into(),
        "host-gradle" => "Host Gradle".into(),
        "host-maven" => "Host Maven".into(),
        "host-npm" => "Host npm".into(),
        "host-pip" => "Host pip".into(),
        "host-skills" => "Host skills".into(),
        "pkg-cache" => "Package cache".into(),
        "remote-dev" => "Remote dev".into(),
        n if n.starts_with("jdk") => format!("JDK{}", &n[3..]),
        n => {
            let mut c = n.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub kits_dir: PathBuf,
    pub backends: Vec<Backend>,
    pub addons: Vec<Addon>,
    /// Addons only a headless caller can use ([`HEADLESS_ONLY`]): `aisbx up --kit` finds them,
    /// the dashboard never offers them.
    pub headless: Vec<Addon>,
    /// The models on offer for pinning: the list a sandbox on the current connection read last
    /// ([`crate::models::catalog_for`]). Empty at load; the app sets it for the form's connection.
    pub models: Vec<crate::models::Model>,
    /// Where [`Catalog::models`] came from — the pin list's title says it.
    pub models_source: crate::models::Source,
}

impl Catalog {
    pub fn load() -> Result<Self> {
        let kits_dir = find_repo_root()?.join("kits");
        let mut backends = Vec::new();
        let mut addons = Vec::new();
        let mut headless = Vec::new();

        let mut entries: Vec<PathBuf> = fs::read_dir(&kits_dir)
            .with_context(|| format!("reading {}", kits_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| descriptor_path(p).is_file())
            .collect();
        entries.sort();

        for dir in entries {
            let kit = Kit::parse(&dir)?;
            let agent = kit.is_workload.then(|| kit.name.clone());
            match agent {
                Some(agent) => backends.push(Backend { kit, agent }),
                None if HEADLESS_ONLY.contains(&kit.name.as_str()) => headless.push(Addon { kit }),
                None => addons.push(Addon { kit }),
            }
        }

        if backends.is_empty() {
            return Err(anyhow!(
                "no backend kits found under {} — expected e.g. kits/claude-anthropic/",
                kits_dir.display()
            ));
        }
        backends.sort_by_key(|b| (b.rank(), b.agent.clone(), b.kit.name.clone()));
        Ok(Self {
            models: Vec::new(),
            models_source: crate::models::Source::NotYet,
            kits_dir,
            backends,
            addons,
            headless,
        })
    }

    pub fn backend(&self, name: &str) -> Option<&Backend> {
        self.backends.iter().find(|b| b.kit.name == name)
    }

    pub fn addon(&self, name: &str) -> Option<&Addon> {
        self.all_addons().find(|a| a.kit.name == name)
    }

    /// Every addon, the headless-only ones included - for building and reading back a sandbox.
    pub fn all_addons(&self) -> impl Iterator<Item = &Addon> {
        self.addons.iter().chain(self.headless.iter())
    }

    /// The backend that authenticates with this Anthropic account, if the repo ships one.
    pub fn for_account(&self, account: Account) -> Option<&Backend> {
        self.backends.iter().find(|b| b.account() == Some(account))
    }

    /// Indices of the backends the create form offers as their own choice. The setup-token
    /// backend is not among them: the form reaches it through the account radio.
    pub fn offered_backends(&self) -> Vec<usize> {
        (0..self.backends.len())
            .filter(|i| self.backends[*i].account() != Some(Account::Token))
            .collect()
    }

    /// Content hash per kit dir, used to notice that a kit changed after a sandbox baked it in.
    pub fn hashes(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for k in self
            .backends
            .iter()
            .map(|b| &b.kit)
            .chain(self.all_addons().map(|a| &a.kit))
        {
            if let Ok(h) = hash_dir(&k.dir) {
                out.insert(k.name.clone(), h);
            }
        }
        out
    }
}

/// Addons that need an explicit `aisbx up --mount` and do nothing useful, or harm, in a sandbox
/// the dashboard creates.
pub const HEADLESS_ONLY: [&str; 1] = ["pkg-cache"];

/// The addon that links the host's skills into the sandbox before every session aisbx starts
/// ([`crate::skill_source::SkillSource::HostFolder`]).
pub const HOST_SKILLS: &str = "host-skills";

impl Kit {
    /// Read one kit directory. Also the check that a *generated* kit — the wiring for a named
    /// setup token, see [`crate::tokens`] — parses the same way as one committed to this repo.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read(dir: &Path) -> Result<Self> {
        Self::parse(dir)
    }

    /// The description's first sentence, its period kept: what a form has room for.
    pub fn first_sentence(&self) -> &str {
        match self.description.find(". ") {
            Some(at) => &self.description[..=at],
            None => &self.description,
        }
    }

    fn parse(dir: &Path) -> Result<Self> {
        let path = descriptor_path(dir);
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let doc = load_descriptor(&text).with_context(|| format!("parsing {}", path.display()))?;
        let caps = capabilities(&doc);
        let name: String = dir.file_name().unwrap_or_default().to_string_lossy().into();
        let display_name = doc["displayName"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| name.clone());
        let description = doc["description"]
            .as_str()
            .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let mut credentials: Vec<String> = Vec::new();
        for cap in caps
            .iter()
            .filter(|c| c.kind == "credential@1" && !c.optional)
        {
            if let Some(service) = cap.config["service"].as_str() {
                if !credentials.iter().any(|s| s == service) {
                    credentials.push(service.to_string());
                }
            }
        }
        Ok(Self {
            name,
            dir: dir.to_path_buf(),
            display_name,
            description,
            kit_add_blocker: kit_add_blocker(&caps),
            credentials,
            is_workload: doc["kind"].as_str() == Some("workload"),
            changes: read_changes(dir),
            harnesses: read_lines(dir, HARNESSES_FILE),
            volumes: caps.iter().any(|c| c.kind == "volume@1"),
            skills: caps.iter().any(|c| c.kind == "agent-skills@1"),
        })
    }
}

/// A kit folder's descriptor: `<folder>/<folder>.yaml`, the companion-pair layout sbx's kit builder
/// reads (with `<folder>.dockerfile` beside it, when the kit has content).
pub fn descriptor_path(dir: &Path) -> PathBuf {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    dir.join(format!("{name}.yaml"))
}

/// Parse a descriptor's text: the one YAML document it is.
pub fn load_descriptor(text: &str) -> Result<Yaml> {
    let mut docs = YamlLoader::load_from_str(text).context("not YAML")?;
    match docs.is_empty() {
        true => Err(anyhow!("an empty descriptor")),
        false => Ok(docs.swap_remove(0)),
    }
}

/// One request of a descriptor's `capabilities:` list: its type without sbx's
/// `com.docker.sandbox/` namespace (`credential@1`), whether the kit runs without it, and its
/// config.
#[derive(Debug, Clone)]
pub struct Capability {
    pub kind: String,
    pub optional: bool,
    pub config: Yaml,
}

/// The requests of a descriptor, a capability group's members in its place: a group's `optional`
/// is its members' (the v3 spec forbids one on a member).
pub fn capabilities(doc: &Yaml) -> Vec<Capability> {
    let one = |item: &Yaml, optional: bool| {
        let ty = item["type"].as_str().unwrap_or_default();
        Capability {
            kind: ty
                .strip_prefix("com.docker.sandbox/")
                .unwrap_or(ty)
                .to_string(),
            optional,
            config: item["config"].clone(),
        }
    };
    let mut out = Vec::new();
    for item in doc["capabilities"].as_vec().into_iter().flatten() {
        let group = &item["group"];
        if group.is_badvalue() {
            out.push(one(item, item["optional"].as_bool().unwrap_or(false)));
        } else {
            let optional = group["optional"].as_bool().unwrap_or(false);
            for member in group["capabilities"].as_vec().into_iter().flatten() {
                out.push(one(member, optional));
            }
        }
    }
    out
}

/// Why `sbx kit add` would not apply this kit to a live v3 sandbox, or `None` when it can be
/// added.
fn kit_add_blocker(caps: &[Capability]) -> Option<&'static str> {
    let has = |kind: &str| caps.iter().any(|c| c.kind == kind);
    let listed = |c: &Capability| {
        ["install", "runtime"].iter().any(|phase| {
            ["allow", "deny"].iter().any(|list| {
                c.config[*phase][*list]
                    .as_vec()
                    .is_some_and(|v| !v.is_empty())
            })
        })
    };
    if caps.iter().any(|c| {
        c.kind == "lifecycle@1" && c.config["files"].as_vec().is_some_and(|v| !v.is_empty())
    }) {
        return Some("lifecycle files");
    }
    if has("credential@1") {
        return Some("credentials");
    }
    if has("volume@1") {
        return Some("volumes");
    }
    if has("port@1") || has("resources@1") || has("long-running@1") {
        return Some("container settings");
    }
    if caps
        .iter()
        .any(|c| c.kind.starts_with("network-policy@") && listed(c))
    {
        return Some("network rules");
    }
    None
}

/// Stable content hash of a directory tree: sorted `relpath\0sha256(content)` pairs, hashed,
/// where the content of a descriptor, a Dockerfile or a shell script is [`without_comments`]. A
/// Finder `.DS_Store`, the [`CHANGES_FILE`] and the [`HARNESSES_FILE`] are left out, so none
/// marks the kit's sandboxes as drifted.
fn hash_dir(dir: &Path) -> Result<String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for (rel, path) in files {
        if rel == CHANGES_FILE || rel == HARNESSES_FILE {
            continue;
        }
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(Sha256::digest(without_comments(&rel, fs::read(&path)?)));
    }
    Ok(hex(&h.finalize()[..8]))
}

/// A kit file as far as it changes what a sandbox gets: a YAML descriptor as its parsed value, a
/// Dockerfile or a shell script without its comment lines and blank lines. Directives stay: a
/// shebang, and a Dockerfile's or a descriptor's leading `# syntax=`. Any other file, and a file
/// that is not UTF-8 or does not parse, is its bytes.
fn without_comments(rel: &str, bytes: Vec<u8>) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return bytes;
    };
    let directives = |text: &str| -> Vec<String> {
        text.lines()
            .take_while(|l| {
                l.strip_prefix('#')
                    .and_then(|rest| rest.trim_start().split_once('='))
                    .is_some_and(|(key, _)| !key.is_empty() && !key.contains(char::is_whitespace))
            })
            .map(str::to_string)
            .collect()
    };
    let code_lines = |text: &str, keep_first: bool| -> Vec<u8> {
        let mut out = String::new();
        for (i, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if (i == 0 && keep_first) || !(code.is_empty() || code.starts_with('#')) {
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out.into_bytes()
    };
    if rel.ends_with(".yaml") || rel.ends_with(".yml") {
        let Ok(docs) = YamlLoader::load_from_str(text) else {
            return bytes;
        };
        let mut out = directives(text).join("\n");
        for doc in &docs {
            let mut emitted = String::new();
            let mut emitter = yaml_rust2::YamlEmitter::new(&mut emitted);
            emitter.multiline_strings(true);
            if emitter.dump(doc).is_err() {
                return bytes;
            }
            out.push('\n');
            out.push_str(&emitted);
        }
        out.into_bytes()
    } else if rel.ends_with(".dockerfile") || rel.ends_with("Dockerfile") {
        let mut out = directives(text).join("\n").into_bytes();
        out.extend(code_lines(text, false));
        out
    } else if rel.ends_with(".sh") {
        code_lines(text, text.starts_with("#!"))
    } else {
        bytes
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else if path.is_file() && path.file_name().is_none_or(|n| n != ".DS_Store") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Locate the aisbx checkout holding `kits/`: `$AISBX_HOME`, then the running binary's
/// location, then the checkout it was compiled from, then the current directory and its parents.
pub fn find_repo_root() -> Result<PathBuf> {
    let mut tried: Vec<String> = Vec::new();
    let check = |p: PathBuf, tried: &mut Vec<String>| -> Option<PathBuf> {
        if p.join("kits").is_dir() {
            return Some(p);
        }
        tried.push(p.display().to_string());
        None
    };

    if let Some(env) = std::env::var_os("AISBX_HOME") {
        let p = PathBuf::from(env);
        if let Some(found) = check(p, &mut tried) {
            return Ok(found);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for p in exe_candidates(&exe) {
            if let Some(found) = check(p, &mut tried) {
                return Ok(found);
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(parent) = manifest.parent() {
        if let Some(found) = check(parent.to_path_buf(), &mut tried) {
            return Ok(found);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd.as_path());
        while let Some(dir) = cur {
            if dir.join("kits").is_dir() && dir.join("tui").is_dir() {
                return Ok(dir.to_path_buf());
            }
            cur = dir.parent();
        }
    }
    Err(anyhow!(
        "could not find the aisbx checkout (no kits/ directory).\n\
         Set AISBX_HOME to the repo root. Looked in:\n  {}",
        tried.join("\n  ")
    ))
}

/// Where a binary at `exe` may sit below the checkout: the folder three, two and one levels above
/// the binary's own.
fn exe_candidates(exe: &Path) -> Vec<PathBuf> {
    [3usize, 2, 1]
        .into_iter()
        .filter_map(|up| exe.ancestors().nth(up + 1).map(Path::to_path_buf))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kit folder in a scratch directory, holding one descriptor.
    fn scratch_kit(name: &str, descriptor: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("aisbx-kit-{}-{name}", std::process::id()))
            .join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(descriptor_path(&dir), descriptor).unwrap();
        dir
    }

    /// The kit's parsed descriptor, read off the checkout.
    fn shipped(name: &str) -> Yaml {
        let root = find_repo_root().expect("kits/ must be discoverable from the crate");
        let dir = root.join("kits").join(name);
        load_descriptor(&fs::read_to_string(descriptor_path(&dir)).unwrap()).unwrap()
    }

    /// The kit's Dockerfile (its content recipe), read off the checkout.
    fn dockerfile(name: &str) -> String {
        let root = find_repo_root().expect("kits/ must be discoverable from the crate");
        fs::read_to_string(
            root.join("kits")
                .join(name)
                .join(format!("{name}.dockerfile")),
        )
        .unwrap()
    }

    /// The first request of a type in a descriptor.
    fn cap(doc: &Yaml, kind: &str) -> Capability {
        capabilities(doc)
            .into_iter()
            .find(|c| c.kind == kind)
            .unwrap_or_else(|| panic!("no {kind}"))
    }

    /// A YAML list of strings, as strings.
    fn strings(v: &Yaml) -> Vec<String> {
        v.as_vec()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn classifies_backends_and_addons() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        for b in &catalog.backends {
            assert!(b.kit.is_workload, "{}", b.kit.name);
            assert_eq!(b.agent, b.kit.name);
        }
        assert!(catalog.all_addons().all(|a| !a.kit.is_workload));
        assert_eq!(catalog.backends.len(), 7);
    }

    /// Groups flatten into their members, a group's `optional` passing down, and the namespace
    /// goes.
    #[test]
    fn reads_a_descriptors_capabilities() {
        let doc = load_descriptor(
            "schemaVersion: \"3\"\nkind: mixin\ncapabilities:\n\
             \x20 - type: com.docker.sandbox/lifecycle@1\n    config:\n      startup: []\n\
             \x20 - group:\n      optional: true\n      capabilities:\n\
             \x20       - type: com.docker.sandbox/volume@1\n          config: {path: /v}\n\
             \x20       - type: example.com/thing@2\n",
        )
        .unwrap();
        let caps = capabilities(&doc);
        let kinds: Vec<(&str, bool)> = caps.iter().map(|c| (c.kind.as_str(), c.optional)).collect();
        assert_eq!(
            kinds,
            [
                ("lifecycle@1", false),
                ("volume@1", true),
                ("example.com/thing@2", true)
            ]
        );
        assert_eq!(caps[1].config["path"].as_str(), Some("/v"));
        assert!(load_descriptor("").is_err());
    }

    #[test]
    fn reads_the_credential_services_a_kit_declares() {
        let dir = scratch_kit(
            "creds",
            "schemaVersion: \"3\"\nkind: mixin\ndisplayName: Creds\ncapabilities:\n\
             \x20 - type: com.docker.sandbox/credential@1\n    config: {service: needed, phase: runtime}\n\
             \x20 - type: com.docker.sandbox/credential@1\n    optional: true\n    config: {service: optional, phase: runtime}\n\
             \x20 - type: com.docker.sandbox/credential@1\n    config: {service: needed, phase: install}\n",
        );
        let kit = Kit::read(&dir).unwrap();
        // Only an explicit `optional: true` opts a service out; one named twice counts once.
        assert_eq!(kit.credentials, ["needed"]);
        assert_eq!(kit.display_name, "Creds");
        assert_eq!(kit.name, "creds", "the folder's name");
        assert!(!kit.is_workload);
        assert_eq!(kit.description, "", "none declared");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    /// A description written over several lines — a literal block keeps its line breaks — comes
    /// back on one line, and the forms take its first sentence.
    #[test]
    fn reads_a_kits_description_onto_one_line() {
        let dir = scratch_kit(
            "folded",
            "schemaVersion: \"3\"\nkind: mixin\ndescription: |\n  Mounts the cache.\n  \
             Relevant before\n  a build.  Nothing else.\n",
        );
        let kit = Kit::read(&dir).unwrap();
        assert_eq!(
            kit.description,
            "Mounts the cache. Relevant before a build. Nothing else."
        );
        assert_eq!(kit.first_sentence(), "Mounts the cache.");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
        // Every kit this checkout offers says what it is.
        let catalog = Catalog::load().unwrap();
        for addon in &catalog.addons {
            assert!(!addon.kit.description.is_empty(), "{}", addon.kit.name);
        }
    }

    /// Runs the OpenCode update command with real `sh`, `npm` and `opencode` stubbed, under each
    /// env a sandbox can carry, and returns the one call it made.
    #[cfg(unix)]
    fn opencode_update_call(env: &[(&str, &str)]) -> String {
        let dir = std::env::temp_dir().join(format!(
            "aisbx-opencode-update-{}-{}",
            std::process::id(),
            env.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for bin in ["npm", "opencode"] {
            let path = dir.join(bin);
            std::fs::write(&path, format!("#!/bin/sh\necho {bin} \"$@\"\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let argv = agent_update("opencode").unwrap();
        let out = std::process::Command::new(argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .envs(env.iter().copied())
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// The pre-start update is one `sh -c`, so it decides from the sandbox's own env: the kit's
    /// range, `opencode upgrade` otherwise.
    #[cfg(unix)]
    #[test]
    fn opencode_update_follows_the_kit_the_sandbox_was_built_with() {
        let argv = agent_update("opencode").unwrap();
        assert_eq!(argv[..2], ["sh", "-c"]);
        assert_eq!(agent_update("opencode-litellm"), agent_update("opencode"));
        assert_eq!(argv.len(), 3, "one command, so it stays one step");
        assert_eq!(
            opencode_update_call(&[
                ("OPENCODE_LITELLM_VERSION", "2"),
                ("SBX_KIT_OPENCODE_LITELLM", "1")
            ]),
            "npm install -g --force @opencode/cli@2"
        );
        assert_eq!(opencode_update_call(&[]), "opencode upgrade");
    }

    #[test]
    fn codex_and_agy_launch_like_sbx_run_would() {
        for agent in ["codex", "codex-openai"] {
            assert_eq!(agent_command(agent), Some("codex"), "{agent}");
            assert_eq!(
                agent_update(agent),
                Some(&["codex", "update"][..]),
                "{agent}"
            );
            // Codex's account is a host secret its kit marks optional, so the form checks it.
            assert_eq!(agent_credential(agent), Some("openai"), "{agent}");
        }
        assert_eq!(
            agent_launch_args("codex"),
            ["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(agent_command("agy"), Some("agy"));
        assert_eq!(agent_launch_args("agy"), ["--dangerously-skip-permissions"]);
        assert_eq!(agent_update("agy"), Some(&["agy", "update"][..]));
        for agent in [
            "claude",
            "claude-litellm",
            "claude-anthropic",
            "claude-token",
        ] {
            assert_eq!(agent_command(agent), Some("claude"), "{agent}");
            assert_eq!(
                agent_update(agent),
                Some(&["claude", "update"][..]),
                "{agent}"
            );
            assert_eq!(agent_credential(agent), None, "{agent}");
        }
        assert_eq!(
            agent_command("gemini"),
            None,
            "no guess for an agent with no kit here"
        );
        assert!(secret_set_hint("openai").starts_with("sbx secret set openai --oauth"));
        assert_eq!(secret_set_hint("litellm"), "sbx secret set litellm");
    }

    /// Pi's workload is its own agent, so every launch fact is keyed on the kit's own name —
    /// except the flags, which are keyed on the binary [`agent_command`] answers.
    #[test]
    fn pi_launches_from_its_own_prefix_with_no_flags() {
        assert_eq!(agent_command("pi-litellm"), Some("pi"));
        assert!(
            agent_launch_args("pi").is_empty(),
            "Pi has no approval prompt"
        );
        assert_eq!(
            agent_update("pi-litellm"),
            Some(
                &[
                    "npm",
                    "install",
                    "-g",
                    "--prefix",
                    "/home/agent/.local",
                    "@earendil-works/pi-coding-agent"
                ][..]
            )
        );
        assert_eq!(agent_credential("pi-litellm"), None);
        // No other agent picks the facts up by accident.
        assert_eq!(agent_command("pi"), None);
        assert_eq!(agent_update("pi"), None);

        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        let pi = catalog
            .backend("pi-litellm")
            .expect("kits/pi-litellm is a backend");
        assert!(pi.kit.is_workload);
        assert_eq!(pi.agent, "pi-litellm", "a workload is its own agent");
        assert_eq!(pi.base_label(), "Pi");
        assert_eq!(pi.type_label(), "Pi · LiteLLM");
        assert_eq!(
            pi.setup_hint(),
            "a LiteLLM gateway of your choice (a connection); no Anthropic account"
        );
        assert!(!pi.model_overrides());
        assert!(!pi.shares_skills());
        assert_eq!(pi.account(), None);
        assert_eq!(pi.litellm_agent(), Some(crate::litellm::Agent::Pi));
        assert_eq!(
            pi.required_services(),
            Vec::<String>::new(),
            "its connection's key is a custom secret, not a service"
        );
        // Pi itself is installed at build time, from the image's own Node.
        let recipe = dockerfile("pi-litellm");
        assert!(
            recipe.contains("RUN sh /usr/local/lib/aisbx/pi/update.sh build"),
            "{recipe}"
        );
        assert!(
            recipe.contains("ENTRYPOINT [\"/home/agent/.local/bin/pi\"]"),
            "{recipe}"
        );
    }

    #[test]
    fn no_backend_kit_declares_a_secret_it_needs() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        for backend in &catalog.backends {
            assert!(
                backend.kit.credentials.is_empty(),
                "{} credentials: {:?}",
                backend.kit.name,
                backend.kit.credentials
            );
        }
        assert!(
            catalog.backend("claude-anthropic").is_some(),
            "the subscription backend must exist"
        );
        // Codex's `openai` is optional in its kit, yet the form must still check it; agy's
        // `google` key and sign-in are optional and must not be asked for.
        let required = |name: &str| catalog.backend(name).map(Backend::required_services);
        assert_eq!(required("codex-openai"), Some(vec!["openai".to_string()]));
        assert_eq!(required("agy"), Some(Vec::new()));
        assert_eq!(required("claude-anthropic"), Some(Vec::new()));
        assert_eq!(required("claude-litellm"), Some(Vec::new()));
        assert_eq!(required("opencode-litellm"), Some(Vec::new()));
        assert_eq!(required("pi-litellm"), Some(Vec::new()));
    }

    /// A kit's harness list names only harnesses a shipped backend runs, so a typo cannot hide
    /// the kit from every sandbox.
    #[test]
    fn every_harness_list_names_a_shipped_harness() {
        let catalog = Catalog::load().unwrap();
        let harnesses: Vec<&str> = catalog.backends.iter().map(Backend::harness).collect();
        for addon in &catalog.addons {
            for h in &addon.kit.harnesses {
                assert!(harnesses.contains(&h.as_str()), "{}: {h}", addon.kit.name);
            }
        }
        let statusline = catalog.addon("statusline").unwrap();
        assert_eq!(statusline.kit.harnesses, ["claude"]);
    }

    /// Every shipped backend has a provider, and within one provider each harness is one offered
    /// backend: the wizard's two first steps then name exactly one kit.
    #[test]
    fn every_backend_has_a_provider_and_one_backend_per_harness() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        let provider = |name: &str| catalog.backend(name).map(Backend::provider);
        assert_eq!(provider("claude-anthropic"), Some(Provider::Anthropic));
        assert_eq!(provider("claude-token"), Some(Provider::Anthropic));
        assert_eq!(provider("agy"), Some(Provider::Google));
        assert_eq!(provider("codex-openai"), Some(Provider::OpenAi));
        assert_eq!(provider("claude-litellm"), Some(Provider::Litellm));
        assert_eq!(provider("opencode-litellm"), Some(Provider::Litellm));
        assert_eq!(provider("pi-litellm"), Some(Provider::Litellm));
        let offered = catalog.offered_backends();
        for i in &offered {
            let b = &catalog.backends[*i];
            assert_ne!(b.provider(), Provider::Other, "{}", b.kit.name);
            let twins = offered
                .iter()
                .map(|j| &catalog.backends[*j])
                .filter(|o| o.provider() == b.provider() && o.harness() == b.harness())
                .count();
            assert_eq!(twins, 1, "{} shares its provider and harness", b.kit.name);
        }
    }

    /// The two Anthropic accounts are two backends, and the form must offer exactly one of them.
    #[test]
    fn each_anthropic_account_has_exactly_one_backend() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        for account in [Account::Login, Account::Token] {
            let matching: Vec<&str> = catalog
                .backends
                .iter()
                .filter(|b| b.account() == Some(account))
                .map(|b| b.kit.name.as_str())
                .collect();
            assert_eq!(matching.len(), 1, "{account:?} → {matching:?}");
        }
        let token = catalog.for_account(Account::Token).unwrap();
        assert!(token.kit.is_workload, "{}", token.kit.name);
        assert_eq!(token.agent, token.kit.name);
        let login = catalog.for_account(Account::Login).unwrap();
        assert!(login.kit.is_workload);
        assert_eq!(login.agent, "claude-anthropic");
        let anthropic = cap(&shipped("claude-anthropic"), "credential@1");
        assert_eq!(anthropic.config["service"].as_str(), Some("anthropic"));
        assert!(anthropic.optional, "a sandbox signs in with /login inside");
        assert_eq!(
            anthropic.config["oauth"]["tokenEndpoint"]["host"].as_str(),
            Some("platform.claude.com")
        );
        assert!(
            capabilities(&shipped("claude-token"))
                .iter()
                .all(|c| c.kind != "credential@1"),
            "claude-token declares no credential"
        );
        assert!(
            capabilities(&shipped("claude-litellm"))
                .iter()
                .all(|c| c.kind != "credential@1"),
            "claude-litellm declares no credential"
        );

        // …and it is reached through the account radio, not as a fourth backend row.
        let offered: Vec<&str> = catalog
            .offered_backends()
            .into_iter()
            .map(|i| catalog.backends[i].kit.name.as_str())
            .collect();
        assert!(offered.contains(&"claude-anthropic"), "{offered:?}");
        assert!(!offered.contains(&"claude-token"), "{offered:?}");
    }

    #[test]
    fn the_gateway_backends_lead_the_create_form() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        let offered: Vec<&str> = catalog
            .offered_backends()
            .into_iter()
            .map(|i| catalog.backends[i].kit.name.as_str())
            .collect();
        assert_eq!(
            offered,
            [
                "claude-litellm",
                "opencode-litellm",
                "claude-anthropic",
                "agy",
                "codex-openai",
                "pi-litellm"
            ],
            "the first entry is the default for a new sandbox"
        );
    }

    /// Which backends sbx mounts the shared skills store into: the workloads that declare
    /// `agent-skills@1` — on a path of their own, which their hook links into the agent's folder.
    #[test]
    fn the_skill_workloads_share_the_skills_store() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        let shares = |name: &str| catalog.backend(name).map(Backend::shares_skills);
        for name in [
            "claude-litellm",
            "claude-anthropic",
            "claude-token",
            "opencode-litellm",
            "codex-openai",
        ] {
            assert_eq!(shares(name), Some(true), "{name}");
            let skills = cap(&shipped(name), "agent-skills@1");
            assert_eq!(
                skills.config["path"].as_str(),
                Some("/home/agent/.local/share/aisbx/skills-store"),
                "{name}: the store's own path, linked by files/skills/skills-store.sh"
            );
            assert!(skills.optional, "{name}");
        }
        assert_eq!(shares("agy"), Some(false));
        assert_eq!(shares("pi-litellm"), Some(false));
    }

    /// The telemetry switches the Claude Code workloads' image sets: every `DISABLE_*`,
    /// `CLAUDE_CODE_DISABLE_*` and `DO_NOT_TRACK` its Dockerfile assigns outside a comment.
    fn telemetry_switches() -> Vec<String> {
        let mut names: Vec<String> = dockerfile("claude-litellm")
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .flat_map(str::split_whitespace)
            .filter_map(|word| word.split_once('=').map(|(name, _)| name.to_string()))
            .filter(|name| {
                name.starts_with("DISABLE_")
                    || name.starts_with("CLAUDE_CODE_DISABLE_")
                    || name == "DO_NOT_TRACK"
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Whether a shell script runs the `claude` CLI: `claude <subcommand>` outside quotes and
    /// comments (`command -v claude` and a message naming it do not count).
    fn runs_claude(script: &str) -> bool {
        script.lines().any(|line| {
            let (mut code, mut quote) = (String::new(), None);
            for c in line.chars() {
                match (quote, c) {
                    (None, '#') => break,
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, c) => code.push(c),
                    _ => {}
                }
            }
            let words: Vec<&str> = code
                .split(|c: char| c.is_whitespace() || ";&|()".contains(c))
                .filter(|w| !w.is_empty())
                .collect();
            words
                .windows(2)
                .any(|w| w[0] == "claude" && w[1].starts_with(|c: char| c.is_ascii_lowercase()))
        })
    }

    #[test]
    fn every_hook_that_runs_claude_declares_the_telemetry_switches() {
        let switches = telemetry_switches();
        assert_eq!(switches.len(), 9, "{switches:?}");
        assert!(runs_claude(
            "x && claude update || echo \"claude update failed\""
        ));
        assert!(!runs_claude("command -v claude >/dev/null # claude update"));
        let root = find_repo_root().expect("kits/ must be discoverable from the crate");
        let mut found = Vec::new();
        for entry in fs::read_dir(root.join("kits")).unwrap().flatten() {
            let dir = entry.path();
            let Ok(text) = fs::read_to_string(descriptor_path(&dir)) else {
                continue;
            };
            let kit = dir.file_name().unwrap().to_string_lossy().to_string();
            let lifecycles = capabilities(&load_descriptor(&text).unwrap())
                .into_iter()
                .filter(|c| c.kind == "lifecycle@1");
            for lifecycle in lifecycles {
                for hook in lifecycle.config["install"].as_vec().into_iter().flatten() {
                    let (what, body) = match hook["command"].as_str() {
                        Some(inline) => ("an inline hook".to_string(), inline.to_string()),
                        None => {
                            let command = strings(&hook["command"]);
                            let Some(script) = command
                                .iter()
                                .find_map(|a| a.strip_prefix("/usr/local/lib/aisbx/"))
                            else {
                                continue;
                            };
                            let path = dir.join("files").join(script);
                            (script.to_string(), fs::read_to_string(path).unwrap())
                        }
                    };
                    if !runs_claude(&body) {
                        continue;
                    }
                    let env = strings(&hook["env"]);
                    let missing: Vec<&String> =
                        switches.iter().filter(|s| !env.contains(s)).collect();
                    assert!(missing.is_empty(), "{kit}: {what} lacks {missing:?}");
                    found.push(format!("{kit}: {what}"));
                }
            }
        }
        found.sort();
        assert_eq!(
            found,
            [
                "claude-anthropic: claude/seed.sh",
                "claude-anthropic: claude/update.sh",
                "claude-litellm: claude/seed.sh",
                "claude-litellm: claude/update.sh",
                "claude-token: claude/seed.sh",
                "claude-token: claude/update.sh",
                "ponytail: ponytail/activate.sh",
            ]
        );
    }

    /// The other tools an install hook runs get their kit's off switches the same way: each name is
    /// one the kit's image sets, and the hook that runs the tool declares it.
    #[test]
    fn install_hooks_that_run_a_tool_declare_its_off_switches() {
        for (kit, runs, switches) in [
            ("rtk", "rtk/activate.sh", &["RTK_TELEMETRY_DISABLED"][..]),
            (
                "headroom",
                "headroom --version",
                &[
                    "HEADROOM_BEACON",
                    "HEADROOM_TELEMETRY",
                    "HEADROOM_UPDATE_CHECK",
                    "DO_NOT_TRACK",
                ][..],
            ),
        ] {
            let recipe = dockerfile(kit);
            let lifecycle = cap(&shipped(kit), "lifecycle@1");
            let hook = lifecycle.config["install"]
                .as_vec()
                .into_iter()
                .flatten()
                .find(|h| {
                    h["command"].as_str().unwrap_or_default().contains(runs)
                        || strings(&h["command"]).iter().any(|a| a.contains(runs))
                })
                .unwrap_or_else(|| panic!("{kit}: a hook running {runs}"));
            let env = strings(&hook["env"]);
            for switch in switches {
                assert!(
                    recipe.lines().any(|l| !l.trim_start().starts_with('#')
                        && l.split_whitespace()
                            .any(|w| w.starts_with(&format!("{switch}=")))),
                    "{kit}: its image sets {switch}"
                );
                assert!(
                    env.iter().any(|v| v == switch),
                    "{kit}: {env:?} lacks {switch}"
                );
            }
        }
    }

    #[test]
    fn the_npm_updates_at_creation_keep_the_templates_prefix() {
        for name in ["codex-openai", "opencode-litellm"] {
            let lifecycle = cap(&shipped(name), "lifecycle@1");
            let hook = lifecycle.config["install"]
                .as_vec()
                .into_iter()
                .flatten()
                .find(|h| {
                    strings(&h["command"])
                        .iter()
                        .any(|a| a.ends_with("/update.sh"))
                })
                .unwrap_or_else(|| panic!("{name}: an update hook"));
            assert!(
                strings(&hook["env"])
                    .iter()
                    .any(|v| v == "NPM_CONFIG_PREFIX"),
                "{name}: {:?}",
                hook["env"]
            );
        }
    }

    #[test]
    fn detects_every_kit_add_blocker() {
        let caps = |yaml: &str| {
            capabilities(&load_descriptor(&format!("kind: mixin\ncapabilities:\n{yaml}")).unwrap())
        };
        // Image ENV and files (no capability at all), hooks: applied.
        assert_eq!(kit_add_blocker(&[]), None);
        assert_eq!(
            kit_add_blocker(&caps(
                "  - type: com.docker.sandbox/lifecycle@1\n    config:\n      install: [{command: x}]\n      startup: [{command: y}]\n"
            )),
            None
        );
        // Generated files: refused ("declares commands.initFiles").
        assert_eq!(
            kit_add_blocker(&caps(
                "  - type: com.docker.sandbox/lifecycle@1\n    config:\n      files: [{path: /x, content: y}]\n"
            )),
            Some("lifecycle files")
        );
        // A network grant: a confirmation only a terminal can give — in either phase.
        for phase in ["install", "runtime"] {
            assert_eq!(
                kit_add_blocker(&caps(&format!(
                    "  - type: com.docker.sandbox/network-policy@1\n    config:\n      {phase}:\n        allow: [x.io]\n"
                ))),
                Some("network rules"),
                "{phase}"
            );
        }
        // …an empty one is no grant.
        assert_eq!(
            kit_add_blocker(&caps(
                "  - type: com.docker.sandbox/network-policy@1\n    config:\n      runtime:\n        allow: []\n"
            )),
            None
        );
        assert_eq!(
            kit_add_blocker(&caps(
                "  - type: com.docker.sandbox/volume@1\n    config: {path: /v}\n"
            )),
            Some("volumes")
        );
        assert_eq!(
            kit_add_blocker(&caps(
                "  - type: com.docker.sandbox/credential@1\n    optional: true\n    config: {service: s, phase: runtime}\n"
            )),
            Some("credentials")
        );
        assert_eq!(
            kit_add_blocker(&caps("  - type: com.docker.sandbox/long-running@1\n")),
            Some("container settings")
        );
        // A grant inside a group counts as one outside.
        assert_eq!(
            kit_add_blocker(&caps(
                "  - group:\n      optional: true\n      capabilities:\n        - type: com.docker.sandbox/volume@1\n          config: {path: /v}\n"
            )),
            Some("volumes")
        );
    }

    #[test]
    fn the_shipped_kits_report_why_they_are_create_time_only() {
        let Ok(catalog) = Catalog::load() else { return };
        let mut create_time: Vec<(&str, &str)> = catalog
            .addons
            .iter()
            .filter(|a| a.create_time_only())
            .map(|a| (a.kit.name.as_str(), a.create_time_reason().unwrap_or("?")))
            .collect();
        create_time.sort_unstable();
        assert_eq!(
            create_time,
            vec![
                ("headroom", "network rules"),
                ("host-gradle", "extra workspaces"),
                ("host-maven", "extra workspaces"),
                ("host-npm", "volumes"),
                ("host-pip", "volumes"),
                ("host-skills", "extra workspaces"),
                ("ponytail", "network rules"),
                ("remote-dev", "network rules"),
                ("rust", "network rules"),
                ("web", "network rules"),
            ]
        );
        // …and the ones `sbx kit add` applies stay add-able: their content is the image, and they
        // ask for no grant.
        let mut addable: Vec<&str> = catalog
            .addons
            .iter()
            .filter(|a| !a.create_time_only())
            .map(|a| a.kit.name.as_str())
            .collect();
        addable.sort_unstable();
        assert_eq!(addable, vec!["jdk25", "rtk", "statusline"]);
    }

    #[test]
    fn the_headroom_kit_is_a_dashboard_addon_that_stays_silent() {
        let Ok(catalog) = Catalog::load() else { return };
        let addon = catalog
            .addons
            .iter()
            .find(|a| a.kit.name == "headroom")
            .expect("offered on the form and in the kits editor");
        assert!(catalog.headless.iter().all(|a| a.kit.name != "headroom"));
        assert_eq!(HEADLESS_ONLY, ["pkg-cache"]);
        assert_eq!(addon.create_time_reason(), Some("network rules"));
        let recipe = dockerfile("headroom");
        for want in [
            "ENV SBX_KIT_HEADROOM=1",
            "HEADROOM_BEACON=off",
            "HEADROOM_TELEMETRY=off",
            "HEADROOM_UPDATE_CHECK=off",
            "DO_NOT_TRACK=1",
            "ENV HEADROOM_CCR_BACKEND=memory",
        ] {
            assert!(recipe.contains(want), "{want}");
        }
        assert!(
            !recipe
                .lines()
                .any(|l| !l.starts_with('#') && l.contains("ANTHROPIC_BASE_URL")),
            "{recipe}"
        );
        let net = cap(&shipped("headroom"), "network-policy@1");
        assert_eq!(
            strings(&net.config["install"]["allow"]),
            ["pypi.org", "files.pythonhosted.org"]
        );
        assert!(net.config["runtime"].is_badvalue(), "no egress at run time");
        assert!(catalog.hashes().contains_key("headroom"));
    }

    #[test]
    fn no_kit_ships_the_model_list_script() {
        let Ok(catalog) = Catalog::load() else { return };
        // The host fetches the list through its own fetcher sandbox (`crate::model_fetch`).
        for kit in catalog
            .backends
            .iter()
            .map(|b| &b.kit)
            .chain(catalog.all_addons().map(|a| &a.kit))
        {
            assert!(
                !kit.dir.join("files/litellm-models").exists(),
                "{} ships the model-list script",
                kit.name
            );
        }
        // The pin list is the chosen connection's, which the app sets — none at load.
        assert_eq!(catalog.models_source, crate::models::Source::NotYet);
        assert!(catalog.models.is_empty());
    }

    #[test]
    fn agy_declares_a_shared_google_sign_in() {
        let doc = shipped("agy");
        let creds: Vec<Capability> = capabilities(&doc)
            .into_iter()
            .filter(|c| c.kind == "credential@1")
            .collect();
        let by = |service: &str| {
            creds
                .iter()
                .find(|c| c.config["service"].as_str() == Some(service))
                .unwrap_or_else(|| panic!("agy declares {service}"))
        };
        let agy = by("antigravity");
        assert!(agy.optional, "a missing sign-in is no setup error");
        let oauth = &agy.config["oauth"];
        assert_eq!(
            oauth["tokenEndpoint"]["host"].as_str(),
            Some("oauth2.googleapis.com")
        );
        assert_eq!(oauth["tokenEndpoint"]["path"].as_str(), Some("/token"));
        assert_eq!(
            oauth["sentinels"]["accessToken"].as_str(),
            Some("agy-oauth-access-proxy-managed")
        );
        assert_eq!(
            oauth["sentinels"]["refreshToken"].as_str(),
            Some("agy-oauth-refresh-proxy-managed")
        );
        let file = &oauth["credentialFile"];
        assert_eq!(
            file["path"].as_str(),
            Some("~/.gemini/antigravity-cli/antigravity-oauth-token")
        );
        let token = &file["structure"]["token"];
        assert_eq!(token["access_token"].as_str(), Some("{{.AccessToken}}"));
        assert_eq!(token["refresh_token"].as_str(), Some("{{.RefreshToken}}"));
        assert_eq!(token["token_type"].as_str(), Some("Bearer"));
        // In the past, so agy refreshes through the proxy at every start.
        assert_eq!(token["expiry"].as_str(), Some("2000-01-01T00:00:00Z"));
        assert_eq!(file["structure"]["auth_method"].as_str(), Some("consumer"));

        // The bearer goes to exactly these hosts, and each one — like the token endpoint — is
        // reachable. The Gemini API host belongs to the `google` API key alone.
        let mut hosts = strings(&oauth["resourceHosts"]);
        hosts.sort();
        assert_eq!(
            hosts,
            [
                "aicode.googleapis.com",
                "cloudcode-pa.googleapis.com",
                "daily-cloudcode-pa.googleapis.com",
                "www.googleapis.com"
            ]
        );
        let net = cap(&doc, "network-policy@1");
        let allow = strings(&net.config["runtime"]["allow"]);
        for host in hosts
            .iter()
            .map(String::as_str)
            .chain(["oauth2.googleapis.com"])
        {
            assert!(
                allow.iter().any(|a| a == host),
                "{host} is allowed: {allow:?}"
            );
        }
        assert_eq!(
            strings(&net.config["runtime"]["deny"]),
            ["play.googleapis.com"]
        );
        assert!(!allow.iter().any(|a| a == "play.googleapis.com"));
        assert!(!allow.iter().any(|a| a == "cloudaicompanion.googleapis.com"));
        assert!(
            capabilities(&doc).iter().all(|c| c.kind != "volume@1"),
            "agy's state is container-local"
        );

        // The API-key credential next to it.
        let google = by("google");
        assert!(google.optional);
        let key = &google.config["apiKey"];
        assert_eq!(key["name"].as_str(), Some("GEMINI_API_KEY"));
        assert_eq!(key["proxyManaged"].as_bool(), Some(true));
        assert_eq!(
            key["inject"][0]["domain"].as_str(),
            Some("generativelanguage.googleapis.com")
        );
        assert_eq!(key["inject"][0]["header"].as_str(), Some("x-goog-api-key"));

        // Neither credential is a setup error: the sign-in happens inside the sandbox.
        let catalog = Catalog::load().unwrap();
        assert!(catalog.backend("agy").unwrap().kit.credentials.is_empty());
    }

    #[test]
    fn agy_hint_says_the_sign_in_is_kept_on_the_host() {
        let catalog = Catalog::load().expect("kits/ must be discoverable from the crate");
        let hint = catalog
            .backend("agy")
            .expect("the agy backend")
            .setup_hint();
        assert!(!hint.contains("signed in inside the sandbox"), "{hint}");
        assert!(hint.contains("kept on the host"), "{hint}");
        assert!(hint.contains("Gemini API key"), "{hint}");
    }

    #[test]
    fn the_package_cache_kit_is_headless_only_and_opens_no_host() {
        let Ok(catalog) = Catalog::load() else { return };
        assert!(
            catalog.addons.iter().all(|a| a.kit.name != "pkg-cache"),
            "the dashboard never offers it"
        );
        let kit = catalog
            .addon("pkg-cache")
            .expect("aisbx up --kit pkg-cache finds it");
        assert!(
            capabilities(&shipped("pkg-cache"))
                .iter()
                .all(|c| !c.kind.starts_with("network-policy@")),
            "a cache kit opens no host"
        );
        assert!(
            catalog.hashes().contains_key("pkg-cache"),
            "drift is noticed for it too"
        );
        // cargo reads the copied registry offline: its CARGO_HOME is the copy, and the startup
        // hook copies the `cargo` part with the others.
        let recipe = dockerfile("pkg-cache");
        for want in [
            "CARGO_HOME=/home/agent/.pkg-cache/cargo",
            "CARGO_NET_OFFLINE=true",
        ] {
            assert!(recipe.contains(want), "pkg-cache sets {want}");
        }
        let startup =
            std::fs::read_to_string(kit.kit.dir.join("files/pkg-cache/startup.sh")).unwrap();
        assert!(
            startup.contains("for part in npm wheels uv cargo; do"),
            "{startup}"
        );
    }

    #[test]
    fn the_rust_kit_pins_its_toolchain_and_builds_outside_the_workspace() {
        let Ok(catalog) = Catalog::load() else { return };
        let addon = catalog
            .addons
            .iter()
            .find(|a| a.kit.name == "rust")
            .expect("offered on the form and in the kits editor");
        assert_eq!(addon.create_time_reason(), Some("network rules"));
        let recipe = dockerfile("rust");
        for want in [
            "ENV SBX_KIT_RUST=1",
            "ENV CARGO_TARGET_DIR=/home/agent/.cache/cargo-target",
            "V=\"1.99.1\"",
            "sha256sum -c -",
            "T=\"x86_64-unknown-linux-gnu\"",
            "T=\"aarch64-unknown-linux-gnu\"",
        ] {
            assert!(recipe.contains(want), "the rust kit has {want}");
        }
        // Five components per architecture, each with a 64-hex digest.
        let digests = recipe
            .lines()
            .filter(|l| {
                l.trim_start()
                    .split_once(':')
                    .map(|(_, d)| d.trim_end_matches('"'))
                    .is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
            })
            .count();
        assert_eq!(digests, 10, "{recipe}");
        let net = cap(&shipped("rust"), "network-policy@1");
        assert_eq!(
            strings(&net.config["install"]["allow"]),
            [
                "ports.ubuntu.com",
                "archive.ubuntu.com",
                "security.ubuntu.com",
                "download.docker.com"
            ]
        );
        assert!(net.config["runtime"].is_badvalue(), "no egress at run time");
    }

    #[test]
    fn the_host_package_cache_kits_are_create_time_dashboard_addons() {
        let Ok(catalog) = Catalog::load() else { return };
        assert_eq!(HEADLESS_ONLY, ["pkg-cache"]);
        for (name, reason) in [
            ("host-gradle", "extra workspaces"),
            ("host-maven", "extra workspaces"),
            ("host-npm", "volumes"),
            ("host-pip", "volumes"),
            ("host-skills", "extra workspaces"),
        ] {
            let addon = catalog
                .addons
                .iter()
                .find(|a| a.kit.name == name)
                .unwrap_or_else(|| panic!("the create form offers {name}"));
            assert!(
                addon.needs_mounts(),
                "{name} needs its cache folders mounted at creation"
            );
            assert!(addon.create_time_only(), "{name}");
            assert_eq!(addon.create_time_reason(), Some(reason), "{name}");
            assert!(
                catalog.headless.iter().all(|a| a.kit.name != name),
                "{name}"
            );
            assert!(
                capabilities(&shipped(name))
                    .iter()
                    .all(|c| !c.kind.starts_with("network-policy@")),
                "{name} opens no host"
            );
            assert!(
                catalog.hashes().contains_key(name),
                "drift is noticed for {name}"
            );
        }
        assert_eq!(short_kit_label("host-npm"), "Host npm");
        assert_eq!(short_kit_label("host-pip"), "Host pip");
        assert_eq!(short_kit_label("host-skills"), "Host skills");
        assert!(
            catalog.addon("host-skills").is_some(),
            "aisbx up --kit finds it"
        );
    }

    #[test]
    fn a_comment_does_not_change_a_kits_hash() {
        let dir = scratch_kit(
            "commented",
            "# syntax=docker/sandbox-kit:3\n# What it is.\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
        );
        let script = dir.join("files").join("start.sh");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        let docker = dir.join("commented.dockerfile");
        let write = |descriptor: &str, dockerfile: &str, sh: &str| {
            fs::write(descriptor_path(&dir), descriptor).unwrap();
            fs::write(&docker, dockerfile).unwrap();
            fs::write(&script, sh).unwrap();
            hash_dir(&dir).unwrap()
        };
        let base = write(
            "# syntax=docker/sandbox-kit:3\n# What it is.\nkind: mixin  # trailing\nbuild: |\n  # Inline.\n  FROM scratch\n",
            "# syntax=docker/dockerfile:1\n# Old words.\nFROM scratch\n\n# More.\nENV A=1\n",
            "#!/bin/sh\n# Old words.\necho hi\n",
        );
        let reworded = write(
            "# syntax=docker/sandbox-kit:3\n# Other words,\n# two lines.\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
            "# syntax=docker/dockerfile:1\n# New words.\nFROM scratch\nENV A=1\n",
            "#!/bin/sh\n\n# New words,\n  # indented.\necho hi\n",
        );
        assert_eq!(reworded, base, "comments and blank lines are no change");
        let cases = [
            (
                "a descriptor value",
                "# syntax=docker/sandbox-kit:3\nkind: workload\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=1\n",
                "#!/bin/sh\necho hi\n",
            ),
            (
                "a line in a block scalar",
                "# syntax=docker/sandbox-kit:3\nkind: mixin\nbuild: |\n  # Other.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=1\n",
                "#!/bin/sh\necho hi\n",
            ),
            (
                "the descriptor's syntax directive",
                "# syntax=docker/sandbox-kit:4\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=1\n",
                "#!/bin/sh\necho hi\n",
            ),
            (
                "the Dockerfile's syntax directive",
                "# syntax=docker/sandbox-kit:3\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:2\nFROM scratch\nENV A=1\n",
                "#!/bin/sh\necho hi\n",
            ),
            (
                "an instruction",
                "# syntax=docker/sandbox-kit:3\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=2\n",
                "#!/bin/sh\necho hi\n",
            ),
            (
                "a shebang",
                "# syntax=docker/sandbox-kit:3\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=1\n",
                "#!/bin/bash\necho hi\n",
            ),
            (
                "a command",
                "# syntax=docker/sandbox-kit:3\nkind: mixin\nbuild: |\n  # Inline.\n  FROM scratch\n",
                "# syntax=docker/dockerfile:1\nFROM scratch\nENV A=1\n",
                "#!/bin/sh\necho ho\n",
            ),
        ];
        for (what, descriptor, dockerfile, sh) in cases {
            assert_ne!(
                write(descriptor, dockerfile, sh),
                base,
                "{what} is a change"
            );
        }
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn the_list_of_changes_is_read_newest_first_and_is_no_change_itself() {
        let dir = scratch_kit("listed", "kind: mixin\n");
        assert!(read_changes(&dir).is_empty(), "no file, no changes");
        let before = hash_dir(&dir).unwrap();
        fs::write(
            dir.join(CHANGES_FILE),
            "# Newest first.\nFilters git log.\n\n  Faster start.  \n",
        )
        .unwrap();
        assert_eq!(read_changes(&dir), ["Filters git log.", "Faster start."]);
        assert_eq!(hash_dir(&dir).unwrap(), before);
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn every_shipped_change_is_one_short_sentence() {
        let catalog = Catalog::load().unwrap();
        let kits =
            (catalog.backends.iter().map(|b| &b.kit)).chain(catalog.all_addons().map(|a| &a.kit));
        for kit in kits {
            for change in &kit.changes {
                assert!(
                    change.ends_with('.') && change.chars().count() <= 120,
                    "{}/{CHANGES_FILE}: {change}",
                    kit.name
                );
            }
        }
    }

    #[test]
    fn a_finder_file_does_not_change_a_kits_hash() {
        let dir = scratch_kit("hashed", "kind: mixin\n");
        let before = hash_dir(&dir).unwrap();
        fs::write(dir.join(".DS_Store"), b"finder").unwrap();
        assert_eq!(hash_dir(&dir).unwrap(), before);
        fs::write(dir.join("hashed.dockerfile"), b"FROM scratch\n").unwrap();
        assert_ne!(hash_dir(&dir).unwrap(), before, "a real file does");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn the_repo_root_is_found_from_every_binary_location() {
        let repo = std::env::temp_dir().join(format!("aisbx-root-{}", std::process::id()));
        fs::create_dir_all(repo.join("kits")).unwrap();
        let finds = |exe: PathBuf| {
            exe_candidates(&exe)
                .into_iter()
                .find(|p| p.join("kits").is_dir())
        };
        for exe in [
            repo.join("target/release/aisbx"),
            repo.join("target/debug/aisbx-ui"),
            repo.join("bin/unix/build/aisbx"),
            repo.join("bin/windows/build/aisbx-ui.exe"),
            repo.join("bin/aisbx"),
            repo.join("bin/aisbx-ui.exe"),
        ] {
            assert_eq!(
                finds(exe.clone()).as_deref(),
                Some(repo.as_path()),
                "{}",
                exe.display()
            );
        }
        fs::remove_dir_all(&repo).unwrap();

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let parent = manifest.parent().expect("core sits in the checkout");
        assert!(parent.join("kits").is_dir(), "{}", parent.display());
        assert!(find_repo_root().unwrap().join("kits").is_dir());
    }

    #[test]
    fn short_labels() {
        assert_eq!(short_kit_label("rtk"), "RTK");
        assert_eq!(short_kit_label("ponytail"), "Ponytail");
        assert_eq!(short_kit_label("jdk25"), "JDK25");
        assert_eq!(short_kit_label("host-gradle"), "Host Gradle");
        assert_eq!(short_kit_label("host-maven"), "Host Maven");
        assert_eq!(short_kit_label("web"), "Web");
    }

    /// Every kit in `kits/` passes `sbx kit validate` (docs/kits.md, *Validate*). The first run builds
    /// every kit. Run it with the other live checks: `cargo test -- --ignored --nocapture live`.
    #[test]
    #[ignore = "runs sbx; part of the live suite"]
    fn live_every_kit_validates() {
        let kits_dir = Catalog::load().expect("the kits/ directory").kits_dir;
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&kits_dir)
            .expect("reading kits/")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|dir| descriptor_path(dir).is_file())
            .collect();
        dirs.sort();
        assert!(
            dirs.iter().any(|d| d.ends_with("claude-litellm")),
            "{dirs:?}"
        );
        for dir in &dirs {
            // `{"reference", "kind", "valid", "warnings": [...]}`, plus `error` when it is not.
            let out = std::process::Command::new("sbx")
                .args(["kit", "validate", "--json"])
                .arg(dir)
                .output()
                .expect("running `sbx kit validate`");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            println!("sbx kit validate {} → {}", dir.display(), text.trim());
            assert!(out.status.success(), "{text}");
            let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
            assert_eq!(report["valid"], serde_json::json!(true), "{text}");
            // A warning — a deprecation above all — means a kit has aged out of the grammar.
            assert_eq!(report["warnings"], serde_json::json!([]), "{text}");
        }
    }
}

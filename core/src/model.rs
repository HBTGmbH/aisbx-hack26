//! The sandbox view model: what `sbx ls` + `sbx inspect` report, resolved against this repo's
//! kit catalog and the local label registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::kits::{short_kit_label, Catalog};
use crate::sbx::{self, InspectEntry, LsEntry, Secrets};
use crate::state::Registry;

/// Raw per-sandbox facts as gathered by the refresh worker, before catalog resolution.
#[derive(Debug, Clone)]
pub struct Raw {
    pub ls: LsEntry,
    pub inspect: Option<InspectEntry>,
}

/// One refresh cycle's worth of data, handed from the worker thread to the UI.
#[derive(Debug)]
pub struct Snapshot {
    pub sandboxes: Vec<Raw>,
    /// Current content hash per kit name, for drift detection.
    pub kit_hashes: BTreeMap<String, String>,
    /// What `sbx secret ls` reports, or `None` when it failed. Names and placeholders only —
    /// never values, which stay in the host keychain.
    pub secrets: Option<Secrets>,
    /// The MCP servers registered on this host. Host-wide: sbx does not report which servers a
    /// sandbox carries.
    pub mcp: Vec<sbx::McpServer>,
    /// What the shared agent-skills store holds (`sbx skills ls --json`), or `None` when the read
    /// failed. Host-wide: one store, mounted into every sharing sandbox.
    pub skills: Option<sbx::SkillsStore>,
    /// The host's `skills.defaultMode`: what the create form opens on. Re-read every refresh.
    pub skills_default: sbx::SkillsMode,
    /// Requests that sandboxes hold for approval, or `None` when the read failed: the app then
    /// keeps the previous answer.
    pub approvals: Option<Vec<sbx::Approval>>,
    /// Whether `sbx policy init` still has to run (`None`: unknown) — see
    /// [`sbx::network_policy_needs_init`].
    pub policy_needs_init: Option<bool>,
    /// The model fetchers on this host ([`crate::model_fetch`]), by sandbox name. Never in
    /// `sandboxes`: the list does not show them.
    pub fetchers: Vec<String>,
}

/// How a sandbox mounts the host's shared agent-skills store, as far as this dashboard can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skills {
    /// The mode aisbx recorded when it created the sandbox.
    Known(sbx::SkillsMode),
    /// The backend's agent does mount the store, but which way is not recorded: the sandbox was
    /// created outside this dashboard.
    Unrecorded,
}

impl Skills {
    /// The mode to reproduce on a rebuild. `None` (unrecorded) passes no `--skills`, so the host's
    /// `skills.defaultMode` decides.
    pub fn mode(self) -> Option<sbx::SkillsMode> {
        match self {
            Self::Known(mode) => Some(mode),
            Self::Unrecorded => None,
        }
    }
}

/// Which Anthropic subscription account a Claude sandbox is on. The backend kit decides the kind
/// ([`crate::kits::Backend::account`]); the secret store tells the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// The host-global `/login`. Shared by every `/login` sandbox on this machine — sbx
    /// keeps the real tokens on the host at global scope, so they cannot hold different accounts.
    Login,
    /// A named `claude setup-token`, reached through the `CLAUDE_CODE_OAUTH_TOKEN` placeholder the
    /// container carries. Any number of sandboxes can share it.
    Token {
        /// The host-global token's name.
        name: String,
        /// The placeholder baked into this container at creation. It cannot be changed without
        /// rebuilding, which is what makes *rotating* a token free and *switching* one costly.
        placeholder: String,
        /// Is a token actually stored behind the placeholder?
        stored: bool,
    },
}

impl Auth {
    /// Is this sandbox on a setup token rather than the shared `/login`?
    pub fn on_token(&self) -> bool {
        !matches!(self, Auth::Login)
    }

    pub fn token_name(&self) -> Option<&str> {
        match self {
            Auth::Token { name, .. } => Some(name),
            Auth::Login => None,
        }
    }

    /// The `CLAUDE_CODE_OAUTH_TOKEN` value inside the container, for the calls that have to write
    /// a secret against exactly the string the sandbox will send.
    pub fn placeholder(&self) -> Option<&str> {
        match self {
            Auth::Token { placeholder, .. } => Some(placeholder),
            Auth::Login => None,
        }
    }

    /// Short form for the list's row: which account, in as few columns as possible.
    pub fn short(&self) -> String {
        match self {
            Auth::Login => "/login".into(),
            Auth::Token {
                name, stored: true, ..
            } => format!("token {name}"),
            Auth::Token { name, .. } => format!("token {name} missing"),
        }
    }

    /// The sandbox cannot authenticate as things stand.
    pub fn broken(&self) -> bool {
        matches!(self, Auth::Token { stored: false, .. })
    }
}

/// Which account a sandbox is on, from its backend kit, its token kit and the live secret store.
fn auth_of(
    account: crate::kits::Account,
    token: Option<&str>,
    secrets: Option<&Secrets>,
) -> Option<Auth> {
    if account == crate::kits::Account::Login {
        return Some(Auth::Login);
    }
    // A failed `sbx secret ls` is "unknown", not "no token", and so is a sandbox without a token
    // kit: sbx reported no kit list for it.
    let secrets = secrets?;
    let named = token?;
    let placeholder = sbx::named_token_placeholder(named);
    Some(Auth::Token {
        stored: secrets.holds(&placeholder),
        name: named.to_string(),
        placeholder,
    })
}

#[derive(Debug, Clone)]
pub struct Sandbox {
    /// The sbx sandbox name — the identity every `sbx` call uses.
    pub name: String,
    /// Friendly label from the registry, falling back to `name`.
    pub label: String,
    pub agent: String,
    pub running: bool,
    pub workspace: PathBuf,
    pub workspace_missing: bool,
    /// Where the agent opens, relative to `workspace`; `None` is the workspace root. Read from the
    /// registry, because a start folder is not part of the sandbox ([`crate::startdir`]).
    pub start_dir: Option<PathBuf>,
    /// Extra read-only mounts (`host-gradle`, `aisbx up --mount DIR:ro`), without the `:ro` suffix.
    pub ro_mounts: Vec<PathBuf>,
    /// Extra read-write mounts - only `aisbx up --mount DIR` makes one (a cache a script fills).
    pub rw_mounts: Vec<PathBuf>,
    /// Backend kit name (`claude-anthropic`, …) when the sandbox was built from this repo.
    pub backend: Option<String>,
    /// Addon kit names from this repo, in catalog order.
    pub addons: Vec<String>,
    /// Kits sbx reports that this repo does not own.
    pub foreign_kits: Vec<String>,
    /// sbx reported no kits at all (a plain `sbx run`).
    pub kits_unknown: bool,
    /// The kits were matched by name but live in a different aisbx checkout than the one
    /// this binary uses — set to that checkout's `kits/` dir.
    pub other_checkout: Option<PathBuf>,
    /// Built from aisbx's copies of its kits with every volume in RAM (`aisbx up --tmpfs-volumes`,
    /// [`crate::tmpfs`]); read off the kit list.
    pub tmpfs_volumes: bool,
    /// Its backend kit keeps the agent's session history in volumes, so a rebuild keeps it
    /// ([`crate::kits::Kit::volumes`]).
    pub history_in_volumes: bool,
    /// Why a rebuild would bring it onto the current kits, if one would: a kit it carries changed
    /// after it was built, or was replaced.
    pub drift: Option<Drift>,
    pub clone: bool,
    /// How this sandbox mounts the host's shared agent-skills store, read from the registry. `None`
    /// for a backend without a store mount, and for a sandbox not built from this repo's kits.
    pub skills: Option<Skills>,
    /// Host-to-sandbox port mappings, straight off `sbx ls --json`.
    pub ports: Vec<sbx::Port>,
    /// When sbx last saw this sandbox used, Unix seconds (`last_used_at` of `sbx ls --json`). A
    /// stopped row shows it. Never shown as a creation time; that is [`Sandbox::created`].
    pub last_used: Option<u64>,
    /// When the sandbox was created, Unix seconds: sbx's own `created_at`. It is the container's
    /// creation, so a `sbx kit add` moves it.
    pub created: Option<u64>,
    /// The limits recorded at creation (`--cpus`, `-m`); `None` for sbx's "auto". `memory` is as
    /// given (`4g`), `memory_spec` the normalised `-m` that a rebuild passes back.
    pub cpus: Option<u32>,
    pub memory: Option<String>,
    pub memory_spec: Option<String>,
    /// The guest has stopped responding (sbx's recovery state). The row says so first.
    pub unresponsive: bool,
    /// Host folders added after creation with `sbx mount` (`runtime_mounts`), restored on every
    /// start. Not the workspaces given at creation ([`sbx::InspectEntry::runtime_mounts`]).
    pub runtime_mounts: Vec<sbx::RuntimeMount>,
    /// Every folder sbx bind-mounts into the sandbox, as `sbx inspect` reports it; `None` when it
    /// gave no answer.
    pub host_mounts: Option<Vec<BindMount>>,
    /// In detached mode (`b`, or `sbx run -d` from the CLI): it keeps running after its sessions
    /// end. Read off `detached` in `ls --json` / `inspect --json`, never remembered.
    pub keep_running: bool,
    pub image: String,
    pub image_digest: String,
    pub sessions: u32,
    /// How the agent inside authenticates, e.g. `oauth · anthropic`, as sbx reports it.
    pub auth_mode: String,
    /// Which Anthropic account this sandbox is on — `None` for every backend that does not
    /// authenticate against a subscription (the gateway kits, and anything not from this repo).
    pub auth: Option<Auth>,
    /// Which model each of Claude Code's aliases resolves to, read off the generated model kit
    /// ([`crate::models`]). Empty means Claude Code's own defaults.
    pub models: crate::models::Overrides,
    /// The LiteLLM connection this sandbox is on, read off its kit list
    /// ([`crate::litellm::of_kit`]).
    pub litellm: Option<String>,
    /// Docker network name, and the egress proxy enforcing the policy on it.
    pub network: String,
    pub proxy: String,
    /// `global` when the sandbox rides the local policy, `sandbox` when it has scoped rules.
    pub policy_scope: String,
    /// Injected secrets as (name, source) — never the values, which stay on the host.
    pub secrets: Vec<(String, String)>,
    pub mcp_gateway: bool,
    pub daemon_version: String,
    /// The sbx release that created the sandbox, as `sbx inspect` reports it; empty when it does
    /// not.
    pub created_version: String,
}

impl Sandbox {
    /// The overview's `volumes` row - value and gloss - for a sandbox whose kit volumes are in
    /// RAM (`aisbx up --tmpfs-volumes`); `None` for volumes on disk, every other sandbox's case.
    pub fn volumes_note(&self) -> Option<(&'static str, &'static str)> {
        self.tmpfs_volumes.then_some((
            "in RAM (tmpfs)",
            "aisbx up --tmpfs-volumes: emptied at every stop, nothing left after a delete, R keeps them in RAM",
        ))
    }

    /// Managed by this repo — the only sandboxes whose kits the TUI may edit.
    pub fn is_managed(&self) -> bool {
        self.backend.is_some()
    }

    /// The absolute directory the agent opens in: the workspace root unless a start folder says
    /// otherwise. The workspace is mounted at its host path, so the path is the same inside.
    pub fn start_path(&self) -> PathBuf {
        crate::startdir::resolve(&self.workspace, self.start_dir.as_deref())
    }

    /// Has the chosen start folder been deleted or renamed since?
    pub fn start_dir_missing(&self) -> bool {
        !self.workspace_missing
            && !self.mountless()
            && crate::startdir::missing(&self.workspace, self.start_dir.as_deref())
    }

    /// Does this sandbox mount no host folder at all?
    pub fn mountless(&self) -> bool {
        self.workspace.as_os_str().is_empty()
    }

    /// The folder line: the mounted host path, or what stands in for one.
    pub fn folder_label(&self) -> String {
        match self.mountless() {
            true => "no host folder (mountless)".to_string(),
            false => tilde(&self.workspace),
        }
    }

    /// The directory an editor should open over SSH: the workspace's host path, which sbx mounts at
    /// the same path, or [`MOUNTLESS_WORKDIR`] for a mountless sandbox.
    pub fn remote_workspace(&self) -> PathBuf {
        match self.mountless() {
            true => PathBuf::from(MOUNTLESS_WORKDIR),
            false => self.workspace.clone(),
        }
    }

    /// Can this sandbox be given a start folder at all? Any sandbox with a workspace on the host.
    pub fn can_pick_start_dir(&self) -> bool {
        !self.workspace_missing && !self.mountless()
    }

    /// Where the agent opens, written so the mount and the working directory stay distinguishable:
    /// `~/work › notes`.
    pub fn start_label(&self) -> String {
        match self.mountless() {
            true => self.folder_label(),
            false => crate::startdir::describe(&self.workspace, self.start_dir.as_deref()),
        }
    }

    /// TYPE column text: the backend, plus the account or the LiteLLM connection it is on.
    pub fn type_label(&self, catalog: &Catalog) -> String {
        let Some(backend) = self.backend.as_deref().and_then(|b| catalog.backend(b)) else {
            return format!("{} · extern", self.agent);
        };
        match (&self.auth, &self.litellm) {
            (Some(auth), _) => format!("{} · {}", backend.type_label(), auth.short()),
            (None, Some(conn)) => format!("{} {conn}", backend.type_label()),
            (None, None) => backend.type_label(),
        }
    }

    /// Every kit beside its short label (`Host Gradle`, `JDK25`): the GUI's badges.
    pub fn kit_badges(&self) -> Vec<(String, String)> {
        self.kits_named(short_kit_label)
    }

    /// Every kit beside its id, as the CLI names it (`host-gradle`, `jdk25`): the TUI's words in
    /// a row.
    pub fn kit_ids(&self) -> Vec<(String, String)> {
        self.kits_named(str::to_string)
    }

    /// The addons, then the kits of no catalog as `name?`, each beside what `name` makes of it.
    fn kits_named(&self, name: impl Fn(&str) -> String) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> =
            self.addons.iter().map(|k| (k.clone(), name(k))).collect();
        out.extend(
            self.foreign_kits
                .iter()
                .map(|k| (k.clone(), format!("{k}?"))),
        );
        out
    }

    pub fn status_text(&self) -> &'static str {
        if self.running {
            "running"
        } else {
            "stopped"
        }
    }

    /// Where the sandbox lives: always this machine, plus the daemon version and the sbx release
    /// that created the sandbox, each when sbx reports it.
    pub fn location(&self) -> String {
        let mut out = String::from("local");
        if !self.daemon_version.is_empty() {
            out.push_str(&format!("  ·  sandboxd {}", self.daemon_version));
        }
        if !self.created_version.is_empty() {
            out.push_str(&format!("  ·  created by sbx {}", self.created_version));
        }
        out
    }

    /// Everything mounted from the host, primary workspace first, as (path, mode).
    pub fn mounts(&self) -> Vec<(&Path, &'static str)> {
        let mut out = Vec::new();
        if !self.mountless() {
            out.push((
                self.workspace.as_path(),
                if self.clone {
                    "read-only (clone mode)"
                } else {
                    "read-write"
                },
            ));
        }
        out.extend(self.ro_mounts.iter().map(|p| (p.as_path(), "read-only")));
        out.extend(
            self.rw_mounts
                .iter()
                .map(|p| (p.as_path(), "read-write (extra)")),
        );
        out
    }

    /// This sandbox authenticates with a setup token whose secret is gone, so it cannot
    /// authenticate at all until one is stored again.
    pub fn token_missing(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.broken())
    }

    /// A rebuild would bring it onto the current kits ([`Sandbox::drift`]).
    pub fn kit_drift(&self) -> bool {
        self.drift.is_some()
    }

    /// The one warning a row spells out, given how many approvals it has `waiting`. The rank is
    /// decided here, once; the frontends word it.
    pub fn card_note(&self, waiting: usize) -> Option<CardNote> {
        if self.unresponsive {
            Some(CardNote::Unresponsive)
        } else if self.token_missing() {
            Some(CardNote::NoToken)
        } else if waiting > 0 {
            Some(CardNote::Approvals(waiting))
        } else {
            (self.drift.as_ref()).map(|drift| CardNote::KitsChanged(drift.kind()))
        }
    }

    /// The status facts of a row, most important first: `running · 1 session · clone`. A row
    /// short of room drops them from the right.
    pub fn status_parts(&self) -> Vec<String> {
        let mut parts = vec![self.status_text().to_string()];
        // First optional part: detached mode changes what "running" means.
        if self.keep_running {
            parts.push("background".into());
        }
        // Only on a stopped sandbox: on a running one it would always say "now".
        if !self.running {
            if let Some(epoch) = self.last_used {
                parts.push(crate::state::ago(epoch));
            }
        }
        if self.sessions > 0 {
            parts.push(format!(
                "{} session{}",
                self.sessions,
                if self.sessions == 1 { "" } else { "s" }
            ));
        }
        if self.clone {
            parts.push("clone".into());
        }
        parts
    }

    /// Whether the list shows this sandbox under `cwd`, the folder the dashboard shows: its
    /// workspace is `cwd`, or its workspace is a folder above `cwd` and its agent opens in `cwd`.
    pub fn belongs_here(&self, cwd: &Path) -> bool {
        if self.mountless() {
            return false;
        }
        if same_path(&self.workspace, cwd) {
            return true;
        }
        match &self.start_dir {
            Some(rel) => {
                cwd.starts_with(&self.workspace) && same_path(&self.workspace.join(rel), cwd)
            }
            None => false,
        }
    }

    /// Where this sandbox's folder sits from `cwd`, the folder the dashboard shows.
    pub fn folder_relation(&self, cwd: &Path) -> FolderRelation {
        if self.mountless() {
            FolderRelation::Mountless
        } else if self.belongs_here(cwd) {
            FolderRelation::Here
        } else {
            FolderRelation::Elsewhere
        }
    }

    /// What happens once its last session ends — only where that is worth saying: a sandbox in
    /// detached mode, or one serving a port that the idle stop would take down.
    pub fn lifecycle(&self) -> Option<Lifecycle> {
        if self.keep_running {
            Some(Lifecycle::KeepsRunning)
        } else if !self.ports.is_empty() {
            Some(Lifecycle::StopsWithPorts)
        } else {
            None
        }
    }

    /// The image digest shortened for display: at most 12 characters after the first `:`.
    pub fn short_digest(&self) -> Option<&str> {
        let short = self.image_digest.split(':').nth(1)?;
        Some(match short.char_indices().nth(12) {
            Some((end, _)) => &short[..end],
            None => short,
        })
    }

    /// The injected secrets as (environment variable, source). sbx names a custom secret that
    /// sets no variable (a named token's placeholder) `""` or `-`; that is `None` here.
    pub fn injected_secrets(&self) -> Vec<(Option<&str>, &str)> {
        self.secrets
            .iter()
            .map(|(name, source)| {
                let env = match name.as_str() {
                    "" | "-" => None,
                    name => Some(name),
                };
                (env, source.as_str())
            })
            .collect()
    }
}

/// Where a sandbox's folder sits from the folder the dashboard shows — see
/// [`Sandbox::folder_relation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRelation {
    /// It mounts the folder on screen.
    Here,
    /// It mounts some other folder.
    Elsewhere,
    /// It mounts no host folder at all.
    Mountless,
}

/// What a sandbox does once its last session ends — see [`Sandbox::lifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Detached mode: it keeps running until it is stopped.
    KeepsRunning,
    /// It stops itself, and its published ports stop answering.
    StopsWithPorts,
}

/// The sort key of a folder in the list's second section and in the folder list: the folder as
/// shown (`~/…`), case folded. One key, so both orders agree.
pub fn folder_key(path: &Path) -> String {
    tilde(path).to_lowercase()
}

/// The warning a row spells out — see [`Sandbox::card_note`] for the rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardNote {
    /// The guest has stopped responding; stopping it (`s`) recovers it.
    Unresponsive,
    /// A setup-token sandbox whose token is not stored; `A` stores one.
    NoToken,
    /// Requests waiting for an answer, and how many; `y` answers them.
    Approvals(usize),
    /// A rebuild would bring it onto the current kits ([`Sandbox::drift`]); `R` rebuilds it.
    KitsChanged(DriftKind),
}

/// Why a sandbox needs a rebuild to be on the current kits — see [`Sandbox::drift`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// These kits changed after it was built.
    Changed(Vec<KitUpdate>),
}

/// A kit with a newer version than the one a sandbox was built with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitUpdate {
    /// The kit's display name.
    pub name: String,
    /// What its `changes.txt` lists since the build, newest first; empty where it lists nothing
    /// new.
    pub changes: Vec<String>,
}

impl From<&str> for KitUpdate {
    fn from(name: &str) -> Self {
        KitUpdate {
            name: name.to_string(),
            changes: Vec::new(),
        }
    }
}

/// How many of a kit's changes the reason quotes; the rest are counted.
const CHANGES_SHOWN: usize = 3;

/// Which kind of [`Drift`] a row's note names; each has its own words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    Changed,
}

impl DriftKind {
    /// The note a row shows for it, after `⟳`.
    pub fn note(self) -> &'static str {
        match self {
            DriftKind::Changed => "update ready",
        }
    }

    /// The note as a heading of its own.
    pub fn title(self) -> &'static str {
        match self {
            DriftKind::Changed => "Update ready",
        }
    }
}

impl Drift {
    pub fn kind(&self) -> DriftKind {
        match self {
            Drift::Changed(_) => DriftKind::Changed,
        }
    }

    /// Why a rebuild is due and what it brings or costs, for a reader who does not know kits.
    pub fn reason(&self) -> String {
        match self {
            Drift::Changed(kits) => {
                let names: Vec<String> = kits.iter().map(|k| k.name.clone()).collect();
                let mut out = format!(
                    "There is a newer version of {}.",
                    and_list(&names, "its kits")
                );
                for kit in kits.iter().filter(|k| !k.changes.is_empty()) {
                    let shown = kit.changes.len().min(CHANGES_SHOWN);
                    out.push_str(&format!(
                        " New in {}: {}",
                        kit.name,
                        kit.changes[..shown].join(" ")
                    ));
                    match kit.changes.len() - shown {
                        0 => {}
                        1 => out.push_str(" And 1 more change."),
                        more => out.push_str(&format!(" And {more} more changes.")),
                    }
                }
                out.push_str(" The sandbox keeps the old version until it is rebuilt.");
                out
            }
        }
    }
}

/// `A`, `A and B`, `A, B and C`; `none` for an empty list.
fn and_list(items: &[String], none: &str) -> String {
    match items {
        [] => none.to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl CardNote {
    /// A sandbox that cannot run at all is a danger, not a warning.
    pub fn is_danger(self) -> bool {
        matches!(self, CardNote::Unresponsive | CardNote::NoToken)
    }
}

/// A glyph after a row's note, for a condition the note does not name — see
/// [`Sandbox::row_markers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardMarker {
    /// ⚑ — requests waiting for approval.
    Approvals,
    /// ⟳ — an update is ready ([`Sandbox::drift`]).
    KitsChanged,
}

/// A colour family for a badge. Which kits and agents share one is decided here; each frontend
/// maps a family to its own colour (named terminal colours in the TUI, RGB per light or dark mode
/// in the GUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hue {
    Red,
    Green,
    Blue,
    Yellow,
    Magenta,
    Cyan,
    LightGreen,
}

/// Per-kit badge family. `web` is red on purpose: it is the one mixin that opens `**` egress.
/// The host-folder kits read as one family.
pub fn kit_hue(kit: &str) -> Hue {
    match kit {
        "web" => Hue::Red,
        "rtk" => Hue::Green,
        "host-gradle" | "host-maven" | "host-npm" | "host-pip" | "host-skills" => Hue::Blue,
        k if k.starts_with("jdk") => Hue::Yellow,
        _ => Hue::Magenta,
    }
}

/// Backend badge family, keyed on the CLI the agent runs ([`crate::kits::agent_command`]).
pub fn agent_hue(agent: &str) -> Hue {
    match crate::kits::agent_command(agent).unwrap_or(agent) {
        "claude" => Hue::Magenta,
        "opencode" => Hue::Cyan,
        // Pi gets a colour none of the others has.
        "pi" => Hue::LightGreen,
        _ => Hue::Blue,
    }
}

/// Where a mountless sandbox's agent works: the `WORKDIR` of Docker's agent templates, and what
/// the daemon falls back to when an image has no usable one (sbx 0.42's docs on both).
pub const MOUNTLESS_WORKDIR: &str = "/home/agent/workspace";

/// Resolve a refresh snapshot into view models. A sandbox seen for the first time stores the
/// current kit hash in the registry as its drift baseline; only a later change is drift.
pub fn resolve(snapshot: &Snapshot, catalog: &Catalog, registry: &mut Registry) -> Vec<Sandbox> {
    let mut out = Vec::with_capacity(snapshot.sandboxes.len());
    let store = (snapshot.skills.as_ref())
        .map(|s| PathBuf::from(&s.store))
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(sbx::skills_store);

    for raw in &snapshot.sandboxes {
        let name = raw.ls.name.clone();
        let kit_paths: Vec<String> = raw
            .inspect
            .as_ref()
            .map(|i| i.kits.clone())
            .unwrap_or_default();

        let mut backend = None;
        let mut addons = Vec::new();
        let mut foreign_kits = Vec::new();
        let mut other_checkout = None;
        let mut tmpfs_volumes = false;
        let mut token = None;
        let mut models = crate::models::Overrides::default();
        let mut litellm = None;
        for path in &kit_paths {
            let dir_name = Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            // aisbx's copy of one of this checkout's kits with its volumes in RAM: that kit, by
            // name, and no other checkout - see `crate::tmpfs`.
            let in_ram = crate::tmpfs::is_copy(Path::new(path));
            tmpfs_volumes |= in_ram;
            let elsewhere = !in_ram && !Path::new(path).starts_with(&catalog.kits_dir);
            // A named setup token's generated kit (`crate::tokens`), not an addon.
            if let Some(named) = crate::tokens::name_of_kit(&dir_name) {
                token = Some(named.to_string());
                continue;
            }
            // The generated kit that pins the model aliases (`crate::models`).
            if crate::models::is_kit(&dir_name) {
                models = crate::models::read_kit(Path::new(path));
                continue;
            }
            // Generated as well: the list of the host folders it mounts (`crate::folders`), which
            // sbx reports as its mounts.
            if crate::folders::is_kit(&dir_name) {
                continue;
            }
            // Generated as well: the wiring for a LiteLLM connection, which names it.
            if let Some((conn, _)) = crate::litellm::of_kit(&dir_name) {
                litellm = Some(conn.to_string());
                continue;
            }
            // Match on the kit name, not the path, so a moved checkout keeps its sandboxes managed.
            let is_backend = catalog.backend(&dir_name).is_some();
            let is_addon = catalog.addon(&dir_name).is_some();
            if (is_backend || is_addon) && elsewhere {
                other_checkout = Path::new(path).parent().map(Path::to_path_buf);
            }
            if is_backend && backend.is_none() {
                backend = Some(dir_name);
            } else if is_addon {
                if !addons.contains(&dir_name) {
                    addons.push(dir_name);
                }
            } else {
                foreign_kits.push(dir_name);
            }
        }
        // Present addons in catalog order so the badge row is stable across refreshes.
        addons.sort_by_key(|a| {
            catalog
                .addons
                .iter()
                .position(|x| x.kit.name == *a)
                .unwrap_or(usize::MAX)
        });

        let (workspace, ro_mounts, rw_mounts) = split_workspaces(&raw.ls.workspaces);
        let workspace_missing = !workspace.as_os_str().is_empty() && !workspace.is_dir();

        let changed = changed_kits(
            registry,
            &name,
            kit_hashes(&snapshot.kit_hashes, backend.as_deref(), &addons),
            |kit| catalog_kit(catalog, kit).map_or(0, |k| k.changes.len()),
        );
        let history_in_volumes = backend
            .as_deref()
            .and_then(|b| catalog.backend(b))
            .is_some_and(|b| b.kit.volumes);
        let drift = if !changed.is_empty() {
            Some(Drift::Changed(
                changed
                    .iter()
                    .map(|(name, seen)| {
                        let kit = catalog_kit(catalog, name);
                        let all = kit.map_or(&[][..], |k| &k.changes[..]);
                        // Newest first: what came after the build is the head of the list.
                        let new = all.len().saturating_sub(*seen);
                        KitUpdate {
                            name: kit_label(catalog, name),
                            changes: all[..new].to_vec(),
                        }
                    })
                    .collect(),
            ))
        } else {
            None
        };

        let label = registry
            .display_name(&name)
            .map(str::to_string)
            .unwrap_or_else(|| name.clone());

        // Only a subscription backend has an account; a stray custom secret elsewhere is not one.
        let auth = backend
            .as_deref()
            .and_then(|b| catalog.backend(b))
            .and_then(|b| b.account())
            .and_then(|account| auth_of(account, token.as_deref(), snapshot.secrets.as_ref()));

        // Only for a backend that sbx mounts the store for: elsewhere there is no choice to report.
        let skills = backend
            .as_deref()
            .and_then(|b| catalog.backend(b))
            .filter(|b| b.shares_skills())
            .map(|_| match registry.skills(&name) {
                Some(mode) => Skills::Known(mode),
                None => Skills::Unrecorded,
            });

        let inspect = raw.inspect.clone().unwrap_or_default();
        let host_mounts = raw.inspect.is_some().then(|| {
            bind_mounts(
                &inspect.bind_mounts(),
                &workspace,
                store.as_deref(),
                &inspect.dynamic_mounts(),
            )
        });
        out.push(Sandbox {
            label,
            agent: raw.ls.agent.clone(),
            running: raw.ls.status.eq_ignore_ascii_case("running"),
            start_dir: registry.start_dir(&name),
            workspace,
            workspace_missing,
            ro_mounts,
            rw_mounts,
            kits_unknown: kit_paths.is_empty(),
            other_checkout,
            tmpfs_volumes,
            history_in_volumes,
            backend,
            addons,
            foreign_kits,
            drift,
            clone: registry.is_clone(&name),
            skills,
            ports: raw.ls.ports.clone(),
            last_used: raw
                .ls
                .last_used_at
                .as_deref()
                .and_then(crate::state::parse_rfc3339),
            created: raw
                .ls
                .created_at
                .as_deref()
                .and_then(crate::state::parse_rfc3339),
            cpus: raw.ls.cpus,
            memory: raw.ls.memory.clone().filter(|m| !m.trim().is_empty()),
            memory_spec: raw.ls.memory_spec(),
            unresponsive: raw.ls.guest_unresponsive || inspect.guest_unresponsive,
            runtime_mounts: inspect.dynamic_mounts(),
            host_mounts,
            keep_running: raw.ls.detached || inspect.detached,
            image: inspect.image,
            image_digest: inspect.image_digest,
            sessions: inspect.sessions,
            auth_mode: inspect.auth_mode,
            auth,
            models,
            litellm,
            network: inspect.network,
            proxy: inspect.proxy,
            policy_scope: inspect.network_policy.scope,
            secrets: inspect
                .secrets
                .into_iter()
                .map(|s| (s.name, s.source))
                .collect(),
            mcp_gateway: inspect.mcp_gateway,
            daemon_version: inspect.daemon_version,
            created_version: inspect.created_version,
            name,
        });
    }

    out.sort_by_key(|s| s.label.to_lowercase());
    out
}

/// The primary workspace, then the extra ones: read-only (`:ro`) and read-write. An extra one
/// without the suffix is writable from inside.
/// What a bind mount is to its sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// The folder the sandbox is built on; in clone mode, the clone's read-only source.
    Workspace,
    /// sbx's shared agent-skills store.
    Skills,
    /// A folder added with `sbx mount` after creation.
    Added,
    /// Any other: an extra workspace, a kit's folder.
    Folder,
}

/// One folder sbx bind-mounts into a sandbox ([`sbx::InspectEntry::host_mounts`]), with what this
/// host knows of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    pub host: PathBuf,
    /// Where it is inside the sandbox.
    pub target: String,
    pub read_only: bool,
    pub kind: MountKind,
    /// The host folder is gone. Checked on this host: sbx flags only the sandbox as a whole.
    pub missing: bool,
}

/// Name each of sbx's bind mounts by what it is to the sandbox, and check its host folder.
fn bind_mounts(
    mounts: &[sbx::HostMount],
    workspace: &Path,
    store: Option<&Path>,
    added: &[sbx::RuntimeMount],
) -> Vec<BindMount> {
    mounts
        .iter()
        .map(|m| {
            let host = PathBuf::from(&m.host_path);
            let kind = if !workspace.as_os_str().is_empty() && host == workspace {
                MountKind::Workspace
            } else if store == Some(host.as_path()) {
                MountKind::Skills
            } else if added
                .iter()
                .any(|a| a.host_path == m.host_path && a.target() == m.target())
            {
                MountKind::Added
            } else {
                MountKind::Folder
            };
            BindMount {
                missing: !host.exists(),
                target: m.target().to_string(),
                read_only: m.read_only,
                kind,
                host,
            }
        })
        .collect()
}

fn split_workspaces(workspaces: &[String]) -> (PathBuf, Vec<PathBuf>, Vec<PathBuf>) {
    let mut iter = workspaces.iter();
    let primary = iter
        .next()
        .map(|w| PathBuf::from(w.trim_end_matches(":ro")))
        .unwrap_or_default();
    let (mut ro, mut rw) = (Vec::new(), Vec::new());
    for w in iter {
        match crate::folders::spec_host(w) {
            (path, true) => ro.push(PathBuf::from(path)),
            (path, false) => rw.push(PathBuf::from(path)),
        }
    }
    (primary, ro, rw)
}

/// Fold the per-kit hashes of one sandbox's kit set into a single baseline value.
fn kit_hashes(
    hashes: &BTreeMap<String, String>,
    backend: Option<&str>,
    addons: &[String],
) -> Option<BTreeMap<String, String>> {
    let backend = backend?;
    std::iter::once(backend)
        .chain(addons.iter().map(String::as_str))
        .map(|k| Some((k.to_string(), hashes.get(k)?.clone())))
        .collect()
}

/// A kit's display name, or its name where this checkout does not ship it.
fn kit_label(catalog: &Catalog, name: &str) -> String {
    catalog_kit(catalog, name).map_or_else(|| name.to_string(), |k| k.display_name.clone())
}

fn catalog_kit<'a>(catalog: &'a Catalog, name: &str) -> Option<&'a crate::kits::Kit> {
    (catalog.backend(name).map(|b| &b.kit)).or_else(|| catalog.addon(name).map(|a| &a.kit))
}

/// How the registry stores a sandbox's kit hashes: `v2;<kit>=<hash>+<changes>,…`, where
/// `<changes>` is how many lines the kit's `changes.txt` had then. A value of another shape is
/// replaced, not compared.
const HASH_FORMAT: &str = "v2;";

/// A kit's stored hash, and how many changes it listed then.
type Seen = (String, usize);

fn stored_hashes(stored: &str) -> Option<BTreeMap<String, Seen>> {
    stored
        .strip_prefix(HASH_FORMAT)?
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (kit, rest) = p.split_once('=')?;
            let (hash, n) = rest.split_once('+')?;
            Some((kit.to_string(), (hash.to_string(), n.parse().ok()?)))
        })
        .collect()
}

fn store_hashes(hashes: &BTreeMap<String, Seen>) -> String {
    let parts: Vec<String> = hashes
        .iter()
        .map(|(k, (h, n))| format!("{k}={h}+{n}"))
        .collect();
    format!("{HASH_FORMAT}{}", parts.join(","))
}

/// The kits of `current` whose hash differs from the one the registry stored for the sandbox,
/// each with how many changes it listed when it was stored. A kit the stored set lacks was added
/// since and counts as current, a kit it has that the sandbox lost is dropped: the stored set
/// follows the kit list, and only a changed kit keeps its old hash and count. A sandbox with no
/// stored set, or one of another shape, adopts `current`, with `changes` counting each kit's
/// list.
fn changed_kits(
    registry: &mut Registry,
    name: &str,
    current: Option<BTreeMap<String, String>>,
    changes: impl Fn(&str) -> usize,
) -> Vec<(String, usize)> {
    let Some(current) = current else {
        return Vec::new();
    };
    let now = |kit: &str, hash: &str| (hash.to_string(), changes(kit));
    let Some(stored) = registry.kit_hash(name).and_then(stored_hashes) else {
        let adopted = current
            .iter()
            .map(|(k, h)| (k.clone(), now(k, h)))
            .collect();
        registry.set_kit_hash(name, store_hashes(&adopted));
        return Vec::new();
    };
    let mut changed = Vec::new();
    let mut kept = BTreeMap::new();
    for (kit, hash) in &current {
        let seen = match stored.get(kit) {
            Some((old, count)) if old != hash => {
                changed.push((kit.clone(), *count));
                (old.clone(), *count)
            }
            Some(kept) => kept.clone(),
            None => now(kit, hash),
        };
        kept.insert(kit.clone(), seen);
    }
    if kept != stored {
        registry.set_kit_hash(name, store_hashes(&kept));
    }
    changed
}

/// Gather a refresh snapshot on a worker thread: one `sbx ls`, one `sbx inspect` per sandbox and
/// the host-wide reads, run side by side.
pub fn gather(catalog: &Catalog) -> anyhow::Result<Snapshot> {
    std::thread::scope(|s| {
        // Best-effort: a failure makes the secret check and the account readout "unknown".
        let secrets = s.spawn(|| sbx::secrets().ok());
        // An empty list on failure: the create form then offers no MCP section.
        let mcp = s.spawn(sbx::mcp_servers);
        // A failed read keeps the previous answer, not an empty store.
        let skills = s.spawn(|| sbx::skills_ls().ok());
        let skills_default = s.spawn(sbx::skills_default_mode);
        // A failed read keeps the last answer.
        let approvals = s.spawn(|| sbx::approvals().ok());
        let policy_needs_init = s.spawn(sbx::network_policy_needs_init);
        let mut fetchers = Vec::new();
        let sandboxes = sbx::list().map(|entries| {
            let (found, entries): (Vec<_>, Vec<_>) =
                (entries.into_iter()).partition(|ls| crate::model_fetch::is_fetcher(&ls.agent));
            fetchers = found.into_iter().map(|ls| ls.name).collect();
            let inspects =
                each_at_once(&entries, INSPECT_AT_ONCE, |ls| sbx::inspect(&ls.name).ok());
            entries
                .into_iter()
                .zip(inspects)
                .map(|(ls, inspect)| Raw { ls, inspect })
                .collect()
        });
        Ok(Snapshot {
            sandboxes: sandboxes?,
            kit_hashes: catalog.hashes(),
            secrets: joined(secrets),
            mcp: joined(mcp),
            skills: joined(skills),
            skills_default: joined(skills_default),
            approvals: joined(approvals),
            policy_needs_init: joined(policy_needs_init),
            fetchers,
        })
    })
}

/// How many `sbx inspect` calls one refresh runs at the same time.
const INSPECT_AT_ONCE: usize = 8;

/// `f` of every item, at most `at_once` of them at the same time, in the items' order.
fn each_at_once<T: Sync, R: Send>(
    items: &[T],
    at_once: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let f = &f;
    items
        .chunks(at_once.max(1))
        .flat_map(|chunk| {
            std::thread::scope(|s| {
                let handles: Vec<_> = chunk.iter().map(|item| s.spawn(move || f(item))).collect();
                handles.into_iter().map(joined).collect::<Vec<_>>()
            })
        })
        .collect()
}

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

// --- naming -------------------------------------------------------------------------------

/// Reduce a path component to the character set sbx accepts for sandbox names. An underscore is
/// not in it: sbx refuses such a name and does not clean it up.
pub fn sanitize(component: &str) -> String {
    let s: String = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "workspace".into()
    } else {
        s
    }
}

/// The longest name `sbx create --name` accepts.
const NAME_MAX: usize = 63;

/// `<prefix>-<folder>`, with `-2`, `-3`, ... appended when a folder already has a sandbox of that
/// backend. The folder half is trimmed to fit [`NAME_MAX`] and to end in a letter or digit.
pub fn unique_sandbox_name(prefix: &str, folder: &Path, taken: &[String]) -> String {
    let base_folder = folder
        .file_name()
        .map(|s| sanitize(&s.to_string_lossy()))
        .unwrap_or_else(|| "workspace".into());
    // Trim the folder half, never the prefix: the prefix tells two sandboxes on one folder apart.
    // Four characters leave room for the "-99" of a repeat.
    let budget = NAME_MAX.saturating_sub(prefix.len() + 4);
    let folder_part = trim_name(&base_folder, budget.max(1));

    let base = trim_name(&format!("{prefix}-{folder_part}"), NAME_MAX);
    if !taken.iter().any(|t| t == &base) {
        return base;
    }
    (2..)
        .map(|n| {
            format!(
                "{}-{n}",
                trim_name(&base, NAME_MAX - 1 - n.to_string().len())
            )
        })
        .find(|c| !taken.iter().any(|t| t == c))
        .expect("an unused suffix always exists")
}

/// Cut a name to `max` characters and drop any trailing `-` or `.`, which sbx refuses. Falls
/// back to `workspace` only if that leaves nothing.
fn trim_name(s: &str, max: usize) -> String {
    let cut: String = s.chars().take(max).collect();
    let trimmed = cut.trim_end_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "workspace".into()
    } else {
        trimmed
    }
}

/// Default label for a new sandbox, e.g. "OpenCode (RTK, JDK25)".
pub fn auto_label(catalog: &Catalog, backend: &str, addons: &[String]) -> String {
    let base = catalog
        .backend(backend)
        .map(|b| b.base_label())
        .unwrap_or_else(|| backend.to_string());
    if addons.is_empty() {
        return base;
    }
    let list: Vec<String> = catalog
        .addons
        .iter()
        .filter(|a| addons.iter().any(|s| s == &a.kit.name))
        .map(|a| a.short())
        .collect();
    format!("{base} ({})", list.join(", "))
}

/// The addons that link host Gradle and Maven folders, in the order their mounts are listed.
pub const HOST_JVM_KITS: [&str; 2] = ["host-gradle", "host-maven"];

/// The folders below home a JVM cache kit mounts, in the order its startup hook wires them.
fn host_jvm_dirs(kit: &str) -> &'static [&'static str] {
    match kit {
        "host-gradle" => &[".gradle/caches", ".gradle/wrapper/dists"],
        "host-maven" => &[".m2/repository", ".m2/wrapper/dists"],
        _ => &[],
    }
}

/// Host Gradle / Maven cache dirs to mount read-only for the chosen addons: `host-gradle`'s
/// `~/.gradle/{caches,wrapper/dists}`, then `host-maven`'s `~/.m2/{repository,wrapper/dists}`.
/// Only dirs that exist on this host are mounted.
pub fn host_cache_mounts(addons: &[String]) -> Vec<String> {
    host_cache_mounts_in(addons, home_dir().as_deref())
}

fn host_cache_mounts_in(addons: &[String], home: Option<&Path>) -> Vec<String> {
    let Some(home) = home else {
        return Vec::new();
    };
    HOST_JVM_KITS
        .iter()
        .filter(|kit| addons.iter().any(|a| a == *kit))
        .flat_map(|kit| host_jvm_dirs(kit).iter())
        .map(|dir| {
            dir.split('/')
                .fold(home.to_path_buf(), |p, part| p.join(part))
        })
        .filter(|p| p.is_dir())
        .map(|p| format!("{}:ro", p.display()))
        .collect()
}

/// What a create would mount for one host-cache addon now: the create form greys a kit out by
/// it. Read through the same functions as [`kit_mounts`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HostDetect {
    /// `<path>:ro`, as [`kit_mounts`] would pass them.
    pub mounts: Vec<String>,
    /// Why a folder that exists was refused, without the `<kit>: ` prefix.
    pub notes: Vec<String>,
    /// The folders looked for, as `~/<dir>`.
    pub looked_for: Vec<String>,
}

impl HostDetect {
    /// Would ticking it mount anything?
    pub fn detected(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// The folders found, `~`-shortened and without the `:ro` suffix.
    pub fn found(&self) -> Vec<String> {
        self.mounts
            .iter()
            .map(|m| tilde(Path::new(m.strip_suffix(":ro").unwrap_or(m))))
            .collect()
    }

    /// One line for the form: the folders found, or why nothing would be mounted.
    pub fn reason(&self, kit: &str) -> String {
        if self.detected() {
            return format!("found {}", self.found().join(", "));
        }
        let why = match self.notes.is_empty() {
            true if self.looked_for.is_empty() => "no folders known on this host".to_string(),
            true => format!("no {}", self.looked_for.join(" or ")),
            false => self.notes.join("; "),
        };
        match crate::skill_source::detection_note(kit) {
            Some(more) => format!("{why} — {more}"),
            None => why,
        }
    }
}

/// Whether one host-cache addon would mount anything from the home folder `home`; `None` for an
/// addon that is not a host-cache kit. Nothing is remembered: the create form asks again every
/// time it opens.
pub fn host_cache_detection_in(kit: &str, home: Option<&Path>) -> Option<HostDetect> {
    host_cache_detection_on(kit, home, std::env::consts::OS)
}

/// Does addon `kit` mount host folders read-only — a package cache or a skill source? The forms
/// list these first, under *Host caches*: what they offer depends on this host, not on the kit.
pub fn is_host_folder_kit(kit: &str) -> bool {
    HOST_JVM_KITS.contains(&kit)
        || HOST_FOLDER_KITS.contains(&kit)
        || crate::skill_source::needs_mounts(kit)
}

fn host_cache_detection_on(kit: &str, home: Option<&Path>, os: &str) -> Option<HostDetect> {
    if !is_host_folder_kit(kit) {
        return None;
    }
    let addons = [kit.to_string()];
    if HOST_JVM_KITS.contains(&kit) {
        return Some(HostDetect {
            mounts: host_cache_mounts_in(&addons, home),
            notes: Vec::new(),
            looked_for: host_jvm_dirs(kit)
                .iter()
                .map(|d| format!("~/{d}"))
                .collect(),
        });
    }
    let found = package_cache_mounts_on(&addons, home, os);
    let prefix = format!("{kit}: ");
    Some(HostDetect {
        mounts: found.mounts,
        notes: found
            .notes
            .iter()
            .map(|n| n.strip_prefix(&prefix).unwrap_or(n).to_string())
            .collect(),
        looked_for: host_folder_dirs(kit, os)
            .or_else(|| crate::skill_source::host_dirs(kit, os))
            .unwrap_or_default()
            .iter()
            .map(|d| format!("~/{d}"))
            .collect(),
    })
}

/// The read-only extra workspaces a sandbox's addons need, and why a folder one of them wanted was
/// left out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KitMounts {
    /// `<path>:ro`, as `sbx create` takes an extra workspace.
    pub mounts: Vec<String>,
    /// One line per folder that exists but was refused, naming it and the reason.
    pub notes: Vec<String>,
}

/// Every read-only mount the chosen addons need. Headless `up`, the dashboard's create and its
/// rebuild all take their mounts from here.
pub fn kit_mounts(addons: &[String]) -> KitMounts {
    let packages = package_cache_mounts(addons, home_dir().as_deref());
    let mut mounts = host_cache_mounts(addons);
    mounts.extend(packages.mounts);
    KitMounts {
        mounts,
        notes: packages.notes,
    }
}

/// The cache addons whose startup hook overlays a package cache below home, mounted read-only at
/// its own path. The skill sources' addons are in [`crate::skill_source`].
pub const HOST_FOLDER_KITS: [&str; 2] = ["host-npm", "host-pip"];

/// The folders below home a cache kit mounts — the tools' own default cache paths on that host OS
/// (`std::env::consts::OS`); `None` for an OS whose layout nobody has looked at.
fn host_folder_dirs(kit: &str, os: &str) -> Option<&'static [&'static str]> {
    let dirs: &'static [&'static str] = match (kit, os) {
        ("host-npm", "macos" | "linux") => &[".npm/_cacache"],
        ("host-pip", "macos") => &["Library/Caches/pip", ".cache/uv"],
        ("host-pip", "linux") => &[".cache/pip", ".cache/uv"],
        _ => return None,
    };
    Some(dirs)
}

/// The host folders that the `host-npm` / `host-pip` addons and the skill sources mount
/// read-only. A folder mounts only if it really is `<home>/<dir>` and passes [`mount_refusal`]:
/// a missing folder is skipped, any other refusal adds a note. Never an error.
pub fn package_cache_mounts(addons: &[String], home: Option<&Path>) -> KitMounts {
    package_cache_mounts_on(addons, home, std::env::consts::OS)
}

fn package_cache_mounts_on(addons: &[String], home: Option<&Path>, os: &str) -> KitMounts {
    let mut out = KitMounts::default();
    for kit in HOST_FOLDER_KITS {
        if !addons.iter().any(|a| a == kit) {
            continue;
        }
        let found = host_folder_mounts(kit, host_folder_dirs(kit, os), "cache", home, os);
        out.mounts.extend(found.mounts);
        out.notes.extend(found.notes);
    }
    let skills = crate::skill_source::mounts(addons, home, os);
    out.mounts.extend(skills.mounts);
    out.notes.extend(skills.notes);
    out
}

/// One addon's folders below home (`dirs`, `None` for an OS nobody has looked at), each checked by
/// [`package_cache_mount`]; the notes start with `<kit>: ` and call the folders `<what>` folders.
/// The cache kits and the skill sources share it.
pub(crate) fn host_folder_mounts(
    kit: &str,
    dirs: Option<&[&str]>,
    what: &str,
    home: Option<&Path>,
    os: &str,
) -> KitMounts {
    let mut out = KitMounts::default();
    let Some(dirs) = dirs else {
        out.notes.push(format!(
            "{kit}: no host {what} folders are known on {os}, so nothing is mounted"
        ));
        return out;
    };
    let Some(home) = home else {
        out.notes.push(format!(
            "{kit}: no home folder (HOME), so nothing is mounted"
        ));
        return out;
    };
    let real_home = match home.canonicalize() {
        Ok(real) => real,
        Err(e) => {
            out.notes.push(format!(
                "{kit}: not mounting anything: {}: {e}",
                home.display()
            ));
            return out;
        }
    };
    for dir in dirs {
        let candidate = home.join(dir);
        match package_cache_mount(&candidate, &real_home.join(dir), &real_home) {
            Ok(Some(spec)) => out.mounts.push(spec),
            Ok(None) => {}
            Err(why) => out.notes.push(format!(
                "{kit}: not mounting {}: {why}",
                candidate.display()
            )),
        }
    }
    out
}

/// One package-cache folder: `Ok(Some(spec))` to mount, `Ok(None)` when it does not exist, and why
/// not otherwise. `expected` and `home` are resolved paths.
fn package_cache_mount(
    candidate: &Path,
    expected: &Path,
    home: &Path,
) -> Result<Option<String>, String> {
    let real = match candidate.canonicalize() {
        Ok(real) => real,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if real.as_path() != expected {
        return Err(format!(
            "it is reached through a symlink (to {})",
            real.display()
        ));
    }
    if !real.is_dir() {
        return Err("not a directory".into());
    }
    if let Some(why) = mount_refusal(&real, home) {
        return Err(why);
    }
    let text = real.to_str().ok_or("the path is not UTF-8")?;
    if text.contains(':') || text.contains(',') {
        return Err("sbx cannot take a path with ':' or ','".into());
    }
    Ok(Some(format!("{text}:ro")))
}

/// Below the home folder: where keys, tokens and logins are kept.
pub const CREDENTIAL_PLACES: [&str; 11] = [
    ".ssh",
    ".aws",
    ".gnupg",
    ".docker",
    ".kube",
    ".config",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "Library/Keychains",
    ".git-credentials",
];

/// Why a host folder must not become an extra workspace, if it must not: it is or holds the home
/// folder, or it is in or holds a credential place. The caller resolves both paths.
pub fn mount_refusal(real: &Path, home: &Path) -> Option<String> {
    if home.starts_with(real) {
        return Some("that would mount your home folder".into());
    }
    CREDENTIAL_PLACES
        .iter()
        .map(|secret| home.join(secret))
        .find(|place| real.starts_with(place) || place.starts_with(real))
        .map(|place| format!("credentials live there ({})", place.display()))
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Render a path with `~` for the home directory, for compact display.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display());
        }
    }
    path.display().to_string()
}

/// Compare two host paths, resolving symlinks when both sides exist (`/tmp` vs `/private/tmp`
/// on macOS) and falling back to a literal comparison when they do not.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Build a sandbox view model directly, for rendering tests: a v3 sandbox, whose agent is its
/// backend workload's name (sbx's `claude` for one with no backend).
#[cfg(any(test, feature = "test-support"))]
pub fn sample(name: &str, backend: Option<&str>, addons: &[&str]) -> Sandbox {
    Sandbox {
        name: name.to_string(),
        label: name.to_string(),
        agent: backend.unwrap_or("claude").into(),
        running: false,
        workspace: PathBuf::from("/proj"),
        workspace_missing: false,
        start_dir: None,
        ro_mounts: Vec::new(),
        rw_mounts: Vec::new(),
        backend: backend.map(str::to_string),
        addons: addons.iter().map(|a| a.to_string()).collect(),
        foreign_kits: Vec::new(),
        kits_unknown: backend.is_none(),
        other_checkout: None,
        tmpfs_volumes: false,
        // As the shipped kits: only the Claude Code workloads declare volumes.
        history_in_volumes: backend.is_some_and(|b| b.starts_with("claude-")),
        drift: None,
        clone: false,
        last_used: None,
        created: Some(1_700_000_000),
        cpus: None,
        memory: None,
        memory_spec: None,
        unresponsive: false,
        runtime_mounts: Vec::new(),
        host_mounts: None,
        keep_running: false,
        // The real rule, not a copy: a fixture must not claim a store the backend never mounts.
        skills: crate::kits::Catalog::load()
            .ok()
            .and_then(|c| Some(c.backend(backend?)?.shares_skills()))
            .filter(|shares| *shares)
            .map(|_| Skills::Known(sbx::SkillsMode::Readonly)),
        ports: Vec::new(),
        image: "docker/sandbox-templates:claude-docker".into(),
        image_digest: "sha256:0123456789abcdef".into(),
        sessions: 0,
        auth_mode: "oauth · anthropic".into(),
        auth: match backend {
            Some("claude-anthropic") => Some(Auth::Login),
            Some("claude-token") => Some(Auth::Token {
                name: crate::app::DEFAULT_TOKEN_NAME.into(),
                placeholder: sbx::named_token_placeholder(crate::app::DEFAULT_TOKEN_NAME),
                stored: true,
            }),
            _ => None,
        },
        models: crate::models::Overrides::default(),
        // A LiteLLM sandbox is on the connection every test app has (`testkit::acme`).
        litellm: matches!(
            backend,
            Some("claude-litellm" | "opencode-litellm" | "pi-litellm")
        )
        .then(|| "acme".into()),
        network: name.to_string(),
        proxy: "172.17.0.0:3128".into(),
        policy_scope: "global".into(),
        secrets: Vec::new(),
        mcp_gateway: false,
        daemon_version: "v0.51.0".into(),
        created_version: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_mounts_are_named_by_what_they_are_and_checked_on_the_host() {
        let root = std::env::temp_dir().join(format!("aisbx-bind-{}", std::process::id()));
        let (ws, data, store, added) = (
            root.join("ws"),
            root.join("data"),
            root.join("agent-skills"),
            root.join("added"),
        );
        for dir in [&ws, &data, &store, &added] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mount = |host: &Path, target: &str, read_only: bool| sbx::HostMount {
            host_path: host.display().to_string(),
            container_target: target.to_string(),
            read_only,
        };
        let gone = root.join("gone");
        let reported = [
            mount(&ws, &ws.display().to_string(), false),
            mount(&data, "/workspace/data", true),
            mount(&store, "/home/agent/.local/share/aisbx/skills-store", true),
            mount(&added, "/mnt/added", true),
            mount(&gone, &gone.display().to_string(), false),
        ];
        let runtime = [mount(&added, "/mnt/added", true)];
        let mounts = bind_mounts(&reported, &ws, Some(&store), &runtime);
        let kinds: Vec<MountKind> = mounts.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                MountKind::Workspace,
                MountKind::Folder,
                MountKind::Skills,
                MountKind::Added,
                MountKind::Folder
            ]
        );
        assert_eq!(mounts[1].target, "/workspace/data");
        let missing: Vec<bool> = mounts.iter().map(|m| m.missing).collect();
        assert_eq!(missing, [false, false, false, false, true]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_extra_folder_is_not_a_missing_workspace() {
        let ws = std::env::temp_dir().join(format!("aisbx-ws-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let mut ls = sbx::LsEntry {
            name: "box".into(),
            agent: "claude".into(),
            workspaces: vec![ws.display().to_string(), "/gone-extra:ro".into()],
            workspace_missing: true,
            ..Default::default()
        };
        let mut snap = Snapshot {
            sandboxes: vec![Raw {
                ls: ls.clone(),
                inspect: None,
            }],
            kit_hashes: BTreeMap::new(),
            secrets: None,
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            approvals: None,
            policy_needs_init: None,
            fetchers: Vec::new(),
        };
        let catalog = Catalog::load().unwrap();
        let sb = resolve(&snap, &catalog, &mut Registry::empty()).remove(0);
        assert!(
            !sb.workspace_missing,
            "sbx 0.51 sets the flag for an extra folder; the workspace is here"
        );
        assert_eq!(sb.host_mounts, None, "no inspect, no report");
        let _ = std::fs::remove_dir_all(&ws);

        ls.workspaces[0] = "/no/such/aisbx-workspace".into();
        snap.sandboxes[0].ls = ls;
        let sb = resolve(&snap, &catalog, &mut Registry::empty()).remove(0);
        assert!(sb.workspace_missing, "a gone workspace still says so");
    }

    #[test]
    fn the_location_names_the_release_that_created_the_sandbox_when_sbx_does() {
        let mut sb = sample("s", Some("claude-litellm"), &[]);
        assert_eq!(sb.location(), "local  ·  sandboxd v0.51.0");
        sb.created_version = "v0.51.0".into();
        assert_eq!(
            sb.location(),
            "local  ·  sandboxd v0.51.0  ·  created by sbx v0.51.0"
        );
        sb.daemon_version.clear();
        assert_eq!(sb.location(), "local  ·  created by sbx v0.51.0");
    }

    /// The badges and the ids name the same kits in one order, a kit of no catalog marked `?`
    /// in both; an id is one word, where a label may be two.
    #[test]
    fn kits_are_named_by_label_and_by_id_in_one_order() {
        let mut sb = sample("s", Some("claude-anthropic"), &["host-gradle", "jdk25"]);
        sb.foreign_kits = vec!["mcp-postgres".into()];
        let named = |kits: Vec<(String, String)>| kits.into_iter().map(|(_, n)| n).collect();
        let badges: Vec<String> = named(sb.kit_badges());
        let ids: Vec<String> = named(sb.kit_ids());
        assert_eq!(badges, ["Host Gradle", "JDK25", "mcp-postgres?"]);
        assert_eq!(ids, ["host-gradle", "jdk25", "mcp-postgres?"]);
        let kits = |kits: Vec<(String, String)>| kits.into_iter().map(|(k, _)| k).collect();
        let (by_badge, by_id): (Vec<String>, Vec<String>) =
            (kits(sb.kit_badges()), kits(sb.kit_ids()));
        assert_eq!(by_badge, by_id);
    }

    #[test]
    fn each_at_once_keeps_the_order_and_the_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let items: Vec<usize> = (0..20).collect();
        let doubled = each_at_once(&items, 3, |&i| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(30 - (i % 3) as u64 * 10));
            running.fetch_sub(1, Ordering::SeqCst);
            i * 2
        });
        assert_eq!(doubled, (0..20).map(|i| i * 2).collect::<Vec<_>>());
        assert_eq!(
            most.load(Ordering::SeqCst),
            3,
            "three at a time, not one, not more"
        );
    }

    #[test]
    fn sanitizes_to_the_charset_sbx_accepts() {
        assert_eq!(sanitize("my project+1"), "my-project-1");
        assert_eq!(sanitize("aisbx"), "aisbx");
        assert_eq!(sanitize("---"), "workspace");
        // An underscore is not in sbx's charset: sbx refuses it.
        assert_eq!(sanitize("my_project"), "my-project");
    }

    #[test]
    fn derived_names_obey_the_043_length_and_ending_rules() {
        // A folder whose name ends in a period — `v2.` — would otherwise end the sandbox name in
        // one, which sbx refuses.
        let folder = PathBuf::from("/home/u/v2.");
        assert_eq!(
            unique_sandbox_name("claude-anthropic", &folder, &[]),
            "claude-anthropic-v2"
        );

        // A long folder, trimmed to fit — with the backend prefix kept whole, since that is what
        // tells two sandboxes on one folder apart.
        let long = PathBuf::from(format!("/home/u/{}", "a".repeat(120)));
        let name = unique_sandbox_name("claude-litellm", &long, &[]);
        assert!(name.len() <= 63, "{name} is {} chars", name.len());
        assert!(name.starts_with("claude-litellm-"), "{name}");

        // …and still fits once a repeat appends its suffix.
        let taken = vec![name.clone()];
        let second = unique_sandbox_name("claude-litellm", &long, &taken);
        assert!(second.len() <= 63, "{second} is {} chars", second.len());
        assert!(second.ends_with("-2"), "{second}");

        for n in [&name, &second] {
            assert!(
                n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
                "{n} must start alphanumeric"
            );
            assert!(
                n.chars().last().is_some_and(|c| c.is_ascii_alphanumeric()),
                "{n} must end alphanumeric"
            );
            assert!(
                n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'),
                "{n} is outside sbx's charset"
            );
        }
    }

    #[test]
    fn names_a_sandbox_after_backend_and_folder_then_suffixes() {
        let folder = PathBuf::from("/home/u/aisbx");
        assert_eq!(
            unique_sandbox_name("claude-anthropic", &folder, &[]),
            "claude-anthropic-aisbx"
        );
        let taken = vec!["claude-anthropic-aisbx".to_string()];
        assert_eq!(
            unique_sandbox_name("claude-anthropic", &folder, &taken),
            "claude-anthropic-aisbx-2"
        );
        let taken = vec![
            "claude-anthropic-aisbx".to_string(),
            "claude-anthropic-aisbx-2".to_string(),
        ];
        assert_eq!(
            unique_sandbox_name("claude-anthropic", &folder, &taken),
            "claude-anthropic-aisbx-3"
        );
    }

    #[test]
    fn splits_primary_workspace_from_ro_mounts() {
        let (ws, ro, rw) = split_workspaces(&[
            "/proj".to_string(),
            "/home/u/.m2/repository:ro".to_string(),
            "/home/u/.cache/ci-deps/x".to_string(),
            "/srv/data:/workspace/data:ro".to_string(),
            "/srv/libs:/workspace/libs".to_string(),
        ]);
        assert_eq!(ws, PathBuf::from("/proj"));
        assert_eq!(
            ro,
            [PathBuf::from("/home/u/.m2/repository"), "/srv/data".into()],
            "a folder at a target of its own is still its host folder"
        );
        assert_eq!(
            rw,
            [
                PathBuf::from("/home/u/.cache/ci-deps/x"),
                "/srv/libs".into()
            ]
        );
    }

    /// A throwaway HOME for the package-cache tests (never the real one: `HOME` is process-wide
    /// and the tests run in parallel), and the folders to create in it.
    #[cfg(unix)]
    fn temp_home(name: &str, dirs: &[&str]) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("aisbx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        for dir in dirs {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        (base, home)
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The tools' default cache folders, read-only, on both host layouts; what is not there is
    /// skipped without a word, and an OS nobody looked at mounts nothing and says so.
    #[test]
    #[cfg(unix)]
    fn package_caches_mount_the_default_folders_read_only_and_skip_missing_ones() {
        let dirs = [".npm/_cacache", "Library/Caches/pip", ".cache/uv"];
        let (base, home) = temp_home("pkg-mounts", &dirs);
        let real = home.canonicalize().unwrap();
        let ro = |dir: &str| format!("{}:ro", real.join(dir).display());
        let both = strings(&["host-npm", "host-pip"]);

        let mac = package_cache_mounts_on(&both, Some(home.as_path()), "macos");
        assert_eq!(
            mac.mounts,
            vec![
                ro(".npm/_cacache"),
                ro("Library/Caches/pip"),
                ro(".cache/uv")
            ]
        );
        assert!(mac.notes.is_empty(), "{:?}", mac.notes);
        // No ~/.cache/pip here: skipped silently, the uv cache still mounted.
        let linux = package_cache_mounts_on(&both, Some(home.as_path()), "linux");
        assert_eq!(linux.mounts, vec![ro(".npm/_cacache"), ro(".cache/uv")]);
        assert!(linux.notes.is_empty(), "{:?}", linux.notes);
        // One kit, its own folders only.
        let npm = package_cache_mounts_on(&strings(&["host-npm"]), Some(home.as_path()), "macos");
        assert_eq!(npm.mounts, vec![ro(".npm/_cacache")]);
        // Neither kit: nothing, whatever exists.
        assert_eq!(
            package_cache_mounts_on(
                &strings(&["rtk", "host-gradle", "host-maven"]),
                Some(home.as_path()),
                "macos"
            ),
            KitMounts::default()
        );
        let windows = package_cache_mounts_on(&both, Some(home.as_path()), "windows");
        assert!(windows.mounts.is_empty());
        assert_eq!(
            windows.notes.len(),
            2,
            "one note per kit: {:?}",
            windows.notes
        );
        // An empty home mounts nothing and has nothing to say.
        let (empty_base, empty) = temp_home("pkg-empty", &[]);
        let nothing = package_cache_mounts_on(&both, Some(empty.as_path()), "macos");
        assert_eq!(nothing, KitMounts::default());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&empty_base);
    }

    /// A symlink anywhere below home could point a cache mount at another folder — a credential
    /// place, or anything outside home — so such a folder is skipped with a note naming it, and no
    /// other path is mounted in its place.
    #[test]
    #[cfg(unix)]
    fn package_caches_reached_through_a_symlink_are_skipped_with_a_note() {
        use std::os::unix::fs::symlink;
        let (base, home) = temp_home("pkg-links", &[".config/_cacache"]);
        let outside = base.join("outside");
        std::fs::create_dir_all(outside.join("cache/uv")).unwrap();
        std::fs::create_dir_all(outside.join("cacache")).unwrap();
        let npm = strings(&["host-npm"]);

        // ~/.npm -> ~/.config: the cache would be a folder inside a credential place.
        symlink(home.join(".config"), home.join(".npm")).unwrap();
        let out = package_cache_mounts_on(&npm, Some(home.as_path()), "macos");
        assert!(out.mounts.is_empty(), "{:?}", out.mounts);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        let candidate = home.join(".npm/_cacache").display().to_string();
        assert!(out.notes[0].contains(&candidate), "{:?}", out.notes);
        assert!(out.notes[0].contains("symlink"), "{:?}", out.notes);

        // ~/.npm/_cacache -> a folder outside home.
        std::fs::remove_file(home.join(".npm")).unwrap();
        std::fs::create_dir_all(home.join(".npm")).unwrap();
        symlink(outside.join("cacache"), home.join(".npm/_cacache")).unwrap();
        let out = package_cache_mounts_on(&npm, Some(home.as_path()), "macos");
        assert!(out.mounts.is_empty(), "{:?}", out.mounts);
        assert!(
            out.notes.len() == 1 && out.notes[0].contains(&candidate),
            "{:?}",
            out.notes
        );

        // ~/.cache -> outside: a symlinked ancestor of the uv cache.
        symlink(outside.join("cache"), home.join(".cache")).unwrap();
        let out = package_cache_mounts_on(&strings(&["host-pip"]), Some(home.as_path()), "linux");
        assert!(out.mounts.is_empty(), "{:?}", out.mounts);
        let uv = home.join(".cache/uv").display().to_string();
        assert!(
            out.notes.len() == 1 && out.notes[0].contains(&uv),
            "{:?}",
            out.notes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `host-skills` mounts the two global skill folders read-only, `.agents` first — the order
    /// its hook reads them — on both host layouts, and changes nothing for the cache kits.
    #[test]
    #[cfg(unix)]
    fn host_skills_mounts_both_folders_in_order() {
        let (base, home) = temp_home(
            "skills-mounts",
            &[".agents/skills", ".claude/skills", ".npm/_cacache"],
        );
        let real = home.canonicalize().unwrap();
        let ro = |dir: &str| format!("{}:ro", real.join(dir).display());
        let skills = strings(&["host-skills"]);
        for os in ["macos", "linux"] {
            let out = package_cache_mounts_on(&skills, Some(home.as_path()), os);
            assert_eq!(
                out.mounts,
                vec![ro(".agents/skills"), ro(".claude/skills")],
                "{os}"
            );
            assert!(out.notes.is_empty(), "{os}: {:?}", out.notes);
        }
        // Stacked with a cache kit, each brings its own folders and nothing else.
        let both = package_cache_mounts_on(
            &strings(&["host-skills", "host-npm"]),
            Some(home.as_path()),
            "linux",
        );
        assert_eq!(
            both.mounts,
            vec![
                ro(".npm/_cacache"),
                ro(".agents/skills"),
                ro(".claude/skills")
            ]
        );
        let npm = package_cache_mounts_on(&strings(&["host-npm"]), Some(home.as_path()), "linux");
        assert_eq!(npm.mounts, vec![ro(".npm/_cacache")]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A host that has only one of the two folders, or neither, mounts what is there and says
    /// nothing: no skills is not a problem to report.
    #[test]
    #[cfg(unix)]
    fn host_skills_skips_a_missing_folder_silently() {
        let skills = strings(&["host-skills"]);
        let (base, home) = temp_home("skills-missing", &[".claude/skills"]);
        let real = home.canonicalize().unwrap();
        let out = package_cache_mounts_on(&skills, Some(home.as_path()), "macos");
        assert_eq!(
            out.mounts,
            vec![format!("{}:ro", real.join(".claude/skills").display())]
        );
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let (empty_base, empty) = temp_home("skills-none", &[]);
        assert_eq!(
            package_cache_mounts_on(&skills, Some(empty.as_path()), "linux"),
            KitMounts::default()
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&empty_base);
    }

    /// `~/.claude/skills -> ~/.agents/skills` is a common layout, and its resolved target is
    /// never mounted in its place: the folder is skipped with a note naming it, the other one is
    /// still mounted. A plain file in a folder's place is refused the same way.
    #[test]
    #[cfg(unix)]
    fn host_skills_refuses_a_symlinked_folder_with_a_note() {
        use std::os::unix::fs::symlink;
        let (base, home) = temp_home("skills-links", &[".agents/skills"]);
        let skills = strings(&["host-skills"]);
        let real = home.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        symlink(home.join(".agents/skills"), home.join(".claude/skills")).unwrap();
        let out = package_cache_mounts_on(&skills, Some(home.as_path()), "macos");
        assert_eq!(
            out.mounts,
            vec![format!("{}:ro", real.join(".agents/skills").display())]
        );
        let candidate = home.join(".claude/skills").display().to_string();
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(
            out.notes[0].starts_with(&format!("host-skills: not mounting {candidate}: ")),
            "{:?}",
            out.notes
        );
        assert!(out.notes[0].contains("symlink"), "{:?}", out.notes);

        std::fs::remove_file(home.join(".claude/skills")).unwrap();
        std::fs::write(home.join(".claude/skills"), "not a folder").unwrap();
        let out = package_cache_mounts_on(&skills, Some(home.as_path()), "linux");
        assert_eq!(out.mounts.len(), 1, "{:?}", out.mounts);
        assert!(
            out.notes.len() == 1 && out.notes[0].contains("not a directory"),
            "{:?}",
            out.notes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An OS whose skill folders nobody has looked at mounts nothing and says so, once.
    #[test]
    #[cfg(unix)]
    fn host_skills_on_an_unknown_os_notes_it() {
        let (base, home) = temp_home("skills-os", &[".agents/skills", ".claude/skills"]);
        let skills = strings(&["host-skills"]);
        let out = package_cache_mounts_on(&skills, Some(home.as_path()), "windows");
        assert!(out.mounts.is_empty(), "{:?}", out.mounts);
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert_eq!(
            out.notes[0],
            "host-skills: no host skill folders are known on windows, so nothing is mounted"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The refusal that `aisbx up --mount` and the package-cache kits share.
    #[test]
    fn refuses_home_its_ancestors_and_credential_places() {
        let home = Path::new("/h/u");
        assert!(mount_refusal(home, home).unwrap().contains("home folder"));
        assert!(mount_refusal(Path::new("/h"), home)
            .unwrap()
            .contains("home folder"));
        assert!(mount_refusal(Path::new("/h/u/.ssh"), home)
            .unwrap()
            .contains("credentials"));
        let config = mount_refusal(Path::new("/h/u/.config/gh"), home).unwrap();
        assert!(config.contains("/h/u/.config"), "{config}");
        assert!(mount_refusal(Path::new("/h/u/Library"), home)
            .unwrap()
            .contains("Keychains"));
        assert_eq!(mount_refusal(Path::new("/h/u/.npm/_cacache"), home), None);
        assert_eq!(mount_refusal(Path::new("/h/u/.cache/uv"), home), None);
        assert_eq!(mount_refusal(Path::new("/elsewhere"), home), None);
    }

    /// `host-gradle` and `host-maven` mount the host caches, and no other addon mounts anything.
    #[test]
    fn kit_mounts_come_only_from_the_host_cache_kits() {
        let both = kit_mounts(&strings(&["host-gradle", "host-maven"]));
        assert_eq!(
            both.mounts,
            host_cache_mounts(&strings(&["host-gradle", "host-maven"]))
        );
        assert!(both.notes.is_empty());
        assert_eq!(
            kit_mounts(&strings(&["rtk", "web", "jdk25"])),
            KitMounts::default()
        );
    }

    /// Each JVM kit mounts its own tool's folders, read-only and at their own paths, and only the
    /// ones that exist; with both, Gradle's come first.
    #[cfg(unix)]
    #[test]
    fn the_gradle_and_maven_kits_mount_their_own_folders() {
        let (base, home) = temp_home("jvm-mounts", &[".gradle/caches", ".m2/repository"]);
        let ro = |dir: &str| format!("{}:ro", home.join(dir).display());
        let gradle = host_cache_mounts_in(&strings(&["host-gradle"]), Some(home.as_path()));
        assert_eq!(gradle, vec![ro(".gradle/caches")]);
        let maven = host_cache_mounts_in(&strings(&["host-maven"]), Some(home.as_path()));
        assert_eq!(maven, vec![ro(".m2/repository")]);
        std::fs::create_dir_all(home.join(".gradle/wrapper/dists")).unwrap();
        std::fs::create_dir_all(home.join(".m2/wrapper/dists")).unwrap();
        let both = host_cache_mounts_in(
            &strings(&["host-maven", "host-gradle"]),
            Some(home.as_path()),
        );
        assert_eq!(
            both,
            vec![
                ro(".gradle/caches"),
                ro(".gradle/wrapper/dists"),
                ro(".m2/repository"),
                ro(".m2/wrapper/dists"),
            ]
        );
        assert!(host_cache_mounts_in(&strings(&["host-npm"]), Some(home.as_path())).is_empty());
        assert!(host_cache_mounts_in(&strings(&["host-gradle"]), None).is_empty());
        let (empty_base, empty) = temp_home("jvm-empty", &[]);
        let none = host_cache_mounts_in(
            &strings(&["host-gradle", "host-maven"]),
            Some(empty.as_path()),
        );
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&empty_base);
    }

    /// Detection answers "would a create mount something now" for the five host-cache kits, and
    /// nothing for any other addon.
    #[test]
    #[cfg(unix)]
    fn detection_follows_what_a_create_would_mount() {
        let (base, home) = temp_home("detect", &[".npm/_cacache", ".m2/repository"]);
        let detect = |kit: &str| host_cache_detection_on(kit, Some(home.as_path()), "linux");
        for kit in ["host-npm", "host-maven"] {
            let d = detect(kit).unwrap();
            assert!(d.detected(), "{kit}: {d:?}");
            let reason = d.reason(kit);
            assert!(reason.starts_with("found "), "{kit}: {reason}");
        }
        let npm = detect("host-npm").unwrap();
        assert_eq!(
            npm.mounts,
            package_cache_mounts_on(&strings(&["host-npm"]), Some(home.as_path()), "linux").mounts
        );
        for kit in ["host-gradle", "host-pip", "host-skills"] {
            let d = detect(kit).unwrap();
            assert!(!d.detected(), "{kit}: {d:?}");
            assert!(d.mounts.is_empty(), "{kit}");
        }
        assert_eq!(
            detect("host-gradle").unwrap().reason("host-gradle"),
            "no ~/.gradle/caches or ~/.gradle/wrapper/dists"
        );
        assert_eq!(
            detect("host-pip").unwrap().reason("host-pip"),
            "no ~/.cache/pip or ~/.cache/uv"
        );
        let skills = detect("host-skills").unwrap().reason("host-skills");
        assert!(skills.contains("~/.agents/skills"), "{skills}");
        assert!(skills.contains("~/.claude/skills"), "{skills}");
        assert!(
            skills.contains("skills from the sbx store are still shared"),
            "{skills}"
        );
        for other in ["rtk", "web", "jdk25", "statusline"] {
            assert_eq!(detect(other), None, "{other}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A refused folder is not detected, and the reason is the refusal note `kit_mounts` gives.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_cache_is_not_detected_and_says_why() {
        let (base, home) = temp_home("detect-link", &["elsewhere/cacache"]);
        std::fs::create_dir_all(home.join(".npm")).unwrap();
        let target = home.join("elsewhere/cacache");
        std::os::unix::fs::symlink(target, home.join(".npm/_cacache")).unwrap();
        let d = host_cache_detection_on("host-npm", Some(home.as_path()), "linux").unwrap();
        assert!(!d.detected());
        let reason = d.reason("host-npm");
        assert!(reason.contains("symlink"), "{reason}");
        assert!(!reason.starts_with("host-npm: "), "{reason}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn drift_hash_needs_every_kit_to_be_known() {
        let mut hashes = BTreeMap::new();
        hashes.insert("claude-anthropic".to_string(), "aaa".to_string());
        assert!(kit_hashes(&hashes, Some("claude-anthropic"), &[]).is_some());
        // An addon whose hash is missing must not silently produce a different baseline.
        assert!(kit_hashes(&hashes, Some("claude-anthropic"), &["rtk".to_string()]).is_none());
        assert!(kit_hashes(&hashes, None, &[]).is_none());
    }
}

#[cfg(test)]
mod note_tests {
    use super::*;

    fn sandbox() -> Sandbox {
        sample("a", Some("claude-anthropic"), &[])
    }

    /// The four conditions a row can warn about, each switched on alone.
    fn unresponsive(sb: &mut Sandbox) {
        sb.unresponsive = true;
    }
    fn no_token(sb: &mut Sandbox) {
        sb.auth = Some(Auth::Token {
            name: "private".into(),
            placeholder: sbx::named_token_placeholder("private"),
            stored: false,
        });
    }
    fn drifted(sb: &mut Sandbox) {
        sb.drift = Some(crate::model::Drift::Changed(vec!["Statusline".into()]));
    }

    #[test]
    fn a_sandbox_with_nothing_wrong_has_no_note_and_no_marker() {
        let sb = sandbox();
        assert!(!sb.token_missing());
        assert_eq!(sb.card_note(0), None);
        assert!(sb.row_markers(0).is_empty());
    }

    #[test]
    fn each_condition_alone_gives_its_own_note() {
        let mut sb = sandbox();
        unresponsive(&mut sb);
        assert_eq!(sb.card_note(0), Some(CardNote::Unresponsive));

        let mut sb = sandbox();
        no_token(&mut sb);
        assert!(sb.token_missing());
        assert_eq!(sb.card_note(0), Some(CardNote::NoToken));

        let sb = sandbox();
        assert_eq!(sb.card_note(3), Some(CardNote::Approvals(3)));
        assert_eq!(sb.card_note(1), Some(CardNote::Approvals(1)));

        let mut sb = sandbox();
        drifted(&mut sb);
        assert_eq!(
            sb.card_note(0),
            Some(CardNote::KitsChanged(DriftKind::Changed))
        );
    }

    /// Every pair of conditions: the one higher in the rank wins.
    #[test]
    fn the_higher_ranked_note_wins_every_pair() {
        // (switch, waiting, the note it alone gives), highest rank first.
        type Switch = fn(&mut Sandbox);
        let none: Switch = |_| {};
        let ranked: [(Switch, usize, CardNote); 4] = [
            (unresponsive, 0, CardNote::Unresponsive),
            (no_token, 0, CardNote::NoToken),
            (none, 2, CardNote::Approvals(2)),
            (drifted, 0, CardNote::KitsChanged(DriftKind::Changed)),
        ];
        for (hi, (hi_switch, hi_wait, hi_note)) in ranked.iter().enumerate() {
            for (lo_switch, lo_wait, _) in &ranked[hi + 1..] {
                let mut sb = sandbox();
                hi_switch(&mut sb);
                lo_switch(&mut sb);
                assert_eq!(sb.card_note(hi_wait + lo_wait), Some(*hi_note));
            }
        }
    }

    #[test]
    fn only_a_sandbox_that_cannot_run_is_a_danger() {
        assert!(CardNote::Unresponsive.is_danger());
        assert!(CardNote::NoToken.is_danger());
        assert!(!CardNote::Approvals(1).is_danger());
        assert!(!CardNote::KitsChanged(DriftKind::Changed).is_danger());
    }

    /// A missing workspace folder has no note of its own: the list says it once, on the folder's
    /// heading (`roster::FolderHead::missing`), and the row adds no marker for it.
    #[test]
    fn a_missing_workspace_has_no_note_and_no_marker() {
        let mut sb = sandbox();
        sb.workspace_missing = true;
        assert_eq!(sb.card_note(0), None);
        assert!(sb.row_markers(0).is_empty());
    }

    #[test]
    fn a_running_sandbox_lists_background_sessions_and_clone() {
        let mut sb = sandbox();
        sb.running = true;
        // A running row never says how long ago it was used: it would only ever say "now".
        sb.last_used = Some(crate::state::now_epoch());
        assert_eq!(sb.status_parts(), ["running"]);

        sb.sessions = 1;
        assert_eq!(sb.status_parts(), ["running", "1 session"]);
        sb.sessions = 2;
        sb.keep_running = true;
        sb.clone = true;
        assert_eq!(
            sb.status_parts(),
            ["running", "background", "2 sessions", "clone"]
        );
    }

    #[test]
    fn a_stopped_sandbox_says_how_long_ago_only_when_known() {
        let mut sb = sandbox();
        assert_eq!(sb.status_parts(), ["stopped"]);

        sb.last_used = Some(crate::state::now_epoch().saturating_sub(9 * 86_400));
        sb.clone = true;
        assert_eq!(sb.status_parts(), ["stopped", "9 days ago", "clone"]);
    }

    #[test]
    fn kit_hues_keep_the_old_mapping() {
        assert_eq!(kit_hue("web"), Hue::Red);
        assert_eq!(kit_hue("rtk"), Hue::Green);
        assert_eq!(kit_hue("jdk25"), Hue::Yellow);
        assert_eq!(kit_hue("jdk"), Hue::Yellow);
        for other in [
            "statusline",
            "headroom",
            "ponytail",
            "remote-dev",
            "pkg-cache",
            "?",
        ] {
            assert_eq!(kit_hue(other), Hue::Magenta, "{other}");
        }
    }

    /// The host-folder kits read as one family on a row: every one has `host-gradle`'s.
    #[test]
    fn the_host_folder_kits_are_one_hue() {
        assert_eq!(kit_hue("host-gradle"), Hue::Blue);
        for name in ["host-maven", "host-npm", "host-pip", "host-skills"] {
            assert_eq!(kit_hue(name), kit_hue("host-gradle"), "{name}");
        }
    }

    #[test]
    fn a_sandbox_on_a_folder_above_belongs_where_its_agent_opens() {
        let mut sb = sample("s", Some("claude-anthropic"), &[]);
        sb.workspace = PathBuf::from("/repo");
        assert!(sb.belongs_here(Path::new("/repo")));
        assert!(!sb.belongs_here(Path::new("/repo/api")));
        sb.start_dir = Some(PathBuf::from("api"));
        assert!(sb.belongs_here(Path::new("/repo/api")));
        assert!(!sb.belongs_here(Path::new("/repo/web")));
        assert_eq!(
            sb.folder_relation(Path::new("/repo/api")),
            FolderRelation::Here
        );
        sb.workspace = PathBuf::new();
        assert!(
            !sb.belongs_here(Path::new("/repo/api")),
            "a mountless sandbox is nowhere"
        );
    }

    #[test]
    fn agent_hues_follow_the_cli() {
        for claude in [
            "claude",
            "claude-litellm",
            "claude-anthropic",
            "claude-token",
        ] {
            assert_eq!(agent_hue(claude), Hue::Magenta, "{claude}");
        }
        assert_eq!(agent_hue("opencode"), Hue::Cyan);
        assert_eq!(agent_hue("opencode-litellm"), Hue::Cyan);
        assert_eq!(agent_hue("pi-litellm"), Hue::LightGreen);
        assert_eq!(agent_hue("pi"), Hue::LightGreen, "the wizard's harness");
        for other in ["codex", "codex-openai", "agy", "shell"] {
            assert_eq!(agent_hue(other), Hue::Blue, "{other}");
        }
    }
}

/// The details pane's decisions, which both frontends' Overview reads.
#[cfg(test)]
mod overview_tests {
    use super::*;

    fn sandbox() -> Sandbox {
        sample("a", Some("claude-anthropic"), &[])
    }

    #[test]
    fn the_folder_relation_is_here_elsewhere_or_mountless() {
        let mut sb = sandbox();
        let relation = |sb: &Sandbox, cwd: &str| sb.folder_relation(Path::new(cwd));
        assert_eq!(relation(&sb, "/proj"), FolderRelation::Here);
        assert_eq!(relation(&sb, "/elsewhere"), FolderRelation::Elsewhere);
        sb.workspace = PathBuf::new();
        // Mountless wins, whatever the folder on screen is.
        assert_eq!(relation(&sb, ""), FolderRelation::Mountless);
        assert_eq!(relation(&sb, "/proj"), FolderRelation::Mountless);
    }

    #[test]
    fn the_lifecycle_is_said_for_detached_mode_first_then_for_ports() {
        let mut sb = sandbox();
        assert_eq!(sb.lifecycle(), None);
        sb.ports.push(sbx::Port::default());
        assert_eq!(sb.lifecycle(), Some(Lifecycle::StopsWithPorts));
        sb.keep_running = true;
        assert_eq!(sb.lifecycle(), Some(Lifecycle::KeepsRunning));
        sb.ports.clear();
        assert_eq!(sb.lifecycle(), Some(Lifecycle::KeepsRunning));
    }

    #[test]
    fn the_digest_is_cut_to_twelve_characters_after_the_colon() {
        let mut sb = sandbox();
        sb.image_digest = String::new();
        assert_eq!(sb.short_digest(), None);
        sb.image_digest = "no-colon".into();
        assert_eq!(sb.short_digest(), None);
        sb.image_digest = "sha256:abc".into();
        assert_eq!(sb.short_digest(), Some("abc"));
        sb.image_digest = "sha256:0123456789abcdef".into();
        assert_eq!(sb.short_digest(), Some("0123456789ab"));
    }

    #[test]
    fn a_secret_without_a_variable_is_a_placeholder() {
        let mut sb = sandbox();
        assert!(sb.injected_secrets().is_empty());
        sb.secrets = vec![
            (String::new(), "global".into()),
            ("-".into(), "custom".into()),
            ("GH_TOKEN".into(), "github".into()),
        ];
        assert_eq!(
            sb.injected_secrets(),
            [
                (None, "global"),
                (None, "custom"),
                (Some("GH_TOKEN"), "github")
            ]
        );
    }

    #[test]
    fn the_folder_key_is_the_shown_path_case_folded() {
        assert_eq!(folder_key(Path::new("/Work/API")), "/work/api");
        assert!(folder_key(Path::new("/a/B")) < folder_key(Path::new("/a/c")));
    }
}

#[cfg(test)]
mod checkout_tests {
    use super::*;
    use crate::kits::Catalog;
    use crate::sbx::CustomSecret;
    use crate::state::Registry;

    /// One sandbox built from these kits, the way a v3 one reports itself: its agent is the first
    /// kit's folder name (the workload's).
    fn snapshot(kit_paths: &[&str]) -> Snapshot {
        let agent = kit_paths
            .first()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "claude".into());
        Snapshot {
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            approvals: None,
            policy_needs_init: None,
            fetchers: Vec::new(),
            sandboxes: vec![Raw {
                ls: LsEntry {
                    name: "s".into(),
                    agent,
                    status: "stopped".into(),
                    workspaces: vec!["/proj".into()],
                    ..Default::default()
                },
                inspect: Some(InspectEntry {
                    kits: kit_paths.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                }),
            }],
            kit_hashes: BTreeMap::new(),
            secrets: None,
        }
    }

    #[test]
    fn only_the_kits_that_changed_are_drift_and_by_their_name() {
        let mut catalog = Catalog::load().unwrap();
        let rtk_at = catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "rtk")
            .unwrap();
        catalog.addons[rtk_at].kit.changes = vec!["Older.".into()];
        // The shipped lists of the backend and of jdk25 stay out of it: they grow.
        let backend_at = (catalog.backends.iter())
            .position(|b| b.kit.name == "claude-anthropic")
            .unwrap();
        catalog.backends[backend_at].kit.changes = Vec::new();
        let jdk_at = (catalog.addons.iter())
            .position(|a| a.kit.name == "jdk25")
            .unwrap();
        catalog.addons[jdk_at].kit.changes = Vec::new();
        let rtk_name = catalog.addons[rtk_at].kit.display_name.clone();
        let here = catalog.kits_dir.clone();
        let mut registry = Registry::empty();
        let backend = here.join("claude-anthropic").display().to_string();
        let rtk = here.join("rtk").display().to_string();
        let jdk = here.join("jdk25").display().to_string();
        let mut snap = snapshot(&[&backend, &rtk]);
        let hashes = |snap: &mut Snapshot, rtk_hash: &str| {
            snap.kit_hashes = [
                ("claude-anthropic", "b1"),
                ("rtk", rtk_hash),
                ("jdk25", "j1"),
            ]
            .into_iter()
            .map(|(k, h)| (k.to_string(), h.to_string()))
            .collect();
        };
        let drift = |snap: &Snapshot, catalog: &Catalog, registry: &mut Registry| {
            resolve(snap, catalog, registry)[0].drift.clone()
        };
        let update = |changes: &[&str]| {
            Some(Drift::Changed(vec![KitUpdate {
                name: rtk_name.clone(),
                changes: changes.iter().map(|c| c.to_string()).collect(),
            }]))
        };

        // A baseline of another shape is replaced, never compared.
        registry.set_kit_hash("s", "claude-anthropic=old,rtk=old".into());
        hashes(&mut snap, "r1");
        assert_eq!(drift(&snap, &catalog, &mut registry), None);
        assert_eq!(
            registry.kit_hash("s"),
            Some("v2;claude-anthropic=b1+0,rtk=r1+1")
        );
        // So is one without the change counts, even where a hash differs.
        registry.set_kit_hash("s", "v2;claude-anthropic=b1,rtk=old".into());
        assert_eq!(drift(&snap, &catalog, &mut registry), None);
        assert_eq!(
            registry.kit_hash("s"),
            Some("v2;claude-anthropic=b1+0,rtk=r1+1")
        );

        // A new line in changes.txt alone is no update.
        catalog.addons[rtk_at].kit.changes =
            vec!["Newest.".into(), "Newer.".into(), "Older.".into()];
        assert_eq!(drift(&snap, &catalog, &mut registry), None);

        // One kit changes: that kit is named with the lines added since the build, and it stays
        // named until a rebuild.
        hashes(&mut snap, "r2");
        assert_eq!(
            drift(&snap, &catalog, &mut registry),
            update(&["Newest.", "Newer."])
        );
        assert_eq!(
            drift(&snap, &catalog, &mut registry),
            update(&["Newest.", "Newer."])
        );

        // A kit added later is current; one removed is dropped from the baseline.
        hashes(&mut snap, "r1");
        snap.sandboxes[0].inspect.as_mut().unwrap().kits = vec![backend.clone(), jdk.clone()];
        assert_eq!(drift(&snap, &catalog, &mut registry), None);
        assert_eq!(
            registry.kit_hash("s"),
            Some("v2;claude-anthropic=b1+0,jdk25=j1+0")
        );
        registry.forget("s");
    }

    #[test]
    fn the_reason_names_the_kits_in_plain_words() {
        let changed = |kits: &[&str]| {
            Drift::Changed(kits.iter().map(|k| KitUpdate::from(*k)).collect()).reason()
        };
        assert_eq!(
            changed(&["Statusline"]),
            "There is a newer version of Statusline. The sandbox keeps the old version until it \
             is rebuilt."
        );
        assert!(changed(&["A", "B"]).contains(" of A and B."));
        assert!(changed(&["A", "B", "C"]).contains(" of A, B and C."));
        let reason = Drift::Changed(vec!["RTK".into()]).reason();
        assert!(
            reason.contains("rebuil") || reason.contains("Rebuil"),
            "{reason}"
        );
        assert!(
            !reason.contains("disk") && !reason.contains("hash"),
            "{reason}"
        );
    }

    #[test]
    fn the_reason_quotes_what_is_new_in_each_kit() {
        let kit = |name: &str, changes: &[&str]| KitUpdate {
            name: name.into(),
            changes: changes.iter().map(|c| c.to_string()).collect(),
        };
        let reason = Drift::Changed(vec![
            kit("RTK", &["Filters git log.", "Faster."]),
            kit("Statusline", &[]),
            kit("Web", &["One.", "Two.", "Three.", "Four.", "Five."]),
        ])
        .reason();
        assert_eq!(
            reason,
            "There is a newer version of RTK, Statusline and Web. New in RTK: Filters git log. \
             Faster. New in Web: One. Two. Three. And 2 more changes. The sandbox keeps the old \
             version until it is rebuilt."
        );
        let one_more = Drift::Changed(vec![kit("Web", &["1.", "2.", "3.", "4."])]).reason();
        assert!(one_more.contains(" 3. And 1 more change. "), "{one_more}");
    }

    #[test]
    fn the_creation_time_and_limits_come_straight_from_sbx() {
        let catalog = Catalog::load().unwrap();
        let mut registry = Registry::empty();
        registry.forget("s");
        let mut snap = snapshot(&[]);
        snap.sandboxes[0].ls.created_at = Some("2026-07-24T16:02:07Z".into());
        snap.sandboxes[0].ls.cpus = Some(2);
        snap.sandboxes[0].ls.memory = Some("4g".into());
        snap.sandboxes[0].ls.memory_mib = Some(4096);
        snap.sandboxes[0].ls.guest_unresponsive = true;
        let sb = &resolve(&snap, &catalog, &mut registry)[0];
        assert_eq!(
            sb.created,
            crate::state::parse_rfc3339("2026-07-24T16:02:07Z")
        );
        assert_eq!((sb.cpus, sb.memory.as_deref()), (Some(2), Some("4g")));
        assert_eq!(sb.memory_spec.as_deref(), Some("4096m"));
        assert!(sb.unresponsive);
        // Nothing is written to the registry for it.
        assert!(registry.get("s").is_none_or(|e| e.display_name.is_none()));

        // No `created_at` (an sbx that predates it) is no creation time, not a guessed one.
        let plain = snapshot(&[]);
        assert_eq!(resolve(&plain, &catalog, &mut registry)[0].created, None);
        registry.forget("s");
    }

    #[test]
    fn kits_from_a_moved_checkout_still_resolve_but_are_flagged() {
        let catalog = Catalog::load().unwrap();
        let here = catalog.kits_dir.clone();
        let mut registry = Registry::empty();

        // Same checkout → managed, nothing flagged.
        let paths = [
            here.join("claude-anthropic").display().to_string(),
            here.join("rtk").display().to_string(),
        ];
        let sb = &resolve(
            &snapshot(&paths.iter().map(String::as_str).collect::<Vec<_>>()),
            &catalog,
            &mut registry,
        )[0];
        assert_eq!(sb.backend.as_deref(), Some("claude-anthropic"));
        assert_eq!(sb.addons, vec!["rtk".to_string()]);
        assert!(sb.other_checkout.is_none());

        // A different checkout → still managed (matched by name), but flagged.
        let sb = &resolve(
            &snapshot(&[
                "/elsewhere/aisbx/kits/claude-anthropic",
                "/elsewhere/aisbx/kits/rtk",
            ]),
            &catalog,
            &mut registry,
        )[0];
        assert_eq!(sb.backend.as_deref(), Some("claude-anthropic"));
        assert_eq!(sb.addons, vec!["rtk".to_string()]);
        assert_eq!(
            sb.other_checkout.as_deref(),
            Some(Path::new("/elsewhere/aisbx/kits"))
        );

        // A kit this repo does not own stays foreign.
        let sb = &resolve(
            &snapshot(&["/somewhere/mcp-postgres"]),
            &catalog,
            &mut registry,
        )[0];
        assert!(sb.backend.is_none());
        assert_eq!(sb.foreign_kits, vec!["mcp-postgres".to_string()]);

        registry.forget("s");
    }

    /// `aisbx up --tmpfs-volumes` builds from aisbx's copies of the kits ([`crate::tmpfs`]): such a
    /// sandbox is on the same backend and addons, from no other checkout, with its volumes in RAM.
    #[test]
    fn a_sandbox_built_from_the_ram_copies_is_on_this_checkouts_kits() {
        let catalog = Catalog::load().unwrap();
        let (here, ram) = (catalog.kits_dir.clone(), crate::tmpfs::kits_dir());
        let mut registry = Registry::empty();
        let paths = [
            ram.join("claude-anthropic").display().to_string(),
            here.join("rtk").display().to_string(),
            ram.join("host-npm").display().to_string(),
        ];
        let sb = &resolve(
            &snapshot(&paths.iter().map(String::as_str).collect::<Vec<_>>()),
            &catalog,
            &mut registry,
        )[0];
        assert_eq!(sb.backend.as_deref(), Some("claude-anthropic"));
        assert_eq!(sb.addons, vec!["host-npm".to_string(), "rtk".to_string()]); // catalog order
        assert!(sb.foreign_kits.is_empty() && sb.other_checkout.is_none());
        assert!(sb.tmpfs_volumes);
        // The same kits from the checkout itself: on disk.
        let paths = [here.join("claude-anthropic").display().to_string()];
        let sb = &resolve(
            &snapshot(&paths.iter().map(String::as_str).collect::<Vec<_>>()),
            &catalog,
            &mut registry,
        )[0];
        assert!(!sb.tmpfs_volumes && sb.other_checkout.is_none());
    }

    /// The generated model kit is read off the path `sbx inspect` reports, never out of the
    /// registry, and it does not read as a stranger.
    #[test]
    fn a_sandbox_model_pins_are_read_off_the_kit_it_carries() {
        let catalog = Catalog::load().unwrap();
        let mut registry = Registry::empty();
        let backend = catalog.kits_dir.join("claude-litellm");

        let mut want = crate::models::Overrides::default();
        want.set(crate::models::Slot::Sonnet, Some("claude-sonnet-5".into()));
        let kit = crate::models::ensure_kit("s", &want, &catalog.models)
            .unwrap()
            .expect("a non-empty selection produces a kit");

        let snap = snapshot(&[&backend.display().to_string(), &kit.display().to_string()]);
        let sb = &resolve(&snap, &catalog, &mut registry)[0];
        assert_eq!(sb.models, want);
        assert_eq!(sb.backend.as_deref(), Some("claude-litellm"));
        assert!(sb.foreign_kits.is_empty(), "{:?}", sb.foreign_kits);
        assert!(sb.addons.is_empty(), "{:?}", sb.addons);

        // A sandbox without the kit is simply on Claude Code's defaults.
        let plain = snapshot(&[&backend.display().to_string()]);
        assert!(resolve(&plain, &catalog, &mut registry)[0]
            .models
            .is_empty());

        crate::models::remove_kit("s").unwrap();
        registry.forget("s");
    }

    fn with_secrets(kit: &str, custom: Vec<CustomSecret>) -> Snapshot {
        let path = Catalog::load().unwrap().kits_dir.join(kit);
        let mut snap = snapshot(&[&path.display().to_string()]);
        snap.secrets = Some(Secrets {
            services: Vec::new(),
            custom,
        });
        snap
    }

    /// A token row scoped to one sandbox, which aisbx never writes: not an account.
    fn scoped_secret(scope: &str) -> CustomSecret {
        CustomSecret {
            scope: scope.into(),
            targets: vec![sbx::TOKEN_HOST.into()],
            env: sbx::TOKEN_ENV.into(),
            placeholder: format!("sbx-cs-aisbx-{scope}"),
            masked: "sk-ant***".into(),
        }
    }

    /// A sandbox built on the token backend *and* the generated kit for a named token — the two
    /// kit paths `sbx inspect` would report for it.
    fn on_named_token(token: &str, custom: Vec<CustomSecret>) -> Snapshot {
        let backend = Catalog::load().unwrap().kits_dir.join("claude-token");
        let wiring = crate::tokens::kit_dir(token);
        let mut snap = snapshot(&[
            &backend.display().to_string(),
            &wiring.display().to_string(),
        ]);
        snap.secrets = Some(Secrets {
            services: Vec::new(),
            custom,
        });
        snap
    }

    /// A named token: one global row, and nothing scoped to any sandbox.
    fn named_secret(token: &str) -> CustomSecret {
        CustomSecret {
            scope: sbx::GLOBAL_SCOPE.into(),
            targets: vec![sbx::TOKEN_HOST.into()],
            env: String::new(),
            placeholder: sbx::named_token_placeholder(token),
            masked: "sk-ant***".into(),
        }
    }

    #[test]
    fn the_backend_kit_decides_the_account_and_the_secret_store_only_whether_it_works() {
        let catalog = Catalog::load().unwrap();
        let mut registry = Registry::empty();
        let auth = |snap: &Snapshot, registry: &mut Registry| {
            resolve(snap, &catalog, registry)[0].auth.clone()
        };

        // The /login backend is on the shared account, whatever the secret store says.
        registry.forget("s");
        let login = with_secrets("claude-anthropic", vec![scoped_secret("s")]);
        assert_eq!(auth(&login, &mut registry), Some(Auth::Login));

        // A named token names itself: the sandbox carries the kit that wires it, so this needs
        // nothing remembered — the registry is not even consulted.
        registry.forget("s");
        let shared = on_named_token("private", vec![named_secret("private")]);
        assert_eq!(
            auth(&shared, &mut registry),
            Some(Auth::Token {
                name: "private".into(),
                placeholder: sbx::named_token_placeholder("private"),
                stored: true,
            })
        );

        // Wired to a name with nothing stored behind it: the kit sets a placeholder that resolves
        // to nothing, which is a sandbox that cannot authenticate.
        let dangling = on_named_token("private", Vec::new());
        assert!(auth(&dangling, &mut registry).unwrap().broken());

        // Without a token kit sbx reported no kit list: the account is unknown, never a guess,
        // and a scoped row does not change that.
        let bare = with_secrets("claude-token", vec![scoped_secret("s")]);
        assert_eq!(auth(&bare, &mut registry), None);

        // A gateway backend has no Anthropic account at all.
        registry.forget("s");
        let gateway = with_secrets("claude-litellm", vec![scoped_secret("s")]);
        assert_eq!(auth(&gateway, &mut registry), None);

        // `sbx secret ls` failing is "unknown", never "no token".
        registry.forget("s");
        let mut unknown = with_secrets("claude-token", Vec::new());
        unknown.secrets = None;
        assert_eq!(auth(&unknown, &mut registry), None);

        registry.forget("s");
    }
}

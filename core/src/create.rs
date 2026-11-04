//! The create form's answers, grouped by the step of sandbox creation they belong to, and the
//! step-by-step wizard that shows them: its navigation ([`WizardState`]) and what a key does to a
//! row's answer ([`CreateForm::field_key`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::app::{
    choose_model, deny_host_ok, edit_text, port_spec_ok, split_ports, toggle_model_pick,
    AccountChoice, CreateForm, CreateRow, TextInput,
};
use crate::input::{KeyCode, KeyEvent, KeyModifiers};
use crate::kits::{Account, Backend, Catalog, Provider};
use crate::models::{Overrides, Slot};

/// The steps of creating a sandbox, in the order they are answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStep {
    /// Who serves the model: Anthropic, Google, OpenAI or a LiteLLM gateway.
    Provider,
    /// The LiteLLM connection the sandbox is built on: one of the host's, or a new one made here.
    /// Only a LiteLLM provider has one; the others pass it by.
    Connection,
    /// Which agent runs in the sandbox — with the provider, the backend kit — and, on the
    /// subscription backend, its account.
    Harness,
    /// The models a LiteLLM connection offers the agent, and which it starts on.
    Models,
    /// The addon kits stacked onto the backend.
    Kits,
    /// The label, the start folder and the per-sandbox options.
    Workspace,
    /// The answers read back, and the buttons that create, or don't. No answers of its own.
    Review,
}

impl CreateStep {
    pub const ALL: [Self; 7] = [
        Self::Provider,
        Self::Connection,
        Self::Harness,
        Self::Models,
        Self::Kits,
        Self::Workspace,
        Self::Review,
    ];

    /// 1 to 7, the way a step is counted on screen.
    pub fn number(self) -> usize {
        match self {
            Self::Provider => 1,
            Self::Connection => 2,
            Self::Harness => 3,
            Self::Models => 4,
            Self::Kits => 5,
            Self::Workspace => 6,
            Self::Review => 7,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Connection => "Connection",
            Self::Harness => "Harness",
            Self::Models => "Models",
            Self::Kits => "Kits",
            Self::Workspace => "Workspace and options",
            Self::Review => "Review",
        }
    }

    /// The step after this one, `None` after the last.
    pub fn next(self) -> Option<Self> {
        Self::ALL.get(self.number()).copied()
    }

    /// The step before this one, `None` before the first.
    pub fn prev(self) -> Option<Self> {
        self.number().checked_sub(2).map(|i| Self::ALL[i])
    }
}

/// Steps 1 and 3: which backend kit is built — the provider and the harness together.
#[derive(Debug, Clone)]
pub struct HarnessAnswers {
    pub backend: usize,
}

/// Steps 2 to 4: the connection, the account under the harness, and the model picks.
#[derive(Debug, Clone)]
pub struct ConnectionAnswers {
    /// Which Anthropic account the sandbox will be on.
    pub account: AccountChoice,
    /// What the "setup token" radio picks: the last used token, or a new one when there is none.
    pub(crate) default_token: AccountChoice,
    /// The named tokens on this host when the form was opened, in the order they are offered.
    pub tokens: Vec<String>,
    /// The name for a *new* token. Ignored when an existing one is picked.
    pub token_name: TextInput,
    /// The `claude setup-token` value. Never rendered back, never passed as an argument.
    pub token: TextInput,
    /// Which model each of the agent's slots is on ([`Slot::of`]). Opens on the connection's
    /// saved picks; an unset slot is the agent's own choice.
    pub models: Overrides,
    /// Which slot's model list is open, if any; one at a time.
    pub model_pick: Option<Slot>,
    /// The gateway's models the agent is not offered, by id.
    pub disabled: std::collections::BTreeSet<String>,
    /// The connection and agent whose saved picks `models` and `disabled` were loaded from.
    pub picks_for: Option<(String, crate::litellm::Agent)>,
    /// The answers given for other connections and agents while the form is open, so going
    /// OpenCode → Claude Code → OpenCode keeps what was picked for OpenCode.
    pub picks_kept: Vec<((String, crate::litellm::Agent), crate::model_picks::Picks)>,
    /// The LiteLLM connections on this host when the form was opened, sorted by name — read from
    /// their `connection.json`, the authority for which exist.
    pub connections: Vec<crate::litellm::Connection>,
    /// The connection a LiteLLM backend is built on, pre-selected by [`crate::litellm::preselect`];
    /// `None` is *new connection...*, made from [`ConnectionAnswers::new_conn`] with the sandbox.
    pub litellm: Option<String>,
    /// The remembered connection (`ui.last_litellm`), for the row's *(last used)* mark only.
    pub last_litellm: Option<String>,
    /// What *new connection…* is typed into.
    pub new_conn: NewConnection,
}

/// The fields of *new connection…* (and of the `S` dialog's edit): everything a connection is,
/// plus its key. No models: every sandbox reads them from the gateway, and the agent picks one.
#[derive(Debug, Clone, Default)]
pub struct NewConnection {
    pub name: TextInput,
    pub url: TextInput,
    /// The gateway's key. Never rendered back, never passed as an argument, emptied whenever the
    /// form comes back after a failed attempt to store it.
    pub key: TextInput,
}

impl NewConnection {
    /// The connection these fields describe, or the first reason to refuse it: an invalid field, a
    /// name in `existing`, or an empty key. The one check of every place that makes a connection.
    pub fn check(
        &self,
        existing: &[crate::litellm::Connection],
    ) -> Result<crate::litellm::Connection, String> {
        if let Some(problem) = ConnField::ALL
            .iter()
            .find_map(|f| self.field_problem(*f, existing))
        {
            return Err(problem);
        }
        Self::build(self.name.value.trim(), self.url.value.trim())
    }

    /// What is wrong with one field, as [`NewConnection::check`] says it. The fields are checked
    /// in [`ConnField::ALL`]'s order, so the first problem there is the one a save refuses with.
    pub fn field_problem(
        &self,
        field: ConnField,
        existing: &[crate::litellm::Connection],
    ) -> Option<String> {
        match field {
            ConnField::Name => {
                let name = self.name.value.trim();
                if !crate::litellm::valid_name(name) {
                    return Some("name the connection in a–z, 0–9 and “-” (at most 32)".into());
                }
                existing
                    .iter()
                    .any(|c| c.name == name)
                    .then(|| format!("connection {name} exists — pick it in the list"))
            }
            ConnField::Url => crate::litellm::check_url(self.url.value.trim())
                .err()
                .map(|e| format!("{e}")),
            ConnField::Key => self
                .key
                .value
                .trim()
                .is_empty()
                .then(|| "paste the gateway's key — it is stored in the host keychain".into()),
        }
    }

    /// [`NewConnection::check`] for an edit of `old`: the name is `old`'s and not checked again,
    /// and an empty key means "keep the stored one".
    pub fn check_edit(
        &self,
        old: &crate::litellm::Connection,
    ) -> Result<crate::litellm::Connection, String> {
        Self::build(&old.name, self.url.value.trim())
    }

    /// The connection put together and validated as a whole.
    fn build(name: &str, url: &str) -> Result<crate::litellm::Connection, String> {
        let conn = crate::litellm::Connection {
            name: name.to_string(),
            url: url.to_string(),
        };
        crate::litellm::validate(&conn).map_err(|e| format!("{e}"))?;
        Ok(conn)
    }
}

/// The fields of *new connection…*, in the order a save checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnField {
    Name,
    Url,
    Key,
}

impl ConnField {
    pub const ALL: [Self; 3] = [Self::Name, Self::Url, Self::Key];

    /// The create form's row for the field.
    pub fn row(self) -> CreateRow {
        match self {
            Self::Name => CreateRow::LitellmName,
            Self::Url => CreateRow::LitellmUrl,
            Self::Key => CreateRow::LitellmKey,
        }
    }
}

/// Step 5: the addon kits.
#[derive(Debug, Clone)]
pub struct KitsAnswers {
    pub addons: Vec<bool>,
    /// What each host-cache addon would mount on this host, parallel to `addons` (`None` for other
    /// addons). Worked out when the form opens; a kit that would mount nothing cannot be ticked.
    pub host: Vec<Option<crate::model::HostDetect>>,
}

/// Step 6: the label and the per-sandbox options.
#[derive(Debug, Clone)]
pub struct WorkspaceAnswers {
    pub name: TextInput,
    /// Once the user edits the label, stop regenerating it from the kit selection.
    pub name_touched: bool,
    /// The directory inside the workspace the agent opens in, relative to it; `None` is the
    /// workspace root. `App::submit_create` writes it to the registry once the sandbox exists.
    pub start_dir: Option<PathBuf>,
    /// The folder to mount when it is a folder above the dashboard's, picked with `..` in the
    /// start-folder picker; `None` mounts the dashboard's folder. `start_dir` is relative to it.
    pub root: Option<PathBuf>,
    pub clone: bool,
    /// Mount the workspace's `.git` folder read-only ([`crate::folders::git_guard`]).
    pub git_guard: bool,
    /// The workspace has a `.git` folder, so [`CreateRow::GitGuard`] is offered. Read when the
    /// form opens and when the mounted folder changes.
    pub git_offered: bool,
    /// The host folders the sandbox mounts beside the workspace ([`crate::folders::plan`]), read
    /// when the form opens and when the mounted folder changes.
    pub folders: crate::folders::Plan,
    /// More host folders for this sandbox alone, as typed ([`CreateForm::extra_folders`]).
    /// `App::submit_create` writes them to the registry, so a rebuild mounts them again.
    pub extra_folders: TextInput,
    /// How the host's shared agent-skills store is mounted. Opens on the host's own
    /// `skills.defaultMode`, and is always passed explicitly.
    pub skills: crate::sbx::SkillsMode,
    /// Ports to publish at creation, in sbx's grammar, space- or comma-separated.
    pub ports: TextInput,
    /// Hosts this sandbox alone may not reach, in the same space-or-comma grammar as `ports`.
    pub deny_network: TextInput,
    /// Which of [`WorkspaceAnswers::mcp`] go into the sandbox's fixed MCP set. Parallel to
    /// `mcp`, the way `KitsAnswers::host` is to `addons`.
    pub static_mcp: Vec<bool>,
    /// No MCP server at all, and no tools to find one (`--static-mcp none`). Never on together
    /// with a tick in `static_mcp`.
    pub no_mcp: bool,
    /// The MCP servers registered on this host when the form was opened, in the order offered.
    /// Empty on a host with none, which is when the whole section is left off the form.
    pub mcp: Vec<crate::sbx::McpServer>,
}

impl CreateRow {
    /// The step this row's answer belongs to.
    pub fn step(self) -> CreateStep {
        match self {
            CreateRow::Backend(_) => CreateStep::Provider,
            CreateRow::LitellmConn
            | CreateRow::LitellmName
            | CreateRow::LitellmUrl
            | CreateRow::LitellmKey => CreateStep::Connection,
            CreateRow::AuthOauth
            | CreateRow::AuthToken
            | CreateRow::TokenPick(_)
            | CreateRow::TokenNew
            | CreateRow::TokenName
            | CreateRow::Token => CreateStep::Harness,
            CreateRow::ModelsRefresh
            | CreateRow::Model(_)
            | CreateRow::ModelDefault(_)
            | CreateRow::ModelPick(..)
            | CreateRow::ModelOn(_)
            | CreateRow::ConfigFile(_) => CreateStep::Models,
            CreateRow::Addon(_) => CreateStep::Kits,
            CreateRow::Name
            | CreateRow::StartDir
            | CreateRow::ExtraFolders
            | CreateRow::Clone
            | CreateRow::GitGuard
            | CreateRow::Skills
            | CreateRow::Ports
            | CreateRow::DenyNetwork
            | CreateRow::McpNone
            | CreateRow::Mcp(_) => CreateStep::Workspace,
        }
    }
}

impl CreateForm {
    /// The rows of one step, in the order [`CreateForm::rows`] gives them.
    pub fn rows_of(&self, step: CreateStep, catalog: &Catalog) -> Vec<CreateRow> {
        self.rows(catalog)
            .into_iter()
            .filter(|row| row.step() == step)
            .collect()
    }

    /// Why a create would be refused over the answers `step` owns, if it would be: the first of its
    /// rows' problems ([`CreateForm::field_problem`]), in the order the rows are drawn.
    pub fn step_problem(&self, step: CreateStep, catalog: &Catalog) -> Option<String> {
        self.rows_of(step, catalog)
            .into_iter()
            .find_map(|row| self.field_problem(row, catalog))
    }

    /// What is wrong with one row's answer, if a create would be refused over it. Only a new token
    /// and *new connection...* have fields to check, besides the more folders, the ports and the
    /// blocked hosts.
    pub fn field_problem(&self, row: CreateRow, catalog: &Catalog) -> Option<String> {
        let ws = &self.workspace;
        let new_token = self.use_token(catalog) && self.connection.account == AccountChoice::New;
        let new_conn = self.litellm_backend(catalog) && self.connection.litellm.is_none();
        let name = self.connection.token_name.value.trim();
        let field = ConnField::ALL.into_iter().find(|f| f.row() == row);
        let problem = match row {
            CreateRow::ExtraFolders => return self.extra_folders().1.into_iter().next(),
            CreateRow::Ports => return Self::port_problem(ws.ports.value.trim()),
            CreateRow::DenyNetwork => return Self::deny_problem(ws.deny_network.value.trim()),
            _ if field.is_some() && new_conn => {
                let field = field?;
                return (self.connection.new_conn)
                    .field_problem(field, &self.connection.connections);
            }
            CreateRow::TokenName if new_token && !crate::sbx::token_name_ok(name) => {
                "name the token in lower-case letters, digits and dashes"
            }
            CreateRow::TokenName
                if new_token && self.connection.tokens.iter().any(|t| t == name) =>
            {
                "a token of that name exists — pick it in the list"
            }
            CreateRow::Token if new_token && self.connection.token.value.trim().is_empty() => {
                "paste a setup token, or pick /login — generate one with `claude setup-token`"
            }
            _ => return None,
        };
        Some(problem.to_string())
    }

    /// [`CreateForm::field_problem`] when it is worth showing beside the row: after Next or a
    /// create was refused on its step, or once something is typed and the cursor has moved since.
    pub fn visible_problem(&self, row: CreateRow, catalog: &Catalog) -> Option<String> {
        let problem = self.field_problem(row, catalog)?;
        let typed = self.text(row).is_some_and(|t| !t.value.trim().is_empty());
        let editing = self.wizard.editing == Some(row);
        (self.wizard.refused || (typed && !editing)).then_some(problem)
    }

    /// The text a text row holds; `None` for any other row.
    pub fn text(&self, row: CreateRow) -> Option<&TextInput> {
        match row {
            CreateRow::Name => Some(&self.workspace.name),
            CreateRow::TokenName => Some(&self.connection.token_name),
            CreateRow::Token => Some(&self.connection.token),
            CreateRow::ExtraFolders => Some(&self.workspace.extra_folders),
            CreateRow::Ports => Some(&self.workspace.ports),
            CreateRow::DenyNetwork => Some(&self.workspace.deny_network),
            CreateRow::LitellmName => Some(&self.connection.new_conn.name),
            CreateRow::LitellmUrl => Some(&self.connection.new_conn.url),
            CreateRow::LitellmKey => Some(&self.connection.new_conn.key),
            _ => None,
        }
    }

    /// The values of a row whose answer is one of several, in the order both frontends draw
    /// them: the skills modes, tightest first; the host's connections, then *new connection…*.
    /// Empty for any other row.
    pub fn choices(&self, row: CreateRow) -> Vec<String> {
        match row {
            CreateRow::Skills => SKILLS_MODES
                .iter()
                .map(|m| skills_mode_name(*m).to_string())
                .collect(),
            CreateRow::LitellmConn => (self.connection.connections.iter())
                .map(|c| c.name.clone())
                .chain(std::iter::once(NEW_CONNECTION.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Which of [`CreateForm::choices`] the answer is.
    pub fn chosen(&self, row: CreateRow) -> Option<usize> {
        match row {
            CreateRow::Skills => SKILLS_MODES
                .iter()
                .position(|m| *m == self.workspace.skills),
            CreateRow::LitellmConn => Some(
                self.litellm_choice()
                    .and_then(|c| {
                        (self.connection.connections.iter()).position(|o| o.name == c.name)
                    })
                    .unwrap_or(self.connection.connections.len()),
            ),
            _ => None,
        }
    }

    /// Set a choice row's answer to its `value`-th value. False, and nothing changed, where the
    /// row has no such value.
    pub fn pick_choice(&mut self, row: CreateRow, value: usize) -> bool {
        match row {
            CreateRow::Skills => match SKILLS_MODES.get(value) {
                Some(mode) => self.workspace.skills = *mode,
                None => return false,
            },
            CreateRow::LitellmConn => {
                let all = &self.connection.connections;
                if value > all.len() {
                    return false;
                }
                self.connection.litellm = all.get(value).map(|c| c.name.clone());
            }
            _ => return false,
        }
        true
    }

    /// The answers the next form starts from ([`crate::state::CreateDefaults`]).
    pub fn defaults(&self, catalog: &Catalog) -> crate::state::CreateDefaults {
        let backend = (catalog.backends.get(self.harness.backend))
            .map(|b| b.kit.name.clone())
            .unwrap_or_default();
        let mut addons = BTreeMap::new();
        addons.insert(backend.clone(), self.selected_addons(catalog));
        let ws = &self.workspace;
        crate::state::CreateDefaults {
            backend,
            token: self.use_token(catalog),
            addons,
            clone: ws.clone,
            git_guard: ws.git_guard,
            skills: self.skills_offered(catalog).then_some(ws.skills),
            ports: ws.ports.value.trim().to_string(),
            deny_network: ws.deny_network.value.trim().to_string(),
            mcp: self.selected_mcp(),
        }
    }

    /// Set the answers `defaults` holds, as far as this host still offers them: a backend that is
    /// gone, an addon that is gone or would mount nothing, and an MCP server no longer registered
    /// are left out. `false`, and nothing changed, when the backend is not offered.
    pub fn apply_defaults(
        &mut self,
        catalog: &Catalog,
        defaults: &crate::state::CreateDefaults,
        skills_default: crate::sbx::SkillsMode,
    ) -> bool {
        let offered = catalog.offered_backends();
        let Some(backend) = offered
            .into_iter()
            .find(|i| catalog.backends[*i].kit.name == defaults.backend)
        else {
            return false;
        };
        self.harness.backend = backend;
        if self.subscription(catalog) {
            self.connection.account = match defaults.token {
                true => self.token_default(),
                false => AccountChoice::Login,
            };
        }
        let wanted = defaults
            .addons
            .get(&defaults.backend)
            .cloned()
            .unwrap_or_default();
        for (i, addon) in catalog.addons.iter().enumerate() {
            let on = wanted.contains(&addon.kit.name) && !self.addon_unavailable(i);
            self.kits.addons[i] = on;
        }
        let ws = &mut self.workspace;
        ws.clone = defaults.clone;
        ws.git_guard = defaults.git_guard;
        ws.skills = defaults.skills.unwrap_or(skills_default);
        ws.ports.set(&defaults.ports);
        ws.deny_network.set(&defaults.deny_network);
        ws.no_mcp = crate::sbx::is_no_mcp(&defaults.mcp);
        for (on, server) in ws.static_mcp.iter_mut().zip(&ws.mcp) {
            *on = !ws.no_mcp && defaults.mcp.contains(&server.name);
        }
        self.refresh_auto_name(catalog);
        true
    }

    /// Switch the agent's offer of catalog model `i` on or off. A slot on a model switched off is
    /// the agent's own choice again.
    pub(crate) fn toggle_model(&mut self, catalog: &Catalog, i: usize) {
        let Some(id) = catalog.models.get(i).map(|m| m.id.clone()) else {
            return;
        };
        if !self.connection.disabled.remove(&id) {
            for slot in Slot::EVERY {
                if self.connection.models.get(slot) == Some(id.as_str()) {
                    self.connection.models.set(slot, None);
                }
            }
            self.connection.disabled.insert(id);
        }
    }

    /// The connection *new connection...* describes, checked as a save would check it, or the first
    /// reason to refuse it.
    pub fn new_connection(&self) -> Result<crate::litellm::Connection, String> {
        self.connection.new_conn.check(&self.connection.connections)
    }

    /// The first entry of the ports field that is no port mapping.
    fn port_problem(typed: &str) -> Option<String> {
        let ports = split_ports(typed);
        let bad = ports.iter().find(|p| !port_spec_ok(p))?;
        Some(format!(
            "“{bad}” is not a port mapping — use 8080:3000, 3000, or 127.0.0.1:8080:3000/tcp"
        ))
    }

    /// The first entry of the blocked-hosts field that is no host. Same grammar as the ports field.
    fn deny_problem(typed: &str) -> Option<String> {
        let hosts = split_ports(typed);
        let bad = hosts.iter().find(|h| !deny_host_ok(h))?;
        Some(format!(
            "“{bad}” is not a host to deny — use api.example.com, *.example.com or **"
        ))
    }
}

// --- the step-by-step wizard ---------------------------------------------------------------------

/// One agent the wizard's Harness step offers for the picked provider, with the offered backends
/// that run it there.
#[derive(Debug, Clone)]
pub struct Harness {
    /// The CLI these backends run ([`Backend::harness`]): `claude` for the three Claude Code
    /// workloads, which sbx reports as three agents.
    pub agent: String,
    pub label: String,
    /// Indices into `catalog.backends`, in the order the catalog offers them.
    pub backends: Vec<usize>,
}

/// A radio under the harness on the subscription backend: which Anthropic account it signs in
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnChoice {
    /// The subscription backend on the host-global `/login`.
    Login,
    /// The subscription backend on a setup token; the token rows follow it.
    Token,
}

/// A focusable line of the wizard's current step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardRow {
    /// Step 1: a provider, by index into [`CreateForm::providers`].
    Provider(usize),
    /// Step 3: an agent, by index into [`CreateForm::harnesses`].
    Agent(usize),
    /// Step 3: one of the account radios under Claude Code on Anthropic.
    Connection(ConnChoice),
    /// A row of the form itself: the connection, token, model, kit and workspace rows.
    Field(CreateRow),
    /// Inert on step 1, where it is drawn muted.
    Back,
    Next,
    CreateStart,
    CreateOnly,
    Cancel,
}

/// Where the wizard is. Navigation only: the answers stay in [`CreateForm`].
#[derive(Debug, Clone)]
pub struct WizardState {
    pub step: CreateStep,
    /// Index into [`CreateForm::wizard_rows`].
    pub focus: usize,
    /// The backend last picked per provider while the wizard is open, so going LiteLLM / Pi →
    /// Anthropic → LiteLLM comes back on Pi. Gone when the wizard closes.
    pub last_backend: BTreeMap<Provider, usize>,
    /// Next, the trail or a create was refused over this step's answers: from then on its fields
    /// say what is wrong with them, empty ones included ([`CreateForm::visible_problem`]).
    /// Cleared when the wizard moves to another step.
    pub refused: bool,
    /// The text row last typed into, until the cursor moves: a field being typed is not told
    /// what is wrong with it yet.
    pub editing: Option<CreateRow>,
    /// The connection whose list the Models step fetched on its own once already, so a fetch
    /// that failed is not started again at every key.
    pub auto_fetched: Option<String>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            step: CreateStep::Provider,
            focus: 0,
            last_backend: BTreeMap::new(),
            refused: false,
            editing: None,
            auto_fetched: None,
        }
    }
}

/// What the Harness step calls an agent — and the window's map of a sandbox, inside its wall.
pub fn harness_label(backend: &Backend) -> String {
    match backend.harness() {
        "claude" => "Claude Code".into(),
        "opencode" => "OpenCode".into(),
        "codex" => "Codex".into(),
        "agy" => "Antigravity".into(),
        "pi" => "Pi".into(),
        _ => backend.kit.display_name.clone(),
    }
}

/// The TUI's tag beside the label while it is still generated from the agent and the kits.
pub const GENERATED_TAG: &str = "generated";

/// The GUI's line under the label's box while the label is still generated.
pub const GENERATED_GLOSS: &str = "generated from the agent and the kits; type to replace it";

/// What the status line says when New brings back the answers a wizard was left with.
pub const DRAFT_BACK: &str = "your unfinished answers are back — Cancel discards them";

/// The last value of the LiteLLM connection row: a connection made together with the sandbox.
pub const NEW_CONNECTION: &str = "new connection…";

/// The skills modes in the order a choice row offers them, tightest first.
pub const SKILLS_MODES: [crate::sbx::SkillsMode; 3] = [
    crate::sbx::SkillsMode::Off,
    crate::sbx::SkillsMode::Readonly,
    crate::sbx::SkillsMode::Readwrite,
];

/// A skills mode as the forms and the review name it.
pub fn skills_mode_name(mode: crate::sbx::SkillsMode) -> &'static str {
    match mode {
        crate::sbx::SkillsMode::Off => "off",
        crate::sbx::SkillsMode::Readonly => "read-only",
        crate::sbx::SkillsMode::Readwrite => "read-write",
    }
}

/// What a skills mode gives the sandbox, in one line.
pub fn skills_gloss(mode: crate::sbx::SkillsMode) -> &'static str {
    match mode {
        crate::sbx::SkillsMode::Off => "no skills mount — this sandbox is outside the shared store",
        crate::sbx::SkillsMode::Readonly => "the host's shared skills, which it cannot change",
        crate::sbx::SkillsMode::Readwrite => "and it can rewrite what the other sandboxes load",
    }
}

/// The tag the `/login` radio carries before its hint, in the running colour: it is the account
/// most sandboxes should be on, it needs nothing pasted, and it survives every rebuild.
pub const LOGIN_TAG: &str = "recommended";

/// What the `/login` radio says, on the create wizard and the account editor of both frontends.
pub const LOGIN_HINT: &str = "one global login, reused by every sandbox";

/// What the *setup token* radio says. Not picked, it says why to pick it; picked, it says what to
/// do next, which depends on whether this host holds a token yet (`stored`).
pub fn token_hint(selected: bool, stored: usize) -> &'static str {
    match (selected, stored) {
        (false, _) => "a second, independent Anthropic subscription",
        (true, 0) => "name it and paste it in; later sandboxes can use it too",
        (true, _) => "pick one below, or add a new one",
    }
}

/// How many sandboxes already authenticate as a stored token's account: the reason to pick it
/// rather than store another one. Empty when none does.
pub fn token_users(users: usize) -> String {
    match users {
        0 => String::new(),
        1 => "1 sandbox uses it".into(),
        n => format!("{n} sandboxes use it"),
    }
}

/// What an account radio says beside its name.
pub fn connection_hint(choice: ConnChoice, form: &CreateForm, catalog: &Catalog) -> String {
    match choice {
        ConnChoice::Login => LOGIN_HINT.into(),
        ConnChoice::Token => {
            let picked = form.connection_selected(choice, catalog);
            token_hint(picked, form.connection.tokens.len()).into()
        }
    }
}

/// What a harness radio says beside its name: how that agent reaches the provider's model.
pub fn harness_hint(backend: &Backend) -> String {
    match backend.account() {
        Some(_) => "the /login or a setup token, picked below".into(),
        None if backend.litellm_agent().is_some() && backend.harness() == "claude" => {
            "a connection to your gateway; no Anthropic account needed".into()
        }
        None if backend.litellm_agent().is_some() => "a connection to your gateway".into(),
        None => backend.setup_hint().into(),
    }
}

/// What a provider is, in a sentence: the Provider step's gloss under its name.
pub fn provider_blurb(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Your Anthropic subscription: this host's /login or a setup token.",
        Provider::Google => "Your Google account, signed in once and kept on the host.",
        Provider::OpenAi => "Your OpenAI sign-in or API key, kept on the host.",
        Provider::Litellm => "Your own LiteLLM gateway, through a connection and its key.",
        Provider::Other => "",
    }
}

/// The warning under a picked connection whose key this host does not hold: where to store it
/// without leaving the dashboard (`S`, then the connection's own dialog, whose key field stores
/// it).
pub fn missing_key_note(conn: &crate::litellm::Connection) -> String {
    format!(
        "⚠ no key stored for {}: open Secrets (S), pick {} and paste its key",
        crate::litellm::host(&conn.url),
        conn.name
    )
}

/// The note under a focused kit that `sbx kit add` cannot add later, `reason` being what the kit
/// declares that it does not apply ([`crate::kits::Addon::create_time_reason`]).
pub fn only_now_note(reason: &str) -> String {
    format!(
        "Only now: sbx adds no kit with {reason} to an existing sandbox; later means a rebuild."
    )
}

/// The marker a kit row carries when ticking it now is the only chance to have it without a
/// rebuild. Quiet on purpose: most kits carry it.
pub const ONLY_NOW: &str = "only now";

/// What `--static-mcp none` leaves the agent, after the word *none*.
pub const NO_MCP: &str = "no server, and no tools to find or add one";

/// The Review's MCP answer when nothing is ticked: sbx's dynamic mode, where the agent finds and
/// attaches the host's registered servers itself.
pub const DYNAMIC_MCP: &str = "dynamic: the agent finds the host's servers";

/// A sandbox's MCP set as the details name it, from the registry: `None` for the dynamic mode,
/// which has no fixed set to name.
pub fn mcp_set(servers: &[String]) -> Option<String> {
    match (crate::sbx::is_no_mcp(servers), servers.is_empty()) {
        (true, _) => Some(format!("none: {NO_MCP}")),
        (false, true) => None,
        (false, false) => Some(servers.join(" ")),
    }
}

/// The GUI's marker on a kit that `sbx kit add` can add to the sandbox later: the GUI marks the
/// few exceptions, the TUI marks the rule ([`ONLY_NOW`]).
pub const ADD_LATER: &str = "can add later";

/// What the GUI's Kits step says under its heading, for its [`ADD_LATER`] markers.
pub const ADD_LATER_GLOSS: &str =
    "most kits go in only now: adding one later means a rebuild. The ones marked Can add later \
     are the exception";

/// What the Harness step calls an account radio.
pub fn connection_label(choice: ConnChoice) -> &'static str {
    match choice {
        ConnChoice::Login => "/login",
        ConnChoice::Token => "setup token",
    }
}

/// The paragraph explaining the selected account choice, one line per entry, unindented. Each
/// frontend draws it directly under the row it is about, before the other option.
pub fn account_notes(account: &AccountChoice) -> &'static [&'static str] {
    match account {
        AccountChoice::Login => &[
            "You sign in once with /login inside the sandbox. sbx keeps that login on the",
            "host instead of in the container, so every sandbox on this option uses the same",
            "account — including ones created later — and rebuilding one never loses it.",
            "There is only ever one such account per machine.",
        ],
        AccountChoice::New => &[
            "Run  claude setup-token  while signed in as the account you want, then give it",
            "a name and paste it in. The token is kept on this host and the sandbox only",
            "ever sees a placeholder. Later sandboxes can reuse this name, however many.",
        ],
        AccountChoice::Named(_) => &[
            "Already stored on this host, so there is nothing to paste. Every sandbox on",
            "this name uses that one account — this just points one more at it.",
        ],
    }
}

/// What a sandbox's account is now, as the auth editor's *Now* line says it: the text and its
/// note, unindented. `users`: the sandboxes on its token, itself included. The colours — danger
/// for a token not stored, running for a stored one — are each frontend's, keyed on `current`.
pub fn auth_now(current: &crate::model::Auth, users: usize) -> (String, String) {
    use crate::model::Auth;
    match current {
        Auth::Login => (
            "/login (OAuth)".to_string(),
            "the one global login, shared with every other /login sandbox".to_string(),
        ),
        Auth::Token {
            name,
            stored: false,
            ..
        } => (
            format!("setup token “{name}” — not stored"),
            "it cannot authenticate until a token is stored below".to_string(),
        ),
        Auth::Token { name, .. } => (
            format!("setup token “{name}”"),
            match users {
                0 | 1 => "its own account, independent of the /login".to_string(),
                n => format!(
                    "shared with {} other sandbox{}",
                    n - 1,
                    if n == 2 { "" } else { "es" }
                ),
            },
        ),
    }
}

/// One line of the Review, and of the GUI's summary beside every step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The step that owns the answer: a click on the line goes there.
    pub step: CreateStep,
    pub label: &'static str,
    pub value: String,
    /// Still what the form opened with: nothing ticked or typed, the default mode. Both frontends
    /// draw such a line muted, so the answers that were changed stand out. The harness, the
    /// connection and the label say what the sandbox is, so they are never a default.
    pub default: bool,
}

/// The wizard's last step: one line per answer, then what sbx will be told. The token is named,
/// never shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// The host's network policy is not initialised, so sbx would refuse the create.
    pub policy_warning: bool,
    /// In the order shown.
    pub answers: Vec<Answer>,
    /// The sandbox's name — of the *effective* backend: a setup token swaps the kit, and with it
    /// the name.
    pub sbx_name: String,
    /// The workspace first, then what the host-folder kits mount.
    pub mounts: Vec<String>,
    /// Why a host-folder kit mounts less than it could.
    pub notes: Vec<String>,
}

impl Answer {
    /// The value without the explanation that some answers carry after a spaced em dash: for the
    /// clone mode, `on` without "the agent works on its own copy". For the places that put
    /// several answers on one line.
    pub fn brief(&self) -> &str {
        self.value.split(" — ").next().unwrap_or(&self.value)
    }
}

impl Review {
    /// What a step comes to, for the TUI's ledger and the GUI's step rail: its first answer, which
    /// says what the step decided and is always there, then each other answer that is no longer
    /// the form's default. Empty for the Review, which has no answers of its own.
    pub fn digest(&self, step: CreateStep) -> Vec<&Answer> {
        let mut answers = self.answers.iter().filter(|a| a.step == step);
        let first = answers.next();
        first
            .into_iter()
            .chain(answers.filter(|a| !a.default))
            .collect()
    }
}

fn or_none(typed: &str) -> String {
    match typed.trim() {
        "" => "none".into(),
        typed => typed.to_string(),
    }
}

impl CreateForm {
    /// The name sbx will give the sandbox: the *effective* backend's (a setup token swaps the
    /// kit, and with it the name), unique among the sandboxes on this host.
    pub fn sbx_name(&self, app: &crate::app::App) -> String {
        let taken: Vec<String> = app.sandboxes.iter().map(|s| s.name.clone()).collect();
        let root = self.workspace_root(&app.cwd);
        self.effective_backend(&app.catalog)
            .map(|b| crate::model::unique_sandbox_name(&b.kit.name, &root, &taken))
            .unwrap_or_default()
    }

    /// The folder the sandbox mounts: the folder above `cwd` picked in the start-folder picker,
    /// else `cwd`.
    pub fn workspace_root(&self, cwd: &Path) -> PathBuf {
        self.workspace
            .root
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf())
    }

    /// The folders typed into [`CreateRow::ExtraFolders`], checked against those the mounted
    /// folder's plan mounts ([`crate::folders::extras`]), and what is wrong with any of them.
    pub fn extra_folders(&self) -> (Vec<crate::folders::Folder>, Vec<String>) {
        let ws = &self.workspace;
        let home = crate::model::home_dir();
        crate::folders::extras(ws.extra_folders.value.trim(), &ws.folders, home.as_deref())
    }

    /// Whether [`CreateRow::GitGuard`] is offered: the workspace has a `.git` folder, and clone
    /// mode, which mounts the whole workspace read-only, is off.
    pub fn git_guard_offered(&self) -> bool {
        self.workspace.git_offered && !self.workspace.clone
    }

    /// The Review step's lines, for the answers on the form now.
    pub fn review(&self, app: &crate::app::App) -> Review {
        let catalog = &app.catalog;
        let mut answers: Vec<Answer> = Vec::new();
        let mut answer = |step, label, value: String, default| {
            answers.push(Answer {
                step,
                label,
                value,
                default,
            })
        };
        let provider = self.selected_provider(catalog).map(Provider::label);
        let provider = provider.unwrap_or_default().to_string();
        answer(CreateStep::Provider, "Provider", provider, false);
        if self.rows(catalog).contains(&CreateRow::LitellmConn) {
            let connection = match self.connection.litellm.as_deref() {
                Some(name) => name.to_string(),
                None => format!("new {}", self.connection.new_conn.name.value.trim()),
            };
            answer(CreateStep::Connection, "Connection", connection, false);
        }
        answer(
            CreateStep::Harness,
            "Harness",
            self.harness_label(catalog),
            false,
        );
        if self.subscription(catalog) {
            let token = connection_label(ConnChoice::Token);
            let account = match &self.connection.account {
                AccountChoice::Named(name) => format!("{token} · {name}"),
                AccountChoice::New => {
                    let name = self.connection.token_name.value.trim();
                    format!("{token} · new, named {name}")
                }
                AccountChoice::Login => connection_label(ConnChoice::Login).to_string(),
            };
            answer(CreateStep::Harness, "Account", account, false);
        }
        match self.models_agent(catalog) {
            Some(agent) if self.models_offered(catalog) => {
                let pins: Vec<String> = Slot::of(agent)
                    .iter()
                    .filter_map(|slot| {
                        let id = self.connection.models.get(*slot)?;
                        let name = crate::models::display_name(&catalog.models, id)
                            .unwrap_or_else(|| id.to_string());
                        Some(format!("{} → {name}", slot.label()))
                    })
                    .collect();
                let none = pins.is_empty();
                let value = match none {
                    true => Slot::DEFAULT.to_string(),
                    false => pins.join(", "),
                };
                answer(CreateStep::Models, "Models", value, none);
                let total = catalog.models.len();
                let on = self.enabled_models(catalog).len();
                let offered = format!("{on} of {total}");
                answer(CreateStep::Models, "Offered", offered, on == total);
            }
            Some(_) => {
                let value = "no list yet".to_string();
                answer(CreateStep::Models, "Models", value, true);
            }
            None => {
                answer(
                    CreateStep::Models,
                    "Models",
                    Slot::DEFAULT.to_string(),
                    true,
                );
            }
        }
        let addons = self.selected_addons(catalog);
        let kits = or_none(&addons.join(", "));
        answer(CreateStep::Kits, "Kits", kits, addons.is_empty());
        let label = match self.workspace.name.value.trim() {
            "" => self
                .effective_backend(catalog)
                .map(|b| crate::model::auto_label(catalog, &b.kit.name, &addons))
                .unwrap_or_default(),
            typed => typed.to_string(),
        };
        answer(CreateStep::Workspace, "Label", label, false);
        let ws = &self.workspace;
        let root = self.workspace_root(&app.cwd);
        if ws.root.is_some() {
            let mounted = crate::model::tilde(&root);
            answer(CreateStep::Workspace, "Mounted folder", mounted, false);
        }
        if self.start_dir_offered(catalog) {
            let opens = crate::startdir::describe(&root, ws.start_dir.as_deref());
            answer(
                CreateStep::Workspace,
                "Opens in",
                opens,
                ws.start_dir.is_none() && ws.root.is_none(),
            );
        }
        let extra = ws.extra_folders.value.trim();
        let more = or_none(extra);
        answer(
            CreateStep::Workspace,
            "More folders",
            more,
            extra.is_empty(),
        );
        let clone = match ws.clone {
            true => "on — the agent works on its own copy",
            false => "off",
        };
        answer(CreateStep::Workspace, "Clone mode", clone.into(), !ws.clone);
        if self.git_guard_offered() {
            let git = match ws.git_guard {
                true => "read-only",
                false => "writable",
            };
            answer(
                CreateStep::Workspace,
                "Git folder",
                git.into(),
                !ws.git_guard,
            );
        }
        if !ws.folders.folders.is_empty() {
            let listed: Vec<String> = (ws.folders.folders.iter())
                .map(|f| format!("{} ({})", crate::model::tilde(&f.path), f.access.label()))
                .collect();
            answer(
                CreateStep::Workspace,
                "Host folders",
                listed.join(", "),
                true,
            );
        }
        if self.skills_offered(catalog) {
            let skills = skills_mode_name(ws.skills);
            let default = ws.skills == app.skills_default;
            answer(CreateStep::Workspace, "Skills", skills.into(), default);
        }
        let empty = |input: &TextInput| input.value.trim().is_empty();
        let ports = or_none(&ws.ports.value);
        answer(CreateStep::Workspace, "Ports", ports, empty(&ws.ports));
        let deny = or_none(&ws.deny_network.value);
        let open = empty(&ws.deny_network);
        answer(CreateStep::Workspace, "Blocked hosts", deny, open);
        let picked = self.selected_mcp();
        let mcp = match (ws.no_mcp, picked.is_empty()) {
            (true, _) => format!("none: {NO_MCP}"),
            (false, true) => DYNAMIC_MCP.to_string(),
            (false, false) => picked.join(", "),
        };
        answer(CreateStep::Workspace, "MCP servers", mcp, picked.is_empty());

        let crate::model::KitMounts { mounts, mut notes } = crate::model::kit_mounts(&addons);
        notes.extend(ws.folders.notes.iter().cloned());
        let (extra, _) = self.extra_folders();
        let folders = (ws.folders.folders.iter())
            .chain(&extra)
            .map(crate::folders::Folder::shown_spec);
        let git = (self.git_guard_offered() && ws.git_guard)
            .then(|| format!("{}:ro", crate::model::tilde(&root.join(".git"))));
        Review {
            policy_warning: app.policy_needs_init == Some(true),
            answers,
            sbx_name: self.sbx_name(app),
            mounts: std::iter::once(crate::model::tilde(&root))
                .chain(git)
                .chain(mounts)
                .chain(folders)
                .collect(),
            notes,
        }
    }
}

impl CreateForm {
    /// The providers the Provider step offers, in [`Provider`]'s order.
    pub fn providers(&self, catalog: &Catalog) -> Vec<Provider> {
        let mut out: Vec<Provider> = (catalog.offered_backends().into_iter())
            .map(|i| catalog.backends[i].provider())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The provider of the picked backend — Anthropic whether its account is the /login or a
    /// setup token.
    pub fn selected_provider(&self, catalog: &Catalog) -> Option<Provider> {
        catalog
            .backends
            .get(self.harness.backend)
            .map(Backend::provider)
    }

    /// The selected provider's index into [`Self::providers`].
    pub fn selected_provider_index(&self, catalog: &Catalog) -> Option<usize> {
        let provider = self.selected_provider(catalog)?;
        self.providers(catalog).iter().position(|p| *p == provider)
    }

    /// The agents `provider` offers, in the order of each one's first offered backend.
    pub fn harnesses_of(catalog: &Catalog, provider: Provider) -> Vec<Harness> {
        let mut out: Vec<Harness> = Vec::new();
        for i in catalog.offered_backends() {
            let backend = &catalog.backends[i];
            if backend.provider() != provider {
                continue;
            }
            match out.iter_mut().find(|h| h.agent == backend.harness()) {
                Some(harness) => harness.backends.push(i),
                None => out.push(Harness {
                    agent: backend.harness().to_string(),
                    label: harness_label(backend),
                    backends: vec![i],
                }),
            }
        }
        out
    }

    /// The agents the Harness step offers: only the selected provider's.
    pub fn harnesses(&self, catalog: &Catalog) -> Vec<Harness> {
        match self.selected_provider(catalog) {
            Some(provider) => Self::harnesses_of(catalog, provider),
            None => Vec::new(),
        }
    }

    /// The harness of the picked backend ([`Backend::harness`]) — Claude Code on the subscription
    /// backend whether its account is the /login or a setup token.
    pub fn selected_agent<'a>(&self, catalog: &'a Catalog) -> Option<&'a str> {
        catalog
            .backends
            .get(self.harness.backend)
            .map(|b| b.harness())
    }

    /// The selected agent's index into [`Self::harnesses`].
    pub fn selected_harness(&self, catalog: &Catalog) -> Option<usize> {
        let agent = self.selected_agent(catalog)?;
        self.harnesses(catalog)
            .iter()
            .position(|h| h.agent == agent)
    }

    /// The selected agent's name, as step 3 shows it.
    pub fn harness_label(&self, catalog: &Catalog) -> String {
        self.selected_harness(catalog)
            .and_then(|h| self.harnesses(catalog).into_iter().nth(h))
            .map(|h| h.label)
            .unwrap_or_default()
    }

    /// The line a step opens with, and a short gloss: what the step decides, worded for the
    /// answers so far.
    pub fn step_heading(&self, step: CreateStep, catalog: &Catalog) -> (String, &'static str) {
        match step {
            CreateStep::Provider => ("Who serves the model".into(), ""),
            CreateStep::Connection => (
                "The LiteLLM gateway the sandbox talks to".into(),
                "pick one of your connections, or add a new one",
            ),
            CreateStep::Harness => {
                let provider = self.selected_provider(catalog).map(Provider::label);
                let provider = provider.unwrap_or_default();
                (format!("The agent that works with {provider}"), "")
            }
            CreateStep::Models => match (self.models_agent(catalog), catalog.models.len()) {
                (None, _) => (
                    "The agent picks its own models".into(),
                    "only a LiteLLM connection has models to pick here",
                ),
                (Some(_), 0) => (
                    "No model list yet".into(),
                    "Refresh asks the gateway, through a small sandbox of its own",
                ),
                (Some(_), n) => (
                    format!(
                        "The {n} models of {}",
                        self.connection.litellm.as_deref().unwrap_or_default()
                    ),
                    "your picks are kept for the next sandbox on this connection",
                ),
            },
            CreateStep::Kits => (
                "Kits to build into the sandbox".into(),
                "a kit marked only now cannot be added later without a rebuild",
            ),
            CreateStep::Workspace => ("Its name, where it opens, and what it can reach".into(), ""),
            CreateStep::Review => (
                "What sbx will create".into(),
                "nothing is created until you press Create",
            ),
        }
    }

    /// Pick a provider. The backend stays if it is already on that provider; otherwise the one
    /// last picked for it while the wizard is open comes back, else its first offered backend. No
    /// other answer changes — the account included.
    pub fn pick_provider(&mut self, catalog: &Catalog, p: usize) {
        let Some(target) = self.providers(catalog).into_iter().nth(p) else {
            return;
        };
        let current = self.harness.backend;
        if let Some(leaving) = self.selected_provider(catalog) {
            self.wizard.last_backend.insert(leaving, current);
        }
        if self.selected_provider(catalog) != Some(target) {
            let of_target = |b: &usize| catalog.backends[*b].provider() == target;
            let remembered = self.wizard.last_backend.get(&target).copied();
            self.harness.backend = remembered
                .filter(|b| catalog.offered_backends().contains(b) && of_target(b))
                .or_else(|| catalog.offered_backends().into_iter().find(of_target))
                .unwrap_or(current);
        }
        self.refresh_auto_name(catalog);
    }

    /// Pick an agent of the selected provider. No other answer changes — the account included.
    pub fn pick_harness(&mut self, catalog: &Catalog, h: usize) {
        let Some(target) = self.harnesses(catalog).into_iter().nth(h) else {
            return;
        };
        if !target.backends.contains(&self.harness.backend) {
            if let Some(first) = target.backends.first() {
                self.harness.backend = *first;
            }
        }
        self.refresh_auto_name(catalog);
    }

    /// The account radios under the selected harness: `/login` and setup token on the
    /// subscription backend, none elsewhere.
    pub fn connection_choices(&self, catalog: &Catalog) -> Vec<ConnChoice> {
        let backend = catalog.backends.get(self.harness.backend);
        match backend.and_then(Backend::account) {
            Some(_) => vec![ConnChoice::Login, ConnChoice::Token],
            None => Vec::new(),
        }
    }

    /// Is this account radio the one the answers say?
    pub fn connection_selected(&self, choice: ConnChoice, catalog: &Catalog) -> bool {
        match choice {
            ConnChoice::Login => self.subscription(catalog) && !self.connection.account.on_token(),
            ConnChoice::Token => self.use_token(catalog),
        }
    }

    /// Pick an account radio: the subscription backend on `/login`, or on a setup token (the last
    /// used one, else a new one).
    pub fn pick_connection(&mut self, catalog: &Catalog, choice: ConnChoice) {
        let login = (catalog.backends.iter()).position(|b| b.account() == Some(Account::Login));
        let Some(login) = login else {
            return;
        };
        self.harness.backend = login;
        self.connection.account = match choice {
            ConnChoice::Login => AccountChoice::Login,
            ConnChoice::Token => self.token_default(),
        };
        self.refresh_auto_name(catalog);
    }

    pub fn wizard_step(&self) -> CreateStep {
        self.wizard.step
    }

    /// The form's one cursor, an index into [`Self::wizard_rows`].
    pub fn wizard_focus(&self) -> usize {
        self.wizard.focus
    }

    pub fn set_wizard_focus(&mut self, focus: usize) {
        self.wizard.focus = focus;
        self.wizard.editing = None;
    }

    /// The focusable rows of the wizard's current step, ending with its buttons.
    pub fn wizard_rows(&self, catalog: &Catalog) -> Vec<WizardRow> {
        let step = self.wizard_step();
        let mut rows: Vec<WizardRow> = match step {
            CreateStep::Provider => (0..self.providers(catalog).len())
                .map(WizardRow::Provider)
                .collect(),
            // The account radios stand in for the form's account rows; the token, connection and
            // model rows under them are the form's own.
            CreateStep::Harness => (0..self.harnesses(catalog).len())
                .map(WizardRow::Agent)
                .chain(
                    self.connection_choices(catalog)
                        .into_iter()
                        .map(WizardRow::Connection),
                )
                .chain(
                    self.rows_of(step, catalog)
                        .into_iter()
                        .filter(|r| !matches!(r, CreateRow::AuthOauth | CreateRow::AuthToken))
                        .map(WizardRow::Field),
                )
                .collect(),
            CreateStep::Connection
            | CreateStep::Models
            | CreateStep::Kits
            | CreateStep::Workspace => self
                .rows_of(step, catalog)
                .into_iter()
                .map(WizardRow::Field)
                .collect(),
            CreateStep::Review => Vec::new(),
        };
        rows.extend(Self::wizard_button_rows(step));
        rows
    }

    /// The buttons a step ends with, drawn on one line: Next, Back, Cancel on steps 1-6 (Back
    /// inert on step 1), and the four that create or don't on step 7. The main action comes first.
    pub fn wizard_button_rows(step: CreateStep) -> &'static [WizardRow] {
        match step {
            CreateStep::Review => &[
                WizardRow::CreateStart,
                WizardRow::CreateOnly,
                WizardRow::Back,
                WizardRow::Cancel,
            ],
            _ => &[WizardRow::Next, WizardRow::Back, WizardRow::Cancel],
        }
    }

    /// How many rows [`Self::wizard_rows`] ends with that share the button line.
    pub fn wizard_buttons(&self) -> usize {
        Self::wizard_button_rows(self.wizard_step()).len()
    }

    /// Move to `step`, with the cursor on its picked radio: the provider on step 1, the harness on
    /// step 3.
    pub fn wizard_set_step(&mut self, catalog: &Catalog, step: CreateStep) {
        if self.wizard_step() == step {
            return;
        }
        self.wizard.step = step;
        self.set_wizard_focus(0);
        self.wizard.refused = false;
        let picked = match step {
            CreateStep::Provider => self
                .selected_provider_index(catalog)
                .map(WizardRow::Provider),
            CreateStep::Harness => self.selected_harness(catalog).map(WizardRow::Agent),
            _ => None,
        };
        if let Some(picked) = picked {
            self.focus_wizard_row(catalog, picked);
        }
    }

    /// Does the wizard stop at `step` with these answers? Every step but Connection and Models,
    /// which only a LiteLLM backend has anything on; the others pass them by.
    pub fn step_shown(&self, step: CreateStep, catalog: &Catalog) -> bool {
        !matches!(step, CreateStep::Connection | CreateStep::Models)
            || self.litellm_backend(catalog)
    }

    /// The step Next goes to from `step`, past the ones not shown.
    pub fn next_step(&self, step: CreateStep, catalog: &Catalog) -> Option<CreateStep> {
        let mut next = step.next();
        while let Some(s) = next.filter(|s| !self.step_shown(*s, catalog)) {
            next = s.next();
        }
        next
    }

    /// The step Back goes to from `step`, past the ones not shown.
    pub fn prev_step(&self, step: CreateStep, catalog: &Catalog) -> Option<CreateStep> {
        let mut prev = step.prev();
        while let Some(s) = prev.filter(|s| !self.step_shown(*s, catalog)) {
            prev = s.prev();
        }
        prev
    }

    /// Next: on to the next step, unless the current one's answers would refuse a create — then
    /// that message, and stay.
    pub fn wizard_next(&mut self, catalog: &Catalog) -> Result<(), String> {
        let step = self.wizard_step();
        if let Some(problem) = self.step_problem(step, catalog) {
            self.wizard.refused = true;
            return Err(problem);
        }
        if let Some(next) = self.next_step(step, catalog) {
            self.wizard_set_step(catalog, next);
        }
        Ok(())
    }

    /// A step of the trail: back to an earlier step at once; forward only through steps whose
    /// answers pass, stopping on the first that does not.
    pub fn wizard_goto(&mut self, catalog: &Catalog, to: CreateStep) -> Result<(), String> {
        let to = match self.step_shown(to, catalog) {
            true => to,
            false => self.next_step(to, catalog).unwrap_or(to),
        };
        let from = self.wizard_step();
        if to.number() > from.number() {
            for step in CreateStep::ALL[from.number() - 1..to.number() - 1].iter() {
                if let Some(problem) = self.step_problem(*step, catalog) {
                    self.wizard_set_step(catalog, *step);
                    self.wizard.refused = true;
                    return Err(problem);
                }
            }
        }
        self.wizard_set_step(catalog, to);
        Ok(())
    }

    /// The steps whose answers `App::submit_create` checks, in its order: the workspace's first.
    pub const CHECKED_STEPS: [CreateStep; 3] = [
        CreateStep::Workspace,
        CreateStep::Connection,
        CreateStep::Harness,
    ];

    /// The step owning the field a create would be refused over — the first of
    /// [`CreateForm::CHECKED_STEPS`] that refuses.
    pub fn owning_step(&self, catalog: &Catalog) -> Option<CreateStep> {
        CreateForm::CHECKED_STEPS
            .into_iter()
            .find(|step| self.step_problem(*step, catalog).is_some())
    }

    /// Put the cursor on `row` of the current step. False, and nothing moved, where the step has
    /// no such row.
    pub fn focus_wizard_row(&mut self, catalog: &Catalog, row: WizardRow) -> bool {
        match self.wizard_rows(catalog).iter().position(|r| *r == row) {
            Some(at) => {
                self.set_wizard_focus(at);
                true
            }
            None => false,
        }
    }

    /// Move to the step that owns `row` and put the cursor on it. False where that step does not
    /// offer the row with the answers as they stand.
    pub fn focus_field(&mut self, catalog: &Catalog, row: CreateRow) -> bool {
        self.wizard_set_step(catalog, row.step());
        self.focus_wizard_row(catalog, WizardRow::Field(row))
    }

    /// One key on one of the form's rows: typing, a tick, a pick, a model list opened or closed,
    /// the skills mode. Moving, the steps and the buttons are [`FieldEffect::Ignored`].
    pub fn field_key(&mut self, catalog: &Catalog, row: CreateRow, key: KeyEvent) -> FieldEffect {
        // The generated label is a suggestion: the first character typed replaces it. The arrows,
        // Home, End and Backspace take it over for editing instead.
        let replace = row == CreateRow::Name
            && !self.workspace.name_touched
            && matches!(key.code, KeyCode::Char(_))
            && !key.modifiers.contains(KeyModifiers::CONTROL);
        let text = match row {
            CreateRow::Name => Some(&mut self.workspace.name),
            CreateRow::TokenName => Some(&mut self.connection.token_name),
            CreateRow::Token => Some(&mut self.connection.token),
            CreateRow::ExtraFolders => Some(&mut self.workspace.extra_folders),
            CreateRow::Ports => Some(&mut self.workspace.ports),
            CreateRow::DenyNetwork => Some(&mut self.workspace.deny_network),
            CreateRow::LitellmName => Some(&mut self.connection.new_conn.name),
            CreateRow::LitellmUrl => Some(&mut self.connection.new_conn.url),
            CreateRow::LitellmKey => Some(&mut self.connection.new_conn.key),
            _ => None,
        };
        if let Some(field) = text {
            if replace {
                field.set("");
            }
            if !edit_text(field, key) {
                return FieldEffect::Ignored;
            }
            // Only the label stops auto-generating itself once touched; the others are never
            // generated from anything.
            if row == CreateRow::Name {
                self.workspace.name_touched = true;
            }
            self.wizard.editing = Some(row);
            return FieldEffect::Done;
        }

        if let CreateRow::ConfigFile(i) = row {
            return match key.code {
                KeyCode::Enter => FieldEffect::EditConfig(i),
                KeyCode::Delete | KeyCode::Backspace => FieldEffect::ResetConfig(i),
                _ => FieldEffect::Ignored,
            };
        }

        // Did the key open rows right below this one? Then the cursor steps onto the first.
        let mut opened = false;
        match key.code {
            KeyCode::Char(' ') => {
                match row {
                    CreateRow::Backend(i) => self.harness.backend = i,
                    CreateRow::Addon(i) => self.toggle_addon(i),
                    CreateRow::Mcp(i) => self.toggle_mcp(i),
                    CreateRow::McpNone => self.toggle_no_mcp(),
                    CreateRow::Clone => self.workspace.clone = !self.workspace.clone,
                    CreateRow::GitGuard => self.workspace.git_guard = !self.workspace.git_guard,
                    // Three values, so space walks them rather than flipping one — off →
                    // readonly → readwrite → off, tightest first.
                    CreateRow::Skills => self.workspace.skills = self.workspace.skills.next(),
                    // A folder is not a toggle: space opens the picker, the same as ↵.
                    CreateRow::StartDir => return FieldEffect::OpenStartDir,
                    CreateRow::AuthOauth => self.connection.account = AccountChoice::Login,
                    CreateRow::AuthToken => self.connection.account = self.token_default(),
                    CreateRow::TokenPick(i) => {
                        let token = self.connection.tokens[i].clone();
                        self.connection.account = AccountChoice::Named(token);
                    }
                    CreateRow::TokenNew => self.connection.account = AccountChoice::New,
                    CreateRow::LitellmConn => self.cycle_litellm(true),
                    CreateRow::Model(slot) => {
                        toggle_model_pick(&mut self.connection.model_pick, slot);
                    }
                    CreateRow::ModelDefault(slot) => choose_model(
                        &mut self.connection.models,
                        &mut self.connection.model_pick,
                        slot,
                        None,
                    ),
                    CreateRow::ModelPick(slot, i) => choose_model(
                        &mut self.connection.models,
                        &mut self.connection.model_pick,
                        slot,
                        catalog.models.get(i),
                    ),
                    CreateRow::ModelOn(i) => self.toggle_model(catalog, i),
                    CreateRow::ModelsRefresh => return FieldEffect::FetchModels,
                    CreateRow::Name
                    | CreateRow::TokenName
                    | CreateRow::Token
                    | CreateRow::ExtraFolders
                    | CreateRow::Ports
                    | CreateRow::DenyNetwork
                    | CreateRow::LitellmName
                    | CreateRow::LitellmUrl
                    | CreateRow::LitellmKey
                    | CreateRow::ConfigFile(_) => {}
                }
                self.refresh_auto_name(catalog);
            }
            KeyCode::Enter => match row {
                CreateRow::Backend(i) => {
                    self.harness.backend = i;
                    self.refresh_auto_name(catalog);
                }
                CreateRow::Addon(i) => {
                    self.toggle_addon(i);
                    self.refresh_auto_name(catalog);
                }
                CreateRow::Clone => self.workspace.clone = !self.workspace.clone,
                CreateRow::GitGuard => self.workspace.git_guard = !self.workspace.git_guard,
                CreateRow::Skills => self.workspace.skills = self.workspace.skills.next(),
                CreateRow::LitellmConn => self.cycle_litellm(true),
                CreateRow::AuthOauth => self.connection.account = AccountChoice::Login,
                // Picking "setup token" opens the token rows right below.
                CreateRow::AuthToken => {
                    opened = !self.connection.account.on_token();
                    self.connection.account = self.token_default();
                }
                CreateRow::TokenPick(i) => {
                    let token = self.connection.tokens[i].clone();
                    self.connection.account = AccountChoice::Named(token);
                }
                // Same again: "new token…" opens the name and value rows under it.
                CreateRow::TokenNew => {
                    opened = self.connection.account != AccountChoice::New;
                    self.connection.account = AccountChoice::New;
                }
                // Same again: opening an alias's list steps onto its first entry, so ↵↵ is
                // "leave it to Claude Code" and ↵↓…↵ is picking a model.
                CreateRow::Model(slot) => {
                    opened = toggle_model_pick(&mut self.connection.model_pick, slot);
                }
                CreateRow::ModelDefault(slot) => choose_model(
                    &mut self.connection.models,
                    &mut self.connection.model_pick,
                    slot,
                    None,
                ),
                CreateRow::ModelPick(slot, i) => choose_model(
                    &mut self.connection.models,
                    &mut self.connection.model_pick,
                    slot,
                    catalog.models.get(i),
                ),
                CreateRow::StartDir => return FieldEffect::OpenStartDir,
                CreateRow::ModelOn(i) => self.toggle_model(catalog, i),
                CreateRow::ModelsRefresh => return FieldEffect::FetchModels,
                // ↵ on a checkbox ticks it, exactly as space does — an MCP server is picked the
                // same way an addon kit is.
                CreateRow::Mcp(i) => self.toggle_mcp(i),
                CreateRow::McpNone => self.toggle_no_mcp(),
                CreateRow::Name
                | CreateRow::TokenName
                | CreateRow::Token
                | CreateRow::ExtraFolders
                | CreateRow::Ports
                | CreateRow::DenyNetwork
                | CreateRow::LitellmName
                | CreateRow::LitellmUrl
                | CreateRow::LitellmKey
                | CreateRow::ConfigFile(_) => {}
            },
            // The skills mode is the one answer here that is a small enum rather than a tick, so
            // ←→ pick between its values.
            KeyCode::Left | KeyCode::Right if row == CreateRow::Skills => {
                self.workspace.skills = match key.code {
                    KeyCode::Right => self.workspace.skills.next(),
                    _ => self.workspace.skills.prev(),
                };
            }
            // So is the connection: ←→ walk the host's connections.
            KeyCode::Left | KeyCode::Right if row == CreateRow::LitellmConn => {
                self.cycle_litellm(key.code == KeyCode::Right);
            }
            _ => return FieldEffect::Ignored,
        }

        let landing = match row {
            // Picking a model closes its list: back onto the alias.
            CreateRow::ModelDefault(s) | CreateRow::ModelPick(s, _) => CreateRow::Model(s),
            _ if opened => {
                let rows = self.rows(catalog);
                let at = rows.iter().position(|r| *r == row);
                at.and_then(|at| rows.get(at + 1).copied()).unwrap_or(row)
            }
            _ => row,
        };
        self.focus_wizard_row(catalog, WizardRow::Field(landing));
        FieldEffect::Done
    }
}

/// What a wizard row is about and what its answer means, in full: the TUI draws it in the strip
/// under the rows, the GUI in the row's tooltip. Unindented and unwrapped; each frontend wraps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// What the row is, as its label says it.
    pub title: String,
    /// What it does with the answer it holds now.
    pub text: String,
    /// What will bite with that answer, if anything.
    pub warn: Option<String>,
    /// Why a create would be refused over the answer, once that is worth saying
    /// ([`CreateForm::visible_problem`]).
    pub problem: Option<String>,
}

impl Help {
    pub(crate) fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            warn: None,
            problem: None,
        }
    }

    pub(crate) fn warn(mut self, warn: Option<String>) -> Self {
        self.warn = warn;
        self
    }
}

/// A model alias's row, on the create wizard and the kits editor alike: what pinning it covers.
pub(crate) fn alias_help(slot: Slot) -> Help {
    Help::new(
        slot.label(),
        format!(
            "Picks the {}: {}. ↵ lists the models the agent is offered; nothing picked ({}) leaves \
             it to {}.",
            slot.label(),
            slot.hint(),
            slot.unset(),
            slot.agent_name()
        ),
    )
}

/// The default under an open slot.
pub(crate) fn default_model_help(slot: Slot) -> Help {
    Help::new(
        slot.label(),
        format!("Leaves the {} to the agent's own choice.", slot.label()),
    )
}

/// One of the connection's models under an open slot.
pub(crate) fn model_pick_help(catalog: &Catalog, slot: Slot, i: usize) -> Help {
    let (name, id) = catalog
        .models
        .get(i)
        .map(|m| (m.name.clone(), m.id.clone()))
        .unwrap_or_default();
    Help::new(
        slot.label(),
        format!("Puts the {} on {name} ({id}).", slot.label()),
    )
}

/// What an agent is, in a sentence: the Harness step's gloss under its name.
pub fn harness_blurb(agent: &str) -> &'static str {
    match agent {
        "claude" => "Anthropic's coding agent.",
        "opencode" => "An open-source coding agent.",
        "codex" => "OpenAI's coding agent.",
        "agy" => "Google's Antigravity CLI.",
        "pi" => "A minimal coding agent (pi.dev).",
        _ => "",
    }
}

/// The harnesses a provider offers, by name, as the Provider step lists them beside it.
pub fn provider_harnesses(catalog: &Catalog, provider: Provider) -> Vec<String> {
    (CreateForm::harnesses_of(catalog, provider).into_iter())
        .map(|h| h.label)
        .collect()
}

/// `a`, `a or b`, `a, b or c` — a provider's harnesses, as the window's tiles write them.
pub fn or_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// One line under a text field: what goes in it.
pub fn field_gloss(row: CreateRow) -> &'static str {
    match row {
        CreateRow::Name => "the name the sandbox list shows",
        CreateRow::StartDir => "where the agent starts; .. at the top mounts the folder above",
        CreateRow::Clone => "the agent works on its own copy; your files stay read-only",
        CreateRow::GitGuard => "the agent can read the history but not commit or change hooks",
        CreateRow::ExtraFolders => {
            "host folders for this sandbox alone, comma-separated: PATH[:/in/sandbox][:ro]"
        }
        CreateRow::Ports => "published at creation, space- or comma-separated",
        CreateRow::DenyNetwork => "hosts this sandbox alone may not reach",
        CreateRow::TokenName => "lower-case a–z, 0–9 and “-”; later sandboxes pick it by name",
        CreateRow::Token => "the output of claude setup-token",
        CreateRow::LitellmName => "lower-case a–z, 0–9 and “-”, at most 32 characters",
        CreateRow::LitellmUrl => "the gateway's https:// address; the key goes to this host only",
        CreateRow::LitellmKey => "stored in the host keychain, never in the sandbox",
        _ => "",
    }
}

impl CreateForm {
    /// What `row` of the current step is about, worded for the answer it holds now.
    pub fn help(&self, app: &crate::app::App, row: WizardRow) -> Help {
        let catalog = &app.catalog;
        match row {
            WizardRow::Provider(p) => {
                let Some(provider) = self.providers(catalog).into_iter().nth(p) else {
                    return Help::new("", "");
                };
                let harnesses = provider_harnesses(catalog, provider);
                let pick = match harnesses.len() {
                    1 => String::new(),
                    _ => " — the next step picks one".to_string(),
                };
                let text = format!(
                    "{} It runs {}{pick}.",
                    provider_blurb(provider),
                    or_list(&harnesses)
                );
                Help::new(provider.label(), text.trim_start())
            }
            WizardRow::Agent(h) => self.agent_help(app, h),
            WizardRow::Connection(choice) => self.connection_help(app, choice),
            WizardRow::Field(field) => self.field_help(app, field),
            WizardRow::Next => {
                let next = (self.next_step(self.wizard_step(), &app.catalog))
                    .unwrap_or(CreateStep::Review);
                let text = format!(
                    "Checks this step's answers, then goes to step {}, {}.",
                    next.number(),
                    next.title()
                );
                Help::new("Next", text)
            }
            WizardRow::Back => match self.prev_step(self.wizard_step(), &app.catalog) {
                Some(prev) => Help::new(
                    "Back",
                    format!(
                        "Goes back to step {}, {}, keeping every answer.",
                        prev.number(),
                        prev.title()
                    ),
                ),
                None => Help::new(
                    "Back",
                    "This is the first step: there is nothing before it.",
                ),
            },
            WizardRow::CreateStart => Help::new(
                "Create & start",
                format!("Creates {} and opens a session in it.", self.sbx_name(app)),
            ),
            WizardRow::CreateOnly => Help::new(
                "Create only",
                format!(
                    "Creates {} and goes back to the list without starting it.",
                    self.sbx_name(app)
                ),
            ),
            WizardRow::Cancel => Help::new(
                "Cancel",
                "Closes the wizard and discards these answers. Esc on step 1 closes it and keeps \
                 them for the next New.",
            ),
        }
    }

    fn agent_help(&self, app: &crate::app::App, h: usize) -> Help {
        let catalog = &app.catalog;
        let Some(harness) = self.harnesses(catalog).into_iter().nth(h) else {
            return Help::new("", "");
        };
        let Some(b) = harness.backends.first().map(|b| &catalog.backends[*b]) else {
            return Help::new("", "");
        };
        let way = match b.account() {
            Some(_) => "It signs in with the /login or a setup token, picked below.".to_string(),
            None if b.litellm_agent().is_some() => {
                let no_account = match b.harness() {
                    "claude" => " No Anthropic account is needed.",
                    _ => "",
                };
                format!(
                    "It sends its requests to your LiteLLM gateway with the connection's key, \
                     which stays in the host keychain; the sandbox only sees a \
                     placeholder.{no_account}"
                )
            }
            None => format!("{}.", capitalized(b.setup_hint())),
        };
        let text = format!("{} {way}", harness_blurb(&harness.agent));
        let picked = harness.backends.contains(&self.harness.backend);
        let warn = picked.then(|| self.missing_warning(app)).flatten();
        Help::new(harness.label, text.trim_start()).warn(warn)
    }

    /// What the picked backend still lacks on this host, with the commands that store it.
    fn missing_warning(&self, app: &crate::app::App) -> Option<String> {
        let missing = (app.catalog.backends.get(self.harness.backend))
            .map(|b| app.missing_secrets(b))
            .unwrap_or_default();
        (!missing.is_empty()).then(|| {
            format!(
                "{} is not stored on this host: {}",
                missing.join(" and "),
                missing
                    .iter()
                    .map(|s| crate::kits::secret_set_hint(s))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })
    }

    fn connection_help(&self, app: &crate::app::App, choice: ConnChoice) -> Help {
        let catalog = &app.catalog;
        let title = connection_label(choice);
        let text = match choice {
            ConnChoice::Login => {
                "Sign in once with /login inside the sandbox. sbx keeps the login \
                on the host, so every /login sandbox shares one account and a rebuild never loses \
                it."
                .to_string()
            }
            ConnChoice::Token => "A token from claude setup-token, stored on this host under a \
                name; the sandbox only sees a placeholder. Every sandbox on one name shares that \
                account, independent of the /login."
                .to_string(),
        };
        let picked = self.connection_selected(choice, catalog);
        let warn = picked.then(|| self.missing_warning(app)).flatten();
        Help::new(title, text).warn(warn)
    }

    fn field_help(&self, app: &crate::app::App, row: CreateRow) -> Help {
        let catalog = &app.catalog;
        let ws = &self.workspace;
        let help = match row {
            CreateRow::LitellmConn => match self.litellm_choice() {
                Some(conn) => {
                    let text = format!(
                        "{} at {}. The sandbox reads the gateway's models itself, and the agent \
                         picks one. ←→ picks another connection, or {NEW_CONNECTION} to add one \
                         with this sandbox.",
                        conn.name,
                        crate::litellm::host(&conn.url),
                    );
                    let warn = app
                        .litellm_key_missing(&conn.name)
                        .then(|| missing_key_note(conn));
                    Help::new("connection", text).warn(warn)
                }
                None => Help::new(
                    "connection",
                    "A new connection, stored with this sandbox: its key first, then its file. \
                     ←→ picks an existing one.",
                ),
            },
            CreateRow::LitellmName => Help::new(
                "name",
                "The name the connection is listed under on Secrets (S): a–z, 0–9 and “-”, at most \
                 32 characters.",
            ),
            CreateRow::LitellmUrl => Help::new(
                "URL",
                "The gateway's address, https://host with an optional :443 and path. The key is \
                 sent to this host only.",
            ),
            CreateRow::LitellmKey => Help::new(
                "key",
                "The gateway's key. It goes to the host keychain on stdin — never into a file, \
                 the registry or the sandbox.",
            ),
            CreateRow::TokenPick(i) => {
                let token = self.connection.tokens.get(i).cloned().unwrap_or_default();
                let users = token_users(app.token_users(&token));
                let users = match users.is_empty() {
                    true => String::new(),
                    false => format!(" {}.", capitalized(&users)),
                };
                Help::new(
                    token,
                    format!(
                        "Already stored on this host, so there is nothing to paste. Every \
                         sandbox on this name uses that one account.{users}"
                    ),
                )
            }
            CreateRow::TokenNew => Help::new(
                "new token",
                "Run claude setup-token while signed in as the account you want, then name the \
                 token and paste it. Later sandboxes can pick it by its name.",
            ),
            CreateRow::TokenName => Help::new(
                "token name",
                "Lower-case letters, digits and dashes. Later sandboxes pick this account by the \
                 name.",
            ),
            CreateRow::Token => Help::new(
                "token",
                "The output of claude setup-token. It goes to the host keychain on stdin; the \
                 sandbox only ever sees a placeholder.",
            ),
            CreateRow::ConfigFile(i) => return app.config_file_help(self, i),
            CreateRow::Model(slot) => alias_help(slot),
            CreateRow::ModelDefault(slot) => default_model_help(slot),
            CreateRow::ModelPick(slot, i) => model_pick_help(catalog, slot, i),
            CreateRow::ModelsRefresh => {
                let when = match &catalog.models_source {
                    crate::models::Source::Gateway { fetched_at, .. } => format!(
                        "The list is from {}.",
                        crate::state::ago(u64::try_from(*fetched_at).unwrap_or(0))
                    ),
                    crate::models::Source::NotYet => "There is no list yet.".to_string(),
                };
                Help::new(
                    "refresh",
                    format!(
                        "{when} ↵ asks the gateway again, through a small sandbox that only reaches \
                         its host. The list is also fetched once a day when aisbx starts."
                    ),
                )
            }
            CreateRow::ModelOn(i) => {
                let (name, id) = catalog
                    .models
                    .get(i)
                    .map(|m| (m.name.clone(), m.id.clone()))
                    .unwrap_or_default();
                let on = !self.connection.disabled.contains(&id);
                let text = match on {
                    true => format!(
                        "{name} ({id}) is offered to the agent. Space switches it off for every \
                         sandbox on this connection and harness, from its next start."
                    ),
                    false => format!(
                        "{name} ({id}) is switched off: the agent is not offered it. Space \
                         switches it on again."
                    ),
                };
                Help::new(id.clone(), text)
            }
            CreateRow::Addon(i) => {
                let Some(addon) = catalog.addons.get(i) else {
                    return Help::new("", "");
                };
                let mut text = Vec::new();
                if let Some(found) = self.kits.host.get(i).and_then(Option::as_ref) {
                    text.push(match found.detected() {
                        true => format!("Mounts {} read-only.", found.found().join(", ")),
                        false => format!(
                            "Nothing to mount on this host: {}. It cannot be ticked here.",
                            found.reason(&addon.kit.name)
                        ),
                    });
                }
                if let Some(reason) = addon
                    .create_time_reason()
                    .filter(|_| !self.addon_unavailable(i))
                {
                    text.push(only_now_note(reason));
                }
                text.push(addon.kit.first_sentence().to_string());
                Help::new(addon.kit.name.clone(), text.join(" ").trim().to_string())
            }
            CreateRow::Name => Help::new(
                "label",
                match ws.name_touched {
                    false => {
                        "The name the sandbox list shows, generated from the agent and the kits \
                         while it is left alone. Type to replace it, or use ← → to edit it."
                    }
                    true => "The name the sandbox list shows. It stays as typed.",
                },
            ),
            CreateRow::StartDir => Help::new(
                "opens in",
                format!(
                    "All of {} is mounted; this sets the folder the agent starts in, which it \
                     treats as its project. ↵ picks a subfolder; .. at the top mounts the folder \
                     above instead.",
                    crate::model::tilde(&self.workspace_root(&app.cwd))
                ),
            ),
            CreateRow::Clone => Help::new(
                "clone mode",
                "The agent works on a git clone made inside the sandbox, so your files stay \
                 read-only. Needs a git repository; its commits come back as the git remote \
                 sandbox-<name>.",
            ),
            CreateRow::GitGuard => Help::new(
                "git folder read-only",
                "Mounts the workspace's .git folder a second time, read-only: nothing in the \
                 sandbox, not even sudo, can change its hooks, config or history, so no git hook \
                 the agent writes runs on your host. git status, log and diff work; commits are \
                 yours to make.",
            ),
            CreateRow::Skills => {
                let text = match ws.skills {
                    crate::sbx::SkillsMode::Readonly => {
                        "The agent reads the host's skills store and cannot change it, so nothing \
                         this sandbox does reaches another one. Fill the store with K."
                    }
                    crate::sbx::SkillsMode::Readwrite => {
                        "The agent can rewrite a skill in the host store, and every other sharing \
                         sandbox then loads it: one trust boundary around all of them."
                    }
                    crate::sbx::SkillsMode::Off => {
                        "No skills mount: this sandbox sees nothing of the shared store."
                    }
                };
                Help::new(
                    "share skills",
                    format!("{text} Fixed at creation; changing it later means a rebuild."),
                )
            }
            CreateRow::ExtraFolders => Help::new(
                "more folders",
                "Optional. Host folders for this sandbox alone, comma-separated: ~/docs:ro read-only, \
                 ~/libs:/workspace/libs at that path. A rebuild keeps them.",
            ),
            CreateRow::Ports => Help::new(
                "ports",
                "8080:3000 binds host port 8080 to sandbox port 3000; a bare 3000 takes any free \
                 host port. The service must listen on 0.0.0.0 in the sandbox, not 127.0.0.1.",
            ),
            CreateRow::DenyNetwork => Help::new(
                "blocked hosts",
                "Optional; most leave it empty. Hosts this sandbox alone may not reach, say \
                 github.com, which the balanced policy opens: a.example.com, *.example.com, ** \
                 for all. A deny beats every allow.",
            ),
            CreateRow::McpNone => Help::new(
                "no MCP servers",
                "Optional. The agent gets no MCP server and no tools to find or add one: sbx's \
                 --static-mcp none. Untick it and tick no server, and the agent finds the host's \
                 registered servers itself. Later: sbx mcp load <name> --sandbox <sandbox>.",
            ),
            CreateRow::Mcp(i) => {
                let Some(server) = ws.mcp.get(i) else {
                    return Help::new("", "");
                };
                let warn = (!server.ready()).then(|| {
                    format!(
                        "{} is not ready ({}): authorize it on the host with sbx mcp auth {} \
                         first.",
                        server.name, server.status, server.name
                    )
                });
                Help::new(
                    server.name.clone(),
                    "Reached through sbx's MCP gateway, so the server's credentials stay on the \
                     host. Fixed at creation; later: sbx mcp load <name> --sandbox <sandbox>.",
                )
                .warn(warn)
            }
            // The wizard's radios stand in for these: they are never one of its rows.
            CreateRow::Backend(_) | CreateRow::AuthOauth | CreateRow::AuthToken => {
                Help::new("", "")
            }
        };
        Help {
            problem: self.visible_problem(row, catalog),
            ..help
        }
    }
}

/// `text` with its first letter in upper case.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What a key on one of the form's rows ([`CreateForm::field_key`]) leaves for the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldEffect {
    /// Not the row's key: the wizard's own keys — moving, the steps, the buttons — take it.
    Ignored,
    /// The row took the key; nothing is left to do.
    Done,
    /// Open the start-folder picker, which moves the whole form into its own screen and hands it
    /// back afterwards.
    OpenStartDir,
    /// Fetch the connection's model list again, a job that hands the form back on its Models step.
    FetchModels,
    /// Open a config file of the connection in an editor, by its row's index.
    EditConfig(usize),
    /// Drop the edited copy of a config file, by its row's index.
    ResetConfig(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Screen};
    use crate::kits::Account;
    use crate::sbx::McpServer;

    /// The *Now* line: the account and what it is shared with.
    #[test]
    fn auth_now_says_what_the_account_is_shared_with() {
        use crate::model::Auth;
        let token = |stored| Auth::Token {
            name: "work".into(),
            placeholder: crate::sbx::named_token_placeholder("work"),
            stored,
        };
        assert_eq!(auth_now(&Auth::Login, 0).0, "/login (OAuth)");
        assert_eq!(
            auth_now(&token(false), 1),
            (
                "setup token “work” — not stored".to_string(),
                "it cannot authenticate until a token is stored below".to_string()
            )
        );
        assert_eq!(
            auth_now(&token(true), 1).1,
            "its own account, independent of the /login"
        );
        assert_eq!(auth_now(&token(true), 2).1, "shared with 1 other sandbox");
        assert_eq!(auth_now(&token(true), 4).1, "shared with 3 other sandboxes");
    }

    fn typed(name: &str, url: &str, key: &str) -> NewConnection {
        NewConnection {
            name: TextInput::new(name),
            url: TextInput::new(url),
            key: TextInput::new(key),
        }
    }

    /// The one check of a typed connection (valid fields, a new name, a key), with its first
    /// problem.
    #[test]
    fn a_typed_connection_is_checked_by_one_rule() {
        let existing = [crate::testkit::acme()];
        let refused = |name: &str, url: &str, key: &str| {
            typed(name, url, key).check(&existing).expect_err("refused")
        };
        for url in [
            "http://x",
            "https://h:4000",
            "https://h;rm",
            "https://h/a b",
        ] {
            assert!(!refused("beta", url, "k").is_empty(), "{url}");
        }
        for name in ["Name_1", "a\"b", ""] {
            assert!(refused(name, "https://h", "k").starts_with("name the connection"));
        }
        assert!(refused("acme", "https://h", "k").starts_with("connection acme exists"));
        assert!(refused("beta", "https://h", " ").starts_with("paste the gateway's key"));
        for url in ["https://h", "https://h:443/litellm"] {
            let conn = typed("beta", url, "k").check(&existing).expect("accepted");
            assert_eq!(
                conn,
                crate::litellm::Connection {
                    name: "beta".into(),
                    url: url.into()
                }
            );
        }
        // An edit: the name is the old one's, and the key may stay empty.
        let acme = crate::testkit::acme();
        let edit = typed("ignored", "https://h/v2", "")
            .check_edit(&acme)
            .unwrap();
        assert_eq!(
            (edit.name.as_str(), edit.url.as_str()),
            ("acme", "https://h/v2")
        );
        assert!(typed("", "http://h", "").check_edit(&acme).is_err());
    }

    /// The create form refuses with the same rule.
    #[test]
    fn the_create_form_checks_a_new_connection_with_the_shared_rule() {
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let form = form_mut(&mut app);
        form.connection.new_conn = typed("acme", "https://h", "k");
        assert_eq!(
            form.new_connection(),
            form.connection.new_conn.check(&form.connection.connections)
        );
        assert!(form.new_connection().is_err(), "acme is every test app's");
    }

    fn form(app: &App) -> &CreateForm {
        match &app.screen {
            Screen::Create(form) => form,
            _ => panic!("the create form is open"),
        }
    }

    fn form_mut(app: &mut App) -> &mut CreateForm {
        match &mut app.screen {
            Screen::Create(form) => form,
            _ => panic!("the create form is open"),
        }
    }

    fn subscription(app: &App) -> usize {
        app.catalog
            .backends
            .iter()
            .position(|b| b.account() == Some(Account::Login))
            .expect("the /login backend is offered")
    }

    fn server(name: &str) -> McpServer {
        McpServer {
            name: name.into(),
            status: "ready".into(),
            ..McpServer::default()
        }
    }

    /// Each step opens with a line of its own, never its title alone (the trail shows the title),
    /// and the Harness step's names the provider picked.
    #[test]
    fn every_step_opens_with_what_it_decides() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        for step in CreateStep::ALL {
            let (heading, _) = f.step_heading(step, &catalog);
            assert!(!heading.is_empty() && heading != step.title(), "{step:?}");
        }
        let (heading, _) = f.step_heading(CreateStep::Harness, &catalog);
        assert_eq!(heading, "The agent that works with LiteLLM");
        let anthropic = f
            .providers(&catalog)
            .iter()
            .position(|p| *p == Provider::Anthropic)
            .expect("Anthropic is a provider");
        f.pick_provider(&catalog, anthropic);
        let (heading, _) = f.step_heading(CreateStep::Harness, &catalog);
        assert_eq!(heading, "The agent that works with Anthropic");
        let (_, gloss) = f.step_heading(CreateStep::Kits, &catalog);
        assert!(
            gloss.contains(ONLY_NOW),
            "the Kits step says what its marker means"
        );
    }

    /// The harness and account radios say the same in both frontends: the gateway names the
    /// Anthropic account only for Claude Code; the setup token says why while it is not picked
    /// and what to do once it is.
    #[test]
    fn the_harness_and_account_radios_say_what_each_way_means() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let backend = |name: &str| {
            let at = catalog.backend(name);
            at.unwrap_or_else(|| panic!("{name} is a backend"))
        };
        let claude = harness_hint(backend("claude-litellm"));
        assert!(claude.contains("no Anthropic account needed"), "{claude}");
        for gateway in ["opencode-litellm", "pi-litellm"] {
            let hint = harness_hint(backend(gateway));
            assert!(!hint.contains("Anthropic"), "{gateway}: {hint}");
        }
        // Any other backend says what its own setup hint says.
        let codex = backend("codex-openai");
        assert_eq!(harness_hint(codex), codex.setup_hint());
        let login = harness_hint(backend("claude-anthropic"));
        assert!(login.contains("/login or a setup token"), "{login}");
        let f = form_mut(&mut app);
        assert!(
            f.connection_choices(&catalog).is_empty(),
            "no account on LiteLLM"
        );
        let login = catalog.backends.iter().position(|b| b.account().is_some());
        f.harness.backend = login.expect("the /login backend");
        assert_eq!(
            f.connection_choices(&catalog),
            [ConnChoice::Login, ConnChoice::Token]
        );
        assert_eq!(connection_hint(ConnChoice::Login, f, &catalog), LOGIN_HINT);

        let token = |f: &CreateForm| connection_hint(ConnChoice::Token, f, &catalog);
        assert_eq!(token(f), "a second, independent Anthropic subscription");
        f.pick_connection(&catalog, ConnChoice::Token);
        f.connection.tokens = Vec::new();
        assert_eq!(token(f), token_hint(true, 0));
        assert!(token(f).starts_with("name it and paste it in"));
        f.connection.tokens = vec!["work".into()];
        assert_eq!(token(f), "pick one below, or add a new one");

        assert_eq!(token_users(0), "");
        assert_eq!(token_users(1), "1 sandbox uses it");
        assert_eq!(token_users(3), "3 sandboxes use it");
    }

    /// The notes that say where to go: a missing key names the host and the connection to pick on
    /// `S`; a kit that is only for now names what `sbx kit add` does not apply.
    #[test]
    fn the_notes_name_what_to_do() {
        let note = missing_key_note(&crate::testkit::acme());
        assert!(note.contains("litellm.example.com"), "{note}");
        assert!(
            note.contains("Secrets (S)") && note.contains("acme"),
            "{note}"
        );
        let note = only_now_note("network rules");
        assert!(
            note.contains("network rules") && note.contains("rebuild"),
            "{note}"
        );
    }

    /// The Connection step's heading is about the connection alone: the model list and the
    /// harness are later steps.
    #[test]
    fn the_connection_heading_names_no_later_step() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let (heading, gloss) = form(&app).step_heading(CreateStep::Connection, &app.catalog);
        let text = format!("{heading} {gloss}").to_lowercase();
        for later in ["model", "next", "harness", "agent", "week", "day"] {
            assert!(!text.contains(later), "{later}: {text}");
        }
    }

    #[test]
    fn step_titles() {
        let numbered: Vec<(usize, &str)> = CreateStep::ALL
            .iter()
            .map(|s| (s.number(), s.title()))
            .collect();
        assert_eq!(
            numbered,
            [
                (1, "Provider"),
                (2, "Connection"),
                (3, "Harness"),
                (4, "Models"),
                (5, "Kits"),
                (6, "Workspace and options"),
                (7, "Review"),
            ]
        );
    }

    #[test]
    fn row_steps() {
        let slot = Slot::ALL[0];
        let cases = [
            (CreateRow::Backend(0), CreateStep::Provider),
            (CreateRow::LitellmConn, CreateStep::Connection),
            (CreateRow::LitellmName, CreateStep::Connection),
            (CreateRow::LitellmUrl, CreateStep::Connection),
            (CreateRow::LitellmKey, CreateStep::Connection),
            (CreateRow::AuthOauth, CreateStep::Harness),
            (CreateRow::AuthToken, CreateStep::Harness),
            (CreateRow::TokenPick(0), CreateStep::Harness),
            (CreateRow::TokenNew, CreateStep::Harness),
            (CreateRow::TokenName, CreateStep::Harness),
            (CreateRow::Token, CreateStep::Harness),
            (CreateRow::ModelsRefresh, CreateStep::Models),
            (CreateRow::Model(slot), CreateStep::Models),
            (CreateRow::ModelDefault(slot), CreateStep::Models),
            (CreateRow::ModelPick(slot, 0), CreateStep::Models),
            (CreateRow::ModelOn(0), CreateStep::Models),
            (CreateRow::Addon(0), CreateStep::Kits),
            (CreateRow::Name, CreateStep::Workspace),
            (CreateRow::StartDir, CreateStep::Workspace),
            (CreateRow::Clone, CreateStep::Workspace),
            (CreateRow::Skills, CreateStep::Workspace),
            (CreateRow::Ports, CreateStep::Workspace),
            (CreateRow::DenyNetwork, CreateStep::Workspace),
            (CreateRow::Mcp(0), CreateStep::Workspace),
        ];
        for (row, step) in cases {
            assert_eq!(row.step(), step, "{row:?}");
        }
    }

    /// Joining the steps' rows gives back the whole form, in order, whatever state it is in.
    #[test]
    fn rows_of_partitions_rows() {
        let check = |app: &App, state: &str| {
            let form = form(app);
            let joined: Vec<CreateRow> = CreateStep::ALL
                .iter()
                .flat_map(|s| form.rows_of(*s, &app.catalog))
                .collect();
            assert_eq!(joined, form.rows(&app.catalog), "{state}");
        };

        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        check(&app, "defaults");

        let subscription = subscription(&app);
        let f = form_mut(&mut app);
        f.harness.backend = subscription;
        f.connection.account = AccountChoice::New;
        check(&app, "subscription, new token");
        assert!(form(&app).rows(&app.catalog).contains(&CreateRow::Token));

        app.open_create();
        form_mut(&mut app).connection.model_pick = Some(Slot::ALL[0]);
        check(&app, "models open");

        app.mcp = vec![server("github"), server("notion")];
        app.open_create();
        check(&app, "MCP servers");
        assert!(form(&app).rows(&app.catalog).contains(&CreateRow::Mcp(1)));
    }

    /// Pi is a harness of its own whose one way to its model is a LiteLLM connection, with the
    /// same Connection row as the other gateway backends — no Pi-specific form code.
    #[test]
    fn pi_is_offered_on_a_litellm_connection() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let pi = app
            .catalog
            .backends
            .iter()
            .position(|b| b.kit.name == "pi-litellm")
            .expect("kits/pi-litellm is a backend");
        let harness = form(&app)
            .harnesses(&app.catalog)
            .into_iter()
            .find(|h| h.agent == "pi")
            .expect("Pi is a harness");
        assert_eq!(harness.label, "Pi");
        assert_eq!(harness.backends, [pi]);
        form_mut(&mut app).harness.backend = pi;
        assert!(form(&app).litellm_backend(&app.catalog));
        assert!(form(&app)
            .rows_of(CreateStep::Connection, &app.catalog)
            .contains(&CreateRow::LitellmConn));
    }

    /// Step 1 offers each provider once, and step 3 only the harnesses of the picked one; a
    /// provider picked again brings back the harness last picked for it.
    #[test]
    fn the_harness_step_offers_only_the_providers_harnesses() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        assert_eq!(
            f.providers(&catalog),
            [
                Provider::Anthropic,
                Provider::Google,
                Provider::OpenAi,
                Provider::Litellm
            ]
        );
        assert_eq!(f.selected_provider(&catalog), Some(Provider::Litellm));
        let agents = |f: &CreateForm| -> Vec<String> {
            (f.harnesses(&catalog).into_iter())
                .map(|h| h.agent)
                .collect()
        };
        assert_eq!(agents(f), ["claude", "opencode", "pi"]);
        let pi = agents(f).iter().position(|a| a == "pi").unwrap();
        f.pick_harness(&catalog, pi);
        for (p, want) in [
            (Provider::Anthropic, vec!["claude"]),
            (Provider::Google, vec!["agy"]),
            (Provider::OpenAi, vec!["codex"]),
        ] {
            let at = f.providers(&catalog).iter().position(|o| *o == p).unwrap();
            f.pick_provider(&catalog, at);
            assert_eq!(f.selected_provider(&catalog), Some(p));
            assert_eq!(agents(f), want, "{p:?}");
        }
        let litellm = (f.providers(&catalog).iter())
            .position(|o| *o == Provider::Litellm)
            .unwrap();
        f.pick_provider(&catalog, litellm);
        assert_eq!(f.selected_agent(&catalog), Some("pi"), "Pi comes back");
        let review = f.review(&app_ref(&catalog));
        let answer = |label: &str| {
            let a = review.answers.iter().find(|a| a.label == label);
            a.map(|a| (a.step, a.value.clone()))
        };
        assert_eq!(
            answer("Provider"),
            Some((CreateStep::Provider, "LiteLLM".into()))
        );
        assert_eq!(answer("Harness"), Some((CreateStep::Harness, "Pi".into())));
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn the_details_name_a_fixed_mcp_set_and_none_but_not_the_dynamic_mode() {
        assert_eq!(mcp_set(&[]), None);
        let set = ["github".to_string(), "notion".to_string()];
        assert_eq!(mcp_set(&set).as_deref(), Some("github notion"));
        let none = mcp_set(&["NONE".to_string()]).expect("named");
        assert!(
            none.starts_with("none: ") && none.ends_with(NO_MCP),
            "{none}"
        );
    }

    /// The row the wizard's one cursor is on.
    fn focused(form: &CreateForm, catalog: &Catalog) -> WizardRow {
        form.wizard_rows(catalog)[form.wizard_focus()]
    }

    #[test]
    fn field_key_ticks_cycles_and_types() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.mcp = vec![server("notion")];
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        let space = key(KeyCode::Char(' '));

        // A tick box flips on space and on ↵, and the cursor stays on it.
        assert!(f.focus_field(&catalog, CreateRow::Clone));
        let done = f.field_key(&catalog, CreateRow::Clone, space);
        assert_eq!(done, FieldEffect::Done);
        assert!(f.workspace.clone);
        f.field_key(&catalog, CreateRow::Clone, key(KeyCode::Enter));
        assert!(!f.workspace.clone);
        assert_eq!(focused(f, &catalog), WizardRow::Field(CreateRow::Clone));
        f.field_key(&catalog, CreateRow::Mcp(0), key(KeyCode::Enter));
        assert_eq!(f.workspace.static_mcp, vec![true]);
        f.field_key(&catalog, CreateRow::McpNone, key(KeyCode::Enter));
        assert!(f.workspace.no_mcp && f.workspace.static_mcp == vec![false]);
        f.field_key(&catalog, CreateRow::McpNone, space);
        assert!(!f.workspace.no_mcp);
        f.field_key(&catalog, CreateRow::Mcp(0), space);

        // The skills mode walks on space and on ←→; on any other row ←→ is the wizard's.
        let start = f.workspace.skills;
        f.field_key(&catalog, CreateRow::Skills, key(KeyCode::Right));
        assert_eq!(f.workspace.skills, start.next());
        f.field_key(&catalog, CreateRow::Skills, key(KeyCode::Left));
        assert_eq!(f.workspace.skills, start);
        f.field_key(&catalog, CreateRow::Skills, space);
        assert_eq!(f.workspace.skills, start.next());
        let right = f.field_key(&catalog, CreateRow::Clone, key(KeyCode::Right));
        assert_eq!(right, FieldEffect::Ignored);

        // A text row takes typing and leaves ↵ and ↑↓ to the wizard; a typed label sticks.
        let typed = f.field_key(&catalog, CreateRow::Ports, key(KeyCode::Char('8')));
        assert_eq!(typed, FieldEffect::Done);
        assert_eq!(f.workspace.ports.value, "8");
        for code in [KeyCode::Enter, KeyCode::Down, KeyCode::Esc] {
            let left = f.field_key(&catalog, CreateRow::Ports, key(code));
            assert_eq!(left, FieldEffect::Ignored, "{code:?}");
        }
        assert!(!f.workspace.name_touched);
        f.field_key(&catalog, CreateRow::Name, key(KeyCode::Char('x')));
        assert!(f.workspace.name_touched);

        // The start folder is the app's to open: the picker takes the whole screen.
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let effect = f.field_key(&catalog, CreateRow::StartDir, key(code));
            assert_eq!(effect, FieldEffect::OpenStartDir, "{code:?}");
        }
    }

    /// The generated label is a suggestion: the first character typed replaces it, and an arrow
    /// takes it over for editing instead. Once typed, a label is edited like any other field.
    #[test]
    fn the_first_character_typed_replaces_the_generated_label() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        let generated = f.workspace.name.value.clone();
        assert!(!generated.is_empty() && !f.workspace.name_touched);
        for c in "api".chars() {
            f.field_key(&catalog, CreateRow::Name, key(KeyCode::Char(c)));
        }
        assert_eq!(f.workspace.name.value, "api");
        assert!(f.workspace.name_touched);

        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let f = form_mut(&mut app);
        f.field_key(&catalog, CreateRow::Name, key(KeyCode::Left));
        f.field_key(&catalog, CreateRow::Name, key(KeyCode::Char('!')));
        let mut edited = generated.clone();
        edited.insert(generated.len() - 1, '!');
        assert_eq!(f.workspace.name.value, edited);

        // Any other text row keeps what it holds.
        f.workspace.ports = TextInput::new("80");
        f.field_key(&catalog, CreateRow::Ports, key(KeyCode::Char('8')));
        assert_eq!(f.workspace.ports.value, "808");
    }

    /// A step's digest is what it decided, then only what was changed: the TUI's ledger line and
    /// the GUI's rail entry. An answer's brief drops its explanation.
    #[test]
    fn a_steps_digest_is_its_decision_and_what_was_changed() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let values = |app: &App, step| {
            let review = form(app).review(app);
            let digest: Vec<String> = review
                .digest(step)
                .iter()
                .map(|a| format!("{}={}", a.label, a.brief()))
                .collect();
            digest.join(" ")
        };
        assert_eq!(values(&app, CreateStep::Provider), "Provider=LiteLLM");
        assert_eq!(values(&app, CreateStep::Connection), "Connection=acme");
        assert_eq!(values(&app, CreateStep::Harness), "Harness=Claude Code");
        assert_eq!(values(&app, CreateStep::Kits), "Kits=none");
        let label = form(&app).workspace.name.value.clone();
        assert_eq!(
            values(&app, CreateStep::Workspace),
            format!("Label={label}")
        );
        assert_eq!(values(&app, CreateStep::Review), "");

        let f = form_mut(&mut app);
        f.workspace.clone = true;
        f.workspace.ports = TextInput::new("8080:3000");
        assert_eq!(
            values(&app, CreateStep::Workspace),
            format!("Label={label} Clone mode=on Ports=8080:3000")
        );
        let review = form(&app).review(&app);
        let clone = review.answers.iter().find(|a| a.label == "Clone mode");
        assert!(clone.is_some_and(|a| a.value.len() > a.brief().len()));
    }

    #[test]
    fn field_key_steps_into_what_it_opens_and_back_onto_the_alias() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let slot = Slot::ALL[0];
        let f = form_mut(&mut app);
        let alias = WizardRow::Field(CreateRow::Model(slot));

        // ↵ opens an alias's list and steps onto its first entry.
        assert!(
            f.focus_field(&catalog, CreateRow::Model(slot)),
            "the gateway pins models"
        );
        f.field_key(&catalog, CreateRow::Model(slot), key(KeyCode::Enter));
        assert_eq!(f.connection.model_pick, Some(slot));
        let first = WizardRow::Field(CreateRow::ModelDefault(slot));
        assert_eq!(focused(f, &catalog), first);
        // Picking a model pins it, closes the list and goes back onto the alias.
        f.field_key(&catalog, CreateRow::ModelPick(slot, 0), key(KeyCode::Enter));
        let id = catalog.models[0].id.as_str();
        assert_eq!(f.connection.models.get(slot), Some(id));
        assert_eq!(f.connection.model_pick, None);
        assert_eq!(focused(f, &catalog), alias);
        // Space opens it too, without moving; "(default)" unpins.
        f.field_key(&catalog, CreateRow::Model(slot), key(KeyCode::Char(' ')));
        assert_eq!(f.connection.model_pick, Some(slot));
        assert_eq!(focused(f, &catalog), alias);
        let space = key(KeyCode::Char(' '));
        f.field_key(&catalog, CreateRow::ModelDefault(slot), space);
        assert_eq!(f.connection.models.get(slot), None);
        assert_eq!(focused(f, &catalog), alias);

        // "new token…" opens the name and value rows and steps onto the name; a stored token is
        // picked where it is.
        let subscription = subscription(&app);
        let f = form_mut(&mut app);
        f.harness.backend = subscription;
        f.connection.tokens = vec!["work".into()];
        f.connection.account = AccountChoice::Named("work".into());
        assert!(f.focus_field(&catalog, CreateRow::TokenNew));
        f.field_key(&catalog, CreateRow::TokenNew, key(KeyCode::Enter));
        assert_eq!(f.connection.account, AccountChoice::New);
        assert_eq!(focused(f, &catalog), WizardRow::Field(CreateRow::TokenName));
        f.field_key(&catalog, CreateRow::TokenPick(0), key(KeyCode::Char(' ')));
        assert_eq!(f.connection.account, AccountChoice::Named("work".into()));
        let picked = WizardRow::Field(CreateRow::TokenPick(0));
        assert_eq!(focused(f, &catalog), picked);
    }

    /// A field's problem shows beside it once it is worth saying: not while the field is typed
    /// into, nor while a required one is empty — until Next is refused on its step.
    #[test]
    fn a_fields_problem_shows_once_it_is_worth_saying() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let subscription = subscription(&app);
        let f = form_mut(&mut app);
        assert!(f.focus_field(&catalog, CreateRow::Ports));
        f.workspace.ports = TextInput::new("nop");
        f.field_key(&catalog, CreateRow::Ports, key(KeyCode::Char('e')));
        assert_eq!(f.workspace.ports.value, "nope");
        assert_eq!(
            f.field_problem(CreateRow::Ports, &catalog).as_deref(),
            Some(BAD_PORT)
        );
        assert_eq!(
            f.visible_problem(CreateRow::Ports, &catalog),
            None,
            "being typed"
        );
        assert!(f.focus_field(&catalog, CreateRow::DenyNetwork));
        let shown = f.visible_problem(CreateRow::Ports, &catalog);
        assert_eq!(
            shown.as_deref(),
            Some(BAD_PORT),
            "left with something typed"
        );
        assert!(f.focus_field(&catalog, CreateRow::Ports));
        let back = f.visible_problem(CreateRow::Ports, &catalog);
        assert_eq!(
            back.as_deref(),
            Some(BAD_PORT),
            "and when the cursor is back"
        );
        assert_eq!(f.field_problem(CreateRow::Clone, &catalog), None);

        f.harness.backend = subscription;
        f.connection.account = AccountChoice::New;
        f.connection.token_name = TextInput::new("personal");
        f.wizard_set_step(&catalog, CreateStep::Harness);
        let token = f.field_problem(CreateRow::Token, &catalog);
        assert_eq!(token.as_deref(), Some(NO_VALUE));
        assert_eq!(f.visible_problem(CreateRow::Token, &catalog), None, "empty");
        assert!(f.wizard_next(&catalog).is_err());
        assert!(f.wizard.refused);
        let shown = f.visible_problem(CreateRow::Token, &catalog);
        assert_eq!(shown.as_deref(), Some(NO_VALUE), "after a refusal");
        let help = f.help(&app_ref(&catalog), WizardRow::Field(CreateRow::Token));
        assert_eq!(help.problem.as_deref(), Some(NO_VALUE));
    }

    /// More folders are typed on the Workspace step, refused there by the first bad entry, and
    /// reviewed as typed, with what sbx gets among the mounts.
    #[test]
    fn more_folders_are_checked_on_their_step_and_reviewed_with_their_targets() {
        let root = std::env::temp_dir().join(format!("aisbx-tests-more-{}", std::process::id()));
        for d in ["proj", "data"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = root.join("proj");
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        let rows = f.rows_of(CreateStep::Workspace, &catalog);
        let at = |row| rows.iter().position(|r| *r == row);
        assert_eq!(
            at(CreateRow::ExtraFolders),
            at(CreateRow::Clone).map(|i| i - 1)
        );
        assert!(CreateRow::ExtraFolders.is_text());
        assert!(f.focus_field(&catalog, CreateRow::ExtraFolders));
        for c in "docs".chars() {
            f.field_key(&catalog, CreateRow::ExtraFolders, key(KeyCode::Char(c)));
        }
        let relative = "“docs”: give the host folder as an absolute path or ~/…";
        let problem = f.field_problem(CreateRow::ExtraFolders, &catalog);
        assert_eq!(problem.as_deref(), Some(relative));
        let step = f.step_problem(CreateStep::Workspace, &catalog);
        assert_eq!(step.as_deref(), Some(relative));
        assert!(f.wizard_goto(&catalog, CreateStep::Review).is_err());

        let typed = format!("{}:/workspace/data:ro", root.join("data").display());
        f.workspace.extra_folders = TextInput::new(typed.clone());
        assert_eq!(f.field_problem(CreateRow::ExtraFolders, &catalog), None);
        let review = match &app.screen {
            Screen::Create(form) => form.review(&app),
            _ => panic!("the create form is open"),
        };
        let more = review.answers.iter().find(|a| a.label == "More folders");
        assert_eq!(
            more.map(|a| (a.value.as_str(), a.default)),
            Some((typed.as_str(), false))
        );
        let shown = format!(
            "{}:/workspace/data:ro",
            crate::model::tilde(&root.join("data"))
        );
        assert_eq!(review.mounts.last(), Some(&shown), "{:?}", review.mounts);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An app for `help` to read: the one the form was opened in is borrowed by the form.
    fn app_ref(catalog: &std::sync::Arc<Catalog>) -> App {
        App::for_test(Catalog::clone(catalog), Vec::new())
    }

    /// Each field of *new connection…* says its own problem, and the first of them in field order
    /// is what a save refuses with.
    #[test]
    fn a_new_connections_fields_are_checked_one_by_one() {
        let existing = [crate::testkit::acme()];
        let typed = typed("acme", "http://x", " ");
        let problems: Vec<String> = ConnField::ALL
            .iter()
            .filter_map(|f| typed.field_problem(*f, &existing))
            .collect();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].starts_with("connection acme exists"));
        assert!(problems[1].contains("https://"));
        assert!(problems[2].starts_with("paste the gateway's key"));
        assert_eq!(typed.check(&existing).unwrap_err(), problems[0]);
        let fine = super::tests::typed("beta", "https://h", "k");
        for field in ConnField::ALL {
            assert_eq!(fine.field_problem(field, &existing), None, "{field:?}");
        }
    }

    /// The values a choice row offers, and which one is picked.
    #[test]
    fn a_choice_rows_values_are_offered_in_order() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let f = form_mut(&mut app);
        assert_eq!(
            f.choices(CreateRow::Skills),
            ["off", "read-only", "read-write"]
        );
        assert!(f.pick_choice(CreateRow::Skills, 0));
        assert_eq!(f.workspace.skills, crate::sbx::SkillsMode::Off);
        assert_eq!(f.chosen(CreateRow::Skills), Some(0));
        assert!(!f.pick_choice(CreateRow::Skills, 3));
        assert!(!f.pick_choice(CreateRow::Clone, 0), "not a choice row");
        assert!(f.choices(CreateRow::Clone).is_empty());
    }

    /// Every row of every step says what it is about, in the states the form can be in; the
    /// skills row's help follows the mode, a kit that only installs at creation says so, and Back
    /// on step 1 says why it does nothing.
    #[test]
    fn every_wizard_row_has_help() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.mcp = vec![server("notion")];
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let view = app_ref(&catalog);
        let subscription = subscription(&app);
        let check = |f: &mut CreateForm, state: &str| {
            for step in CreateStep::ALL {
                f.wizard_set_step(&catalog, step);
                for row in f.wizard_rows(&catalog) {
                    let help = f.help(&view, row);
                    assert!(
                        !help.title.is_empty() && !help.text.is_empty(),
                        "{state}: {step:?} {row:?} {help:?}"
                    );
                }
            }
        };
        let f = form_mut(&mut app);
        check(f, "defaults");
        f.connection.litellm = None;
        check(f, "new connection");
        f.connection.model_pick = Some(Slot::ALL[1]);
        check(f, "a model list open");
        f.harness.backend = subscription;
        f.connection.account = AccountChoice::New;
        check(f, "a new token");
        f.connection.tokens = vec!["work".into()];
        f.connection.account = AccountChoice::Named("work".into());
        check(f, "a stored token");

        let skills = |f: &CreateForm| f.help(&view, WizardRow::Field(CreateRow::Skills)).text;
        let texts: Vec<String> = SKILLS_MODES
            .iter()
            .map(|mode| {
                f.workspace.skills = *mode;
                skills(f)
            })
            .collect();
        assert!(texts[0] != texts[1] && texts[1] != texts[2], "{texts:?}");
        let web = catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "web")
            .expect("web is an addon");
        let help = f.help(&view, WizardRow::Field(CreateRow::Addon(web)));
        assert!(help.text.contains("Only now"), "{help:?}");
        f.wizard_set_step(&catalog, CreateStep::Provider);
        let back = f.help(&view, WizardRow::Back).text;
        assert!(back.contains("first step"), "{back}");
    }

    const BAD_PORT: &str =
        "“nope” is not a port mapping — use 8080:3000, 3000, or 127.0.0.1:8080:3000/tcp";
    const BAD_HOST: &str =
        "“bad!host” is not a host to deny — use api.example.com, *.example.com or **";
    const BAD_NAME: &str = "name the token in lower-case letters, digits and dashes";
    const TAKEN: &str = "a token of that name exists — pick it in the list";
    const NO_VALUE: &str =
        "paste a setup token, or pick /login — generate one with `claude setup-token`";

    #[test]
    fn step_problem_messages() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        for step in CreateStep::ALL {
            assert_eq!(form(&app).step_problem(step, &catalog), None, "{step:?}");
        }
        let workspace = |app: &App| form(app).step_problem(CreateStep::Workspace, &catalog);
        let connection = |app: &App| form(app).step_problem(CreateStep::Harness, &catalog);

        // The ports come first, then the blocked hosts.
        let f = form_mut(&mut app);
        f.workspace.ports = TextInput::new("8080, nope");
        f.workspace.deny_network = TextInput::new("api.example.com bad!host");
        assert_eq!(workspace(&app).as_deref(), Some(BAD_PORT));
        form_mut(&mut app).workspace.ports = TextInput::default();
        assert_eq!(workspace(&app).as_deref(), Some(BAD_HOST));

        // A new token is checked only on a subscription backend.
        let f = form_mut(&mut app);
        f.connection.account = AccountChoice::New;
        f.connection.token_name = TextInput::new("Bad Name");
        assert_eq!(connection(&app), None, "no account on the gateway");
        let subscription = subscription(&app);
        form_mut(&mut app).harness.backend = subscription;
        assert_eq!(connection(&app).as_deref(), Some(BAD_NAME));
        let f = form_mut(&mut app);
        f.connection.tokens = vec!["work".into()];
        f.connection.token_name = TextInput::new(" work ");
        assert_eq!(connection(&app).as_deref(), Some(TAKEN));
        form_mut(&mut app).connection.token_name = TextInput::new("personal");
        assert_eq!(connection(&app).as_deref(), Some(NO_VALUE));
        form_mut(&mut app).connection.token = TextInput::new("x");
        assert_eq!(connection(&app), None);
        // An existing token has nothing to check.
        let f = form_mut(&mut app);
        f.connection.token = TextInput::default();
        f.connection.account = AccountChoice::Named("work".into());
        assert_eq!(connection(&app), None);
    }
}

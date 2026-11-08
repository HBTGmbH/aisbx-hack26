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
    /// The connections of the picked provider's kind ([`crate::litellm::Kind`]) on this host when
    /// the form was opened, sorted by name — read from their `connection.json`, the authority for
    /// which exist. What the Connection step offers.
    pub connections: Vec<crate::litellm::Connection>,
    /// Every connection on this host, of both kinds: a new one's name must be free among them.
    pub all: Vec<crate::litellm::Connection>,
    /// The engines found on this computer that no connection is on yet, offered after the
    /// connections on a local provider ([`crate::local::discover`]).
    pub found: Vec<crate::local::Found>,
    /// The found engine *new connection…* was filled from, by index into `found`.
    pub found_pick: Option<usize>,
    /// The engines of this computer were looked for since the form opened.
    pub discovered: bool,
    /// The connection a LiteLLM backend is built on, pre-selected by [`crate::litellm::preselect`];
    /// `None` is *new connection...*, made from [`ConnectionAnswers::new_conn`] with the sandbox.
    pub litellm: Option<String>,
    /// The remembered connection (`ui.last_litellm`), for the row's *(last used)* mark only.
    pub last_litellm: Option<String>,
    /// What *new connection…* is typed into.
    pub new_conn: NewConnection,
}

/// The fields of *new connection…* (and of the `S` dialog's edit): everything a connection is,
/// plus its key. No models: the host reads them from the endpoint, and the agent picks one.
#[derive(Debug, Clone, Default)]
pub struct NewConnection {
    pub name: TextInput,
    pub url: TextInput,
    /// The endpoint's key. Never rendered back, never passed as an argument, emptied whenever the
    /// form comes back after a failed attempt to store it.
    pub key: TextInput,
    /// The kind of connection this form makes; `None` takes either, as the URL says.
    pub kind: Option<crate::litellm::Kind>,
    /// The engine a found engine filled these fields with, kept with the connection.
    pub engine: Option<String>,
    /// That engine refused aisbx's request without a key: the connection needs one.
    pub needs_key: bool,
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
        Self::build(
            self.name.value.trim(),
            self.url.value.trim(),
            self.engine.as_deref(),
        )
    }

    /// The URL the field stands for ([`crate::litellm::normalize_url`]): a local engine's port
    /// is `http://localhost:PORT`.
    pub fn url(&self) -> String {
        crate::litellm::normalize_url(&self.url.value)
    }

    /// Is this a local engine: the kind the form makes, else the kind the URL names.
    pub fn is_local(&self) -> bool {
        self.kind
            .unwrap_or_else(|| crate::litellm::Kind::of(&self.url()))
            == crate::litellm::Kind::Local
    }

    /// What is wrong with one field, as [`NewConnection::check`] says it. The fields are checked
    /// in [`ConnField::ALL`]'s order, so the first problem there is the one a save refuses with.
    pub fn field_problem(
        &self,
        field: ConnField,
        existing: &[crate::litellm::Connection],
    ) -> Option<String> {
        use crate::litellm::Kind;
        let noun = match self.is_local() {
            true => "engine",
            false => "connection",
        };
        match field {
            ConnField::Name => {
                let name = self.name.value.trim();
                if !crate::litellm::valid_name(name) {
                    return Some(format!("name the {noun} in a–z, 0–9 and “-” (at most 32)"));
                }
                let taken = existing.iter().find(|c| c.name == name)?;
                Some(match self.kind.is_none_or(|k| k == taken.kind()) {
                    true => format!("{noun} {name} exists — pick it in the list"),
                    false => format!(
                        "{name} is the name of a {} — pick another name",
                        match taken.kind() {
                            Kind::Gateway => "LiteLLM connection",
                            Kind::Local => "local engine",
                        }
                    ),
                })
            }
            ConnField::Url => {
                let url = self.url();
                if let Err(e) = crate::litellm::check_url(&url) {
                    return Some(match self.kind {
                        Some(Kind::Local) => {
                            "give the engine's port, or http://localhost:PORT".to_string()
                        }
                        _ => format!("{e}"),
                    });
                }
                match (self.kind, Kind::of(&url)) {
                    (Some(Kind::Local), Kind::Gateway) => Some(
                        "an engine runs on this computer: give its port, or http://localhost:PORT"
                            .into(),
                    ),
                    (Some(Kind::Gateway), Kind::Local) => Some(
                        "a gateway's address starts with https:// — for an engine on this \
                         computer, pick the Local engine provider"
                            .into(),
                    ),
                    _ => None,
                }
            }
            ConnField::Key => {
                let empty = self.key.value.trim().is_empty();
                match (empty, self.is_local(), self.needs_key) {
                    (false, _, _) | (true, true, false) => None,
                    (true, true, true) => Some(
                        "the engine wants a key — paste it; it is stored in the host keychain"
                            .into(),
                    ),
                    (true, false, _) => {
                        Some("paste the gateway's key — it is stored in the host keychain".into())
                    }
                }
            }
        }
    }

    /// [`NewConnection::check`] for an edit of `old`: the name is `old`'s and not checked again,
    /// and an empty key means "keep the stored one".
    pub fn check_edit(
        &self,
        old: &crate::litellm::Connection,
    ) -> Result<crate::litellm::Connection, String> {
        Self::build(&old.name, self.url.value.trim(), old.engine.as_deref())
    }

    /// The connection put together and validated as a whole. An engine stays only on a local
    /// URL.
    fn build(
        name: &str,
        url: &str,
        engine: Option<&str>,
    ) -> Result<crate::litellm::Connection, String> {
        let mut conn = crate::litellm::Connection::new(name, crate::litellm::normalize_url(url));
        if conn.is_local() {
            conn.engine = engine.map(str::to_string);
        }
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
    /// More host folders for this sandbox alone, picked one at a time, each as sbx takes an extra
    /// workspace: `PATH` or `PATH:ro` ([`CreateForm::extra_folders`]). `App::submit_create`
    /// writes them to the registry, so a rebuild mounts them again.
    pub extra_folders: Vec<String>,
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
            | CreateRow::ModelsFold
            | CreateRow::Model(_)
            | CreateRow::ModelDefault(_)
            | CreateRow::ModelPick(..)
            | CreateRow::ModelOn(_)
            | CreateRow::FilesFold
            | CreateRow::ConfigFile(_) => CreateStep::Models,
            CreateRow::Addon(_) => CreateStep::Kits,
            CreateRow::Name
            | CreateRow::StartDir
            | CreateRow::Folder(_)
            | CreateRow::FolderAdd
            | CreateRow::Clone
            | CreateRow::GitGuard
            | CreateRow::Skills
            | CreateRow::AdvancedFold
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
        self.refused_row(step, catalog).map(|(_, problem)| problem)
    }

    /// The row [`CreateForm::step_problem`] refuses over, with its problem.
    fn refused_row(&self, step: CreateStep, catalog: &Catalog) -> Option<(CreateRow, String)> {
        self.rows_of(step, catalog)
            .into_iter()
            .find_map(|row| Some((row, self.field_problem(row, catalog)?)))
    }

    /// Refuse to leave `step` over its answers, if they would refuse a create: say so on its
    /// fields from now on, open the fold of the row refused over, and give its problem.
    fn refuse(&mut self, step: CreateStep, catalog: &Catalog) -> Option<String> {
        let (row, problem) = self.refused_row(step, catalog)?;
        self.wizard.refused = true;
        self.reveal(row);
        Some(problem)
    }

    /// What is wrong with one row's answer, if a create would be refused over it. Only a new token
    /// and *new connection...* have fields to check, besides the picked folders, the ports and
    /// the blocked hosts.
    pub fn field_problem(&self, row: CreateRow, catalog: &Catalog) -> Option<String> {
        let ws = &self.workspace;
        let new_token = self.use_token(catalog) && self.connection.account == AccountChoice::New;
        let new_conn = self.litellm_backend(catalog) && self.connection.litellm.is_none();
        let name = self.connection.token_name.value.trim();
        let field = ConnField::ALL.into_iter().find(|f| f.row() == row);
        let problem = match row {
            CreateRow::Folder(i) => {
                let spec = ws.extra_folders.get(i)?;
                let mark = format!("“{spec}”");
                let problems = self.extra_folders().1;
                return problems.into_iter().find(|p| p.starts_with(&mark));
            }
            CreateRow::Ports => return Self::port_problem(ws.ports.value.trim()),
            CreateRow::DenyNetwork => return Self::deny_problem(ws.deny_network.value.trim()),
            _ if field.is_some() && new_conn => {
                let field = field?;
                return (self.connection.new_conn).field_problem(field, &self.connection.all);
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
            CreateRow::Folder(_) => FOLDER_ACCESS
                .iter()
                .map(|a| a.label().to_string())
                .collect(),
            CreateRow::LitellmConn => (self.connection.connections.iter())
                .map(|c| c.name.clone())
                .chain(self.found_offered().iter().map(found_label))
                .chain(std::iter::once(
                    self.new_conn_label(self.connection.new_conn.kind)
                        .to_string(),
                ))
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
            CreateRow::LitellmConn => {
                let saved = self.connection.connections.len();
                let picked = self.litellm_choice().and_then(|c| {
                    (self.connection.connections.iter()).position(|o| o.name == c.name)
                });
                Some(match (picked, self.connection.found_pick) {
                    (Some(i), _) => i,
                    (None, Some(k)) => saved + k,
                    (None, None) => saved + self.found_offered().len(),
                })
            }
            CreateRow::Folder(i) if i < self.workspace.extra_folders.len() => {
                Some(usize::from(!self.folder_read_only(i)))
            }
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
                let saved = self.connection.connections.len();
                let found = self.found_offered().len();
                match value {
                    v if v < saved => {
                        self.connection.litellm = Some(self.connection.connections[v].name.clone());
                        self.connection.found_pick = None;
                    }
                    v if v < saved + found => self.fill_from_found(v - saved),
                    v if v == saved + found => {
                        if self.connection.found_pick.take().is_some() {
                            let kind = self.connection.new_conn.kind;
                            self.connection.new_conn = NewConnection {
                                kind,
                                ..NewConnection::default()
                            };
                        }
                        self.connection.litellm = None;
                    }
                    _ => return false,
                }
            }
            CreateRow::Folder(i) => {
                let (Some(spec), Some(access)) = (
                    self.workspace.extra_folders.get(i).cloned(),
                    FOLDER_ACCESS.get(value),
                ) else {
                    return false;
                };
                let host = crate::folders::spec_host(&spec).0.to_string();
                self.workspace.extra_folders[i] = match access {
                    crate::folders::Access::Read => format!("{host}:ro"),
                    crate::folders::Access::Write => host,
                };
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
        self.connection.new_conn.check(&self.connection.all)
    }

    /// The engines found on this computer that the Connection row offers: on a local provider
    /// only.
    pub fn found_offered(&self) -> &[crate::local::Found] {
        match self.connection.new_conn.kind {
            Some(crate::litellm::Kind::Local) => &self.connection.found,
            _ => &[],
        }
    }

    /// *new connection…* on a gateway provider, *new engine…* on a local one.
    pub fn new_conn_label(&self, kind: Option<crate::litellm::Kind>) -> &'static str {
        match kind {
            Some(crate::litellm::Kind::Local) => NEW_ENGINE,
            _ => NEW_CONNECTION,
        }
    }

    /// Fill *new engine…* from the `k`-th found engine: its name, URL and the key aisbx read.
    pub fn fill_from_found(&mut self, k: usize) {
        let Some(found) = self.connection.found.get(k) else {
            return;
        };
        let all = &self.connection.all;
        let name = found.default_name(&|n| all.iter().any(|c| c.name == n));
        self.connection.new_conn = NewConnection {
            name: TextInput::new(&name),
            url: TextInput::new(found.url()),
            key: TextInput::new(found.key.as_deref().unwrap_or_default()),
            kind: Some(crate::litellm::Kind::Local),
            engine: Some(found.engine.slug().to_string()),
            needs_key: found.needs_key,
        };
        self.connection.litellm = None;
        self.connection.found_pick = Some(k);
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
    /// Step 7: the answers of one step, which ↵ goes back to.
    Edit(CreateStep),
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
    /// The Models step shows the agent's slots and the models it is offered
    /// ([`CreateRow::ModelsFold`]).
    pub models_open: bool,
    /// The Models step shows the connection's config files ([`CreateRow::FilesFold`]).
    pub files_open: bool,
    /// The Workspace step shows the ports, the blocked hosts and the MCP servers
    /// ([`CreateRow::AdvancedFold`]).
    pub advanced_open: bool,
    /// The form opened on the last create's answers ([`CreateForm::apply_defaults`]), which the
    /// Review's heading says.
    pub prefilled: bool,
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
            models_open: false,
            files_open: false,
            advanced_open: false,
            prefilled: false,
        }
    }
}

/// What the Harness step calls an agent — and the window's map of a sandbox, inside its wall.
pub fn harness_label(backend: &Backend) -> String {
    match backend.harness() {
        "claude" => "Claude Code".into(),
        crate::kits::CLAUDE_BARE => "Claude Code bare".into(),
        "opencode" => "OpenCode".into(),
        "codex" => "Codex".into(),
        "agy" => "Antigravity".into(),
        "pi" => "Pi".into(),
        _ => backend.kit.display_name.clone(),
    }
}

/// The TUI's tag beside the label while it is still generated from the answers.
pub const GENERATED_TAG: &str = "generated";

/// The GUI's line under the label's box while the label is still generated.
pub const GENERATED_GLOSS: &str = "made from your answers; type to replace it";

/// What the status line says when New brings back the answers a wizard was left with.
pub const DRAFT_BACK: &str = "your unfinished answers are back";

/// What the status line says when New opens on the last create's answers.
pub const PREFILLED: &str = "prefilled with the settings of your last sandbox";

/// The last value of the LiteLLM connection row: a connection made together with the sandbox.
pub const NEW_CONNECTION: &str = "new connection…";

/// The last value of the engine row on a local provider: an engine added with the sandbox.
pub const NEW_ENGINE: &str = "new engine…";

/// A found engine as the engine row offers it: `llama-server on localhost:8095 (found)`.
pub fn found_label(found: &crate::local::Found) -> String {
    format!(
        "{} on localhost:{} (found)",
        found.engine.label(),
        found.port
    )
}

/// What a picked folder's choice row offers, tightest first.
pub const FOLDER_ACCESS: [crate::folders::Access; 2] =
    [crate::folders::Access::Read, crate::folders::Access::Write];

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
        crate::sbx::SkillsMode::Off => "the agent gets none of the shared skills",
        crate::sbx::SkillsMode::Readonly => "the agent uses the shared skills",
        crate::sbx::SkillsMode::Readwrite => "the agent uses and changes the shared skills",
    }
}

/// The tag the `/login` radio carries before its hint, in the running colour: it is the account
/// most sandboxes should be on, it needs nothing pasted, and it survives every rebuild.
pub const LOGIN_TAG: &str = "recommended";

/// What the `/login` radio says, on the create wizard and the account editor of both frontends.
pub const LOGIN_HINT: &str = "sign in once, and every /login sandbox uses it";

/// What the *setup token* radio says. Not picked, it says why to pick it; picked, it says what to
/// do next, which depends on whether this host holds a token yet (`stored`).
pub fn token_hint(selected: bool, stored: usize) -> &'static str {
    match (selected, stored) {
        (false, _) => "for a second Claude account",
        (true, 0) => "paste a token and name it",
        (true, _) => "pick one, or add a new one",
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

/// What a provider serves, in a few words: the Provider step's gloss beside its name.
pub fn provider_blurb(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Claude models",
        Provider::Google => "Gemini models",
        Provider::OpenAi => "GPT models",
        Provider::Litellm => "the models of your LiteLLM gateway",
        Provider::Local => {
            "models served on this computer: llama.cpp, Ollama, LM Studio, MLX, vLLM"
        }
        Provider::Other => "",
    }
}

/// The warning under a picked connection whose key this host does not hold, and where to store
/// it without leaving the dashboard: Secrets, then the connection's own dialog.
pub fn missing_key_note(conn: &crate::litellm::Connection) -> String {
    format!(
        "⚠ the key of {} is missing: add it in Secrets (S)",
        conn.name
    )
}

/// The note under a focused kit that `sbx kit add` cannot add later
/// ([`crate::kits::Addon::create_time_reason`]).
pub const ONLY_NOW_NOTE: &str = "Adding it later rebuilds the sandbox.";

/// The marker a kit row carries when ticking it now is the only chance to have it without a
/// rebuild. Quiet on purpose: most kits carry it.
pub const ONLY_NOW: &str = "only now";

/// The tick for `--static-mcp none`, and the Review's and the details' word for it.
pub const NO_MCP: &str = "no MCP servers";

/// The details' word for sbx's dynamic MCP mode: the agent finds and adds the host's servers.
pub const HOST_MCP: &str = "the servers set up on this computer";

/// A sandbox's MCP servers as the details name them, from the registry: `None` for the dynamic
/// mode, which has no fixed set to name.
pub fn mcp_set(servers: &[String]) -> Option<String> {
    match (crate::sbx::is_no_mcp(servers), servers.is_empty()) {
        (true, _) => Some("none".into()),
        (false, true) => None,
        (false, false) => Some(servers.join(" ")),
    }
}

/// The GUI's marker on a kit that `sbx kit add` can add to the sandbox later: the GUI marks the
/// few exceptions, the TUI marks the rule ([`ONLY_NOW`]).
pub const ADD_LATER: &str = "can add later";

/// What the GUI's Kits step says under its heading, for its [`ADD_LATER`] markers.
pub const ADD_LATER_GLOSS: &str =
    "optional; adding a kit later rebuilds the sandbox, unless it is marked Can add later";

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
            "Sign in with /login inside the sandbox. Every /login sandbox then uses that",
            "account, also after a rebuild.",
        ],
        AccountChoice::New => &[
            "Run  claude setup-token  as the account you want, then name the token and",
            "paste it. Later sandboxes can pick it by its name.",
        ],
        AccountChoice::Named(_) => {
            &["Stored on this computer. Sandboxes on the same token share its account."]
        }
    }
}

/// What a sandbox's account is now, as the auth editor's *Now* line says it: the text and its
/// note, unindented. `users`: the sandboxes on its token, itself included. The colours — danger
/// for a token not stored, running for a stored one — are each frontend's, keyed on `current`.
pub fn auth_now(current: &crate::model::Auth, users: usize) -> (String, String) {
    use crate::model::Auth;
    match current {
        Auth::Login => (
            "/login".to_string(),
            "shared with every /login sandbox".to_string(),
        ),
        Auth::Token {
            name,
            stored: false,
            ..
        } => (
            format!("setup token “{name}” — not stored"),
            "it cannot sign in until a token is stored below".to_string(),
        ),
        Auth::Token { name, .. } => (
            format!("setup token “{name}”"),
            match users {
                0 | 1 => "no other sandbox uses it".to_string(),
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

    /// The folders picked for this sandbox ([`CreateRow::Folder`]), checked against those the
    /// mounted folder's plan mounts ([`crate::folders::extras_of`]), and what is wrong with any
    /// of them.
    pub fn extra_folders(&self) -> (Vec<crate::folders::Folder>, Vec<String>) {
        let ws = &self.workspace;
        let home = crate::model::home_dir();
        let specs = ws.extra_folders.iter().map(String::as_str);
        crate::folders::extras_of(specs, &ws.folders, home.as_deref())
    }

    /// Add a host folder to the sandbox's own, read-only; a folder already picked stays as it
    /// is. The index of its row ([`CreateRow::Folder`]).
    pub fn add_folder(&mut self, path: &Path) -> usize {
        let folders = &mut self.workspace.extra_folders;
        let spec = |ro: bool| format!("{}{}", path.display(), if ro { ":ro" } else { "" });
        match folders
            .iter()
            .position(|f| *f == spec(true) || *f == spec(false))
        {
            Some(at) => at,
            None => {
                folders.push(spec(true));
                folders.len() - 1
            }
        }
    }

    /// Whether picked folder `i` is read-only: its spec ends in `:ro`.
    pub fn folder_read_only(&self, i: usize) -> bool {
        (self.workspace.extra_folders.get(i)).is_some_and(|f| crate::folders::spec_host(f).1)
    }

    /// Whether [`CreateRow::GitGuard`] is offered: the workspace has a `.git` folder, and clone
    /// mode, which mounts the whole workspace read-only, is off.
    pub fn git_guard_offered(&self) -> bool {
        self.workspace.git_offered && !self.workspace.clone
    }

    /// The Review step's lines, for the answers on the form now. A seldom used answer
    /// ([`CreateRow::AdvancedFold`]) has a line only while it is not the form's default.
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
                None => format!("new: {}", self.connection.new_conn.name.value.trim()),
            };
            let label = match self.local(catalog) {
                true => "Engine",
                false => "Connection",
            };
            answer(CreateStep::Connection, label, connection, false);
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
                AccountChoice::Named(name) => format!("{token} {name}"),
                AccountChoice::New => {
                    let name = self.connection.token_name.value.trim();
                    format!("new {token} {name}")
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
                let first =
                    (self.enabled_models(catalog).first()).map(|i| catalog.models[*i].name.clone());
                let value = match (none, self.local(catalog), first) {
                    (true, true, Some(first)) => format!("{ENGINE_FIRST}: {first}"),
                    (true, _, _) => AGENT_PICKS.to_string(),
                    (false, _, _) => pins.join(", "),
                };
                answer(CreateStep::Models, "Models", value, none);
                let total = catalog.models.len();
                let on = self.enabled_models(catalog).len();
                if on < total {
                    let offered = format!("{on} of {total}");
                    answer(CreateStep::Models, "Offered", offered, false);
                }
            }
            Some(_) => {
                let value = "no list yet".to_string();
                answer(CreateStep::Models, "Models", value, true);
            }
            None => answer(CreateStep::Models, "Models", AGENT_PICKS.to_string(), true),
        }
        let addons = self.selected_addons(catalog);
        let kits = or_none(&addons.join(", "));
        answer(CreateStep::Kits, "Kits", kits, addons.is_empty());
        let account = self.label_account(catalog);
        let label = match self.workspace.name.value.trim() {
            "" => (self.effective_backend(catalog))
                .map(|b| {
                    crate::model::auto_label(catalog, &b.kit.name, account.as_deref(), &addons)
                })
                .unwrap_or_default(),
            typed => typed.to_string(),
        };
        answer(CreateStep::Workspace, "Label", label, false);
        let ws = &self.workspace;
        let root = self.workspace_root(&app.cwd);
        if ws.root.is_some() {
            let mounted = crate::model::tilde(&root);
            answer(CreateStep::Workspace, "Project folder", mounted, false);
        }
        if self.start_dir_offered(catalog) {
            let opens = crate::startdir::describe(&root, ws.start_dir.as_deref());
            answer(
                CreateStep::Workspace,
                "Starts in",
                opens,
                ws.start_dir.is_none() && ws.root.is_none(),
            );
        }
        if !ws.extra_folders.is_empty() {
            let picked: Vec<String> = (ws.extra_folders.iter())
                .map(|spec| shown_folder(spec))
                .collect();
            answer(
                CreateStep::Workspace,
                "Also mounts",
                picked.join(", "),
                false,
            );
        }
        let clone = match ws.clone {
            true => "on",
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
                ".git folder",
                git.into(),
                !ws.git_guard,
            );
        }
        if self.skills_offered(catalog) {
            let skills = skills_mode_name(ws.skills);
            let default = ws.skills == app.skills_default;
            answer(CreateStep::Workspace, "Skills", skills.into(), default);
        }
        if !ws.ports.value.trim().is_empty() {
            let ports = ws.ports.value.trim().to_string();
            answer(CreateStep::Workspace, "Ports", ports, false);
        }
        if !ws.deny_network.value.trim().is_empty() {
            let deny = ws.deny_network.value.trim().to_string();
            answer(CreateStep::Workspace, "Blocked hosts", deny, false);
        }
        let picked = self.selected_mcp();
        if let Some(mcp) = match (ws.no_mcp, picked.is_empty()) {
            (true, _) => Some(NO_MCP.to_string()),
            (false, true) => None,
            (false, false) => Some(picked.join(", ")),
        } {
            answer(CreateStep::Workspace, "MCP servers", mcp, false);
        }

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
                .map(|spec| shown_folder(&spec))
                .collect(),
            notes,
        }
    }
}

/// An extra workspace as the Review writes it: `~/docs (read-only)` for sbx's `~/docs:ro`.
pub fn shown_folder(spec: &str) -> String {
    let (host, read_only) = crate::folders::spec_host(spec);
    let host = crate::model::tilde(Path::new(host));
    match read_only {
        true => format!("{host} (read-only)"),
        false => host,
    }
}

/// The Review's and the ledger's word for models the agent picks itself.
pub const AGENT_PICKS: &str = "the agent picks";

/// The Review's word for an engine's unpicked slots: they are on its first model
/// ([`crate::litellm::engine_slots`]).
pub const ENGINE_FIRST: &str = "the engine's first model";

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
            CreateStep::Provider => ("Choose the AI provider".into(), ""),
            CreateStep::Connection => match self.local(catalog) {
                true => ("Choose the inference engine".into(), "on this computer"),
                false => ("Choose the LiteLLM gateway".into(), ""),
            },
            CreateStep::Harness => ("Choose the coding agent".into(), ""),
            CreateStep::Models => match (self.models_agent(catalog), catalog.models.len()) {
                (None, _) => ("The agent picks its own models".into(), ""),
                (Some(_), 0) => ("No model list yet".into(), ""),
                (Some(_), n) => (
                    format!(
                        "{n} model{} on {}",
                        if n == 1 { "" } else { "s" },
                        self.connection.litellm.as_deref().unwrap_or_default()
                    ),
                    "",
                ),
            },
            CreateStep::Kits => (
                "Add kits".into(),
                "optional · only now: adding it later rebuilds the sandbox",
            ),
            CreateStep::Workspace => ("Name and options".into(), ""),
            CreateStep::Review => match self.wizard.prefilled {
                true => (
                    "Prefilled from your last sandbox".into(),
                    "pick a step to change it",
                ),
                false => ("Check and create".into(), "pick a step to change it"),
            },
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
                .filter(|row| self.row_shown(*row))
                .map(WizardRow::Field)
                .collect(),
            CreateStep::Review => (CreateStep::ALL.into_iter())
                .filter(|s| *s != CreateStep::Review && self.step_shown(*s, catalog))
                .map(WizardRow::Edit)
                .collect(),
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
    /// step 3; on the Review, on *Create & start*.
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
            CreateStep::Review => Some(WizardRow::CreateStart),
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
        if let Some(problem) = self.refuse(step, catalog) {
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
                if self.step_problem(*step, catalog).is_some() {
                    self.wizard_set_step(catalog, *step);
                    let problem = self.refuse(*step, catalog);
                    return Err(problem.unwrap_or_default());
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

    /// Move to the step that owns `row`, open the fold it stands under, and put the cursor on it.
    /// False where that step does not offer the row with the answers as they stand.
    pub fn focus_field(&mut self, catalog: &Catalog, row: CreateRow) -> bool {
        self.wizard_set_step(catalog, row.step());
        self.reveal(row);
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
        match row {
            CreateRow::FolderAdd => {
                return match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => FieldEffect::PickFolder,
                    _ => FieldEffect::Ignored,
                };
            }
            CreateRow::Folder(i) => return self.folder_key(catalog, i, key),
            CreateRow::ModelsFold | CreateRow::FilesFold | CreateRow::AdvancedFold => {
                let open = match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => !self.fold_open(row),
                    KeyCode::Right => true,
                    KeyCode::Left => false,
                    _ => return FieldEffect::Ignored,
                };
                self.set_fold(row, open);
                self.focus_wizard_row(catalog, WizardRow::Field(row));
                return FieldEffect::Done;
            }
            _ => {}
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
                    | CreateRow::Ports
                    | CreateRow::DenyNetwork
                    | CreateRow::LitellmName
                    | CreateRow::LitellmUrl
                    | CreateRow::LitellmKey
                    | CreateRow::ConfigFile(_)
                    | CreateRow::Folder(_)
                    | CreateRow::FolderAdd
                    | CreateRow::ModelsFold
                    | CreateRow::FilesFold
                    | CreateRow::AdvancedFold => {}
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
                CreateRow::LitellmConn => match self.connection.litellm {
                    Some(_) => return FieldEffect::Next,
                    None => opened = true,
                },
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
                | CreateRow::Ports
                | CreateRow::DenyNetwork
                | CreateRow::LitellmName
                | CreateRow::LitellmUrl
                | CreateRow::LitellmKey
                | CreateRow::ConfigFile(_)
                | CreateRow::Folder(_)
                | CreateRow::FolderAdd
                | CreateRow::ModelsFold
                | CreateRow::FilesFold
                | CreateRow::AdvancedFold => {}
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

impl CreateForm {
    /// What a fold row says beside its title: what is set under it, or `None` while everything
    /// there is still the form's default.
    pub fn fold_summary(&self, app: &crate::app::App, row: CreateRow) -> Option<String> {
        let catalog = &app.catalog;
        let ws = &self.workspace;
        let mut parts: Vec<String> = Vec::new();
        match row {
            CreateRow::ModelsFold => {
                let agent = self.models_agent(catalog)?;
                for slot in Slot::of(agent) {
                    if let Some(id) = self.connection.models.get(*slot) {
                        let name = crate::models::display_name(&catalog.models, id)
                            .unwrap_or_else(|| id.to_string());
                        parts.push(format!("{} → {name}", slot.label()));
                    }
                }
                let hidden = catalog.models.len() - self.enabled_models(catalog).len();
                if hidden > 0 {
                    parts.push(format!("{hidden} hidden"));
                }
            }
            CreateRow::FilesFold => {
                let edited = (0..)
                    .map_while(|i| app.config_file_row(i))
                    .filter(|(_, state)| *state != crate::litellm::FileState::Generated)
                    .count();
                if edited > 0 {
                    parts.push(format!("{edited} edited"));
                }
            }
            CreateRow::AdvancedFold => {
                if !ws.ports.value.trim().is_empty() {
                    parts.push(format!("ports {}", ws.ports.value.trim()));
                }
                if !ws.deny_network.value.trim().is_empty() {
                    parts.push(format!("blocked {}", ws.deny_network.value.trim()));
                }
                let picked = self.selected_mcp();
                match (ws.no_mcp, picked.is_empty()) {
                    (true, _) => parts.push(NO_MCP.to_string()),
                    (false, false) => parts.push(format!("MCP {}", picked.join(", "))),
                    (false, true) => {}
                }
            }
            _ => return None,
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// A key on picked folder `i`: ↵, space, ← and → switch it between read-only and read-write,
    /// Delete and Backspace drop it, the cursor staying where the row was.
    fn folder_key(&mut self, catalog: &Catalog, i: usize, key: KeyEvent) -> FieldEffect {
        let folders = &mut self.workspace.extra_folders;
        let Some(spec) = folders.get(i).cloned() else {
            return FieldEffect::Ignored;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                folders[i] = match spec.strip_suffix(":ro") {
                    Some(writable) => writable.to_string(),
                    None => format!("{spec}:ro"),
                };
            }
            KeyCode::Delete | KeyCode::Backspace => {
                folders.remove(i);
                let rows = self.wizard_rows(catalog);
                let same = |r: &WizardRow| *r == WizardRow::Field(CreateRow::Folder(i));
                let add = |r: &WizardRow| *r == WizardRow::Field(CreateRow::FolderAdd);
                let at = (rows.iter().position(same)).or_else(|| rows.iter().position(add));
                if let Some(at) = at {
                    self.set_wizard_focus(at);
                }
            }
            _ => return FieldEffect::Ignored,
        }
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
            "When set, {} uses this model for {}. Otherwise, it picks one itself.",
            slot.agent_name(),
            slot.hint()
        ),
    )
}

/// The default under an open slot.
pub(crate) fn default_model_help(slot: Slot) -> Help {
    Help::new(
        slot.label(),
        format!("{} picks the {} itself.", slot.agent_name(), slot.label()),
    )
}

/// One of the connection's models under an open slot.
pub(crate) fn model_pick_help(catalog: &Catalog, slot: Slot, i: usize) -> Help {
    let (name, id) = catalog
        .models
        .get(i)
        .map(|m| (m.name.clone(), m.id.clone()))
        .unwrap_or_default();
    let id = match name == id {
        true => String::new(),
        false => format!(" ({id})"),
    };
    Help::new(
        slot.label(),
        format!(
            "{} uses {name}{id} for the {}.",
            slot.agent_name(),
            slot.label()
        ),
    )
}

/// What an agent is, in a few words: the Harness step's gloss beside its name.
pub fn harness_blurb(agent: &str) -> &'static str {
    match agent {
        "claude" => "Anthropic's coding agent",
        crate::kits::CLAUDE_BARE => "Claude Code without hooks, plugins or CLAUDE.md",
        "opencode" => "an open-source coding agent",
        "codex" => "OpenAI's coding agent",
        "agy" => "Google's coding agent",
        "pi" => "a minimal coding agent",
        _ => "",
    }
}

impl CreateForm {
    /// Is the picked backend built on a local engine?
    pub fn local(&self, catalog: &Catalog) -> bool {
        self.conn_kind(catalog) == Some(crate::litellm::Kind::Local)
    }

    /// The Harness step's gloss beside an agent: on a local engine, how much memory it needs and
    /// how fast it answers ([`Backend::context_note`]); elsewhere what it is.
    pub fn harness_gloss(&self, catalog: &Catalog, harness: &Harness) -> String {
        let note = (harness.backends.first())
            .and_then(|b| catalog.backends.get(*b))
            .and_then(Backend::context_note);
        match note {
            Some(note) => note.to_string(),
            None => harness_blurb(&harness.agent).to_string(),
        }
    }

    /// [`field_gloss`], worded for a local engine on the Connection step's fields.
    pub fn field_gloss_of(&self, catalog: &Catalog, row: CreateRow) -> &'static str {
        match (self.local(catalog), row) {
            (true, CreateRow::LitellmUrl) => "its port, or http://localhost:PORT",
            (true, CreateRow::LitellmKey) => "only if the engine has one; stored on this computer",
            _ => field_gloss(row),
        }
    }

    /// The smallest context window among the models the picked connection serves, where the
    /// engine said it.
    pub fn engine_context(&self, catalog: &Catalog) -> Option<u64> {
        catalog.models.iter().filter_map(|m| m.limits.context).min()
    }

    /// What the picked engine lacks for this backend's harness: Claude Code's API, or the context
    /// the harness needs. `None` off a local engine, or when nothing lacks.
    pub fn engine_warning(&self, catalog: &Catalog, backend: &Backend) -> Option<String> {
        if !self.local(catalog) {
            return None;
        }
        let harness = harness_label(backend);
        let engine = (self.litellm_choice())
            .and_then(|c| c.engine.as_deref())
            .and_then(crate::local::Engine::parse);
        if backend.needs_anthropic_api() && engine.and_then(|e| e.anthropic()) == Some(false) {
            let engine = engine.map(|e| e.label()).unwrap_or_default();
            return Some(format!(
                "{engine} cannot run {harness}: it has no Anthropic API (/v1/messages). Pick Pi or \
                 OpenCode, or another engine."
            ));
        }
        let (Some(needed), Some(context)) =
            (backend.context_needed(), self.engine_context(catalog))
        else {
            return None;
        };
        (context < needed).then(|| {
            let short = crate::local::tokens_short;
            let (needed, context) = (short(needed), short(context));
            format!(
                "{harness} needs more memory than the engine has: {needed} context or more, the \
                 engine has {context}. Start the engine with a larger context, or pick Pi."
            )
        })
    }
}

/// What a harness on a local engine costs, in plain words, for the Harness step's help
/// (docs/local.md, *Which harness*).
fn local_harness_help(backend: &Backend) -> Option<&'static str> {
    let text = match backend.kit.name.as_str() {
        "pi-local" => {
            "Pi sends the model short instructions before your task. So it needs little memory, \
             and the first answer comes in seconds. The best fit for a model on this computer."
        }
        "claude-bare-local" => {
            "Claude Code as claude --bare: short instructions and three tools (Bash, Edit, Read). \
             So it needs little memory and answers fast. It reads no CLAUDE.md, hooks or plugins."
        }
        "opencode-local" => {
            "OpenCode sends the model about four times as much as Pi before your task. So it needs \
             more memory and answers slower. Give the engine 16k context or more."
        }
        "claude-local" => {
            "Claude Code sends the model about eight times as much as Pi before your task. So it \
             needs a lot of memory, and the first answer can take a minute. Give the engine 32k \
             context or more."
        }
        _ => return None,
    };
    Some(text)
}

/// One line under a field or beside an option: what goes in it, or what it does.
pub fn field_gloss(row: CreateRow) -> &'static str {
    match row {
        CreateRow::Name => "the name in the sandbox list",
        CreateRow::StartDir => "where the agent starts working",
        CreateRow::Clone => "the agent works on a copy; your files stay unchanged",
        CreateRow::GitGuard => "the agent can read the Git history, but not change it",
        CreateRow::FolderAdd => "a folder outside the project that the agent may use too",
        CreateRow::ModelsFold => "the agent picks them itself unless you do",
        CreateRow::FilesFold => "for your review",
        CreateRow::AdvancedFold => "ports, blocked hosts, MCP servers",
        CreateRow::Ports => "for example 8080:3000",
        CreateRow::DenyNetwork => "for example api.example.com",
        CreateRow::McpNone => "the agent gets no MCP servers",
        CreateRow::TokenName => "a–z, 0–9 and “-”; later sandboxes pick it by this name",
        CreateRow::Token => "the output of claude setup-token",
        CreateRow::LitellmName => "a–z, 0–9 and “-”, at most 32 characters",
        CreateRow::LitellmUrl => "the gateway's https:// address",
        CreateRow::LitellmKey => "stored on this computer, never in the sandbox",
        _ => "",
    }
}

impl CreateForm {
    /// What `row` of the current step is about, worded for the answer it holds now. Empty where
    /// the row says it all.
    pub fn help(&self, app: &crate::app::App, row: WizardRow) -> Help {
        let catalog = &app.catalog;
        match row {
            WizardRow::Provider(p) => {
                let provider = self.providers(catalog).into_iter().nth(p);
                Help::new(provider.map(Provider::label).unwrap_or_default(), "")
            }
            WizardRow::Agent(h) => self.agent_help(app, h),
            WizardRow::Connection(choice) => self.connection_help(app, choice),
            WizardRow::Field(field) => self.field_help(app, field),
            WizardRow::Edit(step) => Help::new(step.title(), ""),
            WizardRow::Next => Help::new("Next", ""),
            WizardRow::Back => Help::new("Back", ""),
            WizardRow::CreateStart => Help::new(
                "Create & start",
                "Creates the sandbox and starts the agent in it.",
            ),
            WizardRow::CreateOnly => Help::new(
                "Create only",
                "Creates the sandbox without starting the agent.",
            ),
            WizardRow::Cancel => Help::new(
                "Cancel",
                "Discards your answers. Esc on step 1 keeps them for the next new sandbox.",
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
        let own_sign_in = b.account().is_none() && b.litellm_agent().is_none();
        let text = match (
            own_sign_in && !b.setup_hint().is_empty(),
            local_harness_help(b),
        ) {
            (_, Some(local)) => local.to_string(),
            (true, None) => format!("It signs in with {}.", b.setup_hint()),
            (false, None) => String::new(),
        };
        let picked = harness.backends.contains(&self.harness.backend);
        let warn = picked
            .then(|| {
                self.missing_warning(app)
                    .or_else(|| self.engine_warning(catalog, b))
            })
            .flatten();
        Help::new(harness.label, text.trim_start()).warn(warn)
    }

    /// What the picked backend still lacks on this host, with the commands that store it.
    fn missing_warning(&self, app: &crate::app::App) -> Option<String> {
        let missing = (app.catalog.backends.get(self.harness.backend))
            .map(|b| app.missing_secrets(b))
            .unwrap_or_default();
        (!missing.is_empty()).then(|| {
            format!(
                "{} is missing on this computer. Store it with: {}",
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
                "You sign in with /login inside the sandbox. Every /login sandbox uses that \
                 account, also after a rebuild."
            }
            ConnChoice::Token => {
                "Uses a token from claude setup-token. Sandboxes on the same token share its \
                 account, apart from /login."
            }
        };
        let picked = self.connection_selected(choice, catalog);
        let warn = picked.then(|| self.missing_warning(app)).flatten();
        Help::new(title, text).warn(warn)
    }

    fn field_help(&self, app: &crate::app::App, row: CreateRow) -> Help {
        let catalog = &app.catalog;
        let ws = &self.workspace;
        let root = crate::model::tilde(&self.workspace_root(&app.cwd));
        let help = match row {
            CreateRow::LitellmConn if self.local(catalog) => match self.litellm_choice() {
                Some(conn) => Help::new(
                    "engine",
                    format!(
                        "The sandbox uses the engine on {} of this computer.",
                        crate::litellm::place(&conn.url)
                    ),
                ),
                None => match self.connection.found_pick.and_then(|k| self.connection.found.get(k)) {
                    Some(found) => {
                        let key = match &found.key_source {
                            Some(source) => format!(" Its key comes from {source}."),
                            None => String::new(),
                        };
                        Help::new(
                            "engine",
                            format!("Found: {}. Next adds it.{key}", found.summary()),
                        )
                    }
                    None => Help::new(
                        "engine",
                        "Adds an engine on this computer: a name for it, its port and its key, if \
                         it has one.",
                    ),
                },
            },
            CreateRow::LitellmConn => match self.litellm_choice() {
                Some(conn) => {
                    let host = crate::litellm::host(&conn.url);
                    let warn = app
                        .litellm_key_missing(&conn.name)
                        .then(|| missing_key_note(conn));
                    Help::new("connection", format!("The sandbox uses the gateway at {host}."))
                        .warn(warn)
                }
                None => Help::new(
                    "connection",
                    "Adds a gateway: a name for it, its address and its key.",
                ),
            },
            CreateRow::LitellmName if self.local(catalog) => Help::new(
                "name",
                "A name for the engine: a–z, 0–9 and “-”, at most 32 characters.",
            ),
            CreateRow::LitellmUrl if self.local(catalog) => Help::new(
                "URL",
                "The engine's port on this computer, or http://localhost:PORT. The sandbox reaches \
                 it as host.docker.internal on that port, and no other port.",
            ),
            CreateRow::LitellmKey if self.local(catalog) => Help::new(
                "key",
                "The engine's key, if it has one. It is stored on this computer, never in the \
                 sandbox.",
            ),
            CreateRow::LitellmName => Help::new(
                "name",
                "A name for the gateway: a–z, 0–9 and “-”, at most 32 characters.",
            ),
            CreateRow::LitellmUrl => Help::new(
                "URL",
                "The gateway's address, for example https://litellm.example.com. The key is sent \
                 to this address only.",
            ),
            CreateRow::LitellmKey => Help::new(
                "key",
                "The gateway's key. It is stored on this computer, never in the sandbox.",
            ),
            CreateRow::TokenPick(i) => {
                let token = self.connection.tokens.get(i).cloned().unwrap_or_default();
                let users = token_users(app.token_users(&token));
                let users = match users.is_empty() {
                    true => String::new(),
                    false => format!(" {}.", capitalized(&users)),
                };
                Help::new(token, format!("Stored on this computer.{users}"))
            }
            CreateRow::TokenNew => Help::new(
                "new token",
                "Run claude setup-token as the account you want, then name the token and paste \
                 it.",
            ),
            CreateRow::TokenName => Help::new(
                "token name",
                "Lower-case letters, digits and dashes. Later sandboxes pick the token by this \
                 name.",
            ),
            CreateRow::Token => Help::new(
                "token",
                "The output of claude setup-token. It is stored on this computer; the sandbox \
                 never sees it.",
            ),
            CreateRow::ConfigFile(i) => return app.config_file_help(self, i),
            CreateRow::Model(slot) => alias_help(slot),
            CreateRow::ModelDefault(slot) => default_model_help(slot),
            CreateRow::ModelPick(slot, i) => model_pick_help(catalog, slot, i),
            CreateRow::ModelsRefresh => {
                let whose = match self.local(catalog) {
                    true => "engine",
                    false => "gateway",
                };
                let text = match &catalog.models_source {
                    crate::models::Source::Gateway { fetched_at, .. } => format!(
                        "Reads the {whose}'s model list again. The list is from {}.",
                        crate::state::ago(u64::try_from(*fetched_at).unwrap_or(0))
                    ),
                    crate::models::Source::NotYet => format!("Reads the {whose}'s model list."),
                };
                Help::new("refresh", text)
            }
            CreateRow::ModelsFold if self.local(catalog) => Help::new(
                "models",
                "When set, the agent starts on the models you pick, and sees only the models you \
                 leave ticked. Otherwise, it starts on the engine's first model.",
            ),
            CreateRow::ModelsFold => Help::new(
                "models",
                "When set, the agent starts on the models you pick, and sees only the models you \
                 leave ticked. Otherwise, it picks its own models from all of the gateway's.",
            ),
            CreateRow::FilesFold => Help::new(
                "config files",
                "The files the agent reads, as your answers make them. Open one to check or \
                 change it.",
            ),
            CreateRow::ModelOn(i) => {
                let (name, id) = catalog
                    .models
                    .get(i)
                    .map(|m| (m.name.clone(), m.id.clone()))
                    .unwrap_or_default();
                Help::new(
                    id,
                    format!(
                        "When ticked, the agent can use {name}. Otherwise, the agent does not see \
                         it, in every sandbox on this connection."
                    ),
                )
            }
            CreateRow::Addon(i) => {
                let Some(addon) = catalog.addons.get(i) else {
                    return Help::new("", "");
                };
                let mut text = Vec::new();
                if let Some(found) = self.kits.host.get(i).and_then(Option::as_ref) {
                    text.push(match found.detected() {
                        true => "When ticked, the sandbox can read these folders of your \
                                 computer, but not change them."
                            .to_string(),
                        false => format!(
                            "Not found on this computer: {}.",
                            found.reason(&addon.kit.name)
                        ),
                    });
                }
                let only_now = addon.create_time_reason().is_some() && !self.addon_unavailable(i);
                if only_now {
                    text.push(ONLY_NOW_NOTE.to_string());
                }
                text.push(addon.kit.first_sentence().to_string());
                Help::new(addon.kit.name.clone(), text.join(" ").trim().to_string())
            }
            CreateRow::Name => Help::new(
                "label",
                match ws.name_touched {
                    false => "The name the sandbox list shows, made from your answers. Type to \
                              replace it.",
                    true => "The name the sandbox list shows.",
                },
            ),
            CreateRow::StartDir => Help::new(
                "starts in",
                format!(
                    "The agent starts in this folder of {root}. Pick a subfolder to start there \
                     instead."
                ),
            ),
            CreateRow::FolderAdd => Help::new(
                "mount a folder",
                format!(
                    "When added, the agent can also use a folder outside {root}: read-only, unless \
                     you change it. Otherwise, it sees only {root}."
                ),
            ),
            CreateRow::Folder(i) => {
                let spec = ws.extra_folders.get(i).map(String::as_str).unwrap_or("");
                let path = crate::model::tilde(Path::new(crate::folders::spec_host(spec).0));
                let text = match self.folder_read_only(i) {
                    true => format!("The agent can read {path}, but not change it."),
                    false => format!("The agent can read and change {path}."),
                };
                Help::new(path, text)
            }
            CreateRow::Clone => Help::new(
                "clone mode",
                "When enabled, the agent works on its own Git clone, and your files stay \
                 unchanged: get its commits with git fetch sandbox-<name>. Otherwise, the agent \
                 changes your files directly. Needs a Git repository.",
            ),
            CreateRow::GitGuard => Help::new(
                ".git read-only",
                "When enabled, the agent can read the Git history, but cannot commit, change \
                 branches or add Git hooks. Otherwise, it can do all of that.",
            ),
            CreateRow::Skills => {
                let text = match ws.skills {
                    crate::sbx::SkillsMode::Readonly => {
                        "The agent uses the skills you share with every sandbox (K), but cannot \
                         change them."
                    }
                    crate::sbx::SkillsMode::Readwrite => {
                        "The agent uses the shared skills and can change them; every other \
                         sandbox gets its changes."
                    }
                    crate::sbx::SkillsMode::Off => "The agent gets none of the shared skills.",
                };
                Help::new(
                    "share skills",
                    format!("{text} Changing this later rebuilds the sandbox."),
                )
            }
            CreateRow::AdvancedFold => Help::new(
                "more options",
                "Ports, blocked hosts and MCP servers. Most sandboxes need none of them.",
            ),
            CreateRow::Ports => Help::new(
                "ports",
                "When set, a server in the sandbox is reachable from this computer: 8080:3000 \
                 opens sandbox port 3000 at localhost:8080. Otherwise, nothing in the sandbox is \
                 reachable. The server must listen on 0.0.0.0.",
            ),
            CreateRow::DenyNetwork => Help::new(
                "blocked hosts",
                "When set, the sandbox cannot reach these hosts, for example api.example.com or \
                 *.example.com. Otherwise, it reaches every host its network rules allow.",
            ),
            CreateRow::McpNone => Help::new(
                NO_MCP,
                "When enabled, the agent gets no MCP servers and cannot add any. Otherwise, it can \
                 use the MCP servers set up on this computer.",
            ),
            CreateRow::Mcp(i) => {
                let Some(server) = ws.mcp.get(i) else {
                    return Help::new("", "");
                };
                let warn = (!server.ready()).then(|| {
                    format!(
                        "{} is not ready ({}): run sbx mcp auth {} on this computer first.",
                        server.name, server.status, server.name
                    )
                });
                Help::new(
                    server.name.clone(),
                    "When ticked, the agent gets only the ticked servers. Otherwise, it can use \
                     every MCP server set up on this computer.",
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
    /// The row's answer is the step's: on to the next step, as Next goes.
    Next,
    /// Open the start-folder picker, which moves the whole form into its own screen and hands it
    /// back afterwards.
    OpenStartDir,
    /// Fetch the connection's model list again, a job that hands the form back on its Models step.
    FetchModels,
    /// Open a config file of the connection in an editor, by its row's index.
    EditConfig(usize),
    /// Drop the edited copy of a config file, by its row's index.
    ResetConfig(usize),
    /// Open a picker for one more host folder, which comes back as [`CreateRow::Folder`].
    PickFolder,
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
        assert_eq!(auth_now(&Auth::Login, 0).0, "/login");
        assert_eq!(
            auth_now(&token(false), 1),
            (
                "setup token “work” — not stored".to_string(),
                "it cannot sign in until a token is stored below".to_string()
            )
        );
        assert_eq!(auth_now(&token(true), 1).1, "no other sandbox uses it");
        assert_eq!(auth_now(&token(true), 2).1, "shared with 1 other sandbox");
        assert_eq!(auth_now(&token(true), 4).1, "shared with 3 other sandboxes");
    }

    fn typed(name: &str, url: &str, key: &str) -> NewConnection {
        NewConnection {
            name: TextInput::new(name),
            url: TextInput::new(url),
            key: TextInput::new(key),
            ..NewConnection::default()
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
            assert_eq!(conn, crate::litellm::Connection::new("beta", url));
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

    /// Each step opens with a line of its own, never its title alone (the trail shows the title);
    /// the Review's says when the form opened on the last create's answers.
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
        assert_eq!(heading, "Choose the coding agent");
        let (_, gloss) = f.step_heading(CreateStep::Kits, &catalog);
        assert!(
            gloss.contains(ONLY_NOW),
            "the Kits step says what its marker means"
        );
        let (fresh, _) = f.step_heading(CreateStep::Review, &catalog);
        f.wizard.prefilled = true;
        let (prefilled, gloss) = f.step_heading(CreateStep::Review, &catalog);
        assert_ne!(fresh, prefilled);
        assert!(prefilled.contains("last sandbox"), "{prefilled}");
        assert!(gloss.contains("change"), "{gloss}");
    }

    /// The Provider step names neither the harnesses nor the ways to sign in, and its rows need no
    /// help of their own; the harness rows say what each agent is, not how it reaches its model.
    #[test]
    fn the_provider_and_harness_rows_say_only_what_they_choose() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form(&app);
        let harnesses: Vec<String> = (catalog.backends.iter()).map(harness_label).collect();
        for (p, provider) in f.providers(&catalog).into_iter().enumerate() {
            let blurb = provider_blurb(provider);
            for word in harnesses
                .iter()
                .map(String::as_str)
                .chain(["/login", "token", "key"])
            {
                assert!(!blurb.contains(word), "{provider:?}: {blurb}");
            }
            assert_eq!(
                f.help(&app, WizardRow::Provider(p)).text,
                "",
                "{provider:?}"
            );
        }
        for agent in ["claude", "opencode", "codex", "agy", "pi"] {
            let blurb = harness_blurb(agent);
            assert!(blurb.contains("agent"), "{agent}: {blurb}");
            assert!(!blurb.contains("account"), "{agent}: {blurb}");
        }
        let claude = (f.harnesses(&catalog).iter())
            .position(|h| h.agent == "claude")
            .expect("Claude Code runs on LiteLLM");
        let help = f.help(&app, WizardRow::Agent(claude)).text;
        assert!(!help.contains("Anthropic account"), "{help}");
        assert!(!help.contains("placeholder"), "{help}");
    }

    /// On a local engine each harness says in plain words how light it is: how much memory it
    /// needs and how fast it answers, with no token or request counts.
    #[test]
    fn a_local_harness_says_its_memory_and_speed_in_plain_words() {
        let catalog = Catalog::load().unwrap();
        let mut weights = Vec::new();
        for name in [
            "pi-local",
            "claude-bare-local",
            "opencode-local",
            "claude-local",
        ] {
            let b = catalog.backend(name).unwrap();
            let (note, help) = (b.context_note().unwrap(), local_harness_help(b).unwrap());
            weights.push(note.split(':').next().unwrap());
            for text in [note, help] {
                assert!(text.contains("memory"), "{name}: {text}");
                for jargon in ["request", "token", "  "] {
                    assert!(!text.contains(jargon), "{name}: {jargon:?} in {text}");
                }
            }
        }
        assert_eq!(weights, ["light", "light", "medium", "heavy"]);
    }

    /// The account radios say the same in both frontends: the setup token says why while it is
    /// not picked and what to do once it is.
    #[test]
    fn the_account_radios_say_what_each_way_means() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
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
        assert_eq!(token(f), "for a second Claude account");
        f.pick_connection(&catalog, ConnChoice::Token);
        f.connection.tokens = Vec::new();
        assert_eq!(token(f), token_hint(true, 0));
        assert!(token(f).starts_with("paste a token"));
        f.connection.tokens = vec!["work".into()];
        assert_eq!(token(f), "pick one, or add a new one");

        assert_eq!(token_users(0), "");
        assert_eq!(token_users(1), "1 sandbox uses it");
        assert_eq!(token_users(3), "3 sandboxes use it");
    }

    /// The notes that say where to go: a missing key names the connection and Secrets; a kit that
    /// is only for now says what adding it later costs.
    #[test]
    fn the_notes_name_what_to_do() {
        let note = missing_key_note(&crate::testkit::acme());
        assert!(
            note.contains("Secrets (S)") && note.contains("acme"),
            "{note}"
        );
        assert!(ONLY_NOW_NOTE.contains("rebuild"), "{ONLY_NOW_NOTE}");
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
            (CreateRow::ModelsFold, CreateStep::Models),
            (CreateRow::FilesFold, CreateStep::Models),
            (CreateRow::Addon(0), CreateStep::Kits),
            (CreateRow::Name, CreateStep::Workspace),
            (CreateRow::StartDir, CreateStep::Workspace),
            (CreateRow::Folder(0), CreateStep::Workspace),
            (CreateRow::FolderAdd, CreateStep::Workspace),
            (CreateRow::Clone, CreateStep::Workspace),
            (CreateRow::Skills, CreateStep::Workspace),
            (CreateRow::AdvancedFold, CreateStep::Workspace),
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
                Provider::Litellm,
                Provider::Local
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
            (
                Provider::Local,
                vec!["pi", "claude-bare", "opencode", "claude"],
            ),
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
        assert_eq!(mcp_set(&["NONE".to_string()]).as_deref(), Some("none"));
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

    /// The optional answers wait under their folds, closed when the form opens, and are still
    /// checked there: a step refused over one opens its fold, so the problem is on screen.
    #[test]
    fn a_refused_answer_under_a_closed_fold_opens_it() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        assert!(!f.wizard.models_open && !f.wizard.files_open && !f.wizard.advanced_open);
        f.wizard_set_step(&catalog, CreateStep::Workspace);
        let shown = |f: &CreateForm| f.wizard_rows(&catalog);
        let ports = WizardRow::Field(CreateRow::Ports);
        assert!(!shown(f).contains(&ports), "folded");
        assert!(f
            .rows_of(CreateStep::Workspace, &catalog)
            .contains(&CreateRow::Ports));
        f.workspace.ports = TextInput::new("nope");
        assert_eq!(f.wizard_next(&catalog), Err(BAD_PORT.to_string()));
        assert!(f.wizard.advanced_open, "the refusal opened the fold");
        assert!(shown(f).contains(&ports));
        assert_eq!(
            f.fold_summary(&app_ref(&catalog), CreateRow::AdvancedFold)
                .as_deref(),
            Some("ports nope")
        );
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

    /// Folders are picked on the Workspace step, one row each over *mount a folder…*: read-only at
    /// first, switched with the keys, dropped with Delete, refused there when a create would be,
    /// and reviewed in words, among what the sandbox sees.
    #[test]
    fn picked_folders_are_switched_dropped_checked_and_reviewed() {
        let root = std::env::temp_dir().join(format!("aisbx-tests-more-{}", std::process::id()));
        for d in ["proj/sub", "data"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = root.join("proj");
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let f = form_mut(&mut app);
        let data = root.join("data");
        assert_eq!(f.add_folder(&data), 0);
        assert_eq!(
            f.add_folder(&data),
            0,
            "a folder picked twice is listed once"
        );
        let spec = format!("{}:ro", data.display());
        assert_eq!(f.workspace.extra_folders, [spec.as_str()]);
        let rows = f.rows_of(CreateStep::Workspace, &catalog);
        let at = |row| rows.iter().position(|r| *r == row);
        assert_eq!(
            at(CreateRow::Folder(0)),
            at(CreateRow::FolderAdd).map(|i| i - 1)
        );
        assert_eq!(
            at(CreateRow::FolderAdd),
            at(CreateRow::Clone).map(|i| i - 1)
        );
        assert!(!CreateRow::Folder(0).is_text());
        assert!(f.focus_field(&catalog, CreateRow::FolderAdd));
        let effect = f.field_key(&catalog, CreateRow::FolderAdd, key(KeyCode::Enter));
        assert_eq!(effect, FieldEffect::PickFolder);

        assert_eq!(f.choices(CreateRow::Folder(0)), ["read-only", "read-write"]);
        assert_eq!(f.chosen(CreateRow::Folder(0)), Some(0));
        f.field_key(&catalog, CreateRow::Folder(0), key(KeyCode::Char(' ')));
        assert_eq!(f.workspace.extra_folders, [data.display().to_string()]);
        assert!(f.pick_choice(CreateRow::Folder(0), 0));
        assert_eq!(f.workspace.extra_folders, [spec.as_str()]);
        assert_eq!(f.field_problem(CreateRow::Folder(0), &catalog), None);

        assert_eq!(f.add_folder(&root.join("proj").join("sub")), 1);
        let problem = f.field_problem(CreateRow::Folder(1), &catalog);
        assert!(
            problem
                .as_deref()
                .is_some_and(|p| p.contains("in the workspace")),
            "{problem:?}"
        );
        assert_eq!(f.step_problem(CreateStep::Workspace, &catalog), problem);
        assert!(f.focus_field(&catalog, CreateRow::Folder(1)));
        f.field_key(&catalog, CreateRow::Folder(1), key(KeyCode::Delete));
        assert_eq!(f.workspace.extra_folders, [spec.as_str()]);
        assert_eq!(
            f.wizard_rows(&catalog).get(f.wizard_focus()),
            Some(&WizardRow::Field(CreateRow::FolderAdd)),
            "the cursor stays where the row was"
        );

        let review = match &app.screen {
            Screen::Create(form) => form.review(&app),
            _ => panic!("the create form is open"),
        };
        let shown = format!("{} (read-only)", crate::model::tilde(&data));
        let more = review.answers.iter().find(|a| a.label == "Also mounts");
        assert_eq!(
            more.map(|a| (a.value.as_str(), a.default)),
            Some((shown.as_str(), false))
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
        let says_it_all = |row: WizardRow| {
            matches!(
                row,
                WizardRow::Provider(_) | WizardRow::Edit(_) | WizardRow::Next | WizardRow::Back
            )
        };
        let may_say_more = |row: WizardRow| matches!(row, WizardRow::Agent(_));
        let option = |row: WizardRow| {
            matches!(
                row,
                WizardRow::Field(
                    CreateRow::Clone
                        | CreateRow::GitGuard
                        | CreateRow::FolderAdd
                        | CreateRow::ModelsFold
                        | CreateRow::ModelOn(_)
                        | CreateRow::Model(_)
                        | CreateRow::Ports
                        | CreateRow::DenyNetwork
                        | CreateRow::McpNone
                        | CreateRow::Mcp(_)
                )
            )
        };
        let check = |f: &mut CreateForm, state: &str| {
            for fold in [
                CreateRow::ModelsFold,
                CreateRow::FilesFold,
                CreateRow::AdvancedFold,
            ] {
                f.set_fold(fold, true);
            }
            for step in CreateStep::ALL {
                f.wizard_set_step(&catalog, step);
                for row in f.wizard_rows(&catalog) {
                    let help = f.help(&view, row);
                    assert!(!help.title.is_empty(), "{state}: {step:?} {row:?} {help:?}");
                    if !may_say_more(row) {
                        assert_eq!(
                            help.text.is_empty(),
                            says_it_all(row),
                            "{state}: {step:?} {row:?} {help:?}"
                        );
                    }
                    if option(row) {
                        let text = &help.text;
                        assert!(
                            text.starts_with("When ") && text.contains(" Otherwise, "),
                            "{state}: {row:?} {text}"
                        );
                    }
                }
            }
        };
        let f = form_mut(&mut app);
        f.add_folder(Path::new("/srv/docs"));
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
        assert!(help.text.contains(ONLY_NOW_NOTE), "{help:?}");
        for step in CreateStep::ALL {
            f.wizard_set_step(&catalog, step);
            for row in f.wizard_rows(&catalog) {
                let text = f.help(&view, row).text;
                for word in ["placeholder", "keychain", "--static-mcp", "host-global"] {
                    assert!(!text.contains(word), "{row:?}: {text}");
                }
            }
        }
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

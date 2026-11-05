//! Application state and input handling.
//!
//! Every mutation of a sandbox is a [`PendingJob`]: a title, the `sbx` commands to run, and what
//! to record afterwards.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::actions::{Action, Refusal};
use crate::bindings;
use crate::create::{
    ConnChoice, ConnectionAnswers, CreateStep, FieldEffect, HarnessAnswers, KitsAnswers, WizardRow,
    WizardState, WorkspaceAnswers,
};
use crate::find::{self, Find};
use crate::folders;
use crate::geom::{Position, Rect};
use crate::input::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crate::insight::{Fetched, Insight, Payload, Tab};
use crate::job::{Job, Status, Step};
use crate::kits::{Account, Backend, Catalog};
use crate::model::{self, Auth, Sandbox, Snapshot};
use crate::models::{self, Overrides, Slot};
use crate::repo_skills_ui::{self, RepoScope, RepoSkillRow, RepoSkillsForm};
use crate::sbx;
use crate::skill_select;
use crate::skill_source;
use crate::state::Registry;
use crate::stats::Histories;
use crate::terminal;
use crate::tokens;

/// What a container build is doing in its long pauses (see [`crate::job::Step::note`]).
const BUILD_NOTE: &str = "pulling the image if it is missing, then running each kit's install \
     commands — sbx names them as they start; the agent update is the slow one";

/// [`App::mounts_state`]: the state folder, or under `cargo test` one that does not exist.
fn mounts_state() -> PathBuf {
    if cfg!(any(test, feature = "test-support")) {
        std::env::temp_dir().join("aisbx-tests-no-mounts")
    } else {
        crate::state::state_dir()
    }
}

/// [`App::litellm_root`]: the real folder, or under `cargo test` one that does not exist.
fn litellm_root() -> PathBuf {
    if cfg!(any(test, feature = "test-support")) {
        std::env::temp_dir().join("aisbx-tests-no-litellm-connections")
    } else {
        crate::litellm::dir()
    }
}

/// [`App::host_home`]: the real home folder, or under `cargo test` none.
fn host_home() -> Option<PathBuf> {
    if cfg!(any(test, feature = "test-support")) {
        None
    } else {
        model::home_dir()
    }
}

/// Why a sandbox whose folder is gone is not rebuilt — see [`App::rebuild_blocker`].
const WORKSPACE_GONE: &str = "its folder no longer exists on this computer, so it cannot be \
     rebuilt: restore the folder, or delete the sandbox with d";

const AUTO_REFRESH: Duration = Duration::from_secs(5);
/// How long the poll runs before the header shows it as a refresh: longer than its interval.
const SLOW_REFRESH: Duration = Duration::from_secs(5);
const STATUS_TTL: Duration = Duration::from_secs(6);
/// What saving the Repo skills screen's global scope says; the screen says it before the save.
const GLOBAL_SAVED: &str = "saved for every sandbox — existing sandboxes get the change at \
     their next start, new ones at their first";
/// Two clicks on the same row inside this window start the sandbox.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// One cell of the sandbox list: the current folder's sandboxes, the "+ New sandbox" row, then,
/// while expanded, the other folders' sandboxes. Headings ([`crate::roster::Row`]) are not cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// A sandbox, by index into [`App::sandboxes`].
    Sandbox(usize),
    /// The "+ New sandbox" row.
    New,
}

/// A folder the list can be shown from — see [`App::folders`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    /// How many sandboxes mount it.
    pub sandboxes: usize,
    /// It is the folder the list shows now.
    pub current: bool,
}

/// Where the terminal is being handed over to, once the UI has stepped aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// `sbx run --name ...`: the agent session, after the steps of [`Handoff::prelude`].
    Agent {
        name: String,
        /// The folder to open the agent in, if that is not the workspace root
        /// ([`crate::startdir`]): `sbx run --workdir`.
        start: Option<PathBuf>,
        prelude: Vec<Vec<String>>,
    },
    /// `sbx exec -it … bash` — a plain shell inside the sandbox, opening where its agent does.
    Shell {
        name: String,
        workdir: Option<PathBuf>,
    },
    /// An external app on the sandbox's SSH endpoint: `ssh <name>.sbx`, or an editor's own remote
    /// entry point. It carries its argv, because the program is not `sbx`.
    Connect {
        name: String,
        what: &'static str,
        argv: Vec<String>,
        /// Does the program want a terminal window ([`crate::remote::Target::in_terminal`])?
        /// `false`: an editor's launcher, which [`App::open_session`] starts detached.
        terminal: bool,
    },
    /// The user's editor on an edited copy of a connection's config file
    /// ([`crate::litellm::prepare_edit`]).
    Edit { file: String, path: PathBuf },
}

impl Handoff {
    pub fn name(&self) -> &str {
        match self {
            Handoff::Agent { name, .. } => name,
            Handoff::Shell { name, .. } => name,
            Handoff::Connect { name, .. } => name,
            Handoff::Edit { file, .. } => file,
        }
    }

    /// What ended, for the line printed when the terminal comes back.
    pub fn what(&self) -> &'static str {
        match self {
            Handoff::Agent { .. } => "session",
            Handoff::Shell { .. } => "shell",
            Handoff::Connect { what, .. } => what,
            Handoff::Edit { .. } => "edit",
        }
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            Handoff::Agent { name, start, .. } => sbx::attach_argv(name, start.as_deref()),
            Handoff::Shell { name, workdir } => sbx::shell_argv(name, workdir.as_deref()),
            Handoff::Connect { argv, .. } => argv.clone(),
            Handoff::Edit { path, .. } => terminal::editor_argv(
                terminal::Os::current(),
                path,
                terminal::TermEnv::from_process().editor.as_deref(),
            ),
        }
    }

    /// Commands to run to completion *before* the session starts, in order. A failed step does
    /// not stop the session.
    pub fn prelude(&self) -> &[Vec<String>] {
        match self {
            Handoff::Agent { prelude, .. } => prelude,
            _ => &[],
        }
    }
}

// --- mouse --------------------------------------------------------------------------------

/// Something on screen a click can land on. The renderer registers these as it draws
/// ([`App::hit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// A sandbox's row, by its cell in [`App::cells`].
    Card(usize),
    /// The "+ New sandbox" row.
    NewCard,
    /// A details tab, by index into [`Tab::ALL`].
    TabAt(usize),
    /// Behave exactly as if this key had been pressed.
    Press(KeyCode, KeyModifiers),
    /// A row in the create / kits form, by index.
    FormRow(usize),
    /// The same row, only moving the form's cursor onto it — never pressing it ([`App::click`]):
    /// a window's row selects, and the row's own buttons act.
    FormFocus(usize),
    /// One value of a create-form row whose answer is one of several (the skills mode, the
    /// LiteLLM connection): the row by index, then the value by its place in the row.
    FormChoice(usize, usize),
    /// Wheel target: the sandbox list.
    ScrollCards,
    /// Wheel target: the details pane.
    ScrollDetail,
    /// Wheel target: the job log inside the running-job modal.
    ScrollLog,
    /// Wheel target: the key list in the help modal.
    ScrollHelp,
    /// A step in the create wizard's trail, or an answer of its review that leads to one.
    WizardStep(CreateStep),
}

impl Hit {
    pub fn key(code: KeyCode) -> Self {
        Hit::Press(code, KeyModifiers::NONE)
    }

    fn is_scroll_zone(&self) -> bool {
        matches!(
            self,
            Hit::ScrollCards | Hit::ScrollDetail | Hit::ScrollLog | Hit::ScrollHelp
        )
    }
}

// --- forms --------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    pub value: String,
    /// Cursor position in characters (not bytes).
    pub cursor: usize,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.chars().count();
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_at(self.cursor - 1);
        self.value.remove(at);
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor < self.value.chars().count() {
            let at = self.byte_at(self.cursor);
            self.value.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    /// Delete from the start of the line to the cursor, as Ctrl-U does at a shell prompt.
    pub fn delete_to_start(&mut self) {
        let at = self.byte_at(self.cursor);
        self.value.replace_range(..at, "");
        self.cursor = 0;
    }

    /// Delete from the cursor to the end of the line, as Ctrl-K does at a shell prompt.
    pub fn delete_to_end(&mut self) {
        let at = self.byte_at(self.cursor);
        self.value.truncate(at);
    }

    /// Delete the word before the cursor, as Ctrl-W does at a shell prompt: the spaces right
    /// before the cursor, then back to the previous space.
    pub fn delete_word(&mut self) {
        let chars: Vec<char> = self.value.chars().collect();
        let mut start = self.cursor.min(chars.len());
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let (from, to) = (self.byte_at(start), self.byte_at(self.cursor));
        self.value.replace_range(from..to, "");
        self.cursor = start;
    }
}

/// The name a setup token gets when the user does not choose one.
pub const DEFAULT_TOKEN_NAME: &str = "default";

/// Which Anthropic account a form is set to. Only [`AccountChoice::New`] asks for a token value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChoice {
    /// The host-global `/login` — one account per machine.
    Login,
    /// A named token already stored on this host. Nothing to paste.
    Named(String),
    /// A token this form is about to store under a new name.
    New,
}

impl AccountChoice {
    pub fn on_token(&self) -> bool {
        !matches!(self, AccountChoice::Login)
    }

    /// The name of the token this choice means, given what the form's name field holds.
    fn name(&self, typed: &str) -> Option<String> {
        match self {
            AccountChoice::Login => None,
            AccountChoice::Named(name) => Some(name.clone()),
            AccountChoice::New => Some(typed.to_string()),
        }
    }
}

impl CreateRow {
    /// Is this one of the create form's text rows — the ones that own plain typing, and that a
    /// click only focuses?
    pub fn is_text(self) -> bool {
        matches!(
            self,
            CreateRow::Name
                | CreateRow::TokenName
                | CreateRow::Token
                | CreateRow::Ports
                | CreateRow::DenyNetwork
                | CreateRow::LitellmName
                | CreateRow::LitellmUrl
                | CreateRow::LitellmKey
        )
    }

    /// Does this row hold a secret, drawn masked and passed on stdin only?
    pub fn masked(self) -> bool {
        matches!(self, CreateRow::Token | CreateRow::LitellmKey)
    }

    /// The fold row this row stands under, if any: the model picks, the config files and the
    /// options seldom needed are optional, and folded away until asked for.
    pub fn fold(self) -> Option<CreateRow> {
        match self {
            CreateRow::Model(_)
            | CreateRow::ModelDefault(_)
            | CreateRow::ModelPick(..)
            | CreateRow::ModelOn(_) => Some(CreateRow::ModelsFold),
            CreateRow::ConfigFile(_) => Some(CreateRow::FilesFold),
            CreateRow::Ports | CreateRow::DenyNetwork | CreateRow::McpNone | CreateRow::Mcp(_) => {
                Some(CreateRow::AdvancedFold)
            }
            _ => None,
        }
    }
}

/// A focusable line in the create form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRow {
    Backend(usize),
    /// Which LiteLLM connection a LiteLLM backend is built on — ←→ picks among them and *new
    /// connection…*, whose three fields follow.
    LitellmConn,
    LitellmName,
    LitellmUrl,
    /// The new connection's key. Masked, passed on stdin only.
    LitellmKey,
    /// Authenticate with the host-global `/login`.
    AuthOauth,
    /// Authenticate with a setup token — which one is the pick rows below.
    AuthToken,
    /// One of the named tokens stored on this host.
    TokenPick(usize),
    /// Store a new named token instead.
    TokenNew,
    TokenName,
    Token,
    /// Fetch the connection's model list again, through its fetcher sandbox.
    ModelsRefresh,
    /// Shows or hides the optional model picks under it: the agent's slots and the models it is
    /// offered ([`WizardState::models_open`]).
    ModelsFold,
    /// One of the agent's model slots ([`Slot::of`]) — the row that opens the list of models
    /// below it.
    Model(Slot),
    /// "default": the agent picks the model — the state every slot is in unless one is picked.
    ModelDefault(Slot),
    /// One of the gateway's models, by index into the catalog.
    ModelPick(Slot, usize),
    /// One of the gateway's models, by index into the catalog: ticked, the agent is offered it.
    ModelOn(usize),
    /// Shows or hides the config files under it ([`WizardState::files_open`]).
    FilesFold,
    /// One of the config files the connection writes for the agent
    /// ([`crate::litellm::config_file_names`]): ↵ opens it in an editor.
    ConfigFile(usize),
    Addon(usize),
    Name,
    /// Which folder inside the workspace the agent opens in. Opens the same picker `w` does.
    StartDir,
    /// One more host folder for this sandbox alone, by index into
    /// [`WorkspaceAnswers::extra_folders`]: read-only or read-write.
    Folder(usize),
    /// Opens a picker for one more host folder ([`CreateRow::Folder`]).
    FolderAdd,
    Clone,
    /// Mount the workspace's `.git` folder read-only. Only where it has one and clone mode is off.
    GitGuard,
    /// How the host's shared agent-skills store is mounted (`--skills`). Only where sbx mounts it
    /// for the backend's base agent.
    Skills,
    /// Shows or hides the options seldom needed under it: ports, blocked hosts and the MCP
    /// servers ([`WizardState::advanced_open`]).
    AdvancedFold,
    /// Ports to publish while the container is built (`-p`).
    Ports,
    /// Hosts to deny this one sandbox (`--deny-network`).
    DenyNetwork,
    /// No MCP server and no tools to find one (`--static-mcp none`). On every host.
    McpNone,
    /// One MCP server registered on this host, for the sandbox's fixed set (`--static-mcp`). Only
    /// present while the host has servers registered.
    Mcp(usize),
}

/// The create form: its answers, grouped by the step that owns them (see [`CreateStep`]), and
/// where the wizard that walks them is — the step on screen and the one cursor.
#[derive(Debug, Clone)]
pub struct CreateForm {
    pub harness: HarnessAnswers,
    pub connection: ConnectionAnswers,
    pub kits: KitsAnswers,
    pub workspace: WorkspaceAnswers,
    pub wizard: WizardState,
}

impl CreateForm {
    fn new(
        catalog: &Catalog,
        home: Option<&Path>,
        tokens: Vec<String>,
        preferred: Option<&str>,
        mcp: Vec<crate::sbx::McpServer>,
        skills_default: crate::sbx::SkillsMode,
    ) -> Self {
        let default_token = preferred
            .filter(|p| tokens.iter().any(|t| t == p))
            .map(|p| AccountChoice::Named(p.to_string()))
            .or_else(|| tokens.first().map(|t| AccountChoice::Named(t.clone())))
            .unwrap_or(AccountChoice::New);
        let mut form = Self {
            harness: HarnessAnswers {
                backend: catalog.offered_backends().first().copied().unwrap_or(0),
            },
            connection: ConnectionAnswers {
                account: AccountChoice::Login,
                // Empty when tokens exist: the default name could overwrite one of them.
                token_name: TextInput::new(match default_token {
                    AccountChoice::New => DEFAULT_TOKEN_NAME,
                    _ => "",
                }),
                default_token,
                tokens,
                token: TextInput::default(),
                models: Overrides::default(),
                model_pick: None,
                disabled: Default::default(),
                picks_for: None,
                picks_kept: Vec::new(),
                connections: Vec::new(),
                litellm: None,
                last_litellm: None,
                new_conn: crate::create::NewConnection::default(),
            },
            kits: KitsAnswers {
                addons: vec![false; catalog.addons.len()],
                host: catalog
                    .addons
                    .iter()
                    .map(|a| model::host_cache_detection_in(&a.kit.name, home))
                    .collect(),
            },
            workspace: WorkspaceAnswers {
                name: TextInput::default(),
                name_touched: false,
                start_dir: None,
                root: None,
                clone: false,
                git_guard: false,
                git_offered: false,
                folders: Default::default(),
                extra_folders: Vec::new(),
                skills: skills_default,
                ports: TextInput::default(),
                deny_network: TextInput::default(),
                static_mcp: vec![false; mcp.len()],
                no_mcp: false,
                mcp,
            },
            wizard: WizardState::default(),
        };
        form.refresh_auto_name(catalog);
        form
    }

    /// The token account the "setup token" radio selects — the last used one, or a new token when
    /// the host has none yet.
    pub fn token_default(&self) -> AccountChoice {
        self.connection.default_token.clone()
    }

    /// Does the selected backend authenticate against an Anthropic subscription, so that there is
    /// an account to choose at all?
    pub fn subscription(&self, catalog: &Catalog) -> bool {
        catalog
            .backends
            .get(self.harness.backend)
            .is_some_and(|b| b.account().is_some())
    }

    /// Is the selected backend a LiteLLM backend, so a connection has to be picked?
    pub fn litellm_backend(&self, catalog: &Catalog) -> bool {
        catalog
            .backends
            .get(self.harness.backend)
            .is_some_and(|b| b.litellm_agent().is_some())
    }

    /// The connection the form is on, by name — `None` when the host has none.
    pub fn litellm_choice(&self) -> Option<&crate::litellm::Connection> {
        let name = self.connection.litellm.as_deref()?;
        self.connection.connections.iter().find(|c| c.name == name)
    }

    /// Step the Connection row to the next (`forward`) or previous choice — every connection,
    /// then *new connection…* — wrapping.
    pub fn cycle_litellm(&mut self, forward: bool) {
        let all = &self.connection.connections;
        let n = all.len() + 1;
        let at = self
            .connection
            .litellm
            .as_deref()
            .and_then(|name| all.iter().position(|c| c.name == name))
            .unwrap_or(all.len());
        let next = match forward {
            true => (at + 1) % n,
            false => (at + n - 1) % n,
        };
        self.connection.litellm = all.get(next).map(|c| c.name.clone());
    }

    /// Is a setup token being asked for — as opposed to the shared `/login`?
    pub fn use_token(&self, catalog: &Catalog) -> bool {
        self.connection.account.on_token() && self.subscription(catalog)
    }

    /// The backend that will actually be built. Picking "setup token" replaces the selected
    /// backend with the token kit; it does not configure it.
    pub fn effective_backend<'a>(&self, catalog: &'a Catalog) -> Option<&'a Backend> {
        match self.use_token(catalog) {
            true => catalog.for_account(Account::Token),
            false => catalog.backends.get(self.harness.backend),
        }
    }

    /// Would the sandbox this form describes mount the shared agent-skills store? Only where sbx
    /// mounts it for the *effective* backend's base agent.
    pub fn skills_offered(&self, catalog: &Catalog) -> bool {
        self.effective_backend(catalog)
            .is_some_and(Backend::shares_skills)
    }

    /// Can this backend's agent be started somewhere other than the workspace root? Every one can:
    /// `sbx run --workdir` needs nothing from the agent.
    pub fn start_dir_offered(&self, catalog: &Catalog) -> bool {
        self.effective_backend(catalog).is_some()
    }

    /// Are there models to pick? Only where the backend runs on a LiteLLM connection *and* the
    /// connection's list was fetched; else the Models step has its refresh row alone.
    pub fn models_offered(&self, catalog: &Catalog) -> bool {
        !catalog.models.is_empty() && self.models_agent(catalog).is_some()
    }

    /// The agent whose models the Models step picks: the LiteLLM backend's, on an existing
    /// connection. `None` for every other backend, and for *new connection…*.
    pub fn models_agent(&self, catalog: &Catalog) -> Option<crate::litellm::Agent> {
        self.connection.litellm.as_ref()?;
        self.effective_backend(catalog)?.litellm_agent()
    }

    /// The catalog's models the agent is offered, by index: every one not switched off.
    pub fn enabled_models(&self, catalog: &Catalog) -> Vec<usize> {
        (0..catalog.models.len())
            .filter(|i| !self.connection.disabled.contains(&catalog.models[*i].id))
            .collect()
    }

    /// What the Models step's answers come to, as [`crate::model_picks`] keeps them.
    pub fn picks(&self, catalog: &Catalog) -> crate::model_picks::Picks {
        let mut slots = Overrides::default();
        if let Some(agent) = self.models_agent(catalog) {
            for slot in Slot::of(agent) {
                slots.set(*slot, self.connection.models.get(*slot).map(str::to_string));
            }
        }
        crate::model_picks::Picks {
            slots,
            disabled: self.connection.disabled.clone(),
        }
    }

    pub fn rows(&self, catalog: &Catalog) -> Vec<CreateRow> {
        let mut rows: Vec<CreateRow> = catalog
            .offered_backends()
            .into_iter()
            .map(CreateRow::Backend)
            .collect();
        if self.litellm_backend(catalog) {
            rows.push(CreateRow::LitellmConn);
            if self.connection.litellm.is_none() {
                rows.extend([
                    CreateRow::LitellmName,
                    CreateRow::LitellmUrl,
                    CreateRow::LitellmKey,
                ]);
            }
        }
        if self.subscription(catalog) {
            rows.push(CreateRow::AuthOauth);
            rows.push(CreateRow::AuthToken);
            if self.connection.account.on_token() {
                rows.extend((0..self.connection.tokens.len()).map(CreateRow::TokenPick));
                rows.push(CreateRow::TokenNew);
                if self.connection.account == AccountChoice::New {
                    rows.push(CreateRow::TokenName);
                    rows.push(CreateRow::Token);
                }
            }
        }
        if let Some(agent) = self.models_agent(catalog) {
            rows.push(CreateRow::ModelsRefresh);
            if self.models_offered(catalog) {
                rows.push(CreateRow::ModelsFold);
                rows.extend(model_rows(
                    Slot::of(agent),
                    self.connection.model_pick,
                    &self.enabled_models(catalog),
                    CreateRow::Model,
                    CreateRow::ModelDefault,
                    CreateRow::ModelPick,
                ));
                rows.extend((0..catalog.models.len()).map(CreateRow::ModelOn));
            }
            let conn = self.connection.litellm.as_deref().unwrap_or_default();
            let files = crate::litellm::config_file_names(conn, agent).len();
            if files > 0 {
                rows.push(CreateRow::FilesFold);
            }
            rows.extend((0..files).map(CreateRow::ConfigFile));
        }
        rows.extend(self.addon_order(catalog).into_iter().map(CreateRow::Addon));
        rows.push(CreateRow::Name);
        if self.start_dir_offered(catalog) {
            rows.push(CreateRow::StartDir);
        }
        rows.extend((0..self.workspace.extra_folders.len()).map(CreateRow::Folder));
        rows.push(CreateRow::FolderAdd);
        rows.push(CreateRow::Clone);
        if self.git_guard_offered() {
            rows.push(CreateRow::GitGuard);
        }
        if self.skills_offered(catalog) {
            rows.push(CreateRow::Skills);
        }
        rows.push(CreateRow::AdvancedFold);
        rows.push(CreateRow::Ports);
        rows.push(CreateRow::DenyNetwork);
        rows.push(CreateRow::McpNone);
        rows.extend((0..self.workspace.mcp.len()).map(CreateRow::Mcp));
        rows
    }

    /// Is `row` on screen, or folded away under [`CreateRow::ModelsFold`], [`CreateRow::FilesFold`]
    /// or [`CreateRow::AdvancedFold`]? A folded row still holds its answer, and is still checked.
    pub fn row_shown(&self, row: CreateRow) -> bool {
        row.fold().is_none_or(|fold| self.fold_open(fold))
    }

    /// Whether the fold row `fold` shows the rows under it.
    pub fn fold_open(&self, fold: CreateRow) -> bool {
        match fold {
            CreateRow::ModelsFold => self.wizard.models_open,
            CreateRow::FilesFold => self.wizard.files_open,
            CreateRow::AdvancedFold => self.wizard.advanced_open,
            _ => true,
        }
    }

    /// Show or hide the rows under the fold row `fold`.
    pub(crate) fn set_fold(&mut self, fold: CreateRow, open: bool) {
        let wizard = &mut self.wizard;
        match fold {
            CreateRow::ModelsFold => wizard.models_open = open,
            CreateRow::FilesFold => wizard.files_open = open,
            CreateRow::AdvancedFold => wizard.advanced_open = open,
            _ => {}
        }
    }

    /// Open the fold that hides `row`, if one does: a refused answer must be on screen.
    pub fn reveal(&mut self, row: CreateRow) {
        if let Some(fold) = row.fold() {
            self.set_fold(fold, true);
        }
    }

    /// The order the addon rows are offered in: the host-cache kits first (the form's *Host
    /// caches* block), then the others, each in catalog order. The renderer draws them the same way.
    /// A kit that does not work with the picked harness ([`Addon::applies_to`]) is not offered.
    pub fn addon_order(&self, catalog: &Catalog) -> Vec<usize> {
        let harness = self.effective_backend(catalog).map(Backend::harness);
        let applies = |i: &usize| harness.is_none_or(|h| catalog.addons[*i].applies_to(h));
        let host = |i: &usize| self.kits.host.get(*i).is_some_and(Option::is_some);
        let (mut first, rest): (Vec<usize>, Vec<usize>) =
            (0..catalog.addons.len()).filter(applies).partition(host);
        first.extend(rest);
        first
    }

    /// Is addon `i` a host-cache kit that would mount nothing here — drawn greyed out, and never
    /// ticked?
    pub fn addon_unavailable(&self, i: usize) -> bool {
        self.kits
            .host
            .get(i)
            .and_then(Option::as_ref)
            .is_some_and(|d| !d.detected())
    }

    /// Space, ↵ and a click on an addon row: flip it, unless it is unavailable on this host.
    pub(crate) fn toggle_addon(&mut self, i: usize) {
        if !self.addon_unavailable(i) {
            self.kits.addons[i] = !self.kits.addons[i];
        }
    }

    /// Space, ↵ and a click on an MCP server: flip it. A ticked server ends *none*, since sbx
    /// refuses `none` beside a server's name.
    pub(crate) fn toggle_mcp(&mut self, i: usize) {
        let ws = &mut self.workspace;
        if let Some(on) = ws.static_mcp.get_mut(i) {
            *on = !*on;
            if *on {
                ws.no_mcp = false;
            }
        }
    }

    /// Space, ↵ and a click on *none*: flip it. Ticked, it clears every server's tick.
    pub(crate) fn toggle_no_mcp(&mut self) {
        let ws = &mut self.workspace;
        ws.no_mcp = !ws.no_mcp;
        if ws.no_mcp {
            ws.static_mcp.iter_mut().for_each(|on| *on = false);
        }
    }

    /// The addons ticked among those offered ([`Self::addon_order`]), in catalog order: a kit
    /// ticked for another harness stays ticked for it, and is not built.
    pub fn selected_addons(&self, catalog: &Catalog) -> Vec<String> {
        let offered = self.addon_order(catalog);
        catalog
            .addons
            .iter()
            .zip(&self.kits.addons)
            .enumerate()
            .filter(|(i, (_, on))| **on && offered.contains(i))
            .map(|(_, (a, _))| a.kit.name.clone())
            .collect()
    }

    /// The MCP servers ticked, by name, or [`sbx::STATIC_MCP_NONE`] alone: what `--static-mcp` is
    /// given. Empty leaves the sandbox's set to the agent.
    pub fn selected_mcp(&self) -> Vec<String> {
        if self.workspace.no_mcp {
            return vec![sbx::STATIC_MCP_NONE.to_string()];
        }
        self.workspace
            .mcp
            .iter()
            .zip(&self.workspace.static_mcp)
            .filter(|(_, on)| **on)
            .map(|(m, _)| m.name.clone())
            .collect()
    }

    pub(crate) fn refresh_auto_name(&mut self, catalog: &Catalog) {
        if self.workspace.name_touched {
            return;
        }
        let Some(backend) = self.effective_backend(catalog) else {
            return;
        };
        let backend = backend.kit.name.clone();
        let account = self.label_account(catalog);
        let addons = self.selected_addons(catalog);
        let label = model::auto_label(catalog, &backend, account.as_deref(), &addons);
        if label != self.workspace.name.value {
            self.workspace.name.set(label);
        }
    }

    /// How the sandbox signs in, as its generated label names it ([`model::label_account`]): the
    /// `/login`, the token picked or named, or the connection picked or typed.
    pub fn label_account(&self, catalog: &Catalog) -> Option<String> {
        let backend = self.effective_backend(catalog)?;
        let token = match &self.connection.account {
            AccountChoice::Named(name) => Some(name.as_str()),
            AccountChoice::New => Some(self.connection.token_name.value.as_str()),
            AccountChoice::Login => None,
        };
        let typed = self.connection.new_conn.name.value.trim();
        let connection = match self.connection.litellm.as_deref() {
            Some(name) => Some(name),
            None => crate::litellm::valid_name(typed).then_some(typed),
        };
        model::label_account(backend, token, connection)
    }
}

// --- model aliases, shared by the create form and the kits editor -------------------------

/// One row per slot, with the open one's model list - the catalog indices `models` - expanded
/// beneath it.
fn model_rows<R>(
    slots: &[Slot],
    open: Option<Slot>,
    models: &[usize],
    alias: fn(Slot) -> R,
    default: fn(Slot) -> R,
    pick: fn(Slot, usize) -> R,
) -> Vec<R> {
    let mut rows = Vec::new();
    for slot in slots {
        rows.push(alias(*slot));
        if open == Some(*slot) {
            rows.push(default(*slot));
            rows.extend(models.iter().map(|i| pick(*slot, *i)));
        }
    }
    rows
}

/// Open an alias's model list, or close it if it is the one already open. Returns whether rows
/// appeared below the cursor, so the caller can step into them.
pub(crate) fn toggle_model_pick(open: &mut Option<Slot>, slot: Slot) -> bool {
    match *open == Some(slot) {
        true => {
            *open = None;
            false
        }
        false => {
            *open = Some(slot);
            true
        }
    }
}

/// Pin an alias to one of the catalog's models, and close the list.
pub(crate) fn choose_model(
    over: &mut Overrides,
    open: &mut Option<Slot>,
    slot: Slot,
    model: Option<&crate::models::Model>,
) {
    over.set(slot, model.map(|m| m.id.clone()));
    *open = None;
}

/// A focusable line in the auth editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRow {
    Oauth,
    UseToken,
    /// One of the named tokens stored on this host.
    TokenPick(usize),
    /// Store a new named token instead.
    TokenNew,
    Name,
    Value,
    Apply,
    Cancel,
}

impl AuthRow {
    /// A row that takes typed text: ↵ and a click move off it rather than act.
    pub fn is_text(self) -> bool {
        matches!(self, AuthRow::Name | AuthRow::Value)
    }

    /// A row whose value is a secret, drawn one `•` per character.
    pub fn masked(self) -> bool {
        self == AuthRow::Value
    }
}

/// Change which Anthropic account one existing sandbox is on. [`App::auth_plan`] works out what
/// that costs before anything runs.
#[derive(Debug, Clone)]
pub struct AuthForm {
    pub sandbox: String,
    /// What the sandbox is on right now.
    pub current: Auth,
    pub account: AccountChoice,
    /// The named tokens on this host when the form was opened.
    pub tokens: Vec<String>,
    /// The new token's name.
    pub token_name: TextInput,
    pub token: TextInput,
    pub focus: usize,
    /// The sandbox's kit volumes are in RAM ([`model::Sandbox::tmpfs_volumes`]), so the rebuild
    /// an account change is loses its session history - read once when the form opens.
    pub ram_volumes: bool,
}

/// What `sbx rm` leaves of a v3 sandbox: its kit volumes, keyed on the sandbox's name and kit
/// folder.
const VOLUMES_STAY: &str =
    "The agent's earlier sessions stay, and come back to a new sandbox of the \
     same name and kit.";

/// What `sbx rm` leaves of a sandbox whose kit volumes are in RAM (`aisbx up --tmpfs-volumes`,
/// [`crate::tmpfs`]): nothing, since a tmpfs volume never was on disk.
const VOLUMES_IN_RAM: &str = "The agent's earlier sessions go with it: its volumes are in RAM.";

/// Where the session history goes when an account change rebuilds a sandbox whose kit volumes are
/// in RAM (`aisbx up --tmpfs-volumes`): nowhere, and the rebuild keeps the volumes in RAM.
const SWITCH_IN_RAM: &str = "The agent's earlier sessions are lost: its volumes are in RAM.";

/// What a rebuild onto the other account's kit costs, besides the container's contents.
const SWITCH_KIT_LOSES: &str = "What you installed in the sandbox is lost.";

/// Where the session history goes when an account change swaps the workload. sbx keys a kit
/// volume on the sandbox's name and the kit folder.
const SWITCH_KIT_HISTORY: &str =
    "The agent's earlier sessions stay with the old account, and come \
     back if you switch back.";

impl AuthForm {
    /// Apply and Cancel, drawn as one line of buttons: the block ←→ moves within and ↑↓ step
    /// over ([`step_form`]).
    pub const BUTTONS: usize = 2;

    pub fn rows(&self) -> Vec<AuthRow> {
        let mut rows = vec![AuthRow::Oauth, AuthRow::UseToken];
        if self.account.on_token() {
            rows.extend((0..self.tokens.len()).map(AuthRow::TokenPick));
            rows.push(AuthRow::TokenNew);
            if self.account == AccountChoice::New {
                rows.push(AuthRow::Name);
            }
            rows.push(AuthRow::Value);
        }
        rows.push(AuthRow::Apply);
        rows.push(AuthRow::Cancel);
        rows
    }

    /// Switch to storing a new token, and clear a name that is already taken. Returns whether
    /// this opened the rows below.
    fn choose_new(&mut self) -> bool {
        if self.account == AccountChoice::New {
            return false;
        }
        let typed = self.token_name.value.trim().to_string();
        if self.tokens.contains(&typed) || self.current.token_name() == Some(&typed) {
            self.token_name = TextInput::default();
        }
        self.account = AccountChoice::New;
        true
    }

    /// The placeholder the container carries, if it is on a token at all.
    pub fn placeholder(&self) -> Option<&str> {
        self.current.placeholder()
    }

    /// The placeholder the chosen account would use.
    pub fn target_placeholder(&self) -> Option<String> {
        match &self.account {
            AccountChoice::Login => None,
            AccountChoice::Named(name) => Some(sbx::named_token_placeholder(name)),
            AccountChoice::New => Some(sbx::named_token_placeholder(self.token_name.value.trim())),
        }
    }

    /// Applying this rebuilds the container: the account is a different backend kit, or the token
    /// is a different placeholder than the one baked in.
    pub fn needs_recreate(&self) -> bool {
        if self.account.on_token() != self.current.on_token() {
            return true;
        }
        match (self.target_placeholder(), self.placeholder()) {
            (Some(target), Some(current)) => target != current,
            _ => false,
        }
    }

    pub fn changed(&self) -> bool {
        if self.needs_recreate() {
            return true;
        }
        // A sandbox with no token counts as changed, also with the form untouched.
        self.account.on_token() && (self.current.broken() || !self.token.value.trim().is_empty())
    }

    /// Exactly what pressing Apply would do, spelled out before it happens.
    pub fn effect(&self) -> &'static [&'static str] {
        if !self.changed() {
            return &["No changes."];
        }
        if self.needs_recreate() {
            return match (self.account.on_token(), self.current.on_token(), self.ram_volumes) {
                (true, false, false) => &[
                    "Applying this rebuilds the sandbox. Your files stay; the agent's earlier",
                    "sessions stay with /login and come back if you switch back.",
                ],
                (true, false, true) | (false, _, true) | (true, true, true) => &[
                    "Applying this rebuilds the sandbox. Your files stay; the agent's earlier",
                    "sessions are lost.",
                ],
                (false, _, false) => &[
                    "Applying this rebuilds the sandbox on /login, which every /login sandbox shares.",
                    "Your files stay; the agent's earlier sessions stay with the token.",
                ],
                (true, true, false) => &[
                    "Applying this rebuilds the sandbox on the other token. Your files and the agent's",
                    "earlier sessions stay.",
                ],
            };
        }
        &["Applying this stores the token. It works at once; nothing is rebuilt."]
    }
}

/// The skills screen's line on who gets its changes: the sandboxes that share the store.
pub fn skills_shared_line(sharers: usize) -> String {
    match sharers {
        0 => "Shared with every new sandbox that gets the skills; none here yet.".into(),
        1 => "Shared with 1 sandbox here: it gets a change at its next session.".into(),
        n => format!("Shared with {n} sandboxes here: they get a change at their next session."),
    }
}

/// The secrets screen's heading over the MCP servers' own secrets, which no sandbox sees.
pub const MCP_SECRETS: &str = "MCP servers' secrets";

/// The secrets screen's line over its rows: where the values are, and who sees them.
pub const SECRETS_NOTE: &str =
    "Stored on this computer and shown masked. A sandbox never sees them: sbx puts them into its      requests on the way out.";

/// A focusable line on the secrets screen — one stored credential, or the button that closes it.
/// Derived from `sbx secret ls` and the kit catalog on every render, not held in the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRow {
    /// The host-global Anthropic credential `/login` writes — sbx's own `anthropic` service.
    /// Always listed, stored or not.
    Oauth,
    /// The host-global Google sign-in Antigravity's `agy` writes — the `antigravity` service the
    /// `agy` kit's OAuth credential names. Listed whenever the catalog has the `agy` backend.
    AgyLogin,
    /// A named setup token, by name.
    Token(String),
    /// A LiteLLM connection, by name: its `connection.json`, its key's state read from
    /// `sbx secret ls` on every refresh. d deletes both.
    Connection(String),
    /// A connection key no `connection.json` names — what a crash between storing a new
    /// connection's key and writing its file leaves. d removes it.
    LitellmOrphan(String),
    /// A service secret: one a kit's `credentials:` names (stored or still missing), or any other
    /// one stored on this host. `scope` is `None` for the global row.
    Service {
        name: String,
        scope: Option<String>,
    },
    Close,
}

impl SecretRow {
    /// A service secret at global scope — the shape a kit's `credentials:` asks for.
    pub fn global_service(name: &str) -> Self {
        SecretRow::Service {
            name: name.to_string(),
            scope: None,
        }
    }

    /// What this row writes and drops, or `None` for the button.
    fn target(&self) -> Option<SecretTarget> {
        match self {
            SecretRow::Oauth
            | SecretRow::AgyLogin
            | SecretRow::Connection(_)
            | SecretRow::LitellmOrphan(_)
            | SecretRow::Close => None,
            SecretRow::Token(name) => Some(SecretTarget::Token(name.clone())),
            SecretRow::Service { name, scope } => Some(SecretTarget::Service {
                name: name.clone(),
                scope: scope.clone(),
            }),
        }
    }
}

/// What a secret row shows where `sbx secret ls` lists no value (see [`App::secret_view`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Empty {
    /// `sbx secret ls` has not answered yet.
    Unknown,
    /// The Antigravity sign-in, which `sbx secret ls` never lists, with no agy sandbox to ask.
    NotListed,
    /// The Antigravity sign-in, as an agy sandbox's `auth_mode` reports it: stored on the host.
    SignedIn,
    /// The Antigravity sign-in: every agy sandbox reports `auth_mode` `none · antigravity`.
    NotSignedIn,
    /// Nothing stored: a warning.
    NotStored,
}

/// One secret row's facts, decided once for both frontends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretView {
    pub name: String,
    /// sbx's own masked text, never a value.
    pub stored: Option<String>,
    pub note: String,
    /// What to show when `stored` is `None`.
    pub empty: Empty,
}

/// Which connection the `S` dialog is about: a new one (`n`), or an existing one by name (`↵`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnMode {
    New,
    Edit(String),
}

/// A focusable line in the `S` connection dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnRow {
    /// Only for a new connection: an existing one's name is its placeholder, so it never changes.
    Name,
    Url,
    Key,
    Save,
    Cancel,
}

impl ConnRow {
    /// A row that takes typed text: a click only focuses it.
    pub fn is_text(self) -> bool {
        !matches!(self, ConnRow::Save | ConnRow::Cancel)
    }

    /// A row whose value is a secret, drawn one `•` per character.
    pub fn masked(self) -> bool {
        self == ConnRow::Key
    }
}

/// The `S` dialog that adds a LiteLLM connection (`n`) or edits one (`↵`); Save works out
/// [`App::conn_plan`]. For an edit, the empty key field means "keep the stored key".
#[derive(Debug, Clone)]
pub struct ConnForm {
    pub mode: ConnMode,
    pub fields: crate::create::NewConnection,
    /// The connections on this host when the dialog was opened, for a first name check (the save
    /// checks the file again).
    pub connections: Vec<crate::litellm::Connection>,
    pub focus: usize,
}

impl ConnForm {
    /// Save and Cancel, one line of buttons.
    pub const BUTTONS: usize = 2;

    pub fn rows(&self) -> Vec<ConnRow> {
        let mut rows = match self.mode {
            ConnMode::New => vec![ConnRow::Name],
            ConnMode::Edit(_) => Vec::new(),
        };
        rows.extend([ConnRow::Url, ConnRow::Key, ConnRow::Save, ConnRow::Cancel]);
        rows
    }

    /// The same dialog, as it comes back after a key that could not be stored: as typed, the key
    /// emptied.
    fn without_key(mut self) -> Self {
        self.fields.key = TextInput::default();
        self
    }
}

/// A focusable line of the host folders screen (`F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldersRow {
    /// One folder of the list, by index into [`FoldersForm::folders`]: ↵ edits it below, `d`
    /// drops it.
    Folder(usize),
    Path,
    /// Opens the folder picker on the host; Use puts the picked folder into Path.
    Browse,
    /// Read-only or read-write; ←→ and space pick.
    Access,
    Purpose,
    Save,
    Close,
}

impl FoldersRow {
    /// A row that takes typing.
    pub fn is_text(self) -> bool {
        matches!(self, FoldersRow::Path | FoldersRow::Purpose)
    }
}

/// The host folders screen: the host's list ([`crate::folders`]), and the fields that add a
/// folder to it or change one.
#[derive(Debug, Clone, Default)]
pub struct FoldersForm {
    pub folders: Vec<folders::Entry>,
    /// Why the list could not be read; Save is refused while it is set, so the file stays as
    /// it is.
    pub error: Option<String>,
    /// The folder the fields change, by index into `folders`; `None` adds one.
    pub editing: Option<usize>,
    pub path: TextInput,
    pub access: folders::Access,
    pub purpose: TextInput,
    pub focus: usize,
}

impl FoldersForm {
    /// Save and Close, one line of buttons.
    pub const BUTTONS: usize = 2;

    pub fn rows(&self) -> Vec<FoldersRow> {
        let mut rows: Vec<FoldersRow> = (0..self.folders.len()).map(FoldersRow::Folder).collect();
        rows.extend([
            FoldersRow::Path,
            FoldersRow::Browse,
            FoldersRow::Access,
            FoldersRow::Purpose,
            FoldersRow::Save,
            FoldersRow::Close,
        ]);
        rows
    }

    pub fn focused(&self) -> FoldersRow {
        let rows = self.rows();
        rows[self.focus.min(rows.len() - 1)]
    }

    /// What `row` is about, for the TUI's help strip and the GUI's tooltip; no text where the
    /// row's label says it.
    pub fn help(&self, row: FoldersRow) -> crate::create::Help {
        use crate::create::Help;
        use crate::folders::Access;
        let said = |access: Access| {
            let mut label = access.label().to_string();
            label[..1].make_ascii_uppercase();
            format!("{label}: {}.", access.meaning())
        };
        match row {
            FoldersRow::Folder(i) => match self.folders.get(i) {
                Some(f) => Help::new(f.path.display().to_string(), said(f.access)),
                None => Help::new("", ""),
            },
            FoldersRow::Path => Help::new(
                "folder",
                "A folder on this computer, such as ~/notes. Not your home folder, a folder above \
                 it, or a folder with credentials.",
            ),
            FoldersRow::Browse => Help::new("browse", "Pick the folder instead of typing it."),
            FoldersRow::Access => Help::new(
                "access",
                format!("{} {}", said(Access::Read), said(Access::Write)),
            ),
            FoldersRow::Purpose => Help::new(
                "purpose",
                "One line on what the folder holds. The agent reads it, so it knows where to \
                 look.",
            ),
            FoldersRow::Save => Help::new("Save", ""),
            FoldersRow::Close => Help::new("Close", ""),
        }
    }
}

/// What the host folders screen says under its title.
pub const FOLDERS_NOTE: &str = "Every sandbox mounts these folders too, from its next create or \
     rebuild. The agent learns what each folder is for.";

/// A focusable line in the `A` form of a LiteLLM sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnSwitchRow {
    /// ←→ among the connections and *new connection…*, like the create form's row.
    Conn,
    Name,
    Url,
    Key,
    Apply,
    Cancel,
}

impl ConnSwitchRow {
    /// A row that takes typed text: a click only focuses it.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            ConnSwitchRow::Name | ConnSwitchRow::Url | ConnSwitchRow::Key
        )
    }

    /// A row whose value is a secret, drawn one `•` per character.
    pub fn masked(self) -> bool {
        self == ConnSwitchRow::Key
    }
}

/// `A` on a LiteLLM sandbox: move it to another connection, or to a new one, by a rebuild.
#[derive(Debug, Clone)]
pub struct ConnSwitchForm {
    pub sandbox: String,
    /// The connection its kit list names now; `None` when it names none.
    pub current: Option<String>,
    /// The connections on this host when the form was opened, sorted by name.
    pub connections: Vec<crate::litellm::Connection>,
    pub pick: crate::litellm::LitellmPick,
    /// What *new connection…* is typed into.
    pub new_conn: crate::create::NewConnection,
    pub focus: usize,
}

impl ConnSwitchForm {
    /// Apply and Cancel, one line of buttons.
    pub const BUTTONS: usize = 2;

    pub fn rows(&self) -> Vec<ConnSwitchRow> {
        let mut rows = vec![ConnSwitchRow::Conn];
        if self.pick == crate::litellm::LitellmPick::New {
            rows.extend([ConnSwitchRow::Name, ConnSwitchRow::Url, ConnSwitchRow::Key]);
        }
        rows.extend([ConnSwitchRow::Apply, ConnSwitchRow::Cancel]);
        rows
    }

    /// ←→ on the Connection row: every connection by name, then *new connection…*.
    pub fn cycle(&mut self, forward: bool) {
        use crate::litellm::LitellmPick;
        let n = self.connections.len() + 1;
        let at = match &self.pick {
            LitellmPick::Existing(name) => self
                .connections
                .iter()
                .position(|c| c.name == *name)
                .unwrap_or(self.connections.len()),
            LitellmPick::New => self.connections.len(),
        };
        let next = match forward {
            true => (at + 1) % n,
            false => (at + n - 1) % n,
        };
        self.pick = match self.connections.get(next) {
            Some(c) => LitellmPick::Existing(c.name.clone()),
            None => LitellmPick::New,
        };
    }

    /// The connection picked, when it is an existing one.
    pub fn picked(&self) -> Option<&crate::litellm::Connection> {
        match &self.pick {
            crate::litellm::LitellmPick::Existing(name) => {
                self.connections.iter().find(|c| c.name == *name)
            }
            crate::litellm::LitellmPick::New => None,
        }
    }

    /// Would Apply move the sandbox: *new connection…*, or a connection other than its own. The
    /// rule [`App::switch_plan`] answers *no connection changes* by.
    pub fn moving(&self) -> bool {
        match &self.pick {
            crate::litellm::LitellmPick::Existing(name) => {
                self.current.as_deref() != Some(name.as_str())
            }
            crate::litellm::LitellmPick::New => true,
        }
    }

    fn without_key(mut self) -> Self {
        self.new_conn.key = TextInput::default();
        self
    }
}

/// Where a job that stored a key goes back to when it failed, once its log is closed: the form it
/// came from, as typed, its key emptied.
#[derive(Debug, Clone)]
pub enum Restore {
    Create(Box<CreateForm>),
    Connection(Box<ConnForm>),
    ConnSwitch(Box<ConnSwitchForm>),
}

/// What saving the `S` dialog or applying `A` on a LiteLLM sandbox amounts to — worked out with
/// no sbx call, like [`AuthPlan`] (see [`App::conn_plan`], [`App::switch_plan`]).
#[derive(Debug)]
pub enum ConnPlan {
    /// The form stays open; the message says what is missing.
    Refuse(String),
    /// Nothing to do; the message says so.
    Nothing(String),
    /// Only `connection.json` changes and no sandbox is on it: written at once, no sbx call.
    Write(crate::litellm::Connection),
    /// Runs straight away.
    Apply(Box<PendingJob>),
    /// Asked first: it reaches sandboxes on the connection, or it is a rebuild.
    Confirm(Box<Confirm>),
}

/// One sbx setting the settings screen offers: a boolean, with what each value means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSpec {
    pub key: &'static str,
    /// The row's label.
    pub label: &'static str,
    /// The state word for `true`, then for `false`.
    pub words: [&'static str; 2],
    /// What `true` means, then `false`.
    pub meanings: [&'static str; 2],
    /// What a change takes, and anything else a reader must know.
    pub note: &'static str,
    /// The value that keeps more to the sandbox, drawn as the calm state; the other is drawn as a
    /// warning. `None`: neither value lets more out, so neither is drawn either way.
    pub cautious: Option<bool>,
}

impl SettingSpec {
    /// The state word for `value`.
    pub fn word(&self, value: bool) -> &'static str {
        self.words[usize::from(!value)]
    }

    /// What `value` means.
    pub fn meaning(&self, value: bool) -> &'static str {
        self.meanings[usize::from(!value)]
    }

    /// How a frontend draws `value`'s state.
    pub fn tone(&self, value: Option<bool>) -> SettingTone {
        match (value, self.cautious) {
            (None, _) => SettingTone::Unknown,
            (Some(_), None) => SettingTone::Plain,
            (Some(value), Some(cautious)) if value == cautious => SettingTone::Calm,
            (Some(_), Some(_)) => SettingTone::Warn,
        }
    }
}

/// How a setting's state is drawn: [`SettingSpec::tone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingTone {
    Calm,
    Warn,
    Plain,
    Unknown,
}

/// The settings the screen offers, in its order.
pub const SETTINGS: [SettingSpec; 4] = [
    SettingSpec {
        key: sbx::TELEMETRY_DISABLED,
        label: "telemetry",
        words: ["off", "on"],
        meanings: [
            "sbx sends no usage data to Docker",
            "sbx sends usage data to Docker; sbx's default",
        ],
        note: "When on, sbx sends usage data to Docker. Otherwise, it sends none. A change takes \
               effect after `sbx daemon restart`.",
        cautious: Some(true),
    },
    SettingSpec {
        key: sbx::CLIPBOARD_TEXT_COPY,
        label: "clipboard copy",
        words: ["on", "off"],
        meanings: [
            "a sandbox can copy text to your clipboard",
            "a sandbox cannot copy to your clipboard; sbx's default",
        ],
        note:
            "When on, the agent can copy text to your clipboard, and so can anything else in any \
               sandbox. Otherwise, nothing in a sandbox reaches your clipboard.",
        cautious: Some(false),
    },
    SettingSpec {
        key: sbx::CLIPBOARD_IMAGE_PASTE,
        label: "image paste",
        words: ["on", "off"],
        meanings: [
            "Ctrl+V pastes images from your clipboard into the agent",
            "the agent cannot read images from your clipboard; sbx's default",
        ],
        note:
            "When on, Ctrl+V pastes the image on your clipboard into the agent, in every sandbox. \
               Otherwise, no sandbox can read the images on your clipboard.",
        cautious: Some(false),
    },
    SettingSpec {
        key: sbx::SANDBOX_FUSE,
        label: "FUSE",
        words: ["on", "off"],
        meanings: [
            "new sandboxes can mount FUSE file systems; sbx's default",
            "new sandboxes cannot mount file systems",
        ],
        note:
            "When on, new sandboxes can mount FUSE file systems. Otherwise, they cannot, and the \
               host-npm and host-pip kits cannot use your package caches. It applies to sandboxes \
               created after the change.",
        cautious: None,
    },
];

/// The settings screen: the [`SETTINGS`] and the skills mode new sandboxes default to, read
/// from sbx when the screen opens, on F5 and after each change.
#[derive(Debug, Clone)]
pub struct SettingsForm {
    pub focus: usize,
    /// One per [`SETTINGS`], in its order; `None` while unread or when its read failed.
    pub values: Vec<Option<sbx::Setting>>,
    /// `skills.defaultMode`; `None` while unread or when its read failed.
    pub skills: Option<sbx::Setting>,
    /// Why the last read failed.
    pub error: Option<String>,
}

impl Default for SettingsForm {
    fn default() -> Self {
        SettingsForm {
            focus: 0,
            values: vec![None; SETTINGS.len()],
            skills: None,
            error: None,
        }
    }
}

/// A focusable line on the settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// One of the [`SETTINGS`], by index.
    Setting(usize),
    /// `skills.defaultMode`: one of three, not a toggle.
    SkillsDefault,
    Close,
}

/// The skills row's label.
pub const SKILLS_DEFAULT_LABEL: &str = "skills default";

/// The skills row's note: what the mode is the default of.
pub const SKILLS_DEFAULT_NOTE: &str = "Stored with `sbx settings set skills.defaultMode \
     off|readonly|readwrite`. The create wizard opens on it, and `aisbx up` and a plain `sbx create` \
     use it when no `--skills` is given. It reaches only sandboxes created after the change.";

impl SettingsForm {
    pub fn rows(&self) -> Vec<SettingsRow> {
        (0..SETTINGS.len())
            .map(SettingsRow::Setting)
            .chain([SettingsRow::SkillsDefault, SettingsRow::Close])
            .collect()
    }

    /// Read the settings from sbx; nothing runs under `cfg(test)`.
    pub fn read(&mut self) {
        if cfg!(any(test, feature = "test-support")) {
            return;
        }
        self.error = None;
        for (value, spec) in self.values.iter_mut().zip(SETTINGS) {
            match sbx::setting(spec.key) {
                Ok(setting) => *value = Some(setting),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        match sbx::setting(sbx::SKILLS_DEFAULT_MODE) {
            Ok(setting) => self.skills = Some(setting),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    /// The skills mode new sandboxes default to; `None` while unknown.
    pub fn skills_mode(&self) -> Option<sbx::SkillsMode> {
        sbx::SkillsMode::parse(self.skills.as_ref()?.value.as_str()?)
    }

    /// The skills row's state word, what it means, and how it is drawn, for both frontends.
    pub fn skills_view(&self) -> (&'static str, String, SettingTone) {
        let Some(mode) = self.skills_mode() else {
            let meaning = match &self.error {
                Some(e) => format!("could not read it: {e}"),
                None => "not read yet".into(),
            };
            return ("unknown", meaning, SettingTone::Unknown);
        };
        let (meaning, tone) = match mode {
            sbx::SkillsMode::Off => (
                "new sandboxes get no shared skills store",
                SettingTone::Calm,
            ),
            sbx::SkillsMode::Readonly => (
                "new sandboxes see the shared skills and cannot change them; sbx's default",
                SettingTone::Plain,
            ),
            sbx::SkillsMode::Readwrite => (
                "new sandboxes can change the shared skills every sandbox loads",
                SettingTone::Warn,
            ),
        };
        (crate::create::skills_mode_name(mode), meaning.into(), tone)
    }

    /// Setting `i`'s value; `None` while unknown.
    pub fn value(&self, i: usize) -> Option<bool> {
        self.values.get(i)?.as_ref()?.flag()
    }

    /// Setting `i`'s state word and what it means, for both frontends.
    pub fn view(&self, i: usize) -> (&'static str, String) {
        let spec = &SETTINGS[i];
        match (self.value(i), &self.error) {
            (Some(value), _) => match self.env_var(i) {
                Some(var) => (
                    spec.word(value),
                    format!("{}; set by {var} in sbx's environment", spec.meaning(value)),
                ),
                None => (spec.word(value), spec.meaning(value).into()),
            },
            (None, Some(e)) => ("unknown", format!("could not read it: {e}")),
            (None, None) => ("unknown", "not read yet".into()),
        }
    }

    /// The environment variable that sets setting `i` now, if one does.
    pub fn env_var(&self, i: usize) -> Option<&str> {
        let setting = self.values.get(i)?.as_ref()?;
        setting.from_env().then_some(setting.env_var.as_str())
    }
}

/// Under *Close*: what the settings are.
pub const SETTINGS_NOTE: &str = "sbx's settings for this computer: every sandbox follows them.";

/// The secrets screen holds nothing but the cursor: every row on it is read from the host.
#[derive(Debug, Clone, Default)]
pub struct SecretsForm {
    pub focus: usize,
}

/// The approvals screen (`y`), likewise: its rows are `sbx policy approval ls --json`, re-read
/// every refresh, plus the button that closes it.
#[derive(Debug, Clone, Default)]
pub struct ApprovalsForm {
    pub focus: usize,
}

/// A focusable line on the approvals screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRow {
    /// One pending request, by its approval id.
    Request(String),
    Close,
}

/// The skills screen, likewise: the rows are `sbx skills ls --json` plus the three actions.
#[derive(Debug, Clone, Default)]
pub struct SkillsForm {
    pub focus: usize,
}

/// A focusable line on the skills screen (`sbx skills add/ls/rm/update`). Derived from the store
/// on every render, like [`SecretRow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRow {
    /// One skill in the store, by name — all `sbx skills ls --json` reports about it.
    Skill(String),
    /// `sbx skills import --force`: copy this host's own agent skills in.
    Import,
    /// `sbx skills add <repo> --force`: install from a Git repository.
    AddRepo,
    /// `sbx skills update`: refresh everything that came from a repository.
    Update,
    /// The Repo skills screen for every sandbox: the configured skill repos' global picks.
    RepoSkills,
    Close,
}

/// A focusable line in the kit editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitsRow {
    Addon(usize),
    /// One of Claude Code's model aliases — see [`CreateRow::Model`].
    Model(Slot),
    ModelDefault(Slot),
    ModelPick(Slot, usize),
    Apply,
    Cancel,
}

/// Edit what one existing sandbox is built from: its stackable kits, and — on a gateway backend —
/// which model each of Claude Code's aliases resolves to.
#[derive(Debug, Clone)]
pub struct KitsForm {
    pub sandbox: String,
    pub addons: Vec<bool>,
    pub original: Vec<bool>,
    /// Whether this sandbox's backend has model aliases to pin at all — read once when the form
    /// opens, since the backend cannot change here.
    pub models_offered: bool,
    pub models: Overrides,
    pub original_models: Overrides,
    pub model_pick: Option<Slot>,
    pub focus: usize,
    /// The sandbox's kit volumes are in RAM ([`model::Sandbox::tmpfs_volumes`]), so whatever
    /// recreates its container loses the session history. Read once when the form opens.
    pub ram_volumes: bool,
    /// Its backend kit keeps the session history in volumes
    /// ([`model::Sandbox::history_in_volumes`]). Read once when the form opens.
    pub history_in_volumes: bool,
    /// The agent CLI the sandbox runs ([`kits::agent_command`]), which decides the kits offered;
    /// `None` offers every kit.
    pub harness: Option<String>,
}

impl KitsForm {
    /// Where the sandbox keeps its session history, for [`KitsEffect::note`].
    pub fn history(&self) -> History {
        History::of(self.ram_volumes, self.history_in_volumes)
    }

    /// Apply and Cancel, drawn as one line of buttons: the block ←→ moves within and ↑↓ step
    /// over ([`step_form`]).
    pub const BUTTONS: usize = 2;

    pub fn rows(&self, catalog: &Catalog) -> Vec<KitsRow> {
        let mut rows: Vec<KitsRow> = self
            .addon_order(catalog)
            .into_iter()
            .map(KitsRow::Addon)
            .collect();
        if self.models_offered {
            rows.extend(model_rows(
                &Slot::ALL,
                self.model_pick,
                &(0..catalog.models.len()).collect::<Vec<_>>(),
                KitsRow::Model,
                KitsRow::ModelDefault,
                KitsRow::ModelPick,
            ));
        }
        rows.push(KitsRow::Apply);
        rows.push(KitsRow::Cancel);
        rows
    }

    pub fn changed(&self) -> bool {
        self.addons != self.original || self.models_changed()
    }

    /// The order the editor lists the kits in, as the create wizard's Kits step does: the
    /// host-folder kits first ([`model::is_host_folder_kit`]), then the others, in catalog order.
    /// A kit that does not work with the sandbox's harness is left out, unless the sandbox has it.
    pub fn addon_order(&self, catalog: &Catalog) -> Vec<usize> {
        let applies = |i: &usize| {
            self.original.get(*i) == Some(&true)
                || self
                    .harness
                    .as_deref()
                    .is_none_or(|h| catalog.addons[*i].applies_to(h))
        };
        let host = |i: &usize| model::is_host_folder_kit(&catalog.addons[*i].kit.name);
        let (mut first, rest): (Vec<usize>, Vec<usize>) =
            (0..catalog.addons.len()).filter(applies).partition(host);
        first.extend(rest);
        first
    }

    /// What `row` of the editor is about, for the TUI's help strip.
    pub fn help(&self, catalog: &Catalog, row: KitsRow) -> crate::create::Help {
        use crate::create::Help;
        match row {
            KitsRow::Addon(i) => {
                let Some(addon) = catalog.addons.get(i) else {
                    return Help::new("", "");
                };
                let cost = match (self.original.get(i), addon.create_time_reason()) {
                    (Some(true), _) => "On this sandbox now. Removing it rebuilds the sandbox.",
                    (_, Some(_)) => crate::create::ONLY_NOW_NOTE,
                    (_, None) => "It is added without a rebuild.",
                };
                let text = format!("{cost} {}", addon.kit.first_sentence());
                Help::new(addon.kit.name.clone(), text.trim())
            }
            KitsRow::Model(slot) => crate::create::alias_help(slot),
            KitsRow::ModelDefault(slot) => crate::create::default_model_help(slot),
            KitsRow::ModelPick(slot, i) => crate::create::model_pick_help(catalog, slot, i),
            KitsRow::Apply => Help::new("Apply", ""),
            KitsRow::Cancel => Help::new("Cancel", ""),
        }
    }

    pub fn models_changed(&self) -> bool {
        self.models != self.original_models
    }

    /// Was the sandbox already carrying a model kit? Changing what an existing kit sets means a
    /// rebuild; giving a sandbox its first one is an ordinary `sbx kit add`.
    pub fn had_models(&self) -> bool {
        !self.original_models.is_empty()
    }
}

/// Why applying the kits editor rebuilds the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildCause {
    /// Only a pin the sandbox already carries changes: sbx bakes a kit's variables in at build.
    Models,
    /// A kit is removed, or one that needs read-only mounts is added.
    Kits,
    /// The sandbox's kits come from another checkout; a live add would leave it straddling two.
    OtherCheckout,
}

/// What *Apply* on the kits editor does — decided once, here, for `App::submit_kits` and for the
/// note every frontend draws above the buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitsEffect {
    NoChanges,
    /// `sbx kit add` of every added kit (and a first pin), in place.
    KitAdd,
    /// `sbx kit add` is tried, but sbx refuses a kit declaring this field — expect the rebuild
    /// fallback.
    KitAddRefused(&'static str),
    Rebuild(RebuildCause),
}

/// Where a sandbox keeps the agent's session history, and so whether a rebuild keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    /// In its backend kit's volumes, which outlive `sbx rm`: a rebuild keeps it.
    Volumes,
    /// In kit volumes in RAM (`aisbx up --tmpfs-volumes`): a rebuild loses it.
    Ram,
    /// Only in the container, since its backend kit declares no volume: a rebuild loses it.
    Container,
}

impl History {
    /// Where a sandbox keeps it ([`model::Sandbox::tmpfs_volumes`],
    /// [`model::Sandbox::history_in_volumes`]).
    pub fn of(ram_volumes: bool, in_volumes: bool) -> History {
        match (ram_volumes, in_volumes) {
            (true, _) => History::Ram,
            (false, true) => History::Volumes,
            (false, false) => History::Container,
        }
    }

    /// What a rebuild does to it, as one line of a note.
    fn after_rebuild(self) -> &'static str {
        match self {
            History::Volumes => "The agent's earlier sessions stay.",
            History::Ram | History::Container => "The agent's earlier sessions are lost.",
        }
    }
}

impl KitsEffect {
    /// The note the editor shows for this effect, one entry per line, given where the sandbox
    /// keeps its session history ([`KitsForm::history`]).
    pub fn note(self, history: History) -> Vec<String> {
        let rebuild = |lines: &[&str]| {
            let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            out.push(history.after_rebuild().into());
            out
        };
        match self {
            KitsEffect::Rebuild(RebuildCause::Models) => {
                rebuild(&["Applying this rebuilds the sandbox: other models need a rebuild."])
            }
            KitsEffect::Rebuild(RebuildCause::Kits) => rebuild(&[
                "Applying this rebuilds the sandbox, and what you installed in it is lost.",
            ]),
            KitsEffect::Rebuild(RebuildCause::OtherCheckout) => rebuild(&[
                "Applying this rebuilds the sandbox: its kits come from another checkout.",
            ]),
            KitsEffect::KitAddRefused(_) => vec![
                "Applying this tries to add the kits without a rebuild. That will likely fail and"
                    .into(),
                "offer a rebuild instead.".into(),
                history.after_rebuild().into(),
            ],
            KitsEffect::KitAdd => match history {
                History::Ram => vec![
                    "Applying this adds the kits without a rebuild. Your files and what you".into(),
                    "installed stay; the agent's earlier sessions are lost.".into(),
                ],
                _ => vec![
                    "Applying this adds the kits without a rebuild. Your files, the agent's".into(),
                    "sessions and what you installed stay.".into(),
                ],
            },
            KitsEffect::NoChanges => vec!["No changes.".into()],
        }
    }
}

impl KitsForm {
    /// Whether the editor removes a kit the sandbox has.
    pub fn removing(&self) -> bool {
        self.original
            .iter()
            .zip(&self.addons)
            .any(|(was, now)| *was && !*now)
    }

    /// What *Apply* would do. `other_checkout`: the sandbox's kits come from another checkout.
    pub fn effect(&self, catalog: &Catalog, other_checkout: bool) -> KitsEffect {
        if !self.changed() {
            return KitsEffect::NoChanges;
        }
        let added = || {
            catalog
                .addons
                .iter()
                .enumerate()
                .filter(|&(i, _)| self.addons[i] && !self.original[i])
                .map(|(_, a)| a)
        };
        let adding_mounts = added().any(|a| a.needs_mounts());
        let models_rebuild = self.models_changed() && self.had_models();
        if self.removing() || adding_mounts {
            return KitsEffect::Rebuild(RebuildCause::Kits);
        }
        if models_rebuild {
            return KitsEffect::Rebuild(RebuildCause::Models);
        }
        if other_checkout {
            return KitsEffect::Rebuild(RebuildCause::OtherCheckout);
        }
        match added().find_map(|a| a.create_time_reason()) {
            Some(field) => KitsEffect::KitAddRefused(field),
            None => KitsEffect::KitAdd,
        }
    }
}

/// A focusable line on the connect screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectRow {
    /// `sbx setup ssh`. Only present while the managed block is missing.
    SetupSsh,
    /// One of [`crate::remote::Target::ALL`].
    Target(usize),
    Cancel,
}

/// Connecting an editor or app to one sandbox over its SSH endpoint.
#[derive(Debug)]
pub struct ConnectForm {
    pub sandbox: String,
    /// Has `sbx setup ssh` written its managed block? Read from `~/.ssh/config` when the screen
    /// opens and after the setup job, never remembered — the user may delete it by hand.
    pub ssh_ready: bool,
    pub focus: usize,
}

impl ConnectForm {
    pub fn rows(&self) -> Vec<ConnectRow> {
        let mut rows = Vec::new();
        if !self.ssh_ready {
            rows.push(ConnectRow::SetupSsh);
        }
        rows.extend((0..crate::remote::Target::ALL.len()).map(ConnectRow::Target));
        rows.push(ConnectRow::Cancel);
        rows
    }

    pub fn focused(&self) -> ConnectRow {
        let rows = self.rows();
        rows[self.focus.min(rows.len() - 1)]
    }
}

/// How a Connect target is reached from here: its launcher (`Here`), its launcher missing from
/// the host's PATH (`NotOnPath`, naming it), or set up inside the app (`InApp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetLaunch {
    Here,
    NotOnPath(&'static str),
    InApp,
}

/// One target row of the Connect screen, in words a frontend lays out as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetView {
    pub row: ConnectRow,
    pub title: &'static str,
    pub launch: TargetLaunch,
    /// What choosing it does, or why it cannot be launched from here.
    pub status: String,
    /// What to do once it is chosen ([`crate::remote::Target::steps`]).
    pub steps: Vec<String>,
    /// Why the editor's server download will be blocked. Non-empty when the target downloads one,
    /// the sandbox lacks [`crate::remote::REMOTE_DEV_KIT`] and the checkout ships that kit.
    pub kit_warning: Vec<String>,
}

/// What the Connect screen says — the decisions both frontends draw, without their indent or
/// glyphs (`▸`, `·`, `⚠` are the TUI's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectView {
    /// The sandbox's label, for the title.
    pub label: String,
    /// The SSH host ([`sbx::ssh_host`]).
    pub host: String,
    /// *running*, or *stopped: connecting starts it*.
    pub state: &'static str,
    /// `sbx setup ssh` has not written its block yet: the warning and the setup row's note say so.
    pub ssh_missing: bool,
    pub ssh_warning: Vec<String>,
    pub ssh_notes: Vec<String>,
    /// One per [`crate::remote::Target::ALL`] entry, in that order.
    pub targets: Vec<TargetView>,
}

/// Why a request waits, as the approvals screens say it: sbx's default deny in words, an
/// organization's reason as sbx gives it.
pub fn approval_reason(request: &sbx::Approval) -> String {
    match request.default_deny() {
        true => NO_RULE_YET.to_string(),
        false => request.reason(),
    }
}

/// What a request held by sbx's default deny says.
pub const NO_RULE_YET: &str = "no rule allows it yet";

/// A row on the start-folder picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDirRow {
    /// `..` — leave this level. At the workspace root only for a sandbox not created yet, where
    /// it mounts the folder above instead ([`StartDirForm::widens`]).
    Up,
    /// One of this level's subdirectories, by index into [`StartDirForm::dirs`].
    Dir(usize),
    /// Use the browsed directory and hand the terminal over now.
    Start,
    /// Use it, but stay in the dashboard.
    Use,
    Cancel,
}

/// Choosing which directory inside the workspace a sandbox's agent opens in — the `w` screen.
/// It walks the tree on the host, and the buttons commit the level being browsed.
#[derive(Debug)]
pub struct StartDirForm {
    /// Which sandbox — existing or not yet created — the folder is being picked for.
    pub target: StartDirTarget,
    /// What the panel calls it.
    pub label: String,
    /// The workspace root; browsed above only by [`StartDirForm::widens`].
    pub root: PathBuf,
    /// The level being browsed, relative to `root`. Empty is the root itself.
    pub rel: PathBuf,
    /// `rel`'s subdirectories, as [`crate::startdir::children`] listed them.
    pub dirs: Vec<String>,
    pub focus: usize,
}

/// What a picked start folder is *for* — the one difference between the two ways in.
#[derive(Debug)]
pub enum StartDirTarget {
    /// An existing sandbox, by sbx name. The pick is a registry write, and can be started at once.
    Sandbox(String),
    /// The create form, which holds the pick until the sandbox exists — so there is nothing to
    /// start yet, and the form itself travels inside this variant to be handed back untouched.
    New(Box<CreateForm>),
    /// The host folders screen, whose Path field takes the pick. The form travels inside, as
    /// for `New`.
    HostFolder(Box<FoldersForm>),
    /// One more host folder for the create form ([`CreateRow::Folder`]), which travels inside,
    /// as for `New`.
    Folder(Box<CreateForm>),
}

impl StartDirForm {
    /// The count of buttons on the form's last line, like [`AuthForm::BUTTONS`]. Only an existing
    /// sandbox gets Start.
    pub fn buttons(&self) -> usize {
        match self.target {
            StartDirTarget::Sandbox(_) => 3,
            StartDirTarget::New(_) | StartDirTarget::HostFolder(_) | StartDirTarget::Folder(_) => 2,
        }
    }

    pub fn open(
        target: StartDirTarget,
        label: String,
        root: PathBuf,
        rel: Option<PathBuf>,
    ) -> Self {
        let rel = rel.unwrap_or_default();
        let mut form = Self {
            target,
            label,
            dirs: crate::startdir::children(&root.join(&rel)),
            root,
            rel,
            focus: 0,
        };
        // On a leaf level, open on `..`, a row that Left still works from.
        form.focus = match (form.dirs.is_empty(), form.rel.as_os_str().is_empty()) {
            (true, false) => 0,
            _ => form.first_dir_or_start(),
        };
        form
    }

    /// Whether `..` here mounts the folder above: at the root of a sandbox not created yet, where
    /// that folder may be mounted ([`model::mount_refusal`]: never home or a folder above it) and
    /// is not the file system's root.
    pub fn widens(&self) -> bool {
        if !self.rel.as_os_str().is_empty() || !matches!(self.target, StartDirTarget::New(_)) {
            return false;
        }
        let Some(parent) = self.root.parent() else {
            return false;
        };
        let home = model::home_dir();
        parent.parent().is_some() && home.is_none_or(|h| model::mount_refusal(parent, &h).is_none())
    }

    pub fn rows(&self) -> Vec<StartDirRow> {
        let mut rows = Vec::new();
        if !self.rel.as_os_str().is_empty() || self.widens() {
            rows.push(StartDirRow::Up);
        }
        rows.extend((0..self.dirs.len()).map(StartDirRow::Dir));
        if matches!(self.target, StartDirTarget::Sandbox(_)) {
            rows.push(StartDirRow::Start);
        }
        rows.extend([StartDirRow::Use, StartDirRow::Cancel]);
        rows
    }

    pub fn focused(&self) -> StartDirRow {
        let rows = self.rows();
        rows[self.focus.min(rows.len() - 1)]
    }

    /// The directory being browsed, absolute — what the buttons commit.
    pub fn here(&self) -> PathBuf {
        self.root.join(&self.rel)
    }

    /// What to store: the browsed level relative to the workspace, or `None` at the root, which is
    /// how "no start folder" is spelled everywhere else.
    pub fn chosen(&self) -> Option<PathBuf> {
        (!self.rel.as_os_str().is_empty()).then(|| self.rel.clone())
    }

    /// Is this level already the sandbox's start folder? Answered so the buttons can say
    /// *keep* rather than *use*, and so committing an unchanged choice is visibly a no-op.
    pub fn unchanged(&self, current: Option<&Path>) -> bool {
        self.chosen().as_deref() == current
    }

    pub fn descend(&mut self, index: usize) {
        let Some(name) = self.dirs.get(index) else {
            return;
        };
        self.rel = self.rel.join(name);
        self.dirs = crate::startdir::children(&self.here());
        self.focus = self.first_dir_or_start();
    }

    pub fn up(&mut self) {
        let widen = self.widens();
        let leaving = match widen {
            true => self.root.file_name(),
            false => self.rel.file_name(),
        }
        .map(|n| n.to_string_lossy().into_owned());
        if widen {
            if let Some(parent) = self.root.parent() {
                self.root = parent.to_path_buf();
            }
        } else if !self.rel.pop() {
            return;
        }
        self.dirs = crate::startdir::children(&self.here());
        let at = leaving.and_then(|name| self.dirs.iter().position(|d| *d == name));
        // Every directory row comes after `..` where there is one.
        let shift = usize::from(self.rows().first() == Some(&StartDirRow::Up));
        self.focus = match at {
            Some(i) => i + shift,
            None => self.first_dir_or_start(),
        };
    }

    /// The first directory row, or the Start button when this level has no subdirectories.
    fn first_dir_or_start(&self) -> usize {
        let rows = self.rows();
        rows.iter()
            .position(|r| matches!(r, StartDirRow::Dir(_)))
            .unwrap_or_else(|| {
                rows.iter()
                    .position(|r| *r == StartDirRow::Start)
                    .unwrap_or(0)
            })
    }
}

/// A stored secret the dashboard can write or drop, identified the way `sbx secret` identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretTarget {
    /// A service secret in the host keychain — a kit's `credentials:` name, or one of sbx's own.
    /// `scope` is `None` for the global row, or the sandbox a scoped one belongs to.
    Service { name: String, scope: Option<String> },
    /// A named setup token: one global row, shared by every sandbox pointed at the name.
    Token(String),
}

/// What a [`Prompt`]'s answer is for. Every one of them asks for a single line of text, so they
/// share one modal instead of five near-identical ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Set the local label. Never reaches sbx.
    Rename,
    /// `sbx ports <name> --publish <spec>`.
    PublishPort,
    /// `sbx ports <name> --unpublish <spec>`.
    UnpublishPort,
    /// `sbx policy check network --sandbox <name> <target>` — read-only.
    CheckHost,
    /// Store or replace a secret's value. Masked while typed, passed on stdin, never echoed.
    SetSecret(SecretTarget),
    /// `sbx skills add <repo> --force [--skill …]` — a repository, then optionally which of its
    /// skills to take.
    AddSkills,
}

impl Ask {
    /// The lines under the field, for the asks whose answer means the same thing every time. The
    /// secret prompts carry their own ([`Prompt::help`]).
    fn help(&self) -> Vec<String> {
        let lines: &[&str] = match self {
            Ask::Rename => {
                &["Any text is allowed — this label is local to aisbx and never reaches sbx."]
            }
            Ask::PublishPort => &[
                "[[HOST_IP:]HOST_PORT:]SANDBOX_PORT[/PROTOCOL]   e.g.  8080:3000  ·  3000",
                "A bare sandbox port gets an ephemeral host port, reassigned on each restart.",
                "No protocol binds 127.0.0.1 only (tcp4, so http://localhost:<port> works);",
                "add /tcp to bind ::1 as well. A stopped sandbox is started first.",
                "The service inside must listen on 0.0.0.0 (or [::]) — not on 127.0.0.1.",
            ],
            Ask::UnpublishPort => &[
                "HOST_PORT:SANDBOX_PORT[/PROTOCOL], or a bare sandbox port to drop every",
                "mapping to it. The Ports tab lists what is published.",
            ],
            Ask::CheckHost => &[
                "A host, host:port or URL. Bare hosts are checked on port 443.",
                "Read-only: it asks the policy authorizer, it opens no connection.",
            ],
            Ask::SetSecret(_) => &[],
            Ask::AddSkills => &[
                "owner/repo (GitHub) or a Git URL, then optionally the skills to take:",
                "  anthropics/skills              every SKILL.md the repository holds",
                "  anthropics/skills pdf docx     only those two",
                "A skill of the same name already in the store is replaced (--force), and",
                "every sandbox sharing the store sees it at its next agent session.",
            ],
        };
        lines.iter().map(|l| (*l).to_string()).collect()
    }
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub ask: Ask,
    /// What the answer is about: the sandbox, or the secret being written.
    pub subject: String,
    pub input: TextInput,
    /// The lines under the field — what the answer will be used for, and its grammar.
    pub help: Vec<String>,
}

impl Prompt {
    pub fn title(&self) -> String {
        match &self.ask {
            Ask::Rename => "Rename sandbox".into(),
            Ask::PublishPort => "Publish a port".into(),
            Ask::UnpublishPort => "Unpublish a port".into(),
            Ask::CheckHost => "Can this sandbox reach…?".into(),
            Ask::SetSecret(SecretTarget::Service { name, .. }) => {
                format!("Store the “{name}” secret")
            }
            Ask::SetSecret(SecretTarget::Token(name)) => {
                format!("Store the setup token “{name}”")
            }
            Ask::AddSkills => "Add skills from a Git repository".into(),
        }
    }

    /// What answering does, as one lowercase verb: the TUI's footer names ↵ by it, the GUI's
    /// button reads it capitalised — never a bare "OK" or "run".
    pub fn verb(&self) -> &'static str {
        match &self.ask {
            Ask::Rename => "rename",
            Ask::PublishPort => "publish",
            Ask::UnpublishPort => "unpublish",
            Ask::CheckHost => "check",
            Ask::SetSecret(_) => "store",
            Ask::AddSkills => "add",
        }
    }

    /// Is the answer a secret, so that it is drawn as dots rather than as text?
    pub fn masked(&self) -> bool {
        matches!(self.ask, Ask::SetSecret(_))
    }
}

// --- jobs ---------------------------------------------------------------------------------

/// Which setup token a sandbox ended up on, as the job that changed it knows it.
#[derive(Debug, Clone)]
pub struct AuthRecord {
    pub sandbox: String,
    /// The named token's name, or `None` once the sandbox is back on `/login`. The sandbox's own
    /// kit records it, so the registry keeps it only to offer the same one again next time.
    pub token_name: Option<String>,
}

impl AuthRecord {
    fn named(sandbox: &str, token: &str) -> Self {
        Self {
            sandbox: sandbox.to_string(),
            token_name: Some(token.to_string()),
        }
    }

    fn cleared(sandbox: &str) -> Self {
        Self {
            sandbox: sandbox.to_string(),
            token_name: None,
        }
    }
}

/// The status line after aisbx turned sbx's telemetry off on a host where it was left on.
pub const TELEMETRY_TURNED_OFF: &str =
    "sbx's telemetry is now off (telemetry.disabled true); turn it back on in Settings (T)";

/// The same for a headless run, on stderr.
pub const TELEMETRY_TURNED_OFF_HEADLESS: &str =
    "sbx's telemetry is now off (telemetry.disabled true); `sbx settings set telemetry.disabled false` turns it on";

/// What a freshly created sandbox is, beyond what `sbx inspect` will say about it.
#[derive(Debug, Clone)]
pub struct Registered {
    pub name: String,
    pub label: String,
    pub clone: bool,
    /// How the shared agent-skills store is mounted. `None` on a backend that mounts no store.
    pub skills: Option<crate::sbx::SkillsMode>,
    /// Where the agent starts, relative to the workspace; `None` is its root.
    pub start_dir: Option<PathBuf>,
    /// The MCP servers the sandbox was created with (`--static-mcp`).
    pub static_mcp: Vec<String>,
    /// The host folders picked for this sandbox alone, as `sbx create` took them
    /// ([`folders::Folder::sbx_spec`]).
    pub extra_folders: Vec<String>,
}

/// What to record once a job succeeds.
#[derive(Debug, Clone, Default)]
pub struct PostJob {
    /// The sandbox is gone — drop its registry entry.
    pub forget: Option<String>,
    /// A setting changed: read the settings screen's values from sbx again.
    pub reread_settings: bool,
    /// A named token was removed from the host — drop the generated kit that wired it in.
    pub drop_token_kit: Option<String>,
    /// A sandbox was created — remember the choices sbx bakes into the container and then reports
    /// nowhere, so a later recreate can reproduce them.
    pub register: Option<Registered>,
    /// Its kit set changed — re-adopt the current kit hash instead of reporting drift.
    pub rebaseline: Option<String>,
    /// Which Anthropic account it is on now.
    pub auth: Option<AuthRecord>,
    /// A job using this LiteLLM connection succeeded — the create form opens on it next time.
    pub last_litellm: Option<String>,
    /// The job stored this new connection's key: write its `connection.json` (and, for a create
    /// or an `A` move, its generated kit), make it the last used, then start `then`.
    pub save_connection: Option<(crate::litellm::Connection, Option<crate::litellm::Agent>)>,
    /// An existing connection was edited: write its new `connection.json` once the job (its new
    /// key, if one was typed) succeeded. Never the last used: an edit is no use of it.
    pub rewrite_connection: Option<crate::litellm::Connection>,
    /// Started once this job succeeded: the create or rebuild that follows storing a new
    /// connection's key.
    pub then: Option<Box<PendingJob>>,
    /// Where a **failed** job goes back to once its log is closed: the form it came from, as it
    /// was typed, its key emptied.
    pub restore: Option<Restore>,
    /// A LiteLLM connection was deleted: drop its folder (`connection.json`, generated kits,
    /// harvested list) and forget it as the last used.
    pub drop_connection: Option<String>,
    /// Hand the terminal over to this sandbox when the job finishes.
    pub attach: Option<String>,
    /// Where that first session opens, when the create form asked for a start folder: the absolute
    /// directory. Recorded from the form: the snapshot is still in flight.
    pub attach_start: Option<PathBuf>,
    /// The skill sources' pre-start steps for that first session ([`skill_source::install_steps`]
    /// of the form's addons). Recorded from the form, for the same reason.
    pub attach_skill_steps: Vec<Vec<String>>,
    /// Claude Code's model picker for that first session, merged into its settings, which the
    /// create does not write ([`crate::litellm::push_argv_in`]). Recorded from the form too.
    pub attach_config: Option<Vec<String>>,
    /// A model fetch succeeded: read the fetcher's list back onto the host, under the connection
    /// (`(fetcher, connection)`; see [`App::harvest_models`]).
    pub harvest_models: Option<(String, String)>,
    /// Where a job that **succeeded** goes back to, once all that follows it is done: the create
    /// form that started a model fetch, on its Models step.
    pub resume: Option<Restore>,
    /// The create succeeded: keep the form's answers as the next form's defaults.
    pub defaults: Option<crate::state::CreateDefaults>,
    // No field for detached mode: `sbx ls --json` reports it (`detached`).
}

#[derive(Debug, Clone)]
pub struct PendingJob {
    pub title: String,
    pub steps: Vec<Step>,
    pub post: PostJob,
    /// Offered when the job fails — the "recreate instead" escape hatch for a refused
    /// `sbx kit add`.
    pub fallback: Option<Box<PendingJob>>,
    pub fallback_hint: Option<String>,
}

/// A rebuild, and the parts of it its confirmation has to name — counted while the steps are
/// built, so the text and the job cannot disagree.
#[derive(Debug, Clone)]
pub struct RebuildPlan {
    pub job: PendingJob,
    /// Hosts this sandbox alone was denied, going back in as `--deny-network`.
    pub denies: Vec<String>,
    /// `sbx policy …` steps re-adding the rest of its own rules.
    pub readded: usize,
    /// …of which come from answered approval requests.
    pub approved: usize,
    /// It comes back in detached mode.
    pub keep_running: bool,
    /// Host folders an addon wanted that the new container will not mount, and why.
    pub mount_notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: Vec<String>,
    pub danger: bool,
    pub confirm_label: String,
    pub yes: bool,
    pub job: PendingJob,
}

/// What applying the auth editor amounts to — see [`App::auth_plan`].
#[derive(Debug)]
pub enum AuthPlan {
    /// Nothing to do; the message says why.
    Nothing(String),
    /// The user has to supply something first, so the form stays open.
    Refuse(&'static str),
    /// Runs straight away: the sandbox keeps its container.
    Apply(Box<PendingJob>),
    /// Needs confirming first — the container is rebuilt, or the change reaches other sandboxes.
    Confirm(Box<Confirm>),
}

// One screen exists at a time and lives on `App`, so the create form's size costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Screen {
    List,
    Create(CreateForm),
    Kits(KitsForm),
    AuthEdit(AuthForm),
    /// Every credential stored on this host, and what changing one costs.
    Secrets(SecretsForm),
    /// The shared agent-skills store: what is in it, and the commands that change it.
    Skills(SkillsForm),
    /// Skills of the configured skill repos, for every sandbox or for one.
    RepoSkills(RepoSkillsForm),
    /// Connecting an editor or desktop app to a sandbox over SSH.
    Connect(ConnectForm),
    /// Which directory inside the workspace the agent opens in.
    StartDir(StartDirForm),
    /// Requests sandboxes are holding for approval, and the answers to them.
    Approvals(ApprovalsForm),
    /// Adding or editing a LiteLLM connection, from `S`.
    Connection(ConnForm),
    /// Moving a LiteLLM sandbox to another connection (`A`).
    ConnSwitch(ConnSwitchForm),
    /// The host folders every sandbox mounts beside its workspace (`F`).
    Folders(FoldersForm),
    Settings(SettingsForm),
}

/// What a key that tries to leave a running job hears: why the job stays on screen.
pub const JOB_STILL_RUNNING: &str = "still running — this closes when sbx is done";

#[derive(Debug)]
pub enum Modal {
    Help,
    Confirm(Confirm),
    Prompt(Prompt),
    Running(Job, PendingJob),
}

pub struct App {
    pub catalog: Arc<Catalog>,
    pub registry: Registry,
    pub cwd: PathBuf,
    pub sandboxes: Vec<Sandbox>,
    /// The list in reading order — see [`Cell`]. The first section always ends with
    /// [`Cell::New`], so there is a cell to sit on even in a folder that has no sandboxes.
    pub cells: Vec<Cell>,
    /// How many leading cells belong to the current folder. Also the index of [`Cell::New`].
    pub here: usize,
    /// Sandboxes living in another folder — counted whether or not the section listing them is
    /// expanded, because the heading has to say what collapsing hides.
    pub elsewhere: usize,
    /// Is the "other folders" section expanded? Remembered across runs in the registry.
    pub show_other: bool,
    /// Position in the list, over [`App::cells`].
    pub cursor: usize,
    /// A `/` search over the list, while one is typed — see [`crate::find`].
    pub find: Option<Find>,
    pub screen: Screen,
    pub modal: Option<Modal>,
    pub status: Option<(String, bool, Instant)>,
    /// The header's refresh mark: a refresh someone waits for, or a poll slower than
    /// [`SLOW_REFRESH`].
    pub loading: bool,
    pub quit: bool,
    /// Set when the terminal should be handed over to sbx; consumed by the main loop.
    pub handoff: Option<Handoff>,
    pub sbx_version: String,
    /// Update the agent inside a sandbox just before starting it. On by default; `--no-update`
    /// turns it off.
    pub auto_update: bool,
    /// Where the LiteLLM connections are read from and their kits written to:
    /// [`crate::litellm::dir`], or under `cargo test` a folder that does not exist.
    pub litellm_root: PathBuf,
    /// The home folder the create form looks for host caches in: [`model::home_dir`], or under
    /// `cargo test` a scratch folder that holds every cache.
    pub host_home: Option<PathBuf>,
    /// Where the host's list of folders for every sandbox is read from and written to
    /// ([`folders::config_path`]): the state folder, or under `cargo test` a scratch folder.
    pub mounts_state: PathBuf,

    /// Which details tab is on screen, and how far it is scrolled.
    pub tab: Tab,
    pub detail_scroll: u16,
    /// Details pane takes the whole body, hiding the sandbox list.
    pub detail_full: bool,
    /// Scroll offset into the help modal, which is taller than a short terminal.
    pub help_scroll: u16,
    /// The extra `sbx` data behind the tabs, for the selected sandbox only.
    pub insight: Insight,
    /// The Resources tab's readings per sandbox, for the charts: kept across a selection away
    /// and back, dropped once `sbx ls` lists the sandbox stopped or not at all.
    pub histories: Histories,
    /// First list row rendered — the list's scroll offset, counted in [`crate::roster::Row`]s (a
    /// heading is a row too), which is what the renderer builds.
    pub grid_scroll: usize,
    /// Sandbox rows that fit on screen, so page-up/down moves by a screenful.
    pub grid_rows: usize,
    /// What `sbx secret ls` reports, or `None` until the first refresh answers.
    pub secrets: Option<sbx::Secrets>,
    /// The MCP servers registered on this host, re-read every refresh. Never remembered.
    pub mcp: Vec<sbx::McpServer>,
    /// What the shared agent-skills store holds (`sbx skills ls --json`), or `None` until the
    /// first refresh answers. Re-read every refresh, like the secrets and the MCP list.
    pub skills: Option<sbx::SkillsStore>,
    /// The host's `skills.defaultMode`: what a new create form opens its skills row on. sbx's
    /// built-in value until the first refresh answers, and where the read fails.
    pub skills_default: sbx::SkillsMode,
    /// `Some(true)` while the host's global network policy is uninitialized: every `sbx create`
    /// then fails, and the create form refuses to submit. `None` (not answered yet) is fine.
    pub policy_needs_init: Option<bool>,
    /// Requests sandboxes on this host are holding for approval, re-read every refresh.
    pub approvals: Vec<sbx::Approval>,
    /// Has `sbx setup ssh` written its managed block to `~/.ssh/config`? Re-read once per refresh,
    /// not per frame.
    pub ssh_ready: bool,
    /// Mouse reporting is on. Off lets the terminal do its own text selection again.
    pub mouse: bool,
    /// Click targets registered by the last render, topmost last.
    pub hits: Vec<(Rect, Hit)>,
    last_click: Option<(Instant, Hit)>,
    /// The create form left by Esc on its first step or by Ctrl-C, and its folder: the next New
    /// there reopens it. Cancel and a create drop it. In memory only: it can hold a pasted token.
    pub create_draft: Option<(PathBuf, Box<CreateForm>)>,

    tx: Sender<Result<Snapshot>>,
    rx: Receiver<Result<Snapshot>>,
    insight_tx: Sender<Fetched>,
    insight_rx: Receiver<Fetched>,
    /// One-shot results from short read-only probes (`sbx policy check network`), which have an
    /// answer rather than a stream and so want the status line, not the job modal.
    notice_tx: Sender<(bool, String)>,
    notice_rx: Receiver<(bool, String)>,
    /// The model fetchers on this host, by sandbox name, as the last snapshot listed them.
    pub fetchers: Vec<String>,
    /// The connection whose models the catalog offers ([`App::load_models_for`]).
    models_conn: Option<String>,
    /// The start-of-day fetches ran (or were found not due).
    due_fetches_done: bool,
    /// What the background fetches read: connection, fetcher, the fetcher's cache.
    fetch_tx: Sender<(String, String, Option<String>)>,
    fetch_rx: Receiver<(String, String, Option<String>)>,
    /// Under `cargo test`, the connections a start-of-day fetch would have run for.
    #[cfg(any(test, feature = "test-support"))]
    pub background_fetches: Vec<String>,
    #[cfg(not(any(test, feature = "test-support")))]
    background_fetches: Vec<String>,
    refreshing: bool,
    /// A change landed while a snapshot was in flight — that snapshot is stale.
    refresh_pending: bool,
    last_refresh: Instant,
}

impl App {
    /// `show_other` overrides the remembered expand/collapse state when the CLI asked for one.
    pub fn new(
        catalog: Catalog,
        show_other: Option<bool>,
        sbx_version: String,
        mouse: bool,
        auto_update: bool,
    ) -> Result<Self> {
        let (tx, rx) = channel();
        let (insight_tx, insight_rx) = channel();
        let (notice_tx, notice_rx) = channel();
        let (fetch_tx, fetch_rx) = channel();
        let registry = Registry::load();
        let mut app = Self {
            catalog: Arc::new(catalog),
            show_other: show_other.unwrap_or_else(|| !registry.hide_other_folders()),
            registry,
            cwd: std::env::current_dir()?,
            sandboxes: Vec::new(),
            cells: vec![Cell::New],
            here: 0,
            elsewhere: 0,
            cursor: 0,
            find: None,
            screen: Screen::List,
            modal: None,
            status: None,
            loading: true,
            quit: false,
            handoff: None,
            sbx_version,
            auto_update,
            litellm_root: litellm_root(),
            host_home: host_home(),
            mounts_state: mounts_state(),
            tab: Tab::Overview,
            detail_scroll: 0,
            detail_full: false,
            help_scroll: 0,
            insight: Insight::default(),
            histories: Histories::default(),
            grid_scroll: 0,
            grid_rows: 1,
            secrets: None,
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            policy_needs_init: None,
            approvals: Vec::new(),
            ssh_ready: sbx::ssh_configured(),
            mouse,
            hits: Vec::new(),
            last_click: None,
            create_draft: None,
            tx,
            rx,
            insight_tx,
            insight_rx,
            notice_tx,
            notice_rx,
            fetchers: Vec::new(),
            models_conn: None,
            due_fetches_done: false,
            fetch_tx,
            fetch_rx,
            background_fetches: Vec::new(),
            refreshing: false,
            refresh_pending: false,
            last_refresh: Instant::now(),
        };
        app.refresh();
        Ok(app)
    }

    /// The requests this sandbox is holding for approval, in sbx's order.
    pub fn approvals_of(&self, sandbox: &str) -> Vec<&sbx::Approval> {
        self.approvals
            .iter()
            .filter(|a| a.sandbox == sandbox)
            .collect()
    }

    /// How many requests this sandbox is holding for approval.
    pub fn approvals_for(&self, sandbox: &str) -> usize {
        self.approvals_of(sandbox).len()
    }

    /// Is a service secret with this name stored on the host? `None` while that is still unknown
    /// — the difference matters, because "we have not looked yet" must not be shown as "missing".
    pub fn has_secret(&self, name: &str) -> Option<bool> {
        self.secrets.as_ref().map(|s| s.has_service(name))
    }

    /// The credential services a backend needs ([`crate::kits::Backend::required_services`]) that
    /// this host has *not* stored. Not answered yet counts as stored.
    pub fn missing_secrets(&self, backend: &Backend) -> Vec<String> {
        backend
            .required_services()
            .into_iter()
            .filter(|s| self.has_secret(s) == Some(false))
            .collect()
    }

    /// The backend kit a sandbox was built from, if this checkout ships it.
    pub fn backend_of(&self, sb: &Sandbox) -> Option<&Backend> {
        sb.backend.as_deref().and_then(|b| self.catalog.backend(b))
    }

    /// The models a sandbox pins, as (alias, display name — else the id), in alias order. Empty
    /// on a backend that cannot pin them.
    pub fn pinned_models(&self, sb: &Sandbox) -> Vec<(&'static str, String)> {
        if !self.backend_of(sb).is_some_and(Backend::model_overrides) {
            return Vec::new();
        }
        sb.models
            .entries()
            .map(|(slot, id)| {
                let name = models::display_name(&self.catalog.models, id);
                (slot.alias(), name.unwrap_or_else(|| id.to_string()))
            })
            .collect()
    }

    /// The service secrets its backend kit needs that the host is known not to hold — a sandbox
    /// that comes up unauthenticated. Not answered yet counts as stored, as in
    /// [`App::has_secret`].
    pub fn missing_credentials(&self, sb: &Sandbox) -> Vec<String> {
        let Some(backend) = self.backend_of(sb) else {
            return Vec::new();
        };
        backend
            .kit
            .credentials
            .iter()
            .filter(|service| self.has_secret(service) == Some(false))
            .cloned()
            .collect()
    }

    /// The create form, opened on this host's named tokens and the one last used.
    fn new_create_form(&self) -> CreateForm {
        let mut form = CreateForm::new(
            &self.catalog,
            self.host_home.as_deref(),
            self.token_names(),
            self.registry.last_token(),
            self.mcp.clone(),
            self.skills_default,
        );
        self.refresh_form_folders(&mut form);
        form
    }

    /// Read again what the create form shows of its mounted folder: whether it has a `.git`
    /// folder, and the host folders a sandbox on it mounts.
    fn refresh_form_folders(&self, form: &mut CreateForm) {
        let root = form.workspace_root(&self.cwd);
        form.workspace.git_offered = root.join(".git").is_dir();
        form.workspace.folders = self.folder_plan(&root);
    }

    /// The host folders a sandbox on `workspace` mounts beside it ([`folders::plan`]): those its
    /// `companion.json` names and the host's list. A host list that does not load mounts none of
    /// it, and says why.
    pub fn folder_plan(&self, workspace: &Path) -> folders::Plan {
        let (host, mut notes) = match folders::load(&folders::config_path(&self.mounts_state)) {
            Ok(config) => (config.folders, Vec::new()),
            Err(e) => (Vec::new(), vec![format!("{e:#}")]),
        };
        let mut plan = folders::plan(workspace, &host, model::home_dir().as_deref());
        notes.append(&mut plan.notes);
        plan.notes = notes;
        plan
    }

    /// Open a new sandbox's form — `n`, and ↵ or a click on the New row — on the wizard's
    /// first step.
    fn open_new_form(&mut self) {
        match self.create_draft.take() {
            Some((cwd, form)) if cwd == self.cwd => {
                let pick = form.connection.litellm.clone();
                self.screen = Screen::Create(*form);
                self.load_models_for(pick.as_deref());
                self.set_info(crate::create::DRAFT_BACK);
                return;
            }
            other => self.create_draft = other,
        }
        let mut form = self.new_create_form();
        // Read now, never cached: `connection.json` is the authority for which connections exist.
        let connections = crate::litellm::load_all_in(&self.litellm_root);
        let last = self.registry.last_litellm().map(str::to_string);
        form.connection.litellm = match crate::litellm::preselect(&connections, last.as_deref()) {
            crate::litellm::LitellmPick::Existing(name) => Some(name),
            crate::litellm::LitellmPick::New => None,
        };
        form.connection.connections = connections;
        form.connection.last_litellm = last;
        // The cursor opens on the picked provider, which need not be the first.
        if let Some(p) = form.selected_provider_index(&self.catalog) {
            form.focus_wizard_row(&self.catalog, WizardRow::Provider(p));
        }
        let pick = form.connection.litellm.clone();
        let defaults = self.registry.create_defaults().cloned();
        self.screen = Screen::Create(form);
        self.load_models_for(pick.as_deref());
        self.after_wizard_input();
        if let Some(defaults) = defaults {
            self.apply_create_defaults(&defaults);
        }
    }

    /// Open the form on the answers of the last create: everything it can still offer, then on
    /// its Review — or on the first step whose answers would refuse a create.
    fn apply_create_defaults(&mut self, defaults: &crate::state::CreateDefaults) {
        let catalog = Arc::clone(&self.catalog);
        let skills_default = self.skills_default;
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        if !form.apply_defaults(&catalog, defaults, skills_default) {
            return;
        }
        self.after_wizard_input();
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        let refused = CreateStep::ALL
            .into_iter()
            .find(|step| form.step_problem(*step, &catalog).is_some());
        form.wizard.prefilled = true;
        form.wizard_set_step(&catalog, refused.unwrap_or(CreateStep::Review));
        self.set_info(crate::create::PREFILLED);
    }

    /// Put the models of a connection on offer for pinning — the list a sandbox on it harvested
    /// last; nothing before one has, and nothing without a connection.
    pub fn load_models_for(&mut self, connection: Option<&str>) {
        let (models, source) =
            match connection.and_then(|n| crate::litellm::load_in(&self.litellm_root, n)) {
                Some(c) => self.connection_models(&c),
                None => (Vec::new(), crate::models::Source::NotYet),
            };
        let catalog = Arc::make_mut(&mut self.catalog);
        catalog.models = models;
        catalog.models_source = source;
        self.models_conn = connection.map(str::to_string);
    }

    /// A connection's models for pinning: the list a sandbox on it harvested last, else none.
    fn connection_models(
        &self,
        c: &crate::litellm::Connection,
    ) -> (Vec<models::Model>, models::Source) {
        let harvest = self.litellm_root.join(&c.name).join("models.json");
        crate::models::catalog_for(Some(&harvest))
    }

    /// Is a LiteLLM connection's key known to be missing — `sbx secret ls` answered and holds no
    /// row for it on its host? Not answered yet counts as stored, like [`App::has_secret`].
    pub fn litellm_key_missing(&self, name: &str) -> bool {
        let Some(secrets) = self.secrets.as_ref() else {
            return false;
        };
        let from_form = match &self.screen {
            Screen::Create(form) => form
                .connection
                .connections
                .iter()
                .find(|c| c.name == name)
                .cloned(),
            _ => None,
        };
        let Some(conn) = from_form.or_else(|| crate::litellm::load_in(&self.litellm_root, name))
        else {
            return false;
        };
        secrets.litellm_key_state(name, crate::litellm::host(&conn.url)) != Some(true)
    }

    /// The named tokens stored on this host. Empty while `sbx secret ls` has not answered yet.
    pub fn token_names(&self) -> Vec<String> {
        self.secrets
            .as_ref()
            .map(sbx::Secrets::token_names)
            .unwrap_or_default()
    }

    /// How many sandboxes are wired to a named token — what makes it worth naming, and what makes
    /// rotating it a decision about more than one sandbox.
    pub fn token_users(&self, token: &str) -> usize {
        self.sandboxes.iter().filter(|s| on_token(s, token)).count()
    }

    /// The *other* sandboxes on a named token — who else a rotation reaches, by their labels.
    fn sandboxes_on(&self, token: &str, except: &str) -> Vec<String> {
        self.sandboxes
            .iter()
            .filter(|s| s.name != except && on_token(s, token))
            .map(|s| s.label.clone())
            .collect()
    }

    fn addons_of(&self, name: &str) -> Vec<String> {
        self.sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.addons.clone())
            .unwrap_or_default()
    }

    /// The model pins a sandbox carries today — what every rebuild that is not *about* the models
    /// keeps.
    fn models_of(&self, name: &str) -> Overrides {
        self.sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.models.clone())
            .unwrap_or_default()
    }

    // --- data ------------------------------------------------------------------------------

    pub fn refresh(&mut self) {
        self.start_refresh(true);
    }

    /// `shown`: the header shows the refresh at once. The poll's is shown only once it is slow.
    fn start_refresh(&mut self, shown: bool) {
        if self.refreshing {
            self.refresh_pending = true;
            self.loading |= shown;
            return;
        }
        self.refreshing = true;
        self.loading = shown;
        self.last_refresh = Instant::now();
        self.ssh_ready = sbx::ssh_configured();
        let catalog = Arc::clone(&self.catalog);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(model::gather(&catalog));
        });
    }

    /// Drain worker results and drive time-based housekeeping. Called once per frame.
    pub fn tick(&mut self) {
        while let Ok((is_error, message)) = self.notice_rx.try_recv() {
            if is_error {
                self.set_error(message);
            } else {
                self.set_info(message);
            }
        }
        while let Ok(result) = self.rx.try_recv() {
            self.refreshing = false;
            let stale = self.refresh_pending;
            self.loading = false;
            match result {
                Ok(mut snapshot) => {
                    let keep = self.selected().map(|s| s.name.clone());
                    // A failed `sbx secret ls` keeps the previous answer.
                    if let Some(names) = snapshot.secrets.take() {
                        self.secrets = Some(names);
                    }
                    self.mcp = std::mem::take(&mut snapshot.mcp);
                    if let Some(store) = snapshot.skills.take() {
                        self.skills = Some(store);
                    }
                    self.skills_default = snapshot.skills_default;
                    if snapshot.policy_needs_init.is_some() {
                        self.policy_needs_init = snapshot.policy_needs_init;
                    }
                    if let Some(pending) = snapshot.approvals.take() {
                        self.approvals = pending;
                    }
                    self.fetchers = std::mem::take(&mut snapshot.fetchers);
                    self.sandboxes = model::resolve(&snapshot, &self.catalog, &mut self.registry);
                    self.histories.retain_running(&self.sandboxes);
                    // A stale snapshot would drop the label of a sandbox created just now.
                    if !stale {
                        let live: Vec<String> =
                            self.sandboxes.iter().map(|s| s.name.clone()).collect();
                        self.registry.retain_known(&live);
                    }
                    let _ = self.registry.save();
                    self.recompute_cells(keep.as_deref());
                    self.start_due_fetches();
                }
                Err(e) => self.set_error(format!("{e:#}")),
            }
            if stale {
                self.refresh_pending = false;
                self.refresh();
            }
        }

        if let Some(Modal::Running(job, pending)) = &mut self.modal {
            if job.is_finished() {
                let status = job.status();
                match status {
                    Status::Ok => {
                        let pending = pending.clone();
                        self.modal = None;
                        self.set_info(format!("{} — done", pending.title));
                        self.apply_post(&pending.post);
                        self.start_next(&pending.post);
                        self.refresh();
                    }
                    // Failures stay on screen until closed.
                    Status::Failed(_) | Status::Running => {}
                }
            }
        }

        self.tick_fetches();

        if let Some((_, _, at)) = self.status {
            if at.elapsed() > STATUS_TTL {
                self.status = None;
            }
        }

        let idle = matches!(self.screen, Screen::List) && self.modal.is_none();
        let watching = matches!(self.screen, Screen::Approvals(_)) && self.modal.is_none();
        if (idle || watching) && !self.refreshing && self.last_refresh.elapsed() > AUTO_REFRESH {
            self.start_refresh(false);
        }
        if self.refreshing && self.last_refresh.elapsed() > SLOW_REFRESH {
            self.loading = true;
        }
        self.tick_insight(idle);
    }

    /// Keep the details tabs fed. Nothing is fetched unless the list screen is actually on
    /// screen and the visible tab needs it — see [`crate::insight`] for why that matters.
    fn tick_insight(&mut self, idle: bool) {
        while let Ok(fetched) = self.insight_rx.try_recv() {
            // A late answer must not bring back a ring that `retain_running` dropped.
            if let Payload::Resources(Ok(sample)) = &fetched.payload {
                let name = &fetched.name;
                if self.sandboxes.iter().any(|s| s.running && &s.name == name) {
                    self.histories.record(name, sample);
                }
            }
            self.insight.accept(fetched);
        }
        let Some(sb) = self.selected() else {
            self.insight = Insight::default();
            return;
        };
        let (name, running) = (sb.name.clone(), sb.running);
        self.insight.retarget(&name);
        if !running {
            self.insight.clear_resources();
        }
        if idle {
            let tab = self.tab;
            let tx = self.insight_tx.clone();
            self.insight.poll(tab, running, &tx);
        }
    }

    /// Drop a sandbox's skill selection (`skill-selection.json`). Not under `cargo test`, where
    /// the state folder is the real one of whoever runs the suite.
    fn drop_skill_selection(&mut self, name: &str) {
        if cfg!(any(test, feature = "test-support")) {
            return;
        }
        if let Err(e) = skill_select::forget(&crate::state::state_dir(), name) {
            self.set_error(format!("{e:#}"));
        }
    }

    /// The second half of a job that stored a connection's key: write the connection's files,
    /// then start the job that needs them. A failed write starts nothing.
    fn start_next(&mut self, post: &PostJob) {
        if let Some((conn, agent)) = &post.save_connection {
            let saved =
                crate::litellm::save_in(&self.litellm_root, conn).and_then(|()| match agent {
                    Some(agent) => {
                        crate::litellm::ensure_kit_in(&self.litellm_root, conn, *agent).map(|_| ())
                    }
                    None => Ok(()),
                });
            if let Err(e) = saved {
                self.set_error(format!("{e:#}"));
                return;
            }
            self.registry.set_last_litellm(&conn.name);
            let _ = self.registry.save();
            // The model fetch that follows a new key builds its fetcher from this kit.
            if let Err(e) = crate::model_fetch::ensure_kit_in(&self.litellm_root, conn) {
                self.set_error(format!("{e:#}"));
                return;
            }
        }
        if let Some(conn) = &post.rewrite_connection {
            if let Err(e) = crate::litellm::save_in(&self.litellm_root, conn) {
                self.set_error(format!("{e:#}"));
                return;
            }
        }
        if let Some(next) = &post.then {
            self.start_job((**next).clone());
        } else if let Some(resume) = &post.resume {
            self.restore_screen(resume.clone());
        }
    }

    /// Put a form back on screen, as a finished job left it. A create form comes back on the
    /// connection's models as they are now.
    fn restore_screen(&mut self, restore: Restore) {
        match restore {
            Restore::Create(form) => {
                let conn = form.connection.litellm.clone();
                self.screen = Screen::Create(*form);
                self.load_models_for(conn.as_deref());
            }
            Restore::Connection(form) => self.screen = Screen::Connection(*form),
            Restore::ConnSwitch(form) => self.screen = Screen::ConnSwitch(*form),
        }
    }

    fn apply_post(&mut self, post: &PostJob) {
        if post.reread_settings {
            if let Screen::Settings(form) = &mut self.screen {
                form.read();
            }
        }
        if let Some(name) = &post.drop_connection {
            if let Err(e) = crate::litellm::remove_in(&self.litellm_root, name) {
                self.set_error(format!("{e:#}"));
            }
            self.registry.clear_last_litellm(name);
        }
        if let Some((fetcher, conn)) = &post.harvest_models {
            match self.harvest_models(fetcher, conn) {
                Ok(_) => {
                    let file = self.litellm_root.join(conn).join("models.json");
                    let count = crate::models::catalog_for(Some(&file)).0.len();
                    self.set_info(format!("{conn}: {count} models from the gateway"));
                }
                Err(e) => self.set_error(e),
            }
        }
        if let Some(defaults) = &post.defaults {
            self.registry.set_create_defaults(defaults.clone());
        }
        if let Some(name) = &post.forget {
            self.registry.forget(name);
            if let Err(e) = models::remove_kit(name).and_then(|()| folders::remove_kit(name)) {
                self.set_error(format!("{e:#}"));
            }
            self.drop_skill_selection(name);
        }
        if let Some(token) = &post.drop_token_kit {
            if let Err(e) = tokens::remove_kit(token) {
                self.set_error(format!("{e:#}"));
            }
        }
        if let Some(made) = &post.register {
            // A new sandbox never inherits the selection of an earlier one of its name.
            self.drop_skill_selection(&made.name);
            self.registry
                .set_display_name(&made.name, Some(made.label.clone()));
            self.registry.set_clone(&made.name, made.clone);
            self.registry.set_skills(&made.name, made.skills);
            self.registry
                .set_start_dir(&made.name, made.start_dir.as_deref());
            self.registry
                .set_static_mcp(&made.name, made.static_mcp.clone());
            self.registry
                .set_extra_folders(&made.name, made.extra_folders.clone());
        }
        if let Some(name) = post.auth.as_ref().and_then(|r| r.token_name.as_ref()) {
            self.registry.set_last_token(name);
        }
        if let Some(name) = &post.last_litellm {
            self.registry.set_last_litellm(name);
        }
        if let Some(name) = &post.rebaseline {
            self.registry.clear_kit_hash(name);
        }
        if let Some(name) = &post.attach {
            // No update step: the install hook fetched the current release seconds ago.
            let label = self.registry.display_name(name).unwrap_or(name).to_string();
            let mut prelude = vec![sbx::label_argv(name, &label)];
            prelude.extend(post.attach_skill_steps.iter().cloned());
            // OpenCode's and Pi's files came with the create; Claude Code's picker did not.
            prelude.extend(post.attach_config.clone());
            self.handoff = Some(Handoff::Agent {
                name: name.clone(),
                start: post.attach_start.clone(),
                prelude,
            });
        }
        let _ = self.registry.save();
    }

    /// Rebuild the list: this folder's sandboxes, the "+ New sandbox" row, then the rest. `keep`
    /// holds the cursor on a named sandbox across the rebuild.
    fn recompute_cells(&mut self, keep: Option<&str>) {
        // Follow a cursor on the New row; before the first snapshot, every cursor starts there.
        let was_new = self.cells.len() > 1 && self.on_new_card();
        let (here, other) = self.reading_order();

        self.here = here.len();
        self.elsewhere = other.len();
        self.cells = here.into_iter().map(Cell::Sandbox).collect();
        self.cells.push(Cell::New);
        if self.show_other {
            self.cells.extend(other.into_iter().map(Cell::Sandbox));
        }

        self.cursor = keep
            .and_then(|name| self.cell_of(name))
            .or_else(|| was_new.then_some(self.here))
            .unwrap_or(self.cursor)
            .min(self.slots() - 1);
    }

    /// The sandboxes in the order the list reads them, by index into [`App::sandboxes`]: this
    /// folder's, then every other one — whether or not their section is unfolded.
    fn reading_order(&self) -> (Vec<usize>, Vec<usize>) {
        let (mut here, mut other): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        for (i, sb) in self.sandboxes.iter().enumerate() {
            if sb.belongs_here(&self.cwd) {
                here.push(i);
            } else {
                other.push(i);
            }
        }
        other.sort_by(|a, b| {
            let (a, b) = (&self.sandboxes[*a], &self.sandboxes[*b]);
            model::folder_key(&a.workspace)
                .cmp(&model::folder_key(&b.workspace))
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });
        (here, other)
    }

    /// Cells in the list — every listed sandbox plus the "+ New sandbox" row. Never zero.
    pub fn slots(&self) -> usize {
        self.cells.len()
    }

    /// The cursor is on the "+ New sandbox" row rather than on a sandbox.
    pub fn on_new_card(&self) -> bool {
        matches!(self.cells.get(self.cursor), Some(Cell::New))
    }

    pub fn selected(&self) -> Option<&Sandbox> {
        self.sandbox_at(self.cursor)
    }

    /// The sandbox drawn in a list cell, or `None` for the "+ New sandbox" row.
    pub fn sandbox_at(&self, cell: usize) -> Option<&Sandbox> {
        match self.cells.get(cell)? {
            Cell::Sandbox(i) => self.sandboxes.get(*i),
            Cell::New => None,
        }
    }

    /// Which cell holds this sandbox, if it is currently listed.
    fn cell_of(&self, name: &str) -> Option<usize> {
        self.cells
            .iter()
            .position(|c| matches!(c, Cell::Sandbox(i) if self.sandboxes[*i].name == name))
    }

    /// Does this cell belong to the current folder's section?
    pub fn cell_is_here(&self, cell: usize) -> bool {
        cell < self.here
    }

    /// Show or hide the sandboxes that belong to other folders. Expanding moves the cursor into
    /// the section it revealed.
    pub fn toggle_other_folders(&mut self) {
        self.show_other = !self.show_other;
        self.registry.set_hide_other_folders(!self.show_other);
        let _ = self.registry.save();
        let keep = self.selected().map(|s| s.name.clone());
        self.recompute_cells(keep.as_deref());
        if self.show_other && self.elsewhere > 0 {
            self.select(self.here + 1);
        }
    }

    // --- finding a sandbox (`/`) ----------------------------------------------------------

    /// Start a `/` search from where the cursor is.
    fn start_find(&mut self) {
        self.find = Some(Find {
            query: String::new(),
            origin: self.selected().map(|sb| sb.name.clone()),
            showed_other: self.show_other,
        });
    }

    /// The sandboxes the search finds, by index into [`App::sandboxes`], in the order the list
    /// reads them — the folded section's too. None while no search is typed.
    pub fn find_matches(&self) -> Vec<usize> {
        let Some(find) = &self.find else {
            return Vec::new();
        };
        let (here, other) = self.reading_order();
        (here.into_iter().chain(other))
            .filter(|i| find::matches(&self.sandboxes[*i], &self.catalog, &find.query))
            .collect()
    }

    /// Which match the cursor is on, counted from one — `None` when it is on none — and how many
    /// there are: the TUI's `2 of 5`.
    pub fn find_position(&self) -> (Option<usize>, usize) {
        let matches = self.find_matches();
        let on = self.cells.get(self.cursor).and_then(|cell| match cell {
            Cell::Sandbox(i) => matches.iter().position(|m| m == i).map(|p| p + 1),
            Cell::New => None,
        });
        (on, matches.len())
    }

    /// A key while a `/` search is typed. Text goes into the query, and the cursor to the first
    /// match at or after where the search began.
    fn find_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(find) = self.find.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Enter => self.find = None,
            KeyCode::Esc => self.cancel_find(),
            KeyCode::Char('c') if ctrl => self.cancel_find(),
            KeyCode::Down | KeyCode::Tab => self.step_find(1),
            KeyCode::Char('n') if ctrl => self.step_find(1),
            KeyCode::Up | KeyCode::BackTab => self.step_find(-1),
            KeyCode::Char('p') if ctrl => self.step_find(-1),
            KeyCode::Backspace if find.query.is_empty() => self.cancel_find(),
            KeyCode::Backspace => {
                find.query.pop();
                self.refind();
            }
            KeyCode::Char('u') if ctrl => {
                find.query.clear();
                self.refind();
            }
            // The word before, as a shell prompt deletes it.
            KeyCode::Char('w') if ctrl => {
                let kept = find.query.trim_end();
                let cut = (kept.char_indices().rev())
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                find.query.truncate(cut);
                self.refind();
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                find.query.push(c);
                self.refind();
            }
            _ => {}
        }
    }

    /// The reading order as one list: this folder's sandboxes, then the other folders'.
    fn reading_list(&self) -> Vec<usize> {
        let (here, other) = self.reading_order();
        here.into_iter().chain(other).collect()
    }

    /// Where the cursor sits in the reading order `order`, doubled so the New row — between the
    /// two sections — has a place of its own: a sandbox at `p` is `2p + 1`.
    fn find_rank(&self, order: &[usize]) -> usize {
        match self.cells.get(self.cursor) {
            Some(Cell::Sandbox(i)) => order.iter().position(|o| o == i).map_or(0, |p| 2 * p + 1),
            _ => 2 * self.here,
        }
    }

    /// Put the cursor on the first match at or after where the search began, wrapping past the
    /// end of the list — or back where it began, when nothing matches.
    fn refind(&mut self) {
        let Some(find) = &self.find else {
            return;
        };
        let order = self.reading_list();
        let start = match &find.origin {
            Some(name) => (order.iter())
                .position(|i| self.sandboxes[*i].name == *name)
                .unwrap_or(0),
            // The New row closes this folder's section: what follows it is the other folders'.
            None => self.here,
        };
        let matches = self.find_matches();
        let target = (0..order.len())
            .map(|k| order[(start + k) % order.len()])
            .find(|i| matches.contains(i));
        match target {
            Some(i) => self.jump_to(i),
            None => self.back_to_origin(),
        }
    }

    /// The next match after the cursor (`dir` 1) or the one before it (`dir` -1), wrapping.
    fn step_find(&mut self, dir: isize) {
        let order = self.reading_list();
        let ranks: Vec<(usize, usize)> = (self.find_matches().into_iter())
            .filter_map(|i| order.iter().position(|o| *o == i).map(|p| (2 * p + 1, i)))
            .collect();
        let now = self.find_rank(&order);
        let target = match dir {
            1 => (ranks.iter().find(|(r, _)| *r > now)).or(ranks.first()),
            _ => (ranks.iter().rev().find(|(r, _)| *r < now)).or(ranks.last()),
        };
        if let Some(&(_, i)) = target {
            self.jump_to(i);
        }
    }

    /// Put the cursor on `sandboxes[i]`, unfolding the other folders' section for a match there.
    /// The fold is not saved: the search ends with it as it was, or as ↵ leaves it.
    fn jump_to(&mut self, i: usize) {
        let name = self.sandboxes[i].name.clone();
        if self.cell_of(&name).is_none() && !self.show_other {
            self.show_other = true;
            self.recompute_cells(None);
        }
        if let Some(cell) = self.cell_of(&name) {
            self.select(cell);
        }
    }

    /// The cursor and the fold as they were when the search began.
    fn back_to_origin(&mut self) {
        let Some(find) = self.find.clone() else {
            return;
        };
        if self.show_other != find.showed_other {
            self.show_other = find.showed_other;
            self.recompute_cells(find.origin.as_deref());
        }
        let cell = match &find.origin {
            Some(name) => self.cell_of(name).unwrap_or(self.cursor),
            None => self.here,
        };
        self.select(cell);
    }

    /// End the search where it began.
    fn cancel_find(&mut self) {
        self.back_to_origin();
        self.find = None;
    }

    /// Show the list from another folder, as if the dashboard had been started there: `dir`'s
    /// sandboxes become the first section. Nothing is stored, and the process keeps its working
    /// directory. Errors if `dir` is not absolute or not an existing folder; nothing changes then.
    pub fn set_folder(&mut self, dir: PathBuf) -> Result<()> {
        if !dir.is_absolute() {
            anyhow::bail!("{} is not an absolute path", dir.display());
        }
        match std::fs::metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => anyhow::bail!("{} is not a folder", dir.display()),
            Err(_) => anyhow::bail!("{} does not exist", dir.display()),
        }
        self.cwd = dir;
        let keep = self.selected().map(|s| s.name.clone());
        self.recompute_cells(keep.as_deref());
        self.grid_scroll = 0;
        self.detail_scroll = 0;
        Ok(())
    }

    /// Every folder a sandbox mounts, plus the current one even with none, sorted by
    /// [`model::folder_key`]. Mountless sandboxes are left out.
    pub fn folders(&self) -> Vec<Folder> {
        let mut out: Vec<Folder> = Vec::new();
        let mounted = self.sandboxes.iter().filter(|sb| !sb.mountless());
        for (path, n) in mounted.map(|sb| (&sb.workspace, 1)).chain([(&self.cwd, 0)]) {
            let current = model::same_path(path, &self.cwd);
            match out.iter_mut().find(|f| model::same_path(&f.path, path)) {
                Some(folder) => folder.sandboxes += n,
                None => out.push(Folder {
                    path: path.clone(),
                    sandboxes: n,
                    current,
                }),
            }
        }
        out.sort_by_cached_key(|f| model::folder_key(&f.path));
        out
    }

    /// Clear the status message before its time is up — the frontend's close button.
    pub fn dismiss_status(&mut self) {
        self.status = None;
    }

    /// Turn sbx's telemetry off where nobody chose a value ([`sbx::disable_telemetry_by_default`]),
    /// and say so. Called once as a dashboard starts.
    pub fn disable_telemetry_by_default(&mut self) {
        match sbx::disable_telemetry_by_default() {
            Ok(true) => self.set_info(TELEMETRY_TURNED_OFF),
            Ok(false) => {}
            Err(e) => self.set_error(format!("could not turn sbx's telemetry off: {e:#}")),
        }
    }

    pub fn set_info(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false, Instant::now()));
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), true, Instant::now()));
    }

    // --- input -----------------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        match &mut self.screen {
            Screen::List => self.list_key(key),
            Screen::Create(_) => {
                self.create_key(key);
                self.after_wizard_input();
            }
            Screen::Kits(_) => self.kits_key(key),
            Screen::AuthEdit(_) => self.auth_key(key),
            Screen::Secrets(_) => self.secrets_key(key),
            Screen::Skills(_) => self.skills_key(key),
            Screen::RepoSkills(_) => self.repo_skills_key(key),
            Screen::Connect(_) => self.connect_key(key),
            Screen::StartDir(_) => self.start_dir_key(key),
            Screen::Approvals(_) => self.approvals_key(key),
            Screen::Connection(_) => self.conn_key(key),
            Screen::ConnSwitch(_) => self.conn_switch_key(key),
            Screen::Folders(_) => self.folders_key(key),
            Screen::Settings(_) => self.settings_key(key),
        }
    }

    fn list_key(&mut self, key: KeyEvent) {
        if self.find.is_some() {
            return self.find_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let arrows_scroll_detail = self.detail_full;

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            // Esc never quits here: it is pressed in runs, and one press too many would quit.
            KeyCode::Esc => self.detail_full = false,

            KeyCode::Down | KeyCode::Char('j') if ctrl || arrows_scroll_detail => {
                self.scroll_detail(1)
            }
            KeyCode::Up | KeyCode::Char('k') if ctrl || arrows_scroll_detail => {
                self.scroll_detail(-1)
            }
            KeyCode::PageDown if arrows_scroll_detail => self.scroll_detail(10),
            KeyCode::PageUp if arrows_scroll_detail => self.scroll_detail(-10),

            KeyCode::Down | KeyCode::Char('j') => self.step_row(1),
            KeyCode::Up | KeyCode::Char('k') => self.step_row(-1),
            KeyCode::Right | KeyCode::Char('l') => self.step_cursor(1),
            // `h` is the vim left, not help; `?` opens the help.
            KeyCode::Left | KeyCode::Char('h') => self.step_cursor(-1),
            KeyCode::PageDown => self.step_row(self.grid_rows.max(1) as isize),
            KeyCode::PageUp => self.step_row(-(self.grid_rows.max(1) as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(self.slots() - 1),

            KeyCode::Tab => self.select_tab(self.tab.index() + 1),
            KeyCode::BackTab => self.select_tab(self.tab.index() + Tab::ALL.len() - 1),
            KeyCode::Char(c @ '1'..='7') => self.select_tab(c as usize - '1' as usize),
            KeyCode::Char('m') => self.detail_full = !self.detail_full,
            KeyCode::Char('M') => self.mouse = !self.mouse,
            KeyCode::Char('/') => self.start_find(),

            _ => {
                if let Some(action) = Action::from_key(key) {
                    self.perform(action);
                }
            }
        }
    }

    /// Move one row along the list, wrapping at the ends.
    fn step_cursor(&mut self, delta: isize) {
        let slots = self.slots() as isize;
        let next = (self.cursor as isize + delta).rem_euclid(slots) as usize;
        self.select(next);
    }

    /// Move `delta` rows down the list (up when negative), skipping its headings. It does not
    /// wrap: it stops on the first and the last row.
    fn step_row(&mut self, delta: isize) {
        let last = self.slots() as isize - 1;
        self.select((self.cursor as isize + delta).clamp(0, last) as usize);
    }

    /// Put the cursor on a list cell (clamped to the last one) — a key, a click, a frontend.
    pub fn select(&mut self, cell: usize) {
        let next = cell.min(self.slots() - 1);
        if next != self.cursor {
            self.cursor = next;
            self.detail_scroll = 0;
        }
    }

    pub fn select_tab(&mut self, index: usize) {
        let next = Tab::from_index(index % Tab::ALL.len());
        if next != self.tab {
            self.tab = next;
            self.detail_scroll = 0;
        }
    }

    fn scroll_detail(&mut self, delta: i32) {
        self.detail_scroll = (self.detail_scroll as i32 + delta).max(0) as u16;
    }

    // --- mouse -----------------------------------------------------------------------------

    /// Register a click target. Called by the renderer as it draws; later registrations sit on
    /// top, which is exactly the order in which widgets are painted (modals last).
    pub fn hit(&mut self, area: Rect, hit: Hit) {
        if area.width > 0 && area.height > 0 {
            self.hits.push((area, hit));
        }
    }

    pub fn clear_hits(&mut self) {
        self.hits.clear();
    }

    fn hit_at(&self, at: Position, scroll_zone: bool) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, hit)| hit.is_scroll_zone() == scroll_zone && rect.contains(at))
            .map(|(_, hit)| hit.clone())
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        let at = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(hit) = self.hit_at(at, false) {
                    self.click(hit);
                }
            }
            MouseEventKind::ScrollDown => self.on_wheel(at, 1),
            MouseEventKind::ScrollUp => self.on_wheel(at, -1),
            _ => {}
        }
    }

    /// What a click on `hit` does — the one path for every frontend's clicks: a sandbox's row selects
    /// (twice starts), a form row is focused and, unless it is a text row, pressed with ↵.
    pub fn click(&mut self, hit: Hit) {
        let now = Instant::now();
        let double = self
            .last_click
            .as_ref()
            .is_some_and(|(at, prev)| *prev == hit && at.elapsed() < DOUBLE_CLICK);
        self.last_click = Some((now, hit.clone()));
        // A click on the list or a tab ends a search where it lands; a footer hint is a key.
        if !matches!(hit, Hit::Press(..)) {
            self.find = None;
        }

        match hit {
            Hit::Card(i) => {
                self.select(i);
                if double {
                    self.perform(Action::Start);
                }
            }
            Hit::NewCard => {
                // So that Esc out of the form leaves the cursor on the New row.
                self.select(self.here);
                self.open_new_form();
            }
            Hit::TabAt(i) => self.select_tab(i),
            Hit::Press(code, modifiers) => {
                self.on_key(KeyEvent::new(code, modifiers));
            }
            Hit::FormRow(i) => self.activate_form_row(i),
            Hit::FormFocus(i) => self.focus_form_row(i),
            Hit::FormChoice(i, value) => self.pick_form_choice(i, value),
            Hit::WizardStep(step) => self.wizard_goto(step),
            _ => {}
        }
        self.after_wizard_input();
    }

    fn on_wheel(&mut self, at: Position, delta: i32) {
        match self.hit_at(at, true) {
            Some(Hit::ScrollDetail) => self.scroll_detail(delta * 2),
            Some(Hit::ScrollCards) => self.step_row(delta as isize),
            Some(Hit::ScrollHelp) => {
                self.help_scroll = self.help_scroll.saturating_add_signed(delta as i16 * 2);
            }
            Some(Hit::ScrollLog) => {
                if let Some(Modal::Running(job, _)) = &mut self.modal {
                    job.follow = false;
                    job.scroll = job.scroll.saturating_add_signed(delta as i16 * 2);
                }
            }
            _ => {}
        }
    }

    /// Is the form on screen focused on the line of buttons it ends with?
    pub fn on_form_buttons(&self) -> bool {
        let (focus, rows, buttons) = match &self.screen {
            Screen::Create(f) => (
                f.wizard_focus(),
                f.wizard_rows(&self.catalog).len(),
                f.wizard_buttons(),
            ),
            Screen::Kits(f) => (f.focus, f.rows(&self.catalog).len(), KitsForm::BUTTONS),
            Screen::AuthEdit(f) => (f.focus, f.rows().len(), AuthForm::BUTTONS),
            Screen::StartDir(f) => (f.focus, f.rows().len(), f.buttons()),
            Screen::Connection(f) => (f.focus, f.rows().len(), ConnForm::BUTTONS),
            Screen::ConnSwitch(f) => (f.focus, f.rows().len(), ConnSwitchForm::BUTTONS),
            Screen::Folders(f) => (f.focus, f.rows().len(), FoldersForm::BUTTONS),
            _ => return false,
        };
        // `>=`, not `==`: a stale focus past the end is clamped onto the last row, a button.
        focus + buttons >= rows
    }

    /// Is the secrets screen's cursor on a LiteLLM connection, whose ↵ opens its dialog?
    pub fn on_connection_row(&self) -> bool {
        let Screen::Secrets(form) = &self.screen else {
            return false;
        };
        let rows = self.secret_rows();
        matches!(
            rows.get(form.focus.min(rows.len().saturating_sub(1))),
            Some(SecretRow::Connection(_))
        )
    }

    /// Is the create form focused on a row whose answer is one of several named values: the
    /// shared-skills mode or the LiteLLM connection? There Left and Right pick a value.
    pub fn on_create_choice_row(&self) -> bool {
        let Screen::Create(form) = &self.screen else {
            return false;
        };
        let rows = form.wizard_rows(&self.catalog);
        let focus = form.wizard_focus().min(rows.len().saturating_sub(1));
        matches!(
            rows.get(focus),
            Some(WizardRow::Field(
                CreateRow::Skills | CreateRow::LitellmConn | CreateRow::Folder(_)
            ))
        )
    }

    /// Move the cursor of the form on screen onto row `index`, and do nothing else: a row past
    /// the form's last is ignored.
    fn focus_form_row(&mut self, index: usize) {
        let rows = match &self.screen {
            Screen::Create(f) => f.wizard_rows(&self.catalog).len(),
            Screen::Kits(f) => f.rows(&self.catalog).len(),
            Screen::AuthEdit(f) => f.rows().len(),
            Screen::Connection(f) => f.rows().len(),
            Screen::ConnSwitch(f) => f.rows().len(),
            Screen::Secrets(_) => self.secret_rows().len(),
            Screen::Skills(_) => self.skill_rows().len(),
            Screen::RepoSkills(f) => f.rows().len(),
            Screen::Connect(f) => f.rows().len(),
            Screen::StartDir(f) => f.rows().len(),
            Screen::Approvals(_) => self.approval_rows().len(),
            Screen::Folders(f) => f.rows().len(),
            Screen::Settings(f) => f.rows().len(),
            Screen::List => 0,
        };
        if index >= rows {
            return;
        }
        match &mut self.screen {
            Screen::Create(f) => f.set_wizard_focus(index),
            Screen::Kits(f) => f.focus = index,
            Screen::AuthEdit(f) => f.focus = index,
            Screen::Connection(f) => f.focus = index,
            Screen::ConnSwitch(f) => f.focus = index,
            Screen::Secrets(f) => f.focus = index,
            Screen::Skills(f) => f.focus = index,
            Screen::RepoSkills(f) => f.focus = index,
            Screen::Connect(f) => f.focus = index,
            Screen::StartDir(f) => f.focus = index,
            Screen::Approvals(f) => f.focus = index,
            Screen::Folders(f) => f.focus = index,
            Screen::Settings(f) => f.focus = index,
            Screen::List => {}
        }
    }

    /// A click on form row `index`: focus it and, on most rows, do what Enter does.
    fn activate_form_row(&mut self, index: usize) {
        let catalog = Arc::clone(&self.catalog);
        // Read before the mutable borrow below: these screens derive their rows from the app.
        let secret_rows = match self.screen {
            Screen::Secrets(_) => self.secret_rows().len(),
            Screen::Skills(_) => self.skill_rows().len(),
            Screen::Approvals(_) => self.approval_rows().len(),
            _ => 0,
        };
        match &mut self.screen {
            // A text row only takes focus: Enter would step straight off it again.
            Screen::Create(form) => {
                let rows = form.wizard_rows(&catalog);
                if index >= rows.len() {
                    return;
                }
                form.set_wizard_focus(index);
                if !matches!(rows[index], WizardRow::Field(row) if row.is_text()) {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::Kits(form) => {
                if index >= form.rows(&catalog).len() {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::AuthEdit(form) => {
                let rows = form.rows();
                if index >= rows.len() {
                    return;
                }
                form.focus = index;
                if !rows[index].is_text() {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::Connection(form) => {
                let rows = form.rows();
                if index >= rows.len() {
                    return;
                }
                form.focus = index;
                if !rows[index].is_text() {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::ConnSwitch(form) => {
                let rows = form.rows();
                if index >= rows.len() {
                    return;
                }
                form.focus = index;
                if !rows[index].is_text() {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::Secrets(form) => {
                if index >= secret_rows {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::Skills(form) => {
                if index >= secret_rows {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::RepoSkills(form) => {
                if index >= form.rows().len() {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::Connect(form) => {
                // The SSH endpoint line registers `usize::MAX`: out of range, so nothing happens.
                if index >= form.rows().len() {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::StartDir(form) => {
                if index >= form.rows().len() {
                    return;
                }
                form.focus = index;
                self.on_key(KeyEvent::from(KeyCode::Enter));
            }
            Screen::Folders(form) => {
                let rows = form.rows();
                if index >= rows.len() {
                    return;
                }
                form.focus = index;
                // A text row only takes focus, and a folder only gets the cursor: its buttons
                // edit or drop it.
                if matches!(
                    rows[index],
                    FoldersRow::Browse | FoldersRow::Save | FoldersRow::Close
                ) {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::Settings(form) => {
                let rows = form.rows();
                if index >= rows.len() {
                    return;
                }
                form.focus = index;
                if rows[index] == SettingsRow::Close {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            // A click only moves the cursor: Enter on a request allows its egress.
            Screen::Approvals(form) => {
                if index >= secret_rows {
                    return;
                }
                form.focus = index;
                if index + 1 == secret_rows {
                    self.on_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            Screen::List => {}
        }
    }

    // --- secrets ---------------------------------------------------------------------------

    /// Every credential the screen lists, in the order it draws them. Derived on every call from
    /// `sbx secret ls`, the kit catalog and the LiteLLM connections.
    pub fn secret_rows(&self) -> Vec<SecretRow> {
        let mut rows = vec![SecretRow::Oauth];
        if let Some(secrets) = &self.secrets {
            rows.extend(secrets.token_names().into_iter().map(SecretRow::Token));
        }
        // Read now, like every other row: `connection.json` is the authority for which exist.
        let conns = crate::litellm::load_all_in(&self.litellm_root);
        rows.extend(conns.iter().map(|c| SecretRow::Connection(c.name.clone())));
        if let Some(secrets) = &self.secrets {
            rows.extend(
                secrets
                    .litellm_keys()
                    .into_iter()
                    .filter(|(name, _)| !conns.iter().any(|c| c.name == *name))
                    .map(|(name, _)| SecretRow::LitellmOrphan(name.to_string())),
            );
        }
        if self.has_agy() {
            rows.push(SecretRow::AgyLogin);
        }
        rows.extend(self.service_rows());
        rows.push(SecretRow::Close);
        rows
    }

    /// Does the catalog carry the `agy` backend, whose sign-in has a row of its own?
    fn has_agy(&self) -> bool {
        self.catalog.backend("agy").is_some()
    }

    /// Service secrets worth a row: the ones this repo's kits declare, stored or not, then
    /// anything else stored but the OAuth sign-ins. A sandbox-scoped secret is a row of its own.
    fn service_rows(&self) -> Vec<SecretRow> {
        let mut rows: Vec<SecretRow> = Vec::new();
        // The `antigravity` sign-in has its own row; a generic row would offer to overwrite it.
        let agy_row = self.has_agy();
        let kits = self
            .catalog
            .backends
            .iter()
            .map(|b| &b.kit)
            .chain(self.catalog.all_addons().map(|a| &a.kit));
        for kit in kits {
            for service in &kit.credentials {
                if agy_row && service == sbx::AGY_OAUTH_SERVICE {
                    continue;
                }
                let row = SecretRow::global_service(service);
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
        }
        if let Some(secrets) = &self.secrets {
            let mut stored: Vec<&sbx::Service> = secrets
                .services
                .iter()
                .filter(|s| !(s.is_global() && s.name == sbx::OAUTH_SERVICE))
                .filter(|s| !(agy_row && s.is_global() && s.name == sbx::AGY_OAUTH_SERVICE))
                .collect();
            // The MCP gateway's credentials sort last as a block: they have a heading of their own.
            stored.sort_by_key(|s| {
                (
                    sbx::mcp_secret(&s.name).is_some(),
                    s.name.clone(),
                    s.scope.clone(),
                )
            });
            for service in stored {
                let row = SecretRow::Service {
                    name: service.name.clone(),
                    scope: (!service.is_global()).then(|| service.scope.clone()),
                };
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
        }
        rows
    }

    /// Which sandboxes authenticate with the host-global `/login`.
    pub fn login_users(&self) -> usize {
        self.sandboxes
            .iter()
            .filter(|s| matches!(s.auth, Some(Auth::Login)))
            .count()
    }

    /// Which sandboxes were built from the `agy` backend, and so share its sign-in.
    pub fn agy_users(&self) -> usize {
        self.sandboxes
            .iter()
            .filter(|s| s.backend.as_deref() == Some("agy"))
            .count()
    }

    /// Whether the host holds the Antigravity sign-in, as the agy sandboxes' `auth_mode` says
    /// (`oauth · antigravity` or `none · antigravity`); `None` with no agy sandbox to ask.
    pub fn agy_signed_in(&self) -> Option<bool> {
        let modes: Vec<&str> = (self.sandboxes.iter())
            .filter(|s| s.backend.as_deref() == Some("agy"))
            .filter_map(|s| {
                s.auth_mode
                    .strip_suffix(&format!(" · {}", sbx::AGY_OAUTH_SERVICE))
            })
            .collect();
        match modes.as_slice() {
            [] => None,
            modes => Some(modes.iter().any(|m| m.trim() == "oauth")),
        }
    }

    /// Is there a value behind this service row — at the global scope, or against one sandbox?
    fn service_stored(&self, name: &str, scope: Option<&str>) -> bool {
        self.secrets.as_ref().is_some_and(|s| {
            s.services.iter().any(|row| {
                row.name == name
                    && match scope {
                        Some(sandbox) => row.scope == sandbox,
                        None => row.is_global(),
                    }
            })
        })
    }

    /// Which sandboxes were built from a kit that names this service — who a gateway token
    /// belongs to, and who is left unauthenticated without it.
    pub fn service_users(&self, service: &str) -> usize {
        self.sandboxes
            .iter()
            .filter(|sb| {
                self.kits_of(sb)
                    .any(|k| k.credentials.iter().any(|c| c == service))
            })
            .count()
    }

    /// The labels of the sandboxes on a LiteLLM connection, by their kit lists — never by whether
    /// its `connection.json` still exists.
    pub fn litellm_users(&self, name: &str) -> Vec<String> {
        self.sandboxes
            .iter()
            .filter(|s| s.litellm.as_deref() == Some(name))
            .map(|s| s.label.clone())
            .collect()
    }

    /// One secret row as both frontends draw it: what it is, what sbx has stored for it (its own
    /// masked text, never a value), and who it belongs to.
    pub fn secret_view(&self, row: &SecretRow) -> SecretView {
        let (name, stored, note) = match row {
            SecretRow::Oauth => {
                let service = self.secrets.as_ref().and_then(|s| s.oauth());
                (
                    // An API key stored by hand under the service is not a `/login`.
                    match service {
                        Some(s) if !s.is_oauth() => "anthropic (API key)".to_string(),
                        _ => "/login (OAuth)".to_string(),
                    },
                    service.map(|s| s.masked.clone()),
                    match self.login_users() {
                        0 => "no sandbox uses it".to_string(),
                        n => format!("{n} sandbox{}", if n == 1 { "" } else { "es" }),
                    },
                )
            }
            SecretRow::AgyLogin => (
                "Antigravity sign-in".to_string(),
                self.secrets
                    .as_ref()
                    .and_then(|s| s.agy_login())
                    .map(|s| s.masked.clone()),
                match self.agy_users() {
                    0 => "no agy sandbox here".to_string(),
                    n => format!("{n} agy sandbox{}", if n == 1 { "" } else { "es" }),
                },
            ),
            SecretRow::Token(token) => {
                let placeholder = sbx::named_token_placeholder(token);
                (
                    format!("token {token}"),
                    self.secrets
                        .as_ref()
                        .and_then(|s| s.custom.iter().find(|c| c.placeholder == placeholder))
                        .map(|c| c.masked.clone()),
                    match self.token_users(token) {
                        0 => "no sandbox uses it".to_string(),
                        n => format!("{n} sandbox{}", if n == 1 { "" } else { "es" }),
                    },
                )
            }
            // The key counts as stored only for the connection's host.
            SecretRow::Connection(conn) => {
                let read = crate::litellm::load_in(&self.litellm_root, conn);
                let host = read.as_ref().map_or("", |c| crate::litellm::host(&c.url));
                let models = read
                    .as_ref()
                    .map_or(0, |c| self.connection_models(c).0.len());
                let key = self.secrets.as_ref().and_then(|s| s.litellm_key(conn));
                let note = match (key.is_some_and(|k| !k.targets_host(host)), models) {
                    (true, _) => format!("{host} · key stored for another host"),
                    (false, 0) => host.to_string(),
                    (false, n) => format!("{host} · {n} model{}", if n == 1 { "" } else { "s" }),
                };
                (
                    conn.clone(),
                    key.filter(|k| k.targets_host(host))
                        .map(|k| k.masked.clone()),
                    note,
                )
            }
            SecretRow::LitellmOrphan(conn) => (
                conn.clone(),
                self.secrets
                    .as_ref()
                    .and_then(|s| s.litellm_key(conn))
                    .map(|k| k.masked.clone()),
                "⚠ no connection.json names it".to_string(),
            ),
            SecretRow::Service { name, scope } => {
                // Matched on name and scope: a scoped row is not the global one.
                let stored = self.secrets.as_ref().and_then(|s| {
                    s.services.iter().find(|row| {
                        row.name == *name
                            && match scope {
                                Some(sandbox) => row.scope == *sandbox,
                                None => row.is_global(),
                            }
                    })
                });
                // An `mcp:` secret is the MCP gateway's, never a kit's.
                let note = match (sbx::mcp_secret(name), scope) {
                    (Some(mcp), _) => format!("MCP gateway: {}", mcp.server),
                    (None, Some(sandbox)) => format!("this sandbox only: {sandbox}"),
                    (None, None) => match self.kits_needing(name).as_slice() {
                        [] => "no kit here needs it".to_string(),
                        kits => kits.join(", "),
                    },
                };
                (name.clone(), stored.map(|s| s.masked.clone()), note)
            }
            SecretRow::Close => (String::new(), None, String::new()),
        };
        let empty = match (self.secrets.is_some(), row) {
            (false, _) => Empty::Unknown,
            (true, SecretRow::AgyLogin) => match self.agy_signed_in() {
                None => Empty::NotListed,
                Some(true) => Empty::SignedIn,
                Some(false) => Empty::NotSignedIn,
            },
            (true, _) => Empty::NotStored,
        };
        SecretView {
            name,
            stored,
            note,
            empty,
        }
    }

    /// What Enter and d do to the credential under the cursor, spelled out on the row itself. A
    /// change to a global secret reaches running sandboxes: it is never a recreate.
    pub fn secret_notes(&self, row: &SecretRow) -> Vec<String> {
        // Whole sentences, one note each, naming actions by the buttons' words, not by a key.
        let note = |text: &str| vec![text.to_string()];
        match row {
            SecretRow::Connection(_) => vec![
                "A LiteLLM gateway: its address and its key. Every sandbox on it reads the \
                 gateway's models itself; none sees the key."
                    .into(),
                "Edit changes its address or key, Delete removes both; New connection adds \
                 another."
                    .into(),
            ],
            SecretRow::LitellmOrphan(_) => note(
                "A LiteLLM key with no connection: its connection was never saved, or was removed \
                 by hand. Delete removes it.",
            ),
            SecretRow::Oauth => note(
                "One account for every /login sandbox. Sign in with /login inside a sandbox; it \
                 cannot be stored here. Delete signs every /login sandbox out.",
            ),
            SecretRow::AgyLogin => note(
                "One Google account for every agy sandbox. Sign in inside an agy sandbox; it cannot \
                 be stored here. Delete signs every agy sandbox out. A sandbox made before needs \
                 a rebuild (R) to share it.",
            ),
            SecretRow::Token(token) => vec![format!(
                "Every sandbox on the token “{token}” signs in with it. Replace changes it for all \
                 of them, with nothing rebuilt; Delete leaves them with no token."
            )],
            // The MCP gateway's, never injected.
            SecretRow::Service { name, .. } if sbx::mcp_secret(name).is_some() => {
                let mcp = sbx::mcp_secret(name).expect("matched above");
                vec![
                    format!(
                        "The secret of the MCP server “{}”. No sandbox sees it. Store or Replace \
                         writes it; Delete removes it.",
                        mcp.server
                    ),
                    "Restart the sandbox after a change: it keeps the value it started with."
                        .into(),
                ]
            }
            SecretRow::Service { name, scope: None } => vec![format!(
                "Store or Replace writes it; Delete removes it. A sandbox that uses it gets the \
                 change at once — {}.",
                match self.service_users(name) {
                    0 => "no sandbox here does yet".to_string(),
                    n => format!("{n} here, with nothing rebuilt"),
                }
            )],
            SecretRow::Service {
                scope: Some(sandbox),
                ..
            } => vec![format!(
                "Stored for {sandbox} alone: it replaces the secret of the same name for that one \
                 sandbox. Replace and Delete apply at once."
            )],
            SecretRow::Close => Vec::new(),
        }
    }

    /// The heading a secret row is listed under; `None` for the button.
    pub fn secret_heading(row: &SecretRow) -> Option<&'static str> {
        match row {
            SecretRow::Oauth | SecretRow::Token(_) => Some("Anthropic subscription"),
            SecretRow::AgyLogin => Some("Antigravity (agy) — Google account"),
            SecretRow::Connection(_) | SecretRow::LitellmOrphan(_) => Some("LiteLLM connections"),
            SecretRow::Service { name, .. } if sbx::mcp_secret(name).is_some() => Some(MCP_SECRETS),
            SecretRow::Service { .. } => Some("Service tokens"),
            SecretRow::Close => None,
        }
    }

    /// Custom secrets somebody else stored, one line each, then how to change them; empty when
    /// there are none. Listed, but never touched from here.
    pub fn foreign_secret_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .secrets
            .iter()
            .flat_map(|s| s.foreign_custom())
            .map(|c| {
                format!(
                    "{:<24} {:<22} {:<26} {}",
                    c.placeholder,
                    c.targets_label(),
                    if c.env.is_empty() { "-" } else { &c.env },
                    c.scope
                )
            })
            .collect();
        if !lines.is_empty() {
            lines.push("change these with `sbx secret set-custom` / `sbx secret rm`.".into());
        }
        lines
    }

    /// Which of this repo's kits name a service in their `credentials:` — who the token is for.
    fn kits_needing(&self, service: &str) -> Vec<String> {
        self.catalog
            .backends
            .iter()
            .map(|b| &b.kit)
            .chain(self.catalog.all_addons().map(|a| &a.kit))
            .filter(|k| k.credentials.iter().any(|c| c == service))
            .map(|k| k.name.clone())
            .collect()
    }

    /// The kits a sandbox was built from, as this repo's catalog knows them.
    fn kits_of<'a>(&'a self, sb: &'a Sandbox) -> impl Iterator<Item = &'a crate::kits::Kit> + 'a {
        sb.backend
            .as_deref()
            .and_then(|b| self.catalog.backend(b))
            .map(|b| &b.kit)
            .into_iter()
            .chain(
                sb.addons
                    .iter()
                    .filter_map(|a| self.catalog.addon(a))
                    .map(|a| &a.kit),
            )
    }

    fn secrets_key(&mut self, key: KeyEvent) {
        let rows = self.secret_rows();
        let Screen::Secrets(form) = &mut self.screen else {
            return;
        };
        form.focus = form.focus.min(rows.len() - 1);
        let row = rows[form.focus].clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = (form.focus + rows.len() - 1) % rows.len()
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = (form.focus + 1) % rows.len()
            }
            KeyCode::Home | KeyCode::Char('g') => form.focus = 0,
            KeyCode::End | KeyCode::Char('G') => form.focus = rows.len() - 1,
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                SecretRow::Close => self.screen = Screen::List,
                other => self.ask_secret(&other),
            },
            KeyCode::Char('d') | KeyCode::Delete => self.delete_secret(&row),
            // Anywhere on the screen, like the create form's *new connection…*.
            KeyCode::Char('n') => self.open_connection(ConnMode::New),
            KeyCode::F(5) => self.refresh(),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.refresh(),
            _ => {}
        }
    }

    /// Ask for a secret's value — storing a missing one, or replacing one that exists.
    fn ask_secret(&mut self, row: &SecretRow) {
        // A connection's key is one field of its dialog.
        if let SecretRow::Connection(name) = row {
            self.open_connection(ConnMode::Edit(name.clone()));
            return;
        }
        let Some(target) = row.target() else {
            // OAuth sign-ins happen inside a sandbox, so nothing here can write them.
            self.set_info(match row {
                SecretRow::AgyLogin => {
                    "the Antigravity account is signed in from inside an agy sandbox — start one \
                     and sign in; d removes what is stored"
                }
                SecretRow::LitellmOrphan(_) => "no connection.json names this key — d removes it",
                _ => {
                    "the /login account is signed in from inside a sandbox — start one and run \
                     /login; d removes what is stored"
                }
            });
            return;
        };
        let (subject, help) = match &target {
            SecretTarget::Token(name) => (
                format!("setup token “{name}”"),
                vec![
                    "Generate one with  claude setup-token  while signed in as that account."
                        .to_string(),
                    match self.token_users(name) {
                        0 => "No sandbox uses this token yet.".to_string(),
                        1 => "It replaces the value for the 1 sandbox on this name.".to_string(),
                        n => format!(
                            "It replaces the value for all {n} sandboxes on this name, at once."
                        ),
                    },
                    "Nothing is rebuilt: the placeholder each container carries is unchanged."
                        .to_string(),
                ],
            ),
            SecretTarget::Service { name, scope } => (
                name.clone(),
                vec![
                    match scope {
                        Some(sandbox) => format!(
                            "Stored in the host keychain for {sandbox} alone, under the service \
                             “{name}”."
                        ),
                        None => format!("Stored in the host keychain under the service “{name}”."),
                    },
                    "The sandbox only ever sees a sentinel — the proxy swaps the real value in."
                        .to_string(),
                    match (scope.is_some(), self.service_users(name)) {
                        (true, _) => {
                            "It applies to that one sandbox, on its next request.".to_string()
                        }
                        (false, 0) => "A global secret reaches every sandbox that uses it on its \
                                       next request — none here does yet."
                            .to_string(),
                        (false, n) => format!(
                            "A global secret reaches the sandboxes already using it on their next \
                             request — the {n} here pick the new value up with nothing rebuilt."
                        ),
                    },
                ],
            ),
        };
        self.modal = Some(Modal::Prompt(Prompt {
            ask: Ask::SetSecret(target),
            subject,
            input: TextInput::default(),
            help,
        }));
    }

    /// Drop a stored secret from the host, once the confirmation says who it belongs to.
    fn delete_secret(&mut self, row: &SecretRow) {
        let (title, mut body, step, post) = match row {
            SecretRow::Close => return,
            SecretRow::Oauth => {
                if self.secrets.as_ref().is_some_and(|s| s.oauth().is_none()) {
                    self.set_info("nothing is signed in — run /login inside a Claude sandbox");
                    return;
                }
                (
                    "Sign out of the /login account",
                    vec![
                        match self.login_users() {
                            0 => "No sandbox on this host uses the /login account right now."
                                .to_string(),
                            1 => "The 1 sandbox on the /login account loses it.".to_string(),
                            n => format!("All {n} sandboxes on the /login account lose it."),
                        },
                        String::new(),
                        "sbx keeps this credential on the host at global scope, so it is one \
                         account for the whole machine. Dropping it and running /login again is \
                         how you switch to another."
                            .to_string(),
                        String::new(),
                        "No container is touched and nothing is rebuilt — sbx re-seeds each \
                         sandbox from whatever is signed in next."
                            .to_string(),
                    ],
                    Step::new(
                        "sbx secret rm anthropic",
                        sbx::secret_rm_service_argv(None, sbx::OAUTH_SERVICE),
                    ),
                    PostJob::default(),
                )
            }
            SecretRow::AgyLogin => {
                if !self.service_stored(sbx::AGY_OAUTH_SERVICE, None) {
                    self.set_info(
                        "sbx lists no Antigravity sign-in to remove — `sbx secret rm antigravity -f` \
                         is the manual way",
                    );
                    return;
                }
                (
                    "Sign out of the Antigravity account",
                    {
                        vec![
                            match self.agy_users() {
                                0 => "No agy sandbox exists on this host right now.".to_string(),
                                1 => "This removes the shared sign-in from the 1 agy sandbox.".to_string(),
                                n => format!("This removes the shared sign-in from the {n} agy sandboxes."),
                            },
                            String::new(),
                            "sbx keeps this sign-in on the host at global scope, so it is one Google \
                             account for the whole machine. Signing in again inside any agy sandbox \
                             switches accounts."
                                .to_string(),
                            String::new(),
                            "It does not revoke a sign-in held inside a container: a sandbox started \
                             without the credential binding may keep its own local session until it \
                             is rebuilt (R) or signed out inside."
                                .to_string(),
                        ]
                    },
                    Step::new(
                        "sbx secret rm antigravity",
                        sbx::secret_rm_service_argv(None, sbx::AGY_OAUTH_SERVICE),
                    ),
                    PostJob::default(),
                )
            }
            SecretRow::Token(name) => {
                let users = self.sandboxes_on(name, "");
                let mut body = vec![match users.as_slice() {
                    [] => "No sandbox is on this token.".to_string(),
                    others => format!(
                        "{} {} pointed at it and will show “⚠ no token” until one is stored again.",
                        join_names(others),
                        if others.len() == 1 { "is" } else { "are" }
                    ),
                }];
                body.push(String::new());
                body.push(
                    "The token is stored once for this host, so this removes the account itself \
                     — not one sandbox's use of it."
                        .to_string(),
                );
                (
                    "Delete a setup token",
                    body,
                    Step::new(
                        format!("sbx secret rm --placeholder (token “{name}”)"),
                        sbx::secret_rm_named_token_argv(&sbx::named_token_placeholder(name)),
                    ),
                    PostJob {
                        drop_token_kit: Some(name.clone()),
                        ..Default::default()
                    },
                )
            }
            SecretRow::Connection(name) => {
                let users = self.litellm_users(name);
                (
                    "Delete a LiteLLM connection",
                    vec![
                        match users.as_slice() {
                            [] => "No sandbox is on this connection.".to_string(),
                            others => format!(
                                "{} {} on it: they stop authenticating at once, and keep failing \
                                 until a connection of this name is added again.",
                                join_names(others),
                                if others.len() == 1 { "is" } else { "are" }
                            ),
                        },
                        String::new(),
                        "Its connection.json, generated kits and model list go with it."
                            .to_string(),
                    ],
                    Step::new(
                        format!("sbx secret rm --placeholder (connection “{name}”)"),
                        sbx::secret_rm_litellm_key_argv(&crate::litellm::placeholder(name)),
                    ),
                    PostJob {
                        drop_connection: Some(name.clone()),
                        ..Default::default()
                    },
                )
            }
            SecretRow::LitellmOrphan(name) => (
                "Delete a LiteLLM key",
                vec![
                    format!(
                        "No connection.json names “{name}”: its key was stored and the file \
                         never written, or the file was removed by hand."
                    ),
                    String::new(),
                    // The file is gone, the kits are not: the kit lists say who still uses the key.
                    match self.litellm_users(name).as_slice() {
                        [] => "No sandbox is on it, so nothing uses the key.".to_string(),
                        [one] => format!(
                            "{one} is still built on it: it would stop authenticating at once."
                        ),
                        users => format!(
                            "{} are still built on it: they would stop authenticating at once.",
                            join_names(users)
                        ),
                    },
                ],
                Step::new(
                    format!("sbx secret rm --placeholder (LiteLLM key “{name}”)"),
                    sbx::secret_rm_litellm_key_argv(&crate::litellm::placeholder(name)),
                ),
                PostJob::default(),
            ),
            SecretRow::Service { name, scope } => {
                if !self.service_stored(name, scope.as_deref()) {
                    self.set_info(format!("nothing is stored for “{name}”"));
                    return;
                }
                (
                    "Delete a stored secret",
                    vec![
                        match (scope, self.service_users(name)) {
                            (Some(sandbox), _) => format!(
                                "This one is stored for {sandbox} alone. It loses the credential \
                                 on its next request; the global secret of the same name, if \
                                 there is one, is untouched."
                            ),
                            (None, 0) => format!(
                                "No sandbox on this host is built from a kit that needs “{name}”."
                            ),
                            (None, n) => format!(
                                "{n} sandbox(es) are built from a kit that needs “{name}”. The \
                                 running ones lose it on their next request — nothing is \
                                 restarted — and stay unauthenticated until it is stored again."
                            ),
                        },
                        String::new(),
                        format!(
                            "Store it again with  sbx secret set {name}  — or from this screen \
                             with ↵."
                        ),
                    ],
                    Step::new(
                        match scope {
                            Some(sandbox) => format!("sbx secret rm {name} --sandbox {sandbox}"),
                            None => format!("sbx secret rm {name}"),
                        },
                        sbx::secret_rm_service_argv(scope.as_deref(), name),
                    ),
                    PostJob::default(),
                )
            }
        };
        // A deleted connection's model fetcher goes with it.
        let fetcher = match &row {
            SecretRow::Connection(name) => {
                let fetcher = crate::model_fetch::sandbox_name(name);
                (self.fetchers.contains(&fetcher))
                    .then(|| Step::new(format!("sbx rm {fetcher}"), sbx::rm_argv(&fetcher)))
            }
            _ => None,
        };
        // A keyless connection gets no `sbx secret rm`: an error there would keep the folder.
        let keyless = matches!(row, SecretRow::Connection(name)
            if self.secrets.as_ref().is_some_and(|s| s.litellm_key(name).is_none()));
        body.push(String::new());
        body.push(match keyless {
            true => "No key is stored for it. This cannot be undone.".into(),
            false => "The value is removed from the host keychain. This cannot be undone.".into(),
        });
        self.modal = Some(Modal::Confirm(Confirm {
            title: title.into(),
            body,
            danger: true,
            confirm_label: "Delete".into(),
            yes: false,
            job: PendingJob {
                title: step.label.clone(),
                steps: (!keyless)
                    .then_some(step)
                    .into_iter()
                    .chain(fetcher)
                    .collect(),
                post,
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// The job that writes a pasted secret. The value goes in on **stdin** — never in argv, where
    /// `ps` would show it to every process on the host.
    fn store_secret_job(&self, target: &SecretTarget, value: String) -> PendingJob {
        let step = match target {
            SecretTarget::Token(name) => {
                let placeholder = sbx::named_token_placeholder(name);
                Step::with_stdin(
                    format!("sbx secret set-custom ({placeholder})"),
                    sbx::secret_set_named_token_argv(&placeholder),
                    value,
                )
            }
            SecretTarget::Service { name, scope } => Step::with_stdin(
                match scope {
                    Some(sandbox) => format!("sbx secret set {name} --sandbox {sandbox}"),
                    None => format!("sbx secret set {name}"),
                },
                sbx::secret_set_service_argv(scope.as_deref(), name),
                value,
            ),
        };
        PendingJob {
            title: match target {
                SecretTarget::Token(name) => format!("Storing the setup token “{name}”"),
                SecretTarget::Service { name, .. } => format!("Storing the “{name}” secret"),
            },
            steps: vec![step],
            post: PostJob::default(),
            fallback: None,
            fallback_hint: None,
        }
    }

    /// The create wizard's keys. A row's own answer is [`CreateForm::field_key`]'s; only the
    /// start-folder picker comes back here.
    fn create_key(&mut self, key: KeyEvent) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        let rows = form.wizard_rows(&catalog);
        let buttons = form.wizard_buttons();
        let focus = form.wizard_focus().min(rows.len().saturating_sub(1));
        let Some(&row) = rows.get(focus) else {
            return;
        };
        // Vim keys on every row that takes no typing, as on the other screens.
        let key = match (row, key.code) {
            (WizardRow::Field(field), _) if field.is_text() => key,
            (_, KeyCode::Char(c)) if key.modifiers.is_empty() => match c {
                'j' => KeyEvent::from(KeyCode::Down),
                'k' => KeyEvent::from(KeyCode::Up),
                'h' => KeyEvent::from(KeyCode::Left),
                'l' => KeyEvent::from(KeyCode::Right),
                _ => key,
            },
            _ => key,
        };

        // The row's own answer first; what it leaves is the wizard's.
        if let WizardRow::Field(field) = row {
            let before = form.connection.litellm.clone();
            let effect = form.field_key(&catalog, field, key);
            if field == CreateRow::LitellmConn && effect != FieldEffect::Ignored {
                self.litellm_picked(before);
                if effect == FieldEffect::Next {
                    self.wizard_next();
                }
                return;
            }
            match effect {
                FieldEffect::Ignored => {}
                FieldEffect::Next => {
                    self.wizard_next();
                    return;
                }
                // An existing token is the step's answer, like a radio: picked, and on.
                FieldEffect::Done if key.code == KeyCode::Enter => {
                    if matches!(field, CreateRow::TokenPick(_)) {
                        self.wizard_next();
                    }
                    return;
                }
                FieldEffect::Done => return,
                FieldEffect::OpenStartDir => {
                    self.action_create_start_dir();
                    return;
                }
                FieldEffect::FetchModels => {
                    self.wizard_fetch();
                    return;
                }
                FieldEffect::EditConfig(i) => {
                    self.edit_config(i);
                    return;
                }
                FieldEffect::ResetConfig(i) => {
                    self.reset_config(i);
                    return;
                }
                FieldEffect::PickFolder => {
                    self.action_pick_folder();
                    return;
                }
            }
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => self.wizard_create(true),
            KeyCode::Char('c') if ctrl => self.close_wizard(true),
            KeyCode::Esc => self.wizard_back(),
            KeyCode::PageDown => self.wizard_next(),
            KeyCode::PageUp if form.wizard_step() != CreateStep::Provider => self.wizard_back(),
            KeyCode::Char(c @ '1'..='7') if !ctrl => {
                let step = CreateStep::ALL[c as usize - '1' as usize];
                self.wizard_goto(step);
            }
            KeyCode::Up | KeyCode::BackTab => {
                form.set_wizard_focus(step_form(focus, rows.len(), buttons, -1))
            }
            KeyCode::Down | KeyCode::Tab => {
                form.set_wizard_focus(step_form(focus, rows.len(), buttons, 1))
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(next) = step_button(focus, rows.len(), buttons, delta) {
                    form.set_wizard_focus(next);
                }
            }
            KeyCode::Char(' ') => match row {
                WizardRow::Provider(p) => form.pick_provider(&catalog, p),
                WizardRow::Agent(h) => form.pick_harness(&catalog, h),
                WizardRow::Connection(choice) => form.pick_connection(&catalog, choice),
                _ => {}
            },
            KeyCode::Enter => match row {
                WizardRow::Provider(p) => {
                    form.pick_provider(&catalog, p);
                    self.wizard_next();
                }
                // On to the rows the harness brings, an account or a connection; without any,
                // on to the next step.
                WizardRow::Agent(h) => {
                    form.pick_harness(&catalog, h);
                    let rows = form.wizard_rows(&catalog);
                    let after = rows.iter().rposition(|r| matches!(r, WizardRow::Agent(_)));
                    let next = after.map(|at| at + 1).filter(|at| {
                        matches!(
                            rows.get(*at),
                            Some(WizardRow::Connection(_) | WizardRow::Field(_))
                        )
                    });
                    match next {
                        Some(at) => form.set_wizard_focus(at),
                        None => self.wizard_next(),
                    }
                }
                // Except "setup token", whose answer is in the token rows right below it.
                WizardRow::Connection(ConnChoice::Token) => {
                    form.pick_connection(&catalog, ConnChoice::Token);
                    let rows = form.wizard_rows(&catalog);
                    let at = rows
                        .iter()
                        .position(|r| *r == WizardRow::Connection(ConnChoice::Token));
                    if let Some(at) = at {
                        if matches!(rows.get(at + 1), Some(WizardRow::Field(_))) {
                            form.set_wizard_focus(at + 1);
                        }
                    }
                }
                WizardRow::Connection(choice) => {
                    form.pick_connection(&catalog, choice);
                    self.wizard_next();
                }
                WizardRow::Field(_) => {
                    form.set_wizard_focus(step_form(focus, rows.len(), buttons, 1))
                }
                // Drawn muted on step 1, where there is nothing to go back to.
                WizardRow::Back => {
                    if form.wizard_step() != CreateStep::Provider {
                        self.wizard_back();
                    }
                }
                WizardRow::Edit(step) => self.wizard_goto(step),
                WizardRow::Next => self.wizard_next(),
                WizardRow::CreateStart => self.wizard_create(true),
                WizardRow::CreateOnly => self.wizard_create(false),
                WizardRow::Cancel => self.close_wizard(false),
            },
            _ => {}
        }
    }

    /// Leave the wizard for the list. `keep`: hold its answers as the draft the next New in this
    /// folder reopens (Esc on step 1, Ctrl-C); otherwise they go (Cancel).
    fn close_wizard(&mut self, keep: bool) {
        let screen = std::mem::replace(&mut self.screen, Screen::List);
        if let (true, Screen::Create(form)) = (keep, screen) {
            self.create_draft = Some((self.cwd.clone(), Box::new(form)));
        }
    }

    /// After the LiteLLM connection row changed: drop the pins and offer the models of the
    /// connection now picked. An unchanged pick keeps the pins.
    fn litellm_picked(&mut self, before: Option<String>) {
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        if form.connection.litellm == before {
            return;
        }
        form.connection.model_pick = None;
        let pick = form.connection.litellm.clone();
        self.load_models_for(pick.as_deref());
    }

    /// A click on one value of a choice row (the skills mode, the LiteLLM connection): the row
    /// takes the cursor and the answer takes that value. A stored connection whose key is there
    /// answers its step, like a radio: picked, and on.
    fn pick_form_choice(&mut self, index: usize, value: usize) {
        if let Screen::Settings(form) = &mut self.screen {
            if form.rows().get(index) == Some(&SettingsRow::SkillsDefault) {
                form.focus = index;
                if let Some(&mode) = crate::create::SKILLS_MODES.get(value) {
                    self.set_skills_default(mode);
                }
            }
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        let rows = form.wizard_rows(&catalog);
        let Some(WizardRow::Field(row)) = rows.get(index).copied() else {
            return;
        };
        let before = form.connection.litellm.clone();
        if !form.pick_choice(row, value) {
            return;
        }
        form.set_wizard_focus(index);
        if row != CreateRow::LitellmConn {
            return;
        }
        let picked = form.litellm_choice().map(|c| c.name.clone());
        self.litellm_picked(before);
        if picked.is_some_and(|name| !self.litellm_key_missing(&name)) {
            self.wizard_next();
        }
    }

    /// Next, or ↵ on a radio that answers its step: refused with the step's own message.
    fn wizard_next(&mut self) {
        if self.store_new_connection_first() {
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        let from = form.wizard_step();
        match form.wizard_next(&catalog) {
            Ok(()) => self.fetch_models_when_stale(from),
            Err(problem) => self.set_error(problem),
        }
    }

    /// The wizard went from `from` past the Connection step, on an existing connection: fetch its
    /// model list when [`crate::model_fetch::stale_in`] says so, and come back where the wizard is
    /// now. Once per connection while the form is open, so a failed fetch is not started again
    /// at every Next.
    fn fetch_models_when_stale(&mut self, from: CreateStep) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &self.screen else {
            return;
        };
        let connection = CreateStep::Connection.number();
        let past = from.number() <= connection && form.wizard_step().number() > connection;
        let Some(conn) = form.litellm_choice().map(|c| c.name.clone()) else {
            return;
        };
        let asked = form.wizard.auto_fetched.as_deref() == Some(conn.as_str());
        if !past || asked || !form.litellm_backend(&catalog) || self.litellm_key_missing(&conn) {
            return;
        }
        let fetched_at = match (&catalog.models_source, self.models_conn.as_deref()) {
            (models::Source::Gateway { fetched_at, .. }, Some(on)) if on == conn => {
                (!catalog.models.is_empty()).then_some(*fetched_at)
            }
            _ => None,
        };
        let now = crate::state::now_epoch();
        if crate::model_fetch::stale_in(&self.litellm_root, &conn, fetched_at, now) {
            self.wizard_fetch();
        }
    }

    /// Leaving the Connection step with *new connection…*: store its key and write its file
    /// first, then fetch its model list, and come back on the Harness step with the connection
    /// picked. A failed key brings the form back on the Connection step, its key emptied.
    /// `false`: nothing to store, or a field still refuses — the step's own check says so.
    fn store_new_connection_first(&mut self) -> bool {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &self.screen else {
            return false;
        };
        let pending = form.wizard_step() == CreateStep::Connection
            && form.litellm_backend(&catalog)
            && form.connection.litellm.is_none()
            && form
                .step_problem(CreateStep::Connection, &catalog)
                .is_none();
        let agent = form
            .effective_backend(&catalog)
            .and_then(Backend::litellm_agent);
        let (true, Some(agent), Ok(conn)) = (pending, agent, form.new_connection()) else {
            return false;
        };
        // Read again now: `aisbx litellm add`, or another dashboard, may have made it meanwhile.
        if crate::litellm::exists_in(&self.litellm_root, &conn.name) {
            self.set_error(format!("connection {} exists", conn.name));
            return true;
        }
        let key = form.connection.new_conn.key.value.trim().to_string();
        let mut failed = form.clone();
        failed.connection.new_conn.key = TextInput::default();
        let mut stored = form.clone();
        stored.connection.new_conn = crate::create::NewConnection::default();
        stored.connection.connections.push(conn.clone());
        (stored.connection.connections).sort_by(|a, b| a.name.cmp(&b.name));
        stored.connection.litellm = Some(conn.name.clone());
        stored.wizard_set_step(&catalog, CreateStep::Harness);
        stored.wizard.auto_fetched = Some(conn.name.clone());
        // Named now, written once the key is stored (`App::start_next`).
        let kit = crate::model_fetch::kit_dir_in(&self.litellm_root, &conn.name);
        let mut fetch = self.fetch_job_at(&conn, &kit);
        let back = Restore::Create(Box::new(stored));
        fetch.post.resume = Some(back.clone());
        fetch.post.restore = Some(back);
        self.mark_fetched(&conn.name);
        let placeholder = crate::litellm::placeholder(&conn.name);
        self.screen = Screen::List;
        self.start_job(PendingJob {
            title: format!("Storing the key of LiteLLM connection {}", conn.name),
            steps: vec![Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_litellm_key_argv(crate::litellm::host(&conn.url), &placeholder),
                key,
            )],
            post: PostJob {
                save_connection: Some((conn, Some(agent))),
                then: Some(Box::new(fetch)),
                restore: Some(Restore::Create(Box::new(failed))),
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        });
        true
    }

    /// *Refresh* on the Models step, and the wizard leaving the Connection step on a stale list
    /// ([`App::fetch_models_when_stale`]): fetch the connection's list, and come back on the step
    /// the wizard is on whether it worked or not.
    fn wizard_fetch(&mut self) {
        let Screen::Create(form) = &self.screen else {
            return;
        };
        let Some(conn) = form.litellm_choice().cloned() else {
            return;
        };
        let mut back = form.clone();
        back.connection.new_conn.key = TextInput::default();
        back.connection.token = TextInput::default();
        back.wizard.auto_fetched = Some(conn.name.clone());
        let back = Restore::Create(Box::new(back));
        let Some(mut job) = self.fetch_job(&conn) else {
            return;
        };
        self.mark_fetched(&conn.name);
        job.post.resume = Some(back.clone());
        job.post.restore = Some(back);
        self.screen = Screen::List;
        self.start_job(job);
    }

    /// After every key and click on the create form: the generated label follows the answers
    /// (a token or connection named, typed or picked), and the Models step opens on the
    /// connection's saved picks.
    fn after_wizard_input(&mut self) {
        if self.modal.is_some() {
            return;
        }
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        form.refresh_auto_name(&catalog);
        let (Some(agent), Some(conn)) =
            (form.models_agent(&catalog), form.connection.litellm.clone())
        else {
            return;
        };
        let target = (conn.clone(), agent);
        if form.connection.picks_for.as_ref() != Some(&target) {
            let c = &mut form.connection;
            if let Some(left) = c.picks_for.take() {
                let given = crate::model_picks::Picks {
                    slots: std::mem::take(&mut c.models),
                    disabled: std::mem::take(&mut c.disabled),
                };
                c.picks_kept.retain(|(key, _)| *key != left);
                c.picks_kept.push((left, given));
            }
            let kept = c.picks_kept.iter().position(|(key, _)| *key == target);
            let picks = match kept {
                Some(at) => c.picks_kept.remove(at).1,
                None => crate::model_picks::load_in(&self.litellm_root, &conn, agent),
            };
            c.models = picks.slots;
            c.disabled = picks.disabled;
            c.model_pick = None;
            c.picks_for = Some(target);
        }
    }

    /// Esc and Back: one step back with every answer kept; out of the wizard from step 1.
    fn wizard_back(&mut self) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        match form.prev_step(form.wizard_step(), &catalog) {
            Some(prev) => form.wizard_set_step(&catalog, prev),
            None => self.close_wizard(true),
        }
    }

    /// A click on the trail, or on an answer of the review.
    fn wizard_goto(&mut self, step: CreateStep) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        // Past the Connection step, a new connection's key is stored first, as Next does.
        let new_conn = form.litellm_backend(&catalog) && form.connection.litellm.is_none();
        let connection = CreateStep::Connection.number();
        let from = form.wizard_step();
        if new_conn && from.number() <= connection && step.number() > connection {
            if let Err(problem) = form.wizard_goto(&catalog, CreateStep::Connection) {
                self.set_error(problem);
                return;
            }
            if self.store_new_connection_first() {
                return;
            }
            let Screen::Create(form) = &mut self.screen else {
                return;
            };
            if let Err(problem) = form.wizard_goto(&catalog, step) {
                self.set_error(problem);
            }
            return;
        }
        match form.wizard_goto(&catalog, step) {
            Ok(()) => self.fetch_models_when_stale(from),
            Err(problem) => self.set_error(problem),
        }
    }

    /// Create & start, Create only and Ctrl-S: the create, from any step. A refusal over a step's
    /// answers moves the wizard to that step first, next to the field it is about.
    fn wizard_create(&mut self, attach: bool) {
        let catalog = Arc::clone(&self.catalog);
        // The policy refusal comes first in `submit_create` and belongs to no step.
        if self.policy_needs_init != Some(true) {
            if let Screen::Create(form) = &mut self.screen {
                if let Some(step) = form.owning_step(&catalog) {
                    form.wizard_set_step(&catalog, step);
                    form.wizard.refused = true;
                }
            }
        }
        self.submit_create(attach);
    }

    /// The auth editor for one existing sandbox: same two choices as the create form, but what
    /// applying them costs depends on what the container was built with.
    fn auth_key(&mut self, key: KeyEvent) {
        let Screen::AuthEdit(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let row = rows[form.focus.min(rows.len() - 1)];

        let field = match row {
            AuthRow::Name => Some(&mut form.token_name),
            AuthRow::Value => Some(&mut form.token),
            _ => None,
        };
        if let Some(field) = field {
            if edit_text(field, key) {
                return;
            }
        }

        match key.code {
            KeyCode::Esc => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab => {
                form.focus = step_form(form.focus, rows.len(), AuthForm::BUTTONS, -1)
            }
            KeyCode::Down | KeyCode::Tab => {
                form.focus = step_form(form.focus, rows.len(), AuthForm::BUTTONS, 1)
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(next) = step_button(form.focus, rows.len(), AuthForm::BUTTONS, delta) {
                    form.focus = next;
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => match row {
                AuthRow::Oauth => form.account = AccountChoice::Login,
                AuthRow::UseToken => {
                    let opened = !form.account.on_token();
                    // Back to what the sandbox already is, so opening the section is no change.
                    form.account = match (&form.current, form.tokens.first()) {
                        (Auth::Token { name, .. }, _) => AccountChoice::Named(name.clone()),
                        (_, Some(first)) => AccountChoice::Named(first.clone()),
                        (_, None) => AccountChoice::New,
                    };
                    if opened && key.code == KeyCode::Enter {
                        form.focus += 1;
                    }
                }
                AuthRow::TokenPick(i) => {
                    form.account = AccountChoice::Named(form.tokens[i].clone())
                }
                AuthRow::TokenNew => {
                    let opened = form.choose_new();
                    if opened && key.code == KeyCode::Enter {
                        form.focus += 1;
                    }
                }
                AuthRow::Name | AuthRow::Value => form.focus = (form.focus + 1) % rows.len(),
                AuthRow::Apply => self.submit_auth(),
                AuthRow::Cancel => self.screen = Screen::List,
            },
            _ => {}
        }
    }

    fn kits_key(&mut self, key: KeyEvent) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Kits(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows(&catalog);
        let row = rows[form.focus.min(rows.len() - 1)];
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = step_form(form.focus, rows.len(), KitsForm::BUTTONS, -1)
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = step_form(form.focus, rows.len(), KitsForm::BUTTONS, 1)
            }
            // `h`/`l` too, since this form has no text field to want them and binds `j`/`k`.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                let delta = match key.code {
                    KeyCode::Left | KeyCode::Char('h') => -1,
                    _ => 1,
                };
                if let Some(next) = step_button(form.focus, rows.len(), KitsForm::BUTTONS, delta) {
                    form.focus = next;
                }
            }
            KeyCode::Char(' ') => match row {
                KitsRow::Addon(i) => form.addons[i] = !form.addons[i],
                KitsRow::Model(slot) => {
                    toggle_model_pick(&mut form.model_pick, slot);
                }
                KitsRow::ModelDefault(slot) => {
                    choose_model(&mut form.models, &mut form.model_pick, slot, None)
                }
                KitsRow::ModelPick(slot, i) => choose_model(
                    &mut form.models,
                    &mut form.model_pick,
                    slot,
                    catalog.models.get(i),
                ),
                _ => {}
            },
            KeyCode::Enter => match row {
                KitsRow::Addon(i) => form.addons[i] = !form.addons[i],
                KitsRow::Model(slot) => {
                    if toggle_model_pick(&mut form.model_pick, slot) {
                        form.focus += 1;
                    }
                }
                KitsRow::ModelDefault(slot) => {
                    choose_model(&mut form.models, &mut form.model_pick, slot, None)
                }
                KitsRow::ModelPick(slot, i) => choose_model(
                    &mut form.models,
                    &mut form.model_pick,
                    slot,
                    catalog.models.get(i),
                ),
                KitsRow::Apply => self.submit_kits(),
                KitsRow::Cancel => self.screen = Screen::List,
            },
            _ => {}
        }
    }

    fn modal_key(&mut self, key: KeyEvent) {
        match self.modal.as_mut().unwrap() {
            Modal::Help => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.help_scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.help_scroll += 5,
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(5),
                KeyCode::Home => self.help_scroll = 0,
                _ => {
                    self.modal = None;
                    self.help_scroll = 0;
                }
            },
            Modal::Confirm(confirm) => match key.code {
                KeyCode::Esc | KeyCode::Char('n') => self.confirm_answer(false),
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => confirm.yes = !confirm.yes,
                KeyCode::Char('y') => self.confirm_answer(true),
                KeyCode::Enter => {
                    let yes = confirm.yes;
                    self.confirm_answer(yes);
                }
                _ => {}
            },
            Modal::Prompt(form) => match key.code {
                KeyCode::Esc => self.prompt_cancel(),
                KeyCode::Enter => self.prompt_submit(),
                KeyCode::Backspace => form.input.backspace(),
                KeyCode::Delete => form.input.delete(),
                KeyCode::Left => form.input.left(),
                KeyCode::Right => form.input.right(),
                KeyCode::Home => form.input.home(),
                KeyCode::End => form.input.end(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.input.insert(c)
                }
                _ => {}
            },
            Modal::Running(job, pending) => {
                let finished = job.is_finished();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        job.follow = false;
                        job.scroll = job.scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        job.follow = false;
                        job.scroll = job.scroll.saturating_add(1);
                    }
                    KeyCode::PageUp => {
                        job.follow = false;
                        job.scroll = job.scroll.saturating_sub(10);
                    }
                    KeyCode::PageDown => {
                        job.follow = false;
                        job.scroll = job.scroll.saturating_add(10);
                    }
                    KeyCode::End => job.follow = true,
                    KeyCode::Char('r') if finished && pending.fallback.is_some() => {
                        self.job_fallback()
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.job_close(),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.job_close()
                    }
                    _ => {}
                }
            }
        }
    }

    /// Answer the open confirmation: yes runs its job, no closes it.
    pub fn confirm_answer(&mut self, yes: bool) {
        let Some(Modal::Confirm(confirm)) = &self.modal else {
            return;
        };
        if yes {
            let job = confirm.job.clone();
            self.start_job(job);
        } else {
            self.modal = None;
        }
    }

    /// Submit the open prompt's input, as typed into [`Prompt::input`].
    pub fn prompt_submit(&mut self) {
        let Some(Modal::Prompt(form)) = self.modal.take() else {
            return;
        };
        self.submit_prompt(&form);
    }

    /// Close the open prompt; nothing is submitted.
    pub fn prompt_cancel(&mut self) {
        if matches!(self.modal, Some(Modal::Prompt(_))) {
            self.modal = None;
        }
    }

    /// Close a finished job that did not succeed, bringing back the form it restores. A job that
    /// succeeded is left to `tick`, which also runs what follows it.
    pub fn job_close(&mut self) {
        let Some(Modal::Running(job, _)) = &self.modal else {
            return;
        };
        if !job.is_finished() {
            // Leaving would close the job's output pipe under a running step.
            self.set_info(JOB_STILL_RUNNING);
            return;
        }
        let Some(Modal::Running(job, pending)) = &mut self.modal else {
            return;
        };
        if matches!(job.status(), Status::Ok) {
            return;
        }
        let restore = match job.status() {
            Status::Failed(_) => pending.post.restore.take(),
            _ => None,
        };
        self.modal = None;
        if let Some(restore) = restore {
            self.restore_screen(restore);
        }
        self.refresh();
    }

    /// Run a finished job's fallback (the rebuild offered when `sbx kit add` was refused).
    pub fn job_fallback(&mut self) {
        let Some(Modal::Running(job, pending)) = &self.modal else {
            return;
        };
        if !job.is_finished() {
            return;
        }
        if let Some(fallback) = pending.fallback.clone() {
            self.start_job(*fallback);
        }
    }

    // --- actions ---------------------------------------------------------------------------

    /// Can `action` run now? `Err` says how the list screen answers it instead. A body may still
    /// refuse after reading more (a preview, a blocker).
    pub fn can(&self, action: Action) -> Result<(), Refusal> {
        let new_card =
            || Refusal::Error("the cursor is on + New sandbox — press ↵ to create one".into());
        let workspace = |sb: &Sandbox| match sb.workspace_missing {
            true => Err(Refusal::Error(format!(
                "the folder {} no longer exists on this computer",
                model::tilde(&sb.workspace)
            ))),
            false => Ok(()),
        };
        let selected = self.selected();
        match action {
            Action::New
            | Action::Secrets
            | Action::Skills
            | Action::Folders
            | Action::Settings
            | Action::Approvals
            | Action::Refresh
            | Action::Help => Ok(()),
            Action::ToggleOther => match self.elsewhere {
                0 => Err(Refusal::Info(
                    "No other folder has a sandbox to show or hide".into(),
                )),
                _ => Ok(()),
            },
            Action::Start | Action::Shell | Action::Connect => {
                workspace(selected.ok_or_else(new_card)?)
            }
            Action::StartDir => {
                let sb = selected.ok_or_else(new_card)?;
                workspace(sb)?;
                if !sb.can_pick_start_dir() {
                    return Err(Refusal::Error(format!(
                        "no start folder for {} — it mounts no host folder to open a subfolder of",
                        sb.name
                    )));
                }
                Ok(())
            }
            Action::RepoSkills => selected.map(|_| ()).ok_or_else(|| {
                Refusal::Error("the cursor is on + New sandbox — press i on a sandbox".into())
            }),
            Action::RefreshModels => {
                let sb = selected.ok_or_else(new_card)?;
                match self.sandbox_connection(&sb.name) {
                    Some(_) => Ok(()),
                    None => Err(Refusal::Info(format!(
                        "{} is on no LiteLLM connection — only those have a model list",
                        sb.label
                    ))),
                }
            }
            Action::KeepRunning => {
                let sb = selected.ok_or(Refusal::Silent)?;
                workspace(sb)?;
                if sb.keep_running && !sb.is_managed() {
                    return Err(Refusal::Info(format!(
                        "{} keeps running until stopped (s) — only a rebuild undoes that, and \
                         only sandboxes from this repo's kits can be rebuilt here",
                        sb.label
                    )));
                }
                Ok(())
            }
            // Gated on the snapshot, not the preview: no subprocess decides if the key works.
            Action::Prune => match self.can_prune() {
                true => Ok(()),
                false => Err(Refusal::Info(
                    "Nothing to prune — every sandbox on this host is running".into(),
                )),
            },
            Action::EditKits => match selected.ok_or(Refusal::Silent)?.is_managed() {
                true => Ok(()),
                false => Err(Refusal::Error(
                    "kits can only be edited on sandboxes created from this repo's kits".into(),
                )),
            },
            // Nothing to say: the footer does not offer `A` where there is no account to edit.
            Action::Auth => match self.can_edit_auth() {
                true => Ok(()),
                false => Err(Refusal::Silent),
            },
            Action::Rebuild => {
                let sb = selected.ok_or(Refusal::Silent)?;
                match sb.is_managed() {
                    true => Ok(()),
                    false => Err(Refusal::Error(
                        "only sandboxes created from this repo's kits can be rebuilt".into(),
                    )),
                }
            }
            Action::Stop => match selected.ok_or(Refusal::Silent)? {
                sb if !sb.running => Err(Refusal::Info(format!("{} is already stopped", sb.label))),
                _ => Ok(()),
            },
            Action::Rename
            | Action::Delete
            | Action::PublishPort
            | Action::UnpublishPort
            | Action::CheckHost => selected.map(|_| ()).ok_or(Refusal::Silent),
        }
    }

    /// Whether [`Self::can`] lets `action` run now; a refusal is said the way it says it — on the
    /// status line, or not at all.
    fn cleared(&mut self, action: Action) -> bool {
        match self.can(action) {
            Ok(()) => true,
            Err(Refusal::Silent) => false,
            Err(Refusal::Info(text)) => {
                self.set_info(text);
                false
            }
            Err(Refusal::Error(text)) => {
                self.set_error(text);
                false
            }
        }
    }

    /// Run `action` as its key on the list screen does: refused as [`App::can`] says, else its
    /// body. ↵ on the "+ New sandbox" row — a real list cell — opens the create form.
    pub fn perform(&mut self, action: Action) {
        if action == Action::Start && self.on_new_card() {
            self.open_new_form();
            return;
        }
        if !self.cleared(action) {
            return;
        }
        match action {
            Action::Start => self.action_start(),
            Action::Shell => self.action_shell(),
            Action::New => self.open_new_form(),
            Action::EditKits => self.action_edit_kits(),
            Action::Auth => self.action_edit_auth(),
            Action::Secrets => self.screen = Screen::Secrets(SecretsForm::default()),
            Action::Connect => self.action_connect(),
            Action::StartDir => self.action_start_dir(),
            Action::Skills => self.screen = Screen::Skills(SkillsForm::default()),
            Action::Folders => self.open_folders(None),
            Action::Settings => self.open_settings(),
            Action::RepoSkills => self.action_sandbox_repo_skills(),
            Action::Approvals => self.action_approvals(),
            Action::KeepRunning => self.action_keep_running(),
            Action::RefreshModels => self.action_refresh_models(),
            Action::Prune => self.action_prune(),
            Action::Refresh => self.refresh(),
            Action::Rename => self.action_rename(),
            Action::Rebuild => self.action_recreate(),
            Action::Stop => self.action_stop(),
            Action::Delete => self.action_delete(),
            Action::PublishPort => self.action_publish_port(),
            Action::UnpublishPort => self.action_unpublish_port(),
            Action::CheckHost => self.action_check_host(),
            Action::ToggleOther => self.toggle_other_folders(),
            Action::Help => self.modal = Some(Modal::Help),
        }
    }

    /// Open a prepared session in a terminal window: the GUI's end of a [`Handoff`], once its
    /// [`terminal::Launch`] has run the pre-start steps. Returns `Err(NoTerminal)` for the
    /// frontend to explain; every other failure is said on the status line.
    pub fn open_session(&mut self, launch: terminal::Launch) -> Result<(), terminal::LaunchError> {
        let env = terminal::TermEnv::from_process();
        self.open_session_in(launch, terminal::Os::current(), &env)
    }

    fn open_session_in(
        &mut self,
        launch: terminal::Launch,
        os: terminal::Os,
        env: &terminal::TermEnv,
    ) -> Result<(), terminal::LaunchError> {
        let handoff = launch.handoff();
        let name = handoff.name().to_string();
        let label = self.sandbox_label(&name);
        // A config file opens in the desktop's text editor, unless the user named an editor of
        // their own, which wants a terminal.
        if let (Handoff::Edit { file, path }, None) = (handoff, &env.editor) {
            let argv = terminal::open_file_argv(os, path);
            match terminal::start_detached(os, &argv, env, self.notice_tx.clone()) {
                Ok(Ok(())) => self.set_info(format!(
                    "{file} opened in the text editor — save it there; the next start uses it"
                )),
                Ok(Err(e)) => self.set_error(format!("could not open {}: {e}", path.display())),
                Err(e) => self.set_error(e.to_string()),
            }
            return Ok(());
        }
        // An editor's launcher returns at once: started directly, with no terminal window.
        if let Handoff::Connect {
            argv,
            terminal: false,
            ..
        } = handoff
        {
            let program = argv.first().cloned().unwrap_or_default();
            match terminal::start_detached(os, argv, env, self.notice_tx.clone()) {
                Ok(Ok(())) => self.set_info(format!("{label}: `{program}` started")),
                Ok(Err(e)) => self.set_error(format!("could not start `{program}`: {e}")),
                Err(e) => self.set_error(e.to_string()),
            }
            self.refresh();
            return Ok(());
        }
        let result = match terminal::launch_argv(os, &handoff.argv(), env) {
            Ok(argv) => {
                match terminal::open(&argv, self.notice_tx.clone()) {
                    Ok(()) => self.set_info(format!(
                        "{label}: {} opened in a terminal window",
                        handoff.what()
                    )),
                    Err(e) => self.set_error(format!("could not start `{}`: {e}", argv[0])),
                }
                Ok(())
            }
            Err(terminal::LaunchError::NoTerminal) => Err(terminal::LaunchError::NoTerminal),
            Err(e) => {
                self.set_error(e.to_string());
                Ok(())
            }
        };
        self.refresh();
        result
    }

    fn action_start(&mut self) {
        let Some(sb) = self.selected() else { return };
        // `sbx exec` refuses a missing folder only after the terminal handover, so check it here.
        let gone = sb.start_dir_missing().then(|| sb.start_label());
        let (name, agent) = (sb.name.clone(), sb.agent.clone());
        if let Some(gone) = gone {
            self.set_info(format!(
                "{gone} is gone — starting at the top of the workspace instead (w to pick another)"
            ));
        }
        self.handoff = Some(self.agent_handoff(&name, &agent));
    }

    /// The handoff that starts a sandbox's agent, after the label step, the skill sources' steps
    /// its addons switch on, the model-list refresh and the agent update, where they apply.
    fn agent_handoff(&self, name: &str, agent: &str) -> Handoff {
        // Before anything starts it: a sandbox built before aisbx wrote bindings gets them here.
        self.approve_credentials(&self.sandbox_kit_dirs(name));
        // Same fallback as the view model: an unlabelled sandbox is its own sbx name.
        let label = self.registry.display_name(name).unwrap_or(name);
        let mut prelude = vec![sbx::label_argv(name, label)];
        // Not skipped by `--no-update`: local work that the agent's first skill scan needs.
        let addons = self
            .sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.addons.as_slice())
            .unwrap_or_default();
        prelude.extend(skill_source::install_steps(name, addons));
        // Not skipped by `--no-update` either: the host's list and picks, written locally.
        prelude.extend(self.config_push(name));
        prelude.extend(
            self.auto_update
                .then(|| crate::kits::agent_update(agent))
                .flatten()
                .map(|cmd| sbx::exec_argv(name, cmd)),
        );
        let start = self
            .sandboxes
            .iter()
            .find(|s| s.name == name)
            .filter(|s| !s.start_dir_missing())
            .map(|s| s.start_path())
            .filter(|dir| *dir != self.workspace_of(name));
        // No trust step for the start folder: Claude Code trusts a folder below the trusted root.
        Handoff::Agent {
            name: name.to_string(),
            start,
            prelude,
        }
    }

    /// Approve, in sbx's bindings file, the credentials these kits declare ([`crate::bindings`]).
    /// Reported on the status line; never a reason to stop.
    fn approve_credentials(&self, dirs: &[PathBuf]) {
        if let Some(note) = bindings::note(&bindings::ensure(dirs)) {
            let _ = self.notice_tx.send(note);
        }
    }

    /// This repo's kit dirs for the backend and addons a sandbox was built from.
    fn sandbox_kit_dirs(&self, name: &str) -> Vec<PathBuf> {
        let Some(sb) = self.sandboxes.iter().find(|s| s.name == name) else {
            return Vec::new();
        };
        let backend = sb
            .backend
            .as_deref()
            .and_then(|b| self.catalog.backend(b))
            .map(|b| b.kit.dir.clone());
        let addons = sb
            .addons
            .iter()
            .filter_map(|a| self.catalog.addon(a))
            .map(|a| a.kit.dir.clone());
        backend.into_iter().chain(addons).collect()
    }

    /// The LiteLLM connection a sandbox is built on: the one whose generated kit it carries, while
    /// its `connection.json` exists. `None` for any other sandbox.
    fn sandbox_connection(&self, name: &str) -> Option<crate::litellm::Connection> {
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        crate::litellm::load_in(&self.litellm_root, sb.litellm.as_deref()?)
    }

    /// The pre-start step that writes the connection's model list and picks into the agent's
    /// config ([`crate::litellm::agent_files_in`]): OpenCode's config, Pi's two files. `None` for
    /// Claude Code, whose pins are in its kit, and for a sandbox on no connection.
    fn config_push(&self, name: &str) -> Option<Vec<String>> {
        let conn = self.sandbox_connection(name)?;
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        let agent = sb
            .backend
            .as_deref()
            .and_then(crate::litellm::Agent::of_backend)?;
        crate::litellm::push_argv_in(&self.litellm_root, name, &conn, agent)
    }

    /// `L`: fetch the model list of the selected sandbox's connection now.
    fn action_refresh_models(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        let Some(conn) = self.sandbox_connection(&name) else {
            return;
        };
        self.start_fetch(&conn, None);
    }

    /// The job that fetches a connection's model list through its fetcher sandbox, creating the
    /// fetcher first where it does not exist. `None` when its kit cannot be written.
    pub fn fetch_job(&mut self, conn: &crate::litellm::Connection) -> Option<PendingJob> {
        let kit = match crate::model_fetch::ensure_kit_in(&self.litellm_root, conn) {
            Ok(kit) => kit,
            Err(e) => {
                self.set_error(format!("{e:#}"));
                return None;
            }
        };
        Some(self.fetch_job_at(conn, &kit))
    }

    /// [`App::fetch_job`] with the fetcher's kit at `kit`, written or not yet.
    fn fetch_job_at(&self, conn: &crate::litellm::Connection, kit: &Path) -> PendingJob {
        let fetcher = crate::model_fetch::sandbox_name(&conn.name);
        let exists = self.fetchers.contains(&fetcher);
        PendingJob {
            title: format!("Fetching the models of {}", conn.name),
            steps: crate::model_fetch::steps(conn, kit, exists),
            post: PostJob {
                harvest_models: Some((fetcher, conn.name.clone())),
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        }
    }

    /// Fetch a connection's model list now, as a job; `resume` comes back once it succeeded.
    pub fn start_fetch(&mut self, conn: &crate::litellm::Connection, resume: Option<Restore>) {
        let Some(mut job) = self.fetch_job(conn) else {
            return;
        };
        self.mark_fetched(&conn.name);
        job.post.resume = resume;
        self.start_job(job);
    }

    /// Keep a connection's model picks for `agent` ([`crate::model_picks`]). Not under `cargo
    /// test`, whose connections folder every test shares.
    /// The config files the create form's answers make for its connection and agent, with the
    /// connection and the agent: what the Models step's file rows open.
    pub fn wizard_config_files(
        &self,
    ) -> Option<(
        crate::litellm::Connection,
        crate::litellm::Agent,
        Vec<crate::litellm::ConfigFile>,
    )> {
        let Screen::Create(form) = &self.screen else {
            return None;
        };
        let agent = form.models_agent(&self.catalog)?;
        let conn = form.litellm_choice()?.clone();
        let offer =
            crate::litellm::Offer::of(&self.catalog.models, &form.picks(&self.catalog), agent);
        let files = crate::litellm::config_files(&conn, agent, &offer).ok()?;
        Some((conn, agent, files))
    }

    /// The Models step's file row `i`: the file's name and whether the user edited it.
    pub fn config_file_row(&self, i: usize) -> Option<(String, crate::litellm::FileState)> {
        let Screen::Create(form) = &self.screen else {
            return None;
        };
        self.config_file_row_of(form, i)
    }

    fn config_file_row_of(
        &self,
        form: &CreateForm,
        i: usize,
    ) -> Option<(String, crate::litellm::FileState)> {
        let agent = form.models_agent(&self.catalog)?;
        let conn = form.connection.litellm.as_deref()?;
        let file = crate::litellm::config_file_names(conn, agent)
            .into_iter()
            .nth(i)?;
        let state = crate::litellm::file_state(&self.litellm_root, conn, agent, &file);
        Some((file, state))
    }

    /// What a file row means, worded for its state.
    pub fn config_file_help(&self, form: &CreateForm, i: usize) -> crate::create::Help {
        use crate::litellm::{Agent, FileState};
        let Some((file, state)) = self.config_file_row_of(form, i) else {
            return crate::create::Help::new("", "");
        };
        let agent = form.models_agent(&self.catalog);
        let conn = form.connection.litellm.clone().unwrap_or_default();
        let what = match (agent, i) {
            (Some(Agent::Claude), _) => "Claude Code's model picker, in its settings.json",
            (Some(Agent::Opencode), _) => "OpenCode's config",
            (Some(Agent::Pi), 0) => "Pi's model list",
            _ => "Pi's settings",
        };
        let harness = form.harness_label(&self.catalog);
        let now = match state {
            FileState::Generated => format!(
                "Made from your answers. A change you save goes to every {harness} sandbox on \
                 {conn}, from its next start."
            ),
            FileState::Edited | FileState::Invalid(_) => format!(
                "Your copy: every {harness} sandbox on {conn} gets it from its next start, and \
                 your answers no longer change it. Reset goes back to the file made from them."
            ),
        };
        let warn = match state {
            FileState::Invalid(e) => Some(format!(
                "Your copy is not valid JSON ({e}), so the file made from your answers is used."
            )),
            _ => None,
        };
        crate::create::Help::new(file, format!("{what}. {now}")).warn(warn)
    }

    /// ↵ on a file row: write the file's copy, if the user has not edited it yet, and hand it to
    /// an editor.
    fn edit_config(&mut self, i: usize) {
        let Some((conn, agent, files)) = self.wizard_config_files() else {
            return;
        };
        let Some(file) = files.get(i) else { return };
        match crate::litellm::prepare_edit(&self.litellm_root, &conn.name, agent, file) {
            Ok(path) => {
                self.handoff = Some(Handoff::Edit {
                    file: file.file_name().to_string(),
                    path,
                })
            }
            Err(e) => self.set_error(format!("{e:#}")),
        }
    }

    /// Delete on a file row: drop the user's copy.
    fn reset_config(&mut self, i: usize) {
        let Some((conn, agent, files)) = self.wizard_config_files() else {
            return;
        };
        let Some(file) = files.get(i) else { return };
        let name = file.file_name().to_string();
        match crate::litellm::reset_edit(&self.litellm_root, &conn.name, agent, &name) {
            Ok(()) => self.set_info(format!("{name}: back to the generated file")),
            Err(e) => self.set_error(format!("{e:#}")),
        }
    }

    /// The status line after the editor on `file` closed: what the file is now. `true` for an
    /// error.
    pub fn edit_ended(&self, file: &str) -> (String, bool) {
        use crate::litellm::FileState;
        let state = (0..3)
            .filter_map(|i| self.config_file_row(i))
            .find(|(name, _)| name == file)
            .map(|(_, state)| state);
        match state {
            Some(FileState::Edited) => (
                format!("{file}: your copy is used from each sandbox's next start"),
                false,
            ),
            Some(FileState::Invalid(e)) => (
                format!("{file} is not valid JSON ({e}) — the generated file is used"),
                true,
            ),
            _ => (
                format!("{file}: unchanged, the generated file is used"),
                false,
            ),
        }
    }

    fn save_picks(
        &self,
        conn: &str,
        agent: crate::litellm::Agent,
        picks: &crate::model_picks::Picks,
    ) {
        if cfg!(any(test, feature = "test-support")) {
            return;
        }
        if let Err(e) = crate::model_picks::save_in(&self.litellm_root, conn, agent, picks) {
            let _ = self.notice_tx.send((true, format!("{e:#}")));
        }
    }

    /// Record that a connection's list was asked for now. Not under `cargo test`.
    fn mark_fetched(&self, conn: &str) {
        if cfg!(any(test, feature = "test-support")) {
            return;
        }
        let now = crate::state::now_epoch();
        let _ = crate::model_fetch::mark_in(&self.litellm_root, conn, now);
    }

    /// At start, once the first snapshot is in: fetch, in the background, the list of every
    /// connection whose key is stored and which was not asked for in the last day. Nothing runs
    /// under `cargo test`.
    fn start_due_fetches(&mut self) {
        if self.due_fetches_done || self.secrets.is_none() {
            return;
        }
        self.due_fetches_done = true;
        let now = crate::state::now_epoch();
        let due: Vec<crate::litellm::Connection> = crate::litellm::load_all_in(&self.litellm_root)
            .into_iter()
            .filter(|c| !self.litellm_key_missing(&c.name))
            .filter(|c| crate::model_fetch::due_in(&self.litellm_root, &c.name, now))
            .collect();
        for conn in due {
            let Some(job) = self.fetch_job(&conn) else {
                continue;
            };
            self.mark_fetched(&conn.name);
            if cfg!(any(test, feature = "test-support")) {
                self.background_fetches.push(conn.name.clone());
                continue;
            }
            let tx = self.fetch_tx.clone();
            let fetcher = crate::model_fetch::sandbox_name(&conn.name);
            let name = conn.name.clone();
            std::thread::spawn(move || {
                let done = job.steps.iter().all(crate::job::run_quiet);
                let text = done.then(|| sbx::read_models_cache(&fetcher)).flatten();
                let _ = tx.send((name, fetcher, text));
            });
        }
    }

    /// Keep the lists the background fetches read: as [`App::harvest_models`] does, quietly.
    fn tick_fetches(&mut self) {
        while let Ok((conn, fetcher, text)) = self.fetch_rx.try_recv() {
            let Some(text) = text else {
                let note = format!("{conn}: the model list could not be fetched — L tries again");
                self.set_error(note);
                continue;
            };
            let now = crate::state::now_epoch() as i64;
            match self.store_models_harvest(&conn, &fetcher, &text, now) {
                Ok(true) => self.set_info(format!("{conn}: the model list is up to date")),
                Ok(false) => {}
                Err(e) => self.set_error(e),
            }
        }
    }

    /// Read a fetcher's model-list cache onto the host, under its connection `conn`, and into the
    /// catalog while it offers that connection's models. `Ok(true)`: a newer list was stored;
    /// `Ok(false)`: not newer, nothing changed; `Err`: nothing changed, and why. Never under
    /// `cargo test`.
    pub fn harvest_models(&mut self, sandbox: &str, conn: &str) -> Result<bool, String> {
        if cfg!(any(test, feature = "test-support")) {
            return Ok(false);
        }
        let text = sbx::read_models_cache(sandbox)
            .ok_or_else(|| format!("no model list to read from {sandbox}"))?;
        self.store_models_harvest(conn, sandbox, &text, crate::state::now_epoch() as i64)
    }

    /// [`App::harvest_models`] past the `sbx exec`: validate `text` (a fetcher's cache), store it
    /// under `conn` only when it is newer, and then show it.
    fn store_models_harvest(
        &mut self,
        conn: &str,
        sandbox: &str,
        text: &str,
        now: i64,
    ) -> Result<bool, String> {
        let path = self.litellm_root.join(conn).join("models.json");
        let harvest = crate::models::parse_sandbox_cache(text, now)
            .ok_or_else(|| format!("the model list from {sandbox} did not pass validation"))?;
        let stored = crate::models::store_harvest(&path, sandbox, &harvest, now)
            .map_err(|e| format!("could not keep the model list from {sandbox}: {e:#}"))?;
        if stored && self.models_conn.as_deref() == Some(conn) {
            let catalog = Arc::make_mut(&mut self.catalog);
            let named = std::mem::take(&mut catalog.models);
            catalog.models = crate::models::named_from(harvest.models, &named);
            catalog.models_source = crate::models::Source::Gateway {
                sandbox: sandbox.to_string(),
                fetched_at: harvest.fetched_at,
            };
        }
        Ok(stored)
    }

    /// The workspace root of a sandbox this dashboard has in its snapshot.
    fn workspace_of(&self, name: &str) -> PathBuf {
        self.sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.workspace.clone())
            .unwrap_or_default()
    }

    /// Open a shell inside the sandbox instead of its agent — the equivalent of `sbx exec`.
    fn action_shell(&mut self) {
        let Some(sb) = self.selected() else { return };
        let (name, label, running) = (sb.name.clone(), sb.label.clone(), sb.running);
        // A shell opens where the agent does, minus a missing start folder, which `exec` refuses.
        let workdir = (!sb.start_dir_missing() && sb.start_dir.is_some()).then(|| sb.start_path());
        if !running {
            self.set_info(format!("starting {label} for the shell"));
        }
        self.approve_credentials(&self.sandbox_kit_dirs(&name));
        self.handoff = Some(Handoff::Shell { name, workdir });
    }

    // --- connecting an editor or app over SSH ----------------------------------------------

    /// Is there a sandbox to connect an editor to? Every sandbox has an SSH endpoint, whatever it
    /// was built from — this is not one of the kit-managed features — so only the New row is out.
    pub fn can_connect(&self) -> bool {
        self.selected().is_some()
    }

    /// What the open Connect screen says; `None` when none is open or its sandbox is gone.
    /// Whether a launcher is installed is probed on PATH here ([`crate::remote::Target::available`]).
    pub fn connect_view(&self) -> Option<ConnectView> {
        self.connect_view_with(crate::remote::Target::available)
    }

    fn connect_view_with(
        &self,
        available: impl Fn(crate::remote::Target) -> bool,
    ) -> Option<ConnectView> {
        use crate::remote::{Target, REMOTE_DEV_KIT};
        let Screen::Connect(form) = &self.screen else {
            return None;
        };
        let sb = self.sandboxes.iter().find(|s| s.name == form.sandbox)?;
        let (workspace, missing) = (sb.remote_workspace(), !form.ssh_ready);
        // Warn only when this checkout has the kit that opens the server download's egress.
        let has_kit = sb.addons.iter().any(|a| a == REMOTE_DEV_KIT);
        let known_kit = self.catalog.addon(REMOTE_DEV_KIT).is_some();
        let targets = Target::ALL
            .iter()
            .enumerate()
            .map(|(i, target)| {
                let launch = match target.command(&sb.name, &workspace) {
                    None => TargetLaunch::InApp,
                    Some(_) if available(*target) => TargetLaunch::Here,
                    Some(_) => TargetLaunch::NotOnPath(target.launcher().unwrap_or("?")),
                };
                let status = match launch {
                    TargetLaunch::InApp => "set up in the app".to_string(),
                    TargetLaunch::NotOnPath(launcher) => format!("`{launcher}` command not found"),
                    TargetLaunch::Here => String::new(),
                };
                let kit_warning = match target.needs_kit() && !has_kit && known_kit {
                    true => vec![
                        format!(
                            "This sandbox has no {REMOTE_DEV_KIT} kit, so the editor cannot \
                             install its server."
                        ),
                        "Add the kit with Edit kits (e), then try again.".into(),
                    ],
                    false => Vec::new(),
                };
                TargetView {
                    row: ConnectRow::Target(i),
                    title: target.title(),
                    launch,
                    status,
                    steps: target.steps(&sb.name, &workspace),
                    kit_warning,
                }
            })
            .collect();
        Some(ConnectView {
            label: sb.label.clone(),
            host: sbx::ssh_host(&sb.name),
            state: match sb.running {
                true => "running",
                // Worth saying: unlike everything else here, connecting has a side effect.
                false => "stopped: connecting starts it",
            },
            ssh_missing: missing,
            ssh_warning: match missing {
                true => vec!["SSH is not set up yet, so no app can connect.".into()],
                false => Vec::new(),
            },
            ssh_notes: match missing {
                true => vec![
                    "Adds the sandboxes to your SSH settings (~/.ssh/config), once for all.".into(),
                    "You need no SSH key.".into(),
                ],
                false => Vec::new(),
            },
            targets,
        })
    }

    fn action_connect(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.screen = Screen::Connect(ConnectForm {
            sandbox: name,
            ssh_ready: self.ssh_ready,
            focus: 0,
        });
    }

    fn connect_key(&mut self, key: KeyEvent) {
        let Screen::Connect(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let row = rows[form.focus.min(rows.len() - 1)];
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab => {
                form.focus = (form.focus + rows.len() - 1) % rows.len()
            }
            KeyCode::Down | KeyCode::Tab => form.focus = (form.focus + 1) % rows.len(),
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                ConnectRow::SetupSsh => self.submit_setup_ssh(),
                ConnectRow::Target(i) => self.submit_connect(i),
                ConnectRow::Cancel => self.screen = Screen::List,
            },
            _ => {}
        }
    }

    /// Write the managed `Host *.sbx` block. Idempotent, so re-running it is never wrong — but it
    /// touches `~/.ssh/config`, which is worth saying before it does.
    fn submit_setup_ssh(&mut self) {
        self.start_job(PendingJob {
            title: "Setting up SSH access".into(),
            steps: vec![Step::new("sbx setup ssh", sbx::setup_ssh_argv())],
            post: PostJob::default(),
            fallback: None,
            fallback_hint: None,
        });
    }

    /// Hand the terminal to whatever connects this target — or say why nothing was run.
    fn submit_connect(&mut self, index: usize) {
        let Screen::Connect(form) = &self.screen else {
            return;
        };
        let Some(target) = crate::remote::Target::ALL.get(index).copied() else {
            return;
        };
        let name = form.sandbox.clone();
        if !form.ssh_ready {
            self.set_error("SSH is not set up yet: choose Set up SSH now first");
            return;
        }
        let Some(sb) = self.sandboxes.iter().find(|s| s.name == name) else {
            return;
        };
        let workspace = sb.remote_workspace();
        let Some(argv) = target.command(&name, &workspace) else {
            // Gateway and Claude Desktop are configured inside the app; the rows print how.
            self.set_info(format!(
                "Set up {} in the app itself, as its steps say",
                target.title()
            ));
            return;
        };
        if !target.available() {
            self.set_error(format!(
                "`{}` command not found, so aisbx cannot open {}",
                target.launcher().unwrap_or("the launcher"),
                target.title()
            ));
            return;
        }
        self.screen = Screen::List;
        self.handoff = Some(Handoff::Connect {
            name,
            what: "connection",
            argv,
            terminal: target.in_terminal(),
        });
    }

    // --- where the agent opens (the start folder) -------------------------------------------

    /// Is there a sandbox whose start folder can be chosen? It needs a host folder
    /// ([`Sandbox::can_pick_start_dir`]).
    pub fn can_pick_start_dir(&self) -> bool {
        self.selected().is_some_and(|sb| sb.can_pick_start_dir())
    }

    fn action_start_dir(&mut self) {
        let Some(sb) = self.selected() else { return };
        let rel = sb.start_dir.clone().filter(|_| !sb.start_dir_missing());
        let (name, label, root) = (sb.name.clone(), sb.label.clone(), sb.workspace.clone());
        self.screen = Screen::StartDir(StartDirForm::open(
            StartDirTarget::Sandbox(name),
            label,
            root,
            rel,
        ));
    }

    /// Open the same picker from the create form, which has no sandbox yet.
    fn action_create_start_dir(&mut self) {
        let Screen::Create(form) = &mut self.screen else {
            return;
        };
        let (rel, root) = (
            form.workspace.start_dir.clone(),
            form.workspace_root(&self.cwd),
        );
        let label = match form.workspace.name.value.trim() {
            "" => model::tilde(&root),
            typed => typed.to_string(),
        };
        // Replaced wholesale, so the form has to be moved out of the screen it is in.
        let Screen::Create(form) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        self.screen = Screen::StartDir(StartDirForm::open(
            StartDirTarget::New(Box::new(form)),
            label,
            root,
            rel,
        ));
    }

    /// Open the folder picker for one more host folder of the sandbox being created, on the
    /// folder that holds its workspace.
    fn action_pick_folder(&mut self) {
        let Screen::Create(form) = &self.screen else {
            return;
        };
        let workspace = form.workspace_root(&self.cwd);
        let start = (workspace.parent())
            .filter(|p| p.parent().is_some())
            .map(Path::to_path_buf)
            .or_else(model::home_dir)
            .unwrap_or_else(|| workspace.clone());
        let (root, rel) = host_root(&start);
        let Screen::Create(form) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        self.screen = Screen::StartDir(StartDirForm::open(
            StartDirTarget::Folder(Box::new(form)),
            "folder to mount".into(),
            root,
            Some(rel),
        ));
    }

    /// Add the picked folder to the sandbox being created, unless a create would refuse it; then
    /// the picker stays open, saying why.
    fn use_picked_folder(&mut self, here: PathBuf) {
        let real = std::fs::canonicalize(&here).unwrap_or_else(|_| here.clone());
        let refusal = model::home_dir().and_then(|home| model::mount_refusal(&real, &home));
        let Screen::StartDir(picker) = &self.screen else {
            return;
        };
        let StartDirTarget::Folder(create) = &picker.target else {
            return;
        };
        let workspace = create.workspace_root(&self.cwd);
        let inside = std::fs::canonicalize(&workspace).is_ok_and(|ws| real.starts_with(ws))
            || real.starts_with(&workspace);
        let why = match (refusal, inside) {
            (Some(why), _) => Some(why),
            (None, true) => {
                Some("it is in the project folder, which the sandbox has already".into())
            }
            (None, false) => None,
        };
        if let Some(why) = why {
            return self.set_error(format!("{}: {why}", model::tilde(&here)));
        }
        let Screen::StartDir(picker) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        let StartDirTarget::Folder(mut create) = picker.target else {
            return;
        };
        let at = create.add_folder(&real);
        let catalog = Arc::clone(&self.catalog);
        create.focus_wizard_row(&catalog, WizardRow::Field(CreateRow::Folder(at)));
        self.screen = Screen::Create(*create);
    }

    /// Leave the picker for wherever it was opened from, changing nothing.
    fn leave_start_dir(&mut self) {
        let Screen::StartDir(_) = &self.screen else {
            return;
        };
        let Screen::StartDir(form) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        match form.target {
            StartDirTarget::New(create) | StartDirTarget::Folder(create) => {
                self.screen = Screen::Create(*create)
            }
            StartDirTarget::HostFolder(folders) => self.screen = Screen::Folders(*folders),
            StartDirTarget::Sandbox(_) => {}
        }
    }

    fn start_dir_key(&mut self, key: KeyEvent) {
        let Screen::StartDir(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let buttons = form.buttons();
        let row = rows[form.focus.min(rows.len() - 1)];
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.leave_start_dir(),
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = step_form(form.focus, rows.len(), buttons, -1)
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = step_form(form.focus, rows.len(), buttons, 1)
            }
            KeyCode::Right | KeyCode::Char('l') => match row {
                StartDirRow::Dir(i) => form.descend(i),
                StartDirRow::Up => form.up(),
                _ => {
                    if let Some(next) = step_button(form.focus, rows.len(), buttons, 1) {
                        form.focus = next;
                    }
                }
            },
            KeyCode::Left | KeyCode::Char('h') => match row {
                // Left on any list row goes up, as in a file manager.
                StartDirRow::Dir(_) | StartDirRow::Up => form.up(),
                _ => {
                    if let Some(next) = step_button(form.focus, rows.len(), buttons, -1) {
                        form.focus = next;
                    }
                }
            },
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                StartDirRow::Up => form.up(),
                StartDirRow::Dir(i) => form.descend(i),
                StartDirRow::Start => self.submit_start_dir(true),
                StartDirRow::Use => self.submit_start_dir(false),
                StartDirRow::Cancel => self.leave_start_dir(),
            },
            _ => {}
        }
    }

    /// Store the browsed folder as where this sandbox's agent opens, and optionally start it.
    /// Storing is a registry edit only; it applies from the next start.
    fn submit_start_dir(&mut self, start: bool) {
        let Screen::StartDir(form) = &self.screen else {
            return;
        };
        let (chosen, here) = (form.chosen(), form.here());
        // Between opening the screen and pressing the button, something else may have removed it.
        if !here.is_dir() {
            self.set_error(format!("{} is no longer a directory", here.display()));
            return;
        }
        // For a sandbox not created yet, the form carries the choice; `submit_create` records it.
        let name = match &form.target {
            StartDirTarget::Sandbox(name) => name.clone(),
            StartDirTarget::HostFolder(_) => return self.use_host_folder(here),
            StartDirTarget::Folder(_) => return self.use_picked_folder(here),
            StartDirTarget::New(_) => {
                let Screen::StartDir(form) = std::mem::replace(&mut self.screen, Screen::List)
                else {
                    return;
                };
                let StartDirTarget::New(mut create) = form.target else {
                    return;
                };
                create.workspace.start_dir = chosen;
                create.workspace.root =
                    (!model::same_path(&form.root, &self.cwd)).then_some(form.root);
                self.refresh_form_folders(&mut create);
                self.screen = Screen::Create(*create);
                return;
            }
        };
        self.registry.set_start_dir(&name, chosen.as_deref());
        if let Err(err) = self.registry.save() {
            self.set_error(format!("could not save the start folder: {err}"));
            return;
        }
        // Mirror the write into the snapshot until the next refresh reads the registry.
        if let Some(sb) = self.sandboxes.iter_mut().find(|s| s.name == name) {
            sb.start_dir = chosen.clone();
        }
        self.screen = Screen::List;
        let label = self
            .sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|sb| sb.start_label())
            .unwrap_or_else(|| here.display().to_string());
        if !start {
            self.set_info(match chosen.is_some() {
                true => format!("{label} — where ↵ and x now open"),
                false => format!("{label} — back to the whole workspace"),
            });
            return;
        }
        let Some(sb) = self.sandboxes.iter().find(|s| s.name == name) else {
            return;
        };
        let agent = sb.agent.clone();
        self.handoff = Some(self.agent_handoff(&name, &agent));
    }

    // --- the host's shared agent-skills store ----------------------------------------------

    /// Copy the host's agent skills into the store every sharing sandbox mounts. Confirmed first.
    fn action_import_skills(&mut self) {
        let sources: Vec<String> = model::home_dir()
            .map(|home| {
                sbx::SKILL_SOURCES
                    .iter()
                    .filter(|rel| home.join(rel).is_dir())
                    .map(|rel| format!("~/{rel}"))
                    .collect()
            })
            .unwrap_or_default();
        if sources.is_empty() {
            self.set_info(
                "no agent skill directories on this host — nothing for `sbx skills import` to copy",
            );
            return;
        }
        let mut body = vec![
            format!("Copies the skills in {} into", join_names(&sources)),
            "the store sbx mounts into every sandbox that shares it.".into(),
            String::new(),
            self.skills_sharers_line(),
        ];
        body.push(String::new());
        body.push(
            "A skill already in the store is replaced whole, not merged — this runs with --force,"
                .into(),
        );
        body.push("so nothing stops to ask per skill.".into());
        body.push(String::new());
        body.extend(self.skills_trust_lines());
        if let Some(store) = self.skills_store_path() {
            body.push(String::new());
            body.push(format!("Store: {}", model::tilde(&store)));
            body.push("It outlives every sandbox; `sbx reset` is what clears it.".into());
        }
        self.modal = Some(Modal::Confirm(Confirm {
            title: "Import host skills into the shared store".into(),
            body,
            danger: false,
            confirm_label: "Import".into(),
            yes: false,
            job: PendingJob {
                title: "Importing agent skills".into(),
                steps: vec![Step::new(
                    "sbx skills import --force",
                    sbx::skills_import_argv(true),
                )],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// Every line on the skills screen, in the order it draws them: the store's skills, the
    /// actions that change it, and the Close button. `Update` only while the store holds a skill.
    pub fn skill_rows(&self) -> Vec<SkillRow> {
        let mut rows: Vec<SkillRow> = self
            .skills
            .iter()
            .flat_map(|s| s.skills.iter())
            .map(|name| SkillRow::Skill(name.clone()))
            .collect();
        let any = !rows.is_empty();
        rows.push(SkillRow::Import);
        rows.push(SkillRow::AddRepo);
        if any {
            rows.push(SkillRow::Update);
        }
        rows.push(SkillRow::RepoSkills);
        rows.push(SkillRow::Close);
        rows
    }

    /// How many sandboxes here mount the store, read-only ones included: who sees a change to it.
    /// Who can change it from the inside is [`Self::skills_writers`].
    pub fn skills_sharers(&self) -> usize {
        self.sandboxes
            .iter()
            .filter(|s| match s.skills {
                Some(model::Skills::Known(mode)) => mode != sbx::SkillsMode::Off,
                Some(model::Skills::Unrecorded) => true,
                None => false,
            })
            .count()
    }

    /// How many of those could write *back* into the store: the trust boundary. An unrecorded
    /// mount counts as writable.
    pub fn skills_writers(&self) -> usize {
        self.sandboxes
            .iter()
            .filter(|s| match s.skills {
                Some(model::Skills::Known(mode)) => mode.writable(),
                Some(model::Skills::Unrecorded) => true,
                None => false,
            })
            .count()
    }

    /// What write access to the store means here, in the two sentences a confirmation wants.
    pub fn skills_trust_lines(&self) -> Vec<String> {
        match self.skills_writers() {
            0 => vec!["None of them can change the skills: only this screen can.".into()],
            n => vec![format!(
                "{n} of them can change the skills, and every other sandbox gets their changes."
            )],
        }
    }

    /// Where the store is: what `sbx skills ls --json` reported, or the platform's usual place
    /// while that read has not answered.
    pub fn skills_store_path(&self) -> Option<PathBuf> {
        match self.skills.as_ref().filter(|s| !s.store.is_empty()) {
            Some(store) => Some(PathBuf::from(&store.store)),
            None => sbx::skills_store(),
        }
    }

    /// The heading a skills-screen row is listed under; `None` for the button.
    pub fn skill_heading(row: &SkillRow) -> Option<&'static str> {
        match row {
            SkillRow::Skill(_) => Some("Installed skills"),
            SkillRow::Import | SkillRow::AddRepo | SkillRow::Update => Some("Change the store"),
            SkillRow::RepoSkills => Some("Skills from configured skill repos"),
            SkillRow::Close => None,
        }
    }

    /// One skills-screen row: the skill or the command, and what it reaches for.
    pub fn skill_view(&self, row: &SkillRow) -> (String, String) {
        match row {
            SkillRow::Skill(name) => (name.clone(), String::new()),
            SkillRow::Import => ("Import this host's skills".to_string(), {
                let sources = sbx::host_skill_sources();
                match sources.is_empty() {
                    true => "no skill directories on this host".to_string(),
                    false => sources.join(", "),
                }
            }),
            SkillRow::AddRepo => (
                "Add from a Git repository".to_string(),
                "owner/repo or a Git URL".to_string(),
            ),
            SkillRow::Update => (
                "Update from repositories".to_string(),
                "the skills added from a repository".to_string(),
            ),
            SkillRow::RepoSkills => (
                "Repo skills for all sandboxes".to_string(),
                "picked from your skill repos".to_string(),
            ),
            SkillRow::Close => (String::new(), String::new()),
        }
    }

    /// What ↵, d and the letter keys do to the skills-screen row under the cursor.
    pub fn skill_notes(&self, row: &SkillRow) -> Vec<String> {
        // Whole sentences naming what acts, as the secrets' notes do.
        match row {
            SkillRow::Skill(name) => vec![format!(
                "Remove deletes {name} from the store; Update refreshes it from its repository, \
                 if it came from one (sbx does not report which). An imported skill is refreshed \
                 by importing again. Nothing else is remembered about it."
            )],
            SkillRow::Import => vec![
                "Runs `sbx skills import --force`: copies every skill in those directories into \
                 the store, replacing a skill of the same name whole. Asks first."
                    .into(),
            ],
            SkillRow::AddRepo => vec![
                "Asks for a repository, then runs `sbx skills add <repo> --force` — every SKILL.md \
                 in it, or only the names you list after it."
                    .into(),
            ],
            SkillRow::Update => vec![format!(
                "Runs `sbx skills update`: every skill added from a repository is fetched again \
                 and replaced. {} sandbox{} here would load the new versions.",
                self.skills_sharers(),
                if self.skills_sharers() == 1 { "" } else { "es" }
            )],
            SkillRow::RepoSkills => vec![
                "Picks skills of the repos `aisbx skill-repo` configured for every sandbox, \
                 existing and new; Repo skills (i) on a sandbox picks them for that sandbox alone."
                    .into(),
            ],
            SkillRow::Close => Vec::new(),
        }
    }

    fn skills_key(&mut self, key: KeyEvent) {
        let rows = self.skill_rows();
        let Screen::Skills(form) = &mut self.screen else {
            return;
        };
        form.focus = form.focus.min(rows.len() - 1);
        let row = rows[form.focus].clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = (form.focus + rows.len() - 1) % rows.len()
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = (form.focus + 1) % rows.len()
            }
            KeyCode::Home | KeyCode::Char('g') => form.focus = 0,
            KeyCode::End | KeyCode::Char('G') => form.focus = rows.len() - 1,
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                SkillRow::Close => self.screen = Screen::List,
                // A skill row has no action of its own — the two that apply are on their keys.
                SkillRow::Skill(name) => self.set_info(format!(
                    "{name}: d removes it; u refreshes it if it came from a repository"
                )),
                SkillRow::Import => self.action_import_skills(),
                SkillRow::AddRepo => self.ask(Ask::AddSkills, String::new(), String::new()),
                SkillRow::Update => self.action_update_skills(),
                SkillRow::RepoSkills => self.open_repo_skills(RepoScope::Global),
            },
            KeyCode::Char('d') | KeyCode::Delete => {
                if let SkillRow::Skill(name) = row {
                    self.delete_skill(&name);
                }
            }
            KeyCode::Char('i') => self.action_import_skills(),
            KeyCode::Char('a') => self.ask(Ask::AddSkills, String::new(), String::new()),
            KeyCode::Char('u') => self.action_update_skills(),
            KeyCode::F(5) => self.refresh(),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.refresh(),
            _ => {}
        }
    }

    // --- repo skills -----------------------------------------------------------------------

    /// Open the Repo skills screen. It reads the repo list, the caches and the selection file
    /// once, here — never `sbx` — and nothing at all under test.
    fn open_repo_skills(&mut self, scope: RepoScope) {
        let form = if cfg!(any(test, feature = "test-support")) {
            RepoSkillsForm::from_parts(scope, Vec::new(), Ok(skill_select::Selection::default()))
        } else {
            RepoSkillsForm::load(&crate::state::state_dir(), scope)
        };
        self.screen = Screen::RepoSkills(form);
    }

    /// `i` on a sandbox: the selected sandbox's own repo skills.
    fn action_sandbox_repo_skills(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.open_repo_skills(RepoScope::Sandbox(name));
    }

    /// Where Esc and *Close* go: the store screen the global scope was opened from, else the
    /// grid.
    fn close_repo_skills(&mut self) {
        let (global, dirty) = match &self.screen {
            Screen::RepoSkills(form) => (form.scope == RepoScope::Global, form.dirty),
            _ => return,
        };
        self.screen = if global {
            Screen::Skills(SkillsForm::default())
        } else {
            Screen::List
        };
        if dirty {
            self.set_info("closed without saving — nothing was changed");
        }
    }

    fn repo_skills_key(&mut self, key: KeyEvent) {
        let Screen::RepoSkills(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        form.focus = form.focus.min(rows.len() - 1);
        let row = rows[form.focus].clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_repo_skills(),
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = (form.focus + rows.len() - 1) % rows.len()
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = (form.focus + 1) % rows.len()
            }
            KeyCode::Home | KeyCode::Char('g') => form.focus = 0,
            KeyCode::End | KeyCode::Char('G') => form.focus = rows.len() - 1,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_repo_skills()
            }
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                RepoSkillRow::All(_) | RepoSkillRow::Skill { .. } => {
                    if let Err(why) = form.toggle(&row) {
                        self.set_info(why);
                    }
                }
                RepoSkillRow::SyncNow => self.action_sync_repo_skills(),
                RepoSkillRow::Save => self.save_repo_skills(),
                RepoSkillRow::Close => self.close_repo_skills(),
            },
            _ => {}
        }
    }

    /// *Save*: the ticks become the scope's whole list in one write. Nothing is written while
    /// the file cannot be read, or under test.
    fn save_repo_skills(&mut self) {
        let Screen::RepoSkills(form) = &mut self.screen else {
            return;
        };
        if let Some(why) = &form.error {
            let msg = format!("nothing saved: {why}");
            self.set_error(msg);
            return;
        }
        let picks = form.picks();
        let saved = if cfg!(any(test, feature = "test-support")) {
            Ok(())
        } else {
            skill_select::store(&crate::state::state_dir(), form.scope.as_scope(), picks)
        };
        match saved {
            Ok(()) => {
                form.dirty = false;
                let msg = match &form.scope {
                    RepoScope::Global => GLOBAL_SAVED.to_string(),
                    RepoScope::Sandbox(name) => format!(
                        "saved for {name} — applies at its next start, or now with Sync now"
                    ),
                };
                self.set_info(msg);
            }
            Err(skill_select::Error::Refused(why) | skill_select::Error::Failed(why)) => {
                self.set_error(format!("nothing saved: {why}"))
            }
        }
    }

    /// *Sync now*: `aisbx skill sync <sandbox>` as a job — the full sync of what is saved.
    fn action_sync_repo_skills(&mut self) {
        let Screen::RepoSkills(form) = &self.screen else {
            return;
        };
        let RepoScope::Sandbox(name) = &form.scope else {
            return;
        };
        if form.dirty {
            self.set_info("save first — Sync now copies what is saved");
            return;
        }
        let name = name.clone();
        let label = self
            .sandboxes
            .iter()
            .find(|sb| sb.name == name)
            .map_or_else(|| name.clone(), |sb| sb.label.clone());
        self.start_job(PendingJob {
            title: format!("Syncing repo skills into {label}"),
            steps: vec![Step::new(
                format!("sync the selected repo skills into {name}"),
                repo_skills_ui::sync_argv(&name),
            )],
            post: PostJob::default(),
            fallback: None,
            fallback_hint: None,
        });
    }

    /// Refresh every skill that came from a repository (`sbx skills update`), after a
    /// confirmation: it replaces the installed copies.
    fn action_update_skills(&mut self) {
        if self.skills.as_ref().is_none_or(|s| s.skills.is_empty()) {
            self.set_info("the shared store is empty — nothing to update");
            return;
        }
        let mut body = vec![
            "Runs `sbx skills update`: every skill that was added from a Git repository is fetched"
                .into(),
            "again and replaced with the repository's current version.".into(),
            String::new(),
            "Skills copied from this host with `import` have no repository and are left alone —"
                .into(),
            "import again to refresh those.".into(),
        ];
        body.push(String::new());
        body.push(self.skills_sharers_line());
        self.modal = Some(Modal::Confirm(Confirm {
            title: "Update skills from their repositories".into(),
            body,
            danger: false,
            confirm_label: "Update".into(),
            yes: false,
            job: PendingJob {
                title: "Updating agent skills".into(),
                steps: vec![Step::new("sbx skills update", sbx::skills_update_argv(&[]))],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// Remove one skill from the store (`sbx skills rm`).
    fn delete_skill(&mut self, name: &str) {
        let body = vec![
            format!("{name} is removed from the store every sharing sandbox mounts."),
            self.skills_sharers_line(),
            String::new(),
            "A running agent may be reading it. This cannot be undone — add or import it again"
                .into(),
            "to get it back.".into(),
        ];
        self.modal = Some(Modal::Confirm(Confirm {
            title: "Remove a skill from the shared store".into(),
            body,
            danger: true,
            confirm_label: "Remove".into(),
            yes: false,
            job: PendingJob {
                title: format!("Removing the skill {name}"),
                // --force: removal asks, and the job runner has no terminal to answer on.
                steps: vec![Step::new(
                    format!("sbx skills rm {name} --force"),
                    sbx::skills_rm_argv(&[name.to_string()]),
                )],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// One line on who sees a change to the store.
    fn skills_sharers_line(&self) -> String {
        match self.skills_sharers() {
            0 => "No sandbox on this host shares the store yet — new ones will.".into(),
            1 => "1 sandbox here shares the store; new ones do too.".into(),
            n => format!("{n} sandboxes here share the store; new ones do too."),
        }
    }

    // --- approval requests -----------------------------------------------------------------

    /// The pending requests in the order the screen draws them: this folder's sandboxes first,
    /// then the rest, each sandbox's together — so its heading is drawn once.
    pub fn approvals_sorted(&self) -> Vec<&sbx::Approval> {
        let mut out: Vec<&sbx::Approval> = self.approvals.iter().collect();
        let here = |name: &str| {
            self.sandboxes
                .iter()
                .find(|s| s.name == name)
                .is_some_and(|s| s.belongs_here(&self.cwd))
        };
        // Stable, so sbx's own order survives within one sandbox.
        out.sort_by_key(|a| {
            (
                !here(&a.sandbox),
                self.sandbox_label(&a.sandbox).to_lowercase(),
            )
        });
        out
    }

    /// Every row the approvals screen draws, the button last.
    pub fn approval_rows(&self) -> Vec<ApprovalRow> {
        let mut rows: Vec<ApprovalRow> = self
            .approvals_sorted()
            .into_iter()
            .map(|a| ApprovalRow::Request(a.id.clone()))
            .collect();
        rows.push(ApprovalRow::Close);
        rows
    }

    pub fn approval(&self, id: &str) -> Option<&sbx::Approval> {
        self.approvals.iter().find(|a| a.id == id)
    }

    /// The label a sandbox is known by here, or its sbx name for one this snapshot does not list.
    pub fn sandbox_label(&self, name: &str) -> String {
        self.sandboxes
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| name.to_string())
    }

    /// What answering the pending request `id` does, and any options sbx offers beyond allow and
    /// dismiss. Empty for a request no longer pending.
    pub fn approval_notes(&self, id: &str) -> Vec<String> {
        let Some(request) = self.approval(id) else {
            return Vec::new();
        };
        vec![format!(
            "Allow lets {} through for this sandbox from now on; its Network tab lists the rule as \
             approved. Dismiss keeps it blocked, and the agent can ask again.",
            request.title
        )]
    }

    /// Open the approvals screen, whatever is pending.
    fn action_approvals(&mut self) {
        self.screen = Screen::Approvals(ApprovalsForm::default());
        self.refresh();
    }

    // --- settings --------------------------------------------------------------------------

    /// Open the settings screen on sbx's current values.
    pub fn open_settings(&mut self) {
        let mut form = SettingsForm::default();
        form.read();
        self.screen = Screen::Settings(form);
    }

    fn settings_key(&mut self, key: KeyEvent) {
        let Screen::Settings(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        form.focus = form.focus.min(rows.len() - 1);
        let row = rows[form.focus];
        let n = rows.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = (form.focus + n - 1) % n
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => form.focus = (form.focus + 1) % n,
            KeyCode::Home | KeyCode::Char('g') => form.focus = 0,
            KeyCode::End | KeyCode::Char('G') => form.focus = n - 1,
            KeyCode::Enter | KeyCode::Char(' ') => match row {
                SettingsRow::Setting(i) => self.toggle_setting(i),
                SettingsRow::SkillsDefault => self.step_skills_default(true),
                SettingsRow::Close => self.screen = Screen::List,
            },
            KeyCode::Right if row == SettingsRow::SkillsDefault => self.step_skills_default(true),
            KeyCode::Left if row == SettingsRow::SkillsDefault => self.step_skills_default(false),
            KeyCode::F(5) => form.read(),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => form.read(),
            _ => {}
        }
    }

    /// ↵, → or ← on the skills row: the next or the previous mode, tightest first, wrapping.
    fn step_skills_default(&mut self, forward: bool) {
        let Screen::Settings(form) = &self.screen else {
            return;
        };
        match form.skills_mode() {
            Some(mode) => self.set_skills_default(match forward {
                true => mode.next(),
                false => mode.prev(),
            }),
            None => self.set_error(format!(
                "sbx's {SKILLS_DEFAULT_LABEL} could not be read — F5 reads it again"
            )),
        }
    }

    /// Make `mode` the skills default: one `sbx settings set`, then read the settings back. The
    /// next refresh hands it to the create wizard.
    fn set_skills_default(&mut self, mode: sbx::SkillsMode) {
        let Screen::Settings(form) = &self.screen else {
            return;
        };
        if form.skills_mode() == Some(mode) {
            return;
        }
        let key = sbx::SKILLS_DEFAULT_MODE;
        self.start_job(PendingJob {
            title: format!(
                "Making {} the skills default",
                crate::create::skills_mode_name(mode)
            ),
            steps: vec![Step::new(
                format!("sbx settings set {key} {}", mode.flag()),
                sbx::setting_set_argv(key, mode.flag()),
            )],
            post: PostJob {
                reread_settings: true,
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        });
    }

    /// Turn setting `i` the other way: one `sbx settings set`, then read the settings back.
    fn toggle_setting(&mut self, i: usize) {
        let Screen::Settings(form) = &self.screen else {
            return;
        };
        let spec = SETTINGS[i];
        let Some(value) = form.value(i) else {
            self.set_error(format!(
                "sbx's {} setting could not be read — F5 reads it again",
                spec.label
            ));
            return;
        };
        if let Some(var) = form.env_var(i) {
            self.set_error(format!(
                "{var} in sbx's environment sets {}: a change here would not apply",
                spec.label
            ));
            return;
        }
        let new = !value;
        self.start_job(PendingJob {
            title: format!("Turning {} {}", spec.label, spec.word(new)),
            steps: vec![Step::new(
                format!("sbx settings set {} {new}", spec.key),
                sbx::setting_set_argv(spec.key, &new.to_string()),
            )],
            post: PostJob {
                reread_settings: true,
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        });
    }

    fn approvals_key(&mut self, key: KeyEvent) {
        let rows = self.approval_rows();
        let Screen::Approvals(form) = &mut self.screen else {
            return;
        };
        form.focus = form.focus.min(rows.len() - 1);
        let row = rows[form.focus].clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = (form.focus + rows.len() - 1) % rows.len()
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                form.focus = (form.focus + 1) % rows.len()
            }
            KeyCode::Home | KeyCode::Char('g') => form.focus = 0,
            KeyCode::End | KeyCode::Char('G') => form.focus = rows.len() - 1,
            KeyCode::Enter | KeyCode::Char('a') => match row {
                ApprovalRow::Request(id) => self.respond_approval(&id, "allow"),
                ApprovalRow::Close => self.screen = Screen::List,
            },
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => {
                if let ApprovalRow::Request(id) = row {
                    self.respond_approval(&id, "dismiss");
                }
            }
            KeyCode::F(5) => self.refresh(),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.refresh(),
            _ => {}
        }
    }

    /// Answer one request where it is shown, without the approvals screen: *Allow* when `allow`,
    /// else *Dismiss*. It starts the job that `a` and `d` start on the screen.
    pub fn answer_approval(&mut self, id: &str, allow: bool) {
        self.respond_approval(id, if allow { "allow" } else { "dismiss" });
    }

    /// Answer one request. No confirmation: the row being answered is the question, and an
    /// answer is easy to take back.
    fn respond_approval(&mut self, id: &str, option: &str) {
        let Some(request) = self.approval(id).cloned() else {
            return;
        };
        if !request.offers(option) {
            self.set_error(format!(
                "sbx does not offer “{option}” for this request — it offers {}",
                request
                    .options
                    .iter()
                    .map(|o| o.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        }
        let label = self.sandbox_label(&request.sandbox);
        // Out of the list at once, so a second press cannot answer twice; the refresh restores it.
        self.approvals.retain(|a| a.id != id);
        self.start_job(PendingJob {
            title: match option {
                "allow" => format!("Allowing {} for {label}", request.title),
                "dismiss" => format!("Dismissing {} for {label}", request.title),
                other => format!("Answering {} for {label} ({other})", request.title),
            },
            steps: vec![Step::new(
                format!("sbx policy approval respond --option {option}"),
                sbx::approval_respond_argv(id, option),
            )],
            post: PostJob::default(),
            fallback: None,
            fallback_hint: Some(
                "An approval that was answered elsewhere, or that sbx has let go, is no longer \
                 pending — the list refreshes when this closes."
                    .into(),
            ),
        });
    }

    // --- keep running ----------------------------------------------------------------------

    /// Is `b` worth offering on the selected sandbox? Not on one whose folder is gone, which
    /// cannot be started at all.
    pub fn can_keep_running(&self) -> bool {
        self.selected().is_some_and(|sb| !sb.workspace_missing)
    }

    /// Keep a sandbox running after its sessions end (sbx's detached mode) — or, on one that
    /// already does, rebuild it back to stopping on its own, which is the only way back sbx has.
    fn action_keep_running(&mut self) {
        let Some(sb) = self.selected() else { return };
        let (name, label) = (sb.name.clone(), sb.label.clone());
        if sb.keep_running {
            let addons = sb.addons.clone();
            let confirm = self.recreate_confirm_with(
                &name,
                &addons,
                None,
                Some(false),
                "Stop keeping it running",
                vec!["Only a rebuild makes it stop on its own again, so this rebuilds it.".into()],
            );
            match confirm {
                Some(c) => self.modal = Some(Modal::Confirm(c)),
                None => self.set_error(self.rebuild_refusal_text(&name)),
            }
            return;
        }
        let mut body = vec![
            format!("{label}  ({name})"),
            String::new(),
            "It keeps running after its sessions end, so a server on a published port keeps \
             answering, until you stop it (s). Otherwise, sbx stops it about half a minute after \
             its last session."
                .into(),
            String::new(),
            "A stop and a start keep it so. Turning it off again (b) rebuilds the sandbox.".into(),
        ];
        if sb.ports.is_empty() && sb.running {
            body.push(String::new());
            body.push("It has no published ports yet — p publishes one.".into());
        }
        if !sb.running {
            body.push(String::new());
            body.push("It is stopped, so this starts it (no session is opened).".into());
        }
        self.modal = Some(Modal::Confirm(Confirm {
            title: "Keep running in the background".into(),
            body,
            danger: false,
            confirm_label: "Keep running".into(),
            yes: false,
            job: PendingJob {
                title: format!("Keeping {name} running"),
                steps: vec![Step::new("sbx run --detach", sbx::keep_running_argv(&name))],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    fn action_stop(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.start_job(PendingJob {
            title: format!("Stopping {name}"),
            steps: vec![Step::new("sbx stop", sbx::stop_argv(&name))],
            post: PostJob::default(),
            fallback: None,
            fallback_hint: None,
        });
    }

    fn action_rename(&mut self) {
        let Some(sb) = self.selected() else { return };
        let (sandbox, label) = (sb.name.clone(), sb.label.clone());
        self.ask(Ask::Rename, sandbox, label);
    }

    /// Forward a host port into the sandbox.
    fn action_publish_port(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.ask(Ask::PublishPort, name, String::new());
    }

    fn action_unpublish_port(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        let prefill = match self.insight.ports.value.as_deref() {
            Some([only]) => only.spec(),
            _ => String::new(),
        };
        self.ask(Ask::UnpublishPort, name, prefill);
    }

    /// Unpublish `port`, one of those the Ports tab lists: the prompt `u` opens, filled in with
    /// this mapping. A port's own button calls it.
    pub fn unpublish(&mut self, port: &sbx::Port) {
        if !self.cleared(Action::UnpublishPort) {
            return;
        }
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.ask(Ask::UnpublishPort, name, port.spec());
    }

    /// Ask the daemon whether the policy would let this sandbox reach a host.
    fn action_check_host(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        self.ask(Ask::CheckHost, name, String::new());
    }

    fn ask(&mut self, ask: Ask, subject: String, prefill: String) {
        self.modal = Some(Modal::Prompt(Prompt {
            help: ask.help(),
            ask,
            subject,
            input: TextInput::new(prefill),
        }));
    }

    fn submit_prompt(&mut self, form: &Prompt) {
        let value = form.input.value.trim().to_string();
        let name = form.subject.clone();
        match &form.ask {
            Ask::Rename => {
                self.registry
                    .set_display_name(&name, if value.is_empty() { None } else { Some(value) });
                let _ = self.registry.save();
                self.refresh();
            }
            _ if value.is_empty() => {}
            Ask::PublishPort => self.start_job(PendingJob {
                title: format!("Publishing {value} on {name}"),
                steps: vec![Step::new(
                    format!("sbx ports {name} --publish {value}"),
                    sbx::ports_publish_argv(&name, &value),
                )],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: Some(
                    "Port grammar: [[HOST_IP:]HOST_PORT:]SANDBOX_PORT[/PROTOCOL]. \
                     The host port must be free (a mapping published as /tcp binds ::1 too)."
                        .into(),
                ),
            }),
            Ask::UnpublishPort => self.start_job(PendingJob {
                title: format!("Unpublishing {value} on {name}"),
                steps: vec![Step::new(
                    format!("sbx ports {name} --unpublish {value}"),
                    sbx::ports_unpublish_argv(&name, &value),
                )],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            }),
            Ask::AddSkills => {
                let mut parts = value.split_whitespace();
                let repo = parts.next().unwrap_or_default().to_string();
                let skills: Vec<String> = parts.map(str::to_string).collect();
                if !sbx::skills_repo_ok(&repo) {
                    self.set_error(
                        "name the repository as owner/repo or a Git URL, then optionally the \
                         skills to take",
                    );
                    return;
                }
                self.start_job(PendingJob {
                    title: format!("Adding skills from {repo}"),
                    steps: vec![Step::new(
                        format!("sbx skills add {repo} --force"),
                        sbx::skills_add_argv(&repo, &skills),
                    )],
                    post: PostJob::default(),
                    fallback: None,
                    fallback_hint: Some(
                        "owner/repo is GitHub shorthand; any HTTPS or SSH Git URL works too. A \
                         --skill name has to match a directory holding a SKILL.md in the \
                         repository."
                            .into(),
                    ),
                });
            }
            // Read-only and single-answer: it belongs in the status line, not in a job modal.
            Ask::CheckHost => {
                let tx = self.notice_tx.clone();
                self.set_info(format!("checking {value}…"));
                // Under test the check is not asked: it runs `sbx` on the host.
                if cfg!(any(test, feature = "test-support")) {
                    return;
                }
                std::thread::spawn(move || {
                    let _ = tx.send(match sbx::check_host(&name, &value) {
                        // No rule covers it: the sandbox would hold the request for approval.
                        Ok(r) if r.implicit => (
                            true,
                            format!("{} — the sandbox would ask for approval (y)", r.detail),
                        ),
                        Ok(r) => (!r.allowed, r.detail),
                        Err(e) => (true, format!("{e:#}")),
                    });
                });
            }
            Ask::SetSecret(target) => {
                let job = self.store_secret_job(target, value);
                self.start_job(job);
            }
        }
    }

    fn action_delete(&mut self) {
        let Some(sb) = self.selected() else { return };
        let name = sb.name.clone();
        let label = sb.label.clone();
        let mut body = vec![
            format!("{label}  ({name})"),
            String::new(),
            match sb.tmpfs_volumes {
                true => VOLUMES_IN_RAM.into(),
                false if sb.history_in_volumes => VOLUMES_STAY.into(),
                false => "The agent's earlier sessions are deleted with it.".into(),
            },
            match sb.mountless() {
                // A mountless sandbox: its workspace is inside it, and goes with it.
                true => format!(
                    "It mounts no folder, so the files in {} inside it are deleted with it.",
                    model::MOUNTLESS_WORKDIR
                ),
                false => format!("Your files in {} stay.", model::tilde(&sb.workspace)),
            },
        ];

        let steps = vec![Step::new("sbx rm --force", sbx::rm_argv(&name))];
        // A named token stays on the host.
        if let Some(token) = sb.auth.as_ref().and_then(|a| a.token_name()) {
            body.push(String::new());
            body.push(match self.sandboxes_on(token, &name).as_slice() {
                [] => format!(
                    "The setup token “{token}” stays on the host — any new sandbox can be pointed \
                     at it again."
                ),
                others => format!(
                    "The setup token “{token}” stays on the host — {} still uses it.",
                    join_names(others)
                ),
            });
        }

        self.modal = Some(Modal::Confirm(Confirm {
            title: "Delete sandbox".into(),
            body,
            danger: true,
            confirm_label: "Delete".into(),
            yes: false,
            job: PendingJob {
                title: format!("Deleting {name}"),
                steps,
                post: PostJob {
                    forget: Some(name),
                    ..Default::default()
                },
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// Is there anything for `sbx prune` to remove: any stopped sandbox on this host, in every
    /// section, read off the snapshot.
    pub fn can_prune(&self) -> bool {
        self.sandboxes.iter().any(|sb| !sb.running)
    }

    /// Remove every stopped sandbox on the host (`sbx prune`). The confirmation names each one,
    /// as `sbx prune --dry-run` answers.
    fn action_prune(&mut self) {
        // No preview (a failure, or under test): name the stopped sandboxes from the snapshot.
        let preview = if cfg!(any(test, feature = "test-support")) {
            None
        } else {
            sbx::prune_preview().ok()
        };
        let mut body: Vec<String> = match preview.as_ref().filter(|p| !p.would_remove.is_empty()) {
            Some(p) => p.would_remove.iter().map(|c| self.prune_line(c)).collect(),
            None => self
                .sandboxes
                .iter()
                .filter(|sb| !sb.running)
                // The sbx name in brackets only where the label is not it, as `prune_line` does.
                .map(|sb| match sb.label == sb.name {
                    true => sb.name.clone(),
                    false => format!("{}  ({})", sb.label, sb.name),
                })
                .collect(),
        };
        if let Some(skipped) = preview.map(|p| p.skipped()).filter(|s| !s.is_empty()) {
            body.push(String::new());
            body.push(format!(
                "sbx leaves {} alone: it cannot tell how long {} been stopped.",
                join_names(&skipped),
                if skipped.len() == 1 {
                    "it has"
                } else {
                    "they have"
                }
            ));
        }
        body.push(String::new());
        body.push(
            "Each one is deleted. Your files stay, and a running sandbox is never pruned.".into(),
        );
        body.push(
            "Claude Code's earlier sessions stay, and come back to a new sandbox of the same name \
             and kit; the other agents' sessions are deleted with them."
                .into(),
        );
        body.push(String::new());
        body.push("Setup tokens, the /login and the shared skills stay.".into());
        self.modal = Some(Modal::Confirm(Confirm {
            title: "Prune stopped sandboxes".into(),
            body,
            danger: true,
            confirm_label: "Prune".into(),
            yes: false,
            job: PendingJob {
                title: "Pruning stopped sandboxes".into(),
                // --force: the job runner has no terminal for sbx's prompt; the dashboard asked.
                steps: vec![Step::new("sbx prune --force", sbx::prune_argv())],
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
    }

    /// One sandbox in the prune confirmation, rebuilt from the dry run's JSON, with the label
    /// from the registry, which sbx does not know.
    fn prune_line(&self, c: &sbx::PruneCandidate) -> String {
        let label = self
            .sandboxes
            .iter()
            .find(|s| s.name == c.name)
            .map(|s| s.label.as_str())
            .filter(|l| *l != c.name);
        let mut line = match label {
            Some(label) => format!("{label}  ({})", c.name),
            None => c.name.clone(),
        };
        if !c.agent.is_empty() {
            line.push_str(&format!(" · {}", c.agent));
        }
        if let Some(at) = c
            .stopped_at
            .as_deref()
            .and_then(crate::state::parse_rfc3339)
        {
            line.push_str(&format!(" · stopped {}", crate::state::ago(at)));
        }
        if let Some(ws) = c.workspaces.first() {
            let path = Path::new(ws);
            line.push_str(&format!(" · {}", model::tilde(path)));
            if !path.is_dir() {
                line.push_str(" (missing)");
            }
        }
        line
    }

    /// Is there an account to choose for the selected sandbox: an Anthropic account, or a
    /// LiteLLM connection?
    pub fn can_edit_auth(&self) -> bool {
        self.selected()
            .is_some_and(|sb| sb.auth.is_some() || self.on_litellm_backend(sb))
    }

    /// Is this sandbox built from a LiteLLM backend, one whose account is a connection?
    pub fn on_litellm_backend(&self, sb: &Sandbox) -> bool {
        sb.backend
            .as_deref()
            .and_then(|b| self.catalog.backend(b))
            .is_some_and(|b| b.litellm_agent().is_some())
    }

    /// Open the auth editor for the selected sandbox — on a LiteLLM sandbox, the form that moves
    /// it to another connection.
    fn action_edit_auth(&mut self) {
        let Some(sb) = self.selected() else { return };
        if self.on_litellm_backend(sb) {
            let (name, current) = (sb.name.clone(), sb.litellm.clone());
            self.open_conn_switch(&name, current);
            return;
        }
        let Some(current) = sb.auth.clone() else {
            return;
        };
        let name = sb.name.clone();
        let ram_volumes = sb.tmpfs_volumes;
        let token_name = current
            .token_name()
            .unwrap_or(DEFAULT_TOKEN_NAME)
            .to_string();
        let account = match &current {
            Auth::Login => AccountChoice::Login,
            Auth::Token { name, .. } => AccountChoice::Named(name.clone()),
        };
        let mut tokens = self.token_names();
        // A name with no stored token still needs its row, so it can be selected and fixed.
        if let AccountChoice::Named(named) = &account {
            if !tokens.iter().any(|t| t == named) {
                tokens.push(named.clone());
                tokens.sort();
            }
        }
        self.screen = Screen::AuthEdit(AuthForm {
            sandbox: name,
            tokens,
            account,
            current,
            token_name: TextInput::new(token_name),
            token: TextInput::default(),
            focus: 0,
            ram_volumes,
        });
    }

    /// Work out what applying the auth editor comes down to, without doing any of it, so tests
    /// can check the plan without running a command.
    fn auth_plan(&self, form: &AuthForm) -> AuthPlan {
        let name = form.sandbox.clone();
        let token = form.token.value.trim().to_string();
        let typed = form.token_name.value.trim().to_string();

        // A new token has to be namable, and must not quietly take over one that exists.
        if form.account == AccountChoice::New {
            if !sbx::token_name_ok(&typed) {
                return AuthPlan::Refuse("name the token in lower-case letters, digits and dashes");
            }
            if form.tokens.contains(&typed) {
                return AuthPlan::Refuse("a token of that name exists — pick it in the list");
            }
        }
        let Some(label) = form.account.name(&typed).map(|n| match n.is_empty() {
            true => DEFAULT_TOKEN_NAME.to_string(),
            false => n,
        }) else {
            return self.auth_plan_login(form);
        };
        let placeholder = sbx::named_token_placeholder(&label);
        // A named token is not stored just because it has a name: read the secret store.
        let stored = matches!(form.account, AccountChoice::Named(_))
            && self.secrets.as_ref().is_some_and(|s| s.holds(&placeholder));

        if !form.changed() {
            return AuthPlan::Nothing("no auth changes".into());
        }

        // --- the container already points here: only the value can change, and it changes live -
        if !form.needs_recreate() {
            if token.is_empty() && !stored {
                return AuthPlan::Refuse(
                    "paste a token first — generate one with `claude setup-token`",
                );
            }
            if token.is_empty() {
                return AuthPlan::Nothing("no auth changes".into());
            }
            let step = Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_named_token_argv(&placeholder),
                token,
            );
            let job = PendingJob {
                title: format!("Storing the setup token for {name}"),
                steps: vec![step],
                post: PostJob {
                    auth: Some(AuthRecord::named(&name, &label)),
                    ..Default::default()
                },
                fallback: None,
                fallback_hint: None,
            };
            let others = self.sandboxes_on(&label, &name);
            if !others.is_empty() {
                return AuthPlan::Confirm(Box::new(Confirm {
                    title: "Rotate a shared setup token".into(),
                    body: vec![
                        format!("The token “{label}” is used by {}.", join_names(&others)),
                        String::new(),
                        format!(
                            "Storing a new value replaces it for {name} and for {} — they all \
                             authenticate with whatever is behind this one name.",
                            join_names(&others)
                        ),
                        String::new(),
                        "It takes effect on the next request. Nothing is rebuilt.".into(),
                    ],
                    danger: false,
                    confirm_label: "Rotate".into(),
                    yes: true,
                    job,
                }));
            }
            return AuthPlan::Apply(Box::new(job));
        }

        // --- a different placeholder, or a different backend kit: the container is rebuilt -----
        if token.is_empty() && !stored {
            return AuthPlan::Refuse(
                "paste a token first — generate one with `claude setup-token`",
            );
        }
        let Some(backend) = self.catalog.for_account(Account::Token) else {
            return AuthPlan::Refuse("this checkout ships no setup-token kit");
        };
        let Some(mut job) = self.recreate_job_as(
            &name,
            &backend.kit.name,
            &self.addons_of(&name),
            Some(&label),
            &self.models_of(&name),
        ) else {
            return AuthPlan::Refuse(self.rebuild_refusal(&name));
        };

        // The token must exist before `sbx create`, which builds the container.
        let mut prelude = Vec::new();
        if !token.is_empty() {
            prelude.push(Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_named_token_argv(&placeholder),
                token,
            ));
        }
        let mut body = Vec::new();
        job.steps.splice(0..0, prelude);
        job.title = format!("Rebuilding {name} on the setup token “{label}”");
        job.post.auth = Some(AuthRecord::named(&name, &label));

        let others = self.sandboxes_on(&label, &name);
        body.insert(
            0,
            match others.is_empty() {
                true => format!("{name} moves to the setup token “{label}”."),
                false => format!(
                    "{name} moves to the setup token “{label}”, the same account as {}.",
                    join_names(&others)
                ),
            },
        );
        if body.len() == 1 {
            body.push(String::new());
        }
        body.push(format!(
            "sbx bakes {} into a container when it builds it, so pointing this sandbox at \
             another token means rebuilding it{}.",
            sbx::TOKEN_ENV,
            match form.current.on_token() {
                true => "",
                false =>
                    " — onto the claude-token kit, which is also what lets a token reach \
                          Anthropic at all",
            }
        ));
        body.push(String::new());
        body.push(SWITCH_KIT_LOSES.into());
        body.push(match (form.ram_volumes, form.current.on_token()) {
            (true, _) => SWITCH_IN_RAM.into(),
            (false, true) => "The session history stays in its volumes.".into(),
            (false, false) => SWITCH_KIT_HISTORY.into(),
        });
        body.push("Your files stay.".into());

        AuthPlan::Confirm(Box::new(Confirm {
            title: "Change the Anthropic account".into(),
            body,
            danger: true,
            confirm_label: "Rebuild".into(),
            yes: false,
            job,
        }))
    }

    /// The `/login` half of [`App::auth_plan`]: there is no token to name or store, only a
    /// backend kit to swap back to.
    fn auth_plan_login(&self, form: &AuthForm) -> AuthPlan {
        let name = form.sandbox.clone();
        if !form.changed() {
            return AuthPlan::Nothing(format!("{name} is already on the /login account"));
        }
        let Some(backend) = self.catalog.for_account(Account::Login) else {
            return AuthPlan::Refuse("this checkout ships no kit for that account");
        };
        let Some(mut job) = self.recreate_job_as(
            &name,
            &backend.kit.name,
            &self.addons_of(&name),
            None,
            &self.models_of(&name),
        ) else {
            return AuthPlan::Refuse(self.rebuild_refusal(&name));
        };

        let mut body = vec![format!(
            "{name} moves to the host-global /login account, shared with every other /login \
             sandbox on this machine."
        )];
        let token = form
            .current
            .token_name()
            .unwrap_or(DEFAULT_TOKEN_NAME)
            .to_string();
        body.push(String::new());
        if form.current.on_token() {
            body.push(format!(
                "The token “{token}” itself stays on the host — other sandboxes can still use it, \
                 and this one can be pointed back at it."
            ));
        }
        job.title = format!("Rebuilding {name} on the /login account");
        job.post.auth = Some(AuthRecord::cleared(&name));

        body.push(String::new());
        body.push(format!(
            "The two accounts are two different workloads, so this swaps the backend kit to {} \
             and rebuilds the container.",
            backend.kit.name
        ));
        body.push(String::new());
        body.push(SWITCH_KIT_LOSES.into());
        body.push(match form.ram_volumes {
            true => SWITCH_IN_RAM.into(),
            false => SWITCH_KIT_HISTORY.into(),
        });
        body.push("Your files stay.".into());

        AuthPlan::Confirm(Box::new(Confirm {
            title: "Change the Anthropic account".into(),
            body,
            danger: true,
            confirm_label: "Rebuild".into(),
            yes: false,
            job,
        }))
    }

    fn submit_auth(&mut self) {
        let Screen::AuthEdit(form) = &self.screen else {
            return;
        };
        let form = form.clone();
        match self.auth_plan(&form) {
            // A refusal keeps the form open: the user still has to type something.
            AuthPlan::Refuse(message) => self.set_error(message),
            AuthPlan::Nothing(message) => {
                self.screen = Screen::List;
                self.set_info(message);
            }
            AuthPlan::Apply(job) => {
                self.screen = Screen::List;
                self.start_job(*job);
            }
            AuthPlan::Confirm(confirm) => {
                self.screen = Screen::List;
                self.modal = Some(Modal::Confirm(*confirm));
            }
        }
    }

    // --- LiteLLM connections: the `S` dialog and `A` -----------------------------------------

    /// The job that stores a connection's key: one `set-custom` on its placeholder, targeted at
    /// its host, the key on **stdin**. What `post` says happens once it exited 0.
    fn litellm_key_job(
        title: String,
        conn: &crate::litellm::Connection,
        key: String,
        post: PostJob,
    ) -> PendingJob {
        let placeholder = crate::litellm::placeholder(&conn.name);
        PendingJob {
            title,
            steps: vec![Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_litellm_key_argv(crate::litellm::host(&conn.url), &placeholder),
                key,
            )],
            post,
            fallback: None,
            fallback_hint: None,
        }
    }

    /// Open the `S` dialog: empty for a new connection, or on an existing one with its URL
    /// prefilled and the key empty (keep the stored one).
    fn open_connection(&mut self, mode: ConnMode) {
        let connections = crate::litellm::load_all_in(&self.litellm_root);
        let fields = match &mode {
            ConnMode::New => crate::create::NewConnection::default(),
            ConnMode::Edit(name) => {
                let Some(conn) = connections.iter().find(|c| c.name == *name) else {
                    self.set_error(format!("connection {name} no longer exists"));
                    return;
                };
                crate::create::NewConnection {
                    name: TextInput::new(name.clone()),
                    url: TextInput::new(conn.url.clone()),
                    key: TextInput::default(),
                }
            }
        };
        self.screen = Screen::Connection(ConnForm {
            mode,
            fields,
            connections,
            focus: 0,
        });
    }

    // --- the host's folders ------------------------------------------------------------------

    /// Open the host folders screen on the list as the file holds it now, the cursor on the
    /// folder with this path, else on the first field.
    fn open_folders(&mut self, at: Option<&Path>) {
        let mut form = FoldersForm::default();
        match folders::load(&folders::config_path(&self.mounts_state)) {
            Ok(config) => form.folders = config.folders,
            Err(e) => form.error = Some(format!("{e:#}")),
        }
        let rows = form.rows();
        form.focus = at
            .and_then(|path| form.folders.iter().position(|f| f.path == path))
            .map(FoldersRow::Folder)
            .or(Some(FoldersRow::Path))
            .and_then(|row| rows.iter().position(|r| *r == row))
            .unwrap_or(0);
        self.screen = Screen::Folders(form);
    }

    fn folders_key(&mut self, key: KeyEvent) {
        let Screen::Folders(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let row = form.focused();
        let field = match row {
            FoldersRow::Path => Some(&mut form.path),
            FoldersRow::Purpose => Some(&mut form.purpose),
            _ => None,
        };
        if let Some(field) = field {
            if edit_text(field, key) {
                return;
            }
        }
        let step = |focus, delta| step_form(focus, rows.len(), FoldersForm::BUTTONS, delta);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                form.focus = step(form.focus, -1)
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => form.focus = step(form.focus, 1),
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char(' ')
            | KeyCode::Char('h')
            | KeyCode::Char('l')
                if row == FoldersRow::Access =>
            {
                form.access = match form.access {
                    folders::Access::Read => folders::Access::Write,
                    folders::Access::Write => folders::Access::Read,
                };
            }
            KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l')
                if row == FoldersRow::Browse =>
            {
                self.browse_host_folder()
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(next) = step_button(form.focus, rows.len(), FoldersForm::BUTTONS, delta)
                {
                    form.focus = next;
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let FoldersRow::Folder(i) = row {
                    self.delete_folder(i);
                }
            }
            KeyCode::Char('n') => {
                form.editing = None;
                form.path = TextInput::default();
                form.purpose = TextInput::default();
                form.access = folders::Access::Read;
                form.focus = rows
                    .iter()
                    .position(|r| *r == FoldersRow::Path)
                    .unwrap_or(0);
            }
            KeyCode::Enter => match row {
                FoldersRow::Folder(i) => {
                    if let Some(f) = form.folders.get(i).cloned() {
                        form.editing = Some(i);
                        form.path = TextInput::new(f.path.display().to_string());
                        form.purpose = TextInput::new(&f.purpose);
                        form.access = f.access;
                        form.focus = rows
                            .iter()
                            .position(|r| *r == FoldersRow::Path)
                            .unwrap_or(0);
                    }
                }
                FoldersRow::Path => {
                    form.focus = rows
                        .iter()
                        .position(|r| *r == FoldersRow::Access)
                        .unwrap_or(form.focus)
                }
                FoldersRow::Browse => self.browse_host_folder(),
                FoldersRow::Save => self.submit_folder(),
                FoldersRow::Close => self.screen = Screen::List,
                _ => form.focus = step(form.focus, 1),
            },
            _ => {}
        }
    }

    /// Open the folder picker for the Path field, on the folder typed there, else on home.
    fn browse_host_folder(&mut self) {
        let Screen::Folders(form) = &self.screen else {
            return;
        };
        let home = model::home_dir();
        let start = folders::typed_path(&form.path.value, home.as_deref())
            .ok()
            .filter(|p| p.is_dir())
            .or(home)
            .unwrap_or_else(|| self.cwd.clone());
        let (root, rel) = host_root(&start);
        let Screen::Folders(form) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        self.screen = Screen::StartDir(StartDirForm::open(
            StartDirTarget::HostFolder(Box::new(form)),
            "host folder".into(),
            root,
            Some(rel),
        ));
    }

    /// Put the picked folder into the host folders screen's Path field, unless `--mount` would
    /// refuse it; then the picker stays open.
    fn use_host_folder(&mut self, here: PathBuf) {
        if let Some(why) = model::home_dir().and_then(|home| {
            let real = std::fs::canonicalize(&here).unwrap_or_else(|_| here.clone());
            model::mount_refusal(&real, &home)
        }) {
            return self.set_error(format!("{}: {why}", model::tilde(&here)));
        }
        let Screen::StartDir(picker) = std::mem::replace(&mut self.screen, Screen::List) else {
            return;
        };
        let StartDirTarget::HostFolder(mut form) = picker.target else {
            return;
        };
        form.path = TextInput::new(model::tilde(&here));
        form.focus = form
            .rows()
            .iter()
            .position(|r| *r == FoldersRow::Access)
            .unwrap_or(0);
        self.screen = Screen::Folders(*form);
    }

    /// Save the fields of the host folders screen into the host's list.
    fn submit_folder(&mut self) {
        let Screen::Folders(form) = &self.screen else {
            return;
        };
        if let Some(error) = &form.error {
            self.set_error(format!("the list is left as it is: {error}"));
            return;
        }
        let path = match folders::typed_path(&form.path.value, model::home_dir().as_deref()) {
            Ok(path) => path,
            Err(why) => return self.set_error(why),
        };
        let entry = folders::Entry {
            path: path.clone(),
            access: form.access,
            purpose: form
                .purpose
                .value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        };
        if let Some(why) = model::home_dir().and_then(|home| {
            let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            model::mount_refusal(&real, &home)
        }) {
            return self.set_error(format!("{}: {why}", path.display()));
        }
        let saved = match form.editing {
            Some(i) => folders::replace(&self.mounts_state, i, entry),
            None => folders::put(&self.mounts_state, entry),
        };
        if let Err(e) = saved {
            return self.set_error(format!("{e:#}"));
        }
        let missing = !path.is_dir();
        self.open_folders(Some(&path));
        self.set_info(match missing {
            true => format!(
                "{} saved; it is not a folder now, so a sandbox mounts it once it is",
                path.display()
            ),
            false => format!(
                "{} saved — sandboxes mount it from their next create or rebuild",
                path.display()
            ),
        });
    }

    fn delete_folder(&mut self, i: usize) {
        let Screen::Folders(form) = &self.screen else {
            return;
        };
        if form.error.is_some() {
            return self.set_error("the list does not load, so it is left as it is");
        }
        let Some(path) = form.folders.get(i).map(|f| f.path.clone()) else {
            return;
        };
        if let Err(e) = folders::remove(&self.mounts_state, &path) {
            return self.set_error(format!("{e:#}"));
        }
        self.open_folders(None);
        self.set_info(format!(
            "{} dropped — a sandbox keeps it until its next rebuild",
            path.display()
        ));
    }

    /// Back to `S`, on the row of that connection when it has one.
    fn back_to_secrets(&mut self, conn: Option<&str>) {
        self.screen = Screen::Secrets(SecretsForm::default());
        let focus = conn.and_then(|name| {
            self.secret_rows()
                .iter()
                .position(|r| *r == SecretRow::Connection(name.to_string()))
        });
        if let (Screen::Secrets(form), Some(focus)) = (&mut self.screen, focus) {
            form.focus = focus;
        }
    }

    fn conn_key(&mut self, key: KeyEvent) {
        let Screen::Connection(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let row = rows[form.focus.min(rows.len() - 1)];
        let field = match row {
            ConnRow::Name => Some(&mut form.fields.name),
            ConnRow::Url => Some(&mut form.fields.url),
            ConnRow::Key => Some(&mut form.fields.key),
            ConnRow::Save | ConnRow::Cancel => None,
        };
        if let Some(field) = field {
            if edit_text(field, key) {
                return;
            }
        }
        let editing = match &form.mode {
            ConnMode::Edit(name) => Some(name.clone()),
            ConnMode::New => None,
        };
        match key.code {
            KeyCode::Esc => self.back_to_secrets(editing.as_deref()),
            KeyCode::Up | KeyCode::BackTab => {
                form.focus = step_form(form.focus, rows.len(), ConnForm::BUTTONS, -1)
            }
            KeyCode::Down | KeyCode::Tab => {
                form.focus = step_form(form.focus, rows.len(), ConnForm::BUTTONS, 1)
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(next) = step_button(form.focus, rows.len(), ConnForm::BUTTONS, delta) {
                    form.focus = next;
                }
            }
            KeyCode::Enter => match row {
                ConnRow::Save => self.submit_conn(),
                ConnRow::Cancel => self.back_to_secrets(editing.as_deref()),
                _ => form.focus += 1,
            },
            _ => {}
        }
    }

    /// Work out what saving the `S` dialog comes down to, without doing any of it. A new
    /// connection is one job (its key, then its file); an edit costs what changed.
    pub fn conn_plan(&self, form: &ConnForm) -> ConnPlan {
        let key = form.fields.key.value.trim().to_string();
        let name = match &form.mode {
            ConnMode::New => {
                let conn = match form.fields.check(&form.connections) {
                    Ok(conn) => conn,
                    Err(why) => return ConnPlan::Refuse(why),
                };
                // Checked again against the file: `aisbx litellm add` may have made it meanwhile.
                if crate::litellm::exists_in(&self.litellm_root, &conn.name) {
                    return ConnPlan::Refuse(format!("connection {} exists", conn.name));
                }
                // The first key of a connection: its model list right after.
                let kit = crate::model_fetch::kit_dir_in(&self.litellm_root, &conn.name);
                let post = PostJob {
                    save_connection: Some((conn.clone(), None)),
                    then: Some(Box::new(self.fetch_job_at(&conn, &kit))),
                    restore: Some(Restore::Connection(Box::new(form.clone().without_key()))),
                    ..Default::default()
                };
                let title = format!("Storing the key of LiteLLM connection {}", conn.name);
                return ConnPlan::Apply(Box::new(Self::litellm_key_job(title, &conn, key, post)));
            }
            ConnMode::Edit(name) => name.clone(),
        };
        let Some(old) = crate::litellm::load_in(&self.litellm_root, &name) else {
            return ConnPlan::Refuse(format!("connection {name} no longer exists"));
        };
        let new = match form.fields.check_edit(&old) {
            Ok(conn) => conn,
            Err(why) => return ConnPlan::Refuse(why),
        };
        let moves = crate::litellm::moves_host(&old, &new);
        let file_changed = new != old;
        if !file_changed && key.is_empty() {
            return ConnPlan::Nothing("no connection changes".into());
        }
        if moves && key.is_empty() {
            return ConnPlan::Refuse(format!(
                "the host changes ({} → {}) — paste the key again",
                crate::litellm::host(&old.url),
                crate::litellm::host(&new.url)
            ));
        }
        let users = self.litellm_users(&name);
        let job = match key.is_empty() {
            // Only the file: no sbx call at all, and nothing to ask when nobody is on it.
            true if users.is_empty() => return ConnPlan::Write(new),
            true => PendingJob {
                title: format!("Saving LiteLLM connection {name}"),
                steps: Vec::new(),
                post: PostJob {
                    rewrite_connection: Some(new.clone()),
                    ..Default::default()
                },
                fallback: None,
                fallback_hint: None,
            },
            false => {
                let mut job = Self::litellm_key_job(
                    format!("Storing the key of LiteLLM connection {name}"),
                    &new,
                    key.clone(),
                    PostJob {
                        rewrite_connection: file_changed.then(|| new.clone()),
                        restore: Some(Restore::Connection(Box::new(form.clone().without_key()))),
                        ..Default::default()
                    },
                );
                // The fetcher reaches the old host only: it is built again for the new one.
                let fetcher = crate::model_fetch::sandbox_name(&name);
                if moves && self.fetchers.contains(&fetcher) {
                    job.steps.push(Step::new(
                        format!("sbx rm {fetcher}"),
                        sbx::rm_argv(&fetcher),
                    ));
                }
                job
            }
        };
        if users.is_empty() {
            return ConnPlan::Apply(Box::new(job));
        }
        let mut body = vec![
            format!(
                "{} {} on connection “{name}”.",
                join_names(&users),
                if users.len() == 1 { "is" } else { "are" }
            ),
            String::new(),
        ];
        if !key.is_empty() && !moves {
            body.push(
                "The new key reaches them on their next request — nothing is rebuilt.".into(),
            );
        }
        if moves {
            body.push(format!(
                "The host changes from {} to {}: they stop authenticating until rebuilt (R), \
                 since their kit still sends to the old host.",
                crate::litellm::host(&old.url),
                crate::litellm::host(&new.url)
            ));
        }
        if !moves && new.url != old.url {
            body.push(
                "The new URL path reaches them only when rebuilt (R) — their kit carries the old \
                 one."
                    .into(),
            );
        }
        ConnPlan::Confirm(Box::new(Confirm {
            title: "Change a LiteLLM connection".into(),
            body,
            danger: moves,
            confirm_label: "Save".into(),
            yes: !moves,
            job,
        }))
    }

    fn submit_conn(&mut self) {
        let Screen::Connection(form) = &self.screen else {
            return;
        };
        let form = form.clone();
        let editing = match &form.mode {
            ConnMode::Edit(name) => Some(name.clone()),
            ConnMode::New => None,
        };
        match self.conn_plan(&form) {
            ConnPlan::Refuse(why) => self.set_error(why),
            ConnPlan::Nothing(message) => {
                self.back_to_secrets(editing.as_deref());
                self.set_info(message);
            }
            ConnPlan::Write(conn) => match crate::litellm::save_in(&self.litellm_root, &conn) {
                Ok(()) => {
                    self.back_to_secrets(Some(&conn.name));
                    self.set_info(format!("connection {} saved", conn.name));
                    self.refresh();
                }
                Err(e) => self.set_error(format!("{e:#}")),
            },
            // On `S`, under the job's log: a new connection's row is there once it succeeded.
            ConnPlan::Apply(job) => {
                self.back_to_secrets(editing.as_deref());
                self.start_job(*job);
            }
            ConnPlan::Confirm(confirm) => {
                self.back_to_secrets(editing.as_deref());
                self.modal = Some(Modal::Confirm(*confirm));
            }
        }
    }

    /// Open `A` on a LiteLLM sandbox, on the connection it is on — or, when that one is gone, on
    /// the one the create form would offer.
    fn open_conn_switch(&mut self, sandbox: &str, current: Option<String>) {
        let connections = crate::litellm::load_all_in(&self.litellm_root);
        let pick = match current
            .as_deref()
            .filter(|c| connections.iter().any(|x| x.name == *c))
        {
            Some(c) => crate::litellm::LitellmPick::Existing(c.to_string()),
            None => crate::litellm::preselect(&connections, self.registry.last_litellm()),
        };
        self.screen = Screen::ConnSwitch(ConnSwitchForm {
            sandbox: sandbox.to_string(),
            current,
            connections,
            pick,
            new_conn: crate::create::NewConnection::default(),
            focus: 0,
        });
    }

    fn conn_switch_key(&mut self, key: KeyEvent) {
        let Screen::ConnSwitch(form) = &mut self.screen else {
            return;
        };
        let rows = form.rows();
        let row = rows[form.focus.min(rows.len() - 1)];
        let field = match row {
            ConnSwitchRow::Name => Some(&mut form.new_conn.name),
            ConnSwitchRow::Url => Some(&mut form.new_conn.url),
            ConnSwitchRow::Key => Some(&mut form.new_conn.key),
            _ => None,
        };
        if let Some(field) = field {
            if edit_text(field, key) {
                return;
            }
        }
        match key.code {
            KeyCode::Esc => self.screen = Screen::List,
            KeyCode::Up | KeyCode::BackTab => {
                form.focus = step_form(form.focus, rows.len(), ConnSwitchForm::BUTTONS, -1)
            }
            KeyCode::Down | KeyCode::Tab => {
                form.focus = step_form(form.focus, rows.len(), ConnSwitchForm::BUTTONS, 1)
            }
            KeyCode::Left | KeyCode::Right if row == ConnSwitchRow::Conn => {
                form.cycle(key.code == KeyCode::Right)
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(next) =
                    step_button(form.focus, rows.len(), ConnSwitchForm::BUTTONS, delta)
                {
                    form.focus = next;
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter if row == ConnSwitchRow::Conn => form.cycle(true),
            KeyCode::Enter => match row {
                ConnSwitchRow::Apply => self.submit_conn_switch(),
                ConnSwitchRow::Cancel => self.screen = Screen::List,
                _ => form.focus += 1,
            },
            _ => {}
        }
    }

    /// Work out what applying `A` on a LiteLLM sandbox comes down to: nothing, or a rebuild onto
    /// the picked connection, after storing a new one's key, then its file. Always confirmed.
    pub fn switch_plan(&self, form: &ConnSwitchForm) -> ConnPlan {
        use crate::litellm::LitellmPick;
        let name = form.sandbox.as_str();
        let Some(sb) = self.sandboxes.iter().find(|s| s.name == name) else {
            return ConnPlan::Refuse(format!("{name} no longer exists"));
        };
        let (target, key) = match &form.pick {
            LitellmPick::Existing(target) => {
                if !form.moving() {
                    return ConnPlan::Nothing("no connection changes".into());
                }
                let Some(conn) = crate::litellm::load_in(&self.litellm_root, target) else {
                    return ConnPlan::Refuse(format!("connection {target} no longer exists"));
                };
                (conn, None)
            }
            LitellmPick::New => {
                let conn = match form.new_conn.check(&form.connections) {
                    Ok(conn) => conn,
                    Err(why) => return ConnPlan::Refuse(why),
                };
                if crate::litellm::exists_in(&self.litellm_root, &conn.name) {
                    return ConnPlan::Refuse(format!("connection {} exists", conn.name));
                }
                (conn, Some(form.new_conn.key.value.trim().to_string()))
            }
        };
        if let Some(why) = self.move_blocker(name) {
            return ConnPlan::Refuse(why);
        }
        let Some(backend) = sb.backend.clone() else {
            return ConnPlan::Refuse("only sandboxes created from this repo's kits move".into());
        };
        let Some(agent) = self
            .catalog
            .backend(&backend)
            .and_then(Backend::litellm_agent)
        else {
            return ConnPlan::Refuse(format!("{backend} is on no LiteLLM connection"));
        };
        let Some(mut plan) = self.rebuild_plan(
            name,
            &backend,
            &sb.addons,
            None,
            &sb.models,
            None,
            Some(&target),
        ) else {
            return ConnPlan::Refuse(self.rebuild_refusal(name).to_string());
        };
        plan.job.title = format!("Moving {name} to LiteLLM connection {}", target.name);
        plan.job.post.last_litellm = Some(target.name.clone());

        let to = format!("“{}” ({})", target.name, crate::litellm::host(&target.url));
        let mut body = vec![match form.current.as_deref() {
            Some(cur) if form.connections.iter().any(|c| c.name == cur) => {
                format!(
                    "{} moves from LiteLLM connection “{cur}” to {to}.",
                    sb.label
                )
            }
            Some(cur) => format!(
                "{} is on connection “{cur}”, which no longer exists; it moves to {to}.",
                sb.label
            ),
            None => format!("{} moves to LiteLLM connection {to}.", sb.label),
        }];
        if key.is_some() {
            body.push(
                "The new connection's key is stored first, then its connection.json, then the \
                 sandbox is rebuilt on it."
                    .into(),
            );
        }
        body.push(
            "Its connection kit is baked into the container, so moving to another connection \
             rebuilds it."
                .into(),
        );
        // The pins stay; an id the new gateway does not list fails only at request time.
        let catalog = self.connection_models(&target).0;
        let missing: Vec<String> = sb
            .models
            .entries()
            .map(|(_, id)| id.to_string())
            .filter(|id| !catalog.iter().any(|m| m.id == *id))
            .collect();
        if !missing.is_empty() {
            body.push(format!(
                "Its model pins stay, but “{}” lists no {} — change them with e after the move.",
                target.name,
                join_names(&missing)
            ));
        }
        let Some(mut confirm) =
            self.rebuild_confirm(name, plan, "Move to another LiteLLM connection", body)
        else {
            return ConnPlan::Refuse(self.rebuild_refusal(name).to_string());
        };
        if let Some(key) = key {
            let rebuild = confirm.job.clone();
            confirm.job = Self::litellm_key_job(
                format!("Storing the key of LiteLLM connection {}", target.name),
                &target,
                key,
                PostJob {
                    save_connection: Some((target.clone(), Some(agent))),
                    then: Some(Box::new(rebuild)),
                    restore: Some(Restore::ConnSwitch(Box::new(form.clone().without_key()))),
                    ..Default::default()
                },
            );
        }
        ConnPlan::Confirm(Box::new(confirm))
    }

    fn submit_conn_switch(&mut self) {
        let Screen::ConnSwitch(form) = &self.screen else {
            return;
        };
        let form = form.clone();
        match self.switch_plan(&form) {
            ConnPlan::Refuse(why) => self.set_error(why),
            ConnPlan::Nothing(message) => {
                self.screen = Screen::List;
                self.set_info(message);
            }
            // A move is always confirmed; neither comes back from `switch_plan`.
            ConnPlan::Write(_) => self.screen = Screen::List,
            ConnPlan::Apply(job) => {
                self.screen = Screen::List;
                self.start_job(*job);
            }
            ConnPlan::Confirm(confirm) => {
                self.screen = Screen::List;
                self.modal = Some(Modal::Confirm(*confirm));
            }
        }
    }

    fn action_edit_kits(&mut self) {
        let Some(sb) = self.selected() else { return };
        let addons: Vec<bool> = self
            .catalog
            .addons
            .iter()
            .map(|a| sb.addons.iter().any(|n| n == &a.kit.name))
            .collect();
        // Offer this sandbox's connection's models, not those a form showed last.
        let conn = sb.litellm.clone();
        self.load_models_for(conn.as_deref());
        let Some(sb) = self.selected() else { return };
        let models_offered = !self.catalog.models.is_empty()
            && sb
                .backend
                .as_deref()
                .and_then(|b| self.catalog.backend(b))
                .is_some_and(Backend::model_overrides);
        let models = sb.models.clone();
        self.screen = Screen::Kits(KitsForm {
            sandbox: sb.name.clone(),
            original: addons.clone(),
            addons,
            models_offered,
            original_models: models.clone(),
            models,
            model_pick: None,
            focus: 0,
            ram_volumes: sb.tmpfs_volumes,
            history_in_volumes: sb.history_in_volumes,
            harness: crate::kits::agent_command(&sb.agent).map(str::to_string),
        });
    }

    fn action_recreate(&mut self) {
        let Some(sb) = self.selected() else { return };
        let (name, addons) = (sb.name.clone(), sb.addons.clone());
        let confirm = self.recreate_confirm(
            &name,
            &addons,
            None,
            "Rebuild sandbox",
            vec![match &sb.drift {
                Some(drift) => drift.reason(),
                None => "The sandbox is rebuilt from the same backend and kits.".to_string(),
            }],
        );
        match confirm {
            Some(c) => self.modal = Some(Modal::Confirm(c)),
            None => self.set_error(self.rebuild_refusal_text(&name)),
        }
    }

    fn submit_create(&mut self, attach: bool) {
        let catalog = Arc::clone(&self.catalog);
        // A LiteLLM backend's connection: an existing one by name, or a new one with its key.
        let (litellm, new_conn) = match &self.screen {
            Screen::Create(form) if form.litellm_backend(&catalog) => {
                match form.connection.litellm.clone() {
                    Some(name) => (Some(name), None),
                    None => match form.new_connection() {
                        Ok(conn) => {
                            let key = form.connection.new_conn.key.value.trim().to_string();
                            (Some(conn.name.clone()), Some((conn, key)))
                        }
                        // Refused below, with the reason.
                        Err(_) => (None, None),
                    },
                }
            }
            _ => (None, None),
        };
        // Copy the form out before touching `self` again — the status line below is a mutation.
        let (
            backend_idx,
            addons,
            typed_label,
            clone,
            skills,
            typed_ports,
            account,
            typed_name,
            token,
            models,
            start_dir,
            typed_deny,
            static_mcp,
            picks,
            defaults,
            problem,
            workspace,
            git_guard,
            typed_folders,
        ) = {
            let Screen::Create(form) = &self.screen else {
                return;
            };
            (
                form.harness.backend,
                form.selected_addons(&catalog),
                (
                    form.workspace.name.value.trim().to_string(),
                    form.label_account(&catalog),
                ),
                form.workspace.clone,
                // Only what the form was offering: `None` passes no `--skills` at all.
                form.skills_offered(&catalog)
                    .then_some(form.workspace.skills),
                form.workspace.ports.value.trim().to_string(),
                match form.use_token(&catalog) {
                    true => form.connection.account.clone(),
                    false => AccountChoice::Login,
                },
                form.connection.token_name.value.trim().to_string(),
                form.connection.token.value.trim().to_string(),
                // Only what the form offered: a pick left from another backend must not follow.
                match form.models_offered(&catalog) {
                    true => form.picks(&catalog).slots,
                    false => Overrides::default(),
                },
                // Same rule for the start folder.
                match form.start_dir_offered(&catalog) {
                    true => form.workspace.start_dir.clone(),
                    false => None,
                },
                form.workspace.deny_network.value.trim().to_string(),
                form.selected_mcp(),
                form.models_agent(&catalog)
                    .zip(form.connection.litellm.clone())
                    .map(|(agent, conn)| (conn, agent, form.picks(&catalog))),
                form.defaults(&catalog),
                // Each step checks the fields it owns; the workspace's come first.
                CreateForm::CHECKED_STEPS
                    .into_iter()
                    .find_map(|step| form.step_problem(step, &catalog)),
                form.workspace_root(&self.cwd),
                form.git_guard_offered() && form.workspace.git_guard,
                form.workspace.extra_folders.clone(),
            )
        };
        // Checked first: sbx would refuse the create only at the very end.
        if self.policy_needs_init == Some(true) {
            self.set_error(sbx::POLICY_INIT_HINT);
            return;
        }
        if let Some(problem) = problem {
            self.set_error(problem);
            return;
        }
        // Read again now: `aisbx litellm add`, or another dashboard, may have made it meanwhile.
        if let Some((conn, _)) = &new_conn {
            if crate::litellm::exists_in(&self.litellm_root, &conn.name) {
                self.set_error(format!("connection {} exists", conn.name));
                return;
            }
        }
        let ports = split_ports(&typed_ports);
        let deny_network = split_ports(&typed_deny);
        let use_token = account.on_token();
        let token_name = account.name(&typed_name).unwrap_or_default();
        let placeholder = sbx::named_token_placeholder(&token_name);
        // Picking the setup token swaps the backend kit, not just a setting on this one.
        let backend = match use_token {
            true => match catalog.for_account(Account::Token) {
                Some(b) => b,
                None => {
                    self.set_error("this checkout ships no setup-token kit");
                    return;
                }
            },
            false => &catalog.backends[backend_idx],
        };
        let label = match typed_label {
            (typed, account) if typed.is_empty() => {
                model::auto_label(&catalog, &backend.kit.name, account.as_deref(), &addons)
            }
            (typed, _) => typed,
        };

        let taken: Vec<String> = self.sandboxes.iter().map(|s| s.name.clone()).collect();
        let name = model::unique_sandbox_name(&backend.kit.name, &workspace, &taken);

        let model::KitMounts {
            mounts: mut ro_mounts,
            notes,
        } = model::kit_mounts(&addons);
        let mut folder_plan = self.folder_plan(&workspace);
        let home = model::home_dir();
        let picked = typed_folders.iter().map(String::as_str);
        let (extra, problems) = folders::extras_of(picked, &folder_plan, home.as_deref());
        if let Some(problem) = problems.into_iter().next() {
            self.set_error(problem);
            return;
        }
        let extra_folders: Vec<String> = extra.iter().map(folders::Folder::sbx_spec).collect();
        folder_plan.folders.extend(extra);
        ro_mounts.splice(
            0..0,
            git_guard.then(|| folders::git_guard(&workspace)).flatten(),
        );
        ro_mounts.extend(folder_plan.sbx_specs());
        // Only fires when a host cache folder went away after the form opened.
        let notes: Vec<String> = model::HOST_JVM_KITS
            .iter()
            .filter(|kit| addons.iter().any(|a| a == **kit))
            .filter(|kit| model::host_cache_mounts(&[kit.to_string()]).is_empty())
            .map(|kit| format!("{kit}: no host caches found — creating without its mounts"))
            .chain(notes)
            .chain(folder_plan.notes.iter().cloned())
            .collect();
        if !notes.is_empty() {
            self.set_error(notes.join("; "));
        }

        let conn = match &new_conn {
            Some((conn, _)) => Some(conn.clone()),
            None => litellm
                .as_deref()
                .and_then(|n| crate::litellm::load_in(&self.litellm_root, n)),
        };
        // The picks first: the connection's kit is written from them, and they are the next
        // form's defaults.
        if let Some((conn, agent, picks)) = &picks {
            self.save_picks(conn, *agent, picks);
        }
        let Some(kit_dirs) = self.kit_dirs(
            &name,
            &backend.kit.name,
            &addons,
            use_token.then_some(token_name.as_str()),
            &models,
            conn.as_ref(),
        ) else {
            self.set_error("could not write a generated kit for this sandbox");
            return;
        };
        let Some(kit_dirs) = self.with_folder_kit(kit_dirs, &name, &folder_plan, git_guard) else {
            self.set_error("could not write a generated kit for this sandbox");
            return;
        };
        let argv = sbx::create_argv(sbx::Create {
            name: &name,
            agent: &backend.agent,
            agent_kit: backend.kit.is_workload.then_some(backend.kit.dir.as_path()),
            kit_dirs: &kit_dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
            workspace: &workspace,
            mounts: &ro_mounts,
            clone,
            ports: &ports,
            skills,
            deny_network: &deny_network,
            static_mcp: &static_mcp,
            // None: `sbx::create_argv` passes the default limits.
            cpus: None,
            memory: None,
            template: None,
        });

        // The token must be stored before `sbx create`, which builds the container around it.
        let mut steps = Vec::new();
        if account == AccountChoice::New {
            steps.push(Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_named_token_argv(&placeholder),
                token,
            ));
        }
        steps.push(Step::new(format!("sbx create {name}"), argv).noting(BUILD_NOTE));

        // On failure the form comes back on its review, without the secrets.
        let restore = match std::mem::replace(&mut self.screen, Screen::List) {
            Screen::Create(mut form) => {
                form.connection.new_conn.key = TextInput::default();
                form.connection.token = TextInput::default();
                form.wizard_set_step(&catalog, CreateStep::Review);
                Some(form)
            }
            _ => None,
        };
        // After the key is stored the connection exists: restore the form on it.
        let restore_after_key = match (&restore, &new_conn) {
            (Some(form), Some((conn, _))) => {
                let mut form = form.clone();
                form.connection.connections.push(conn.clone());
                form.connection
                    .connections
                    .sort_by(|a, b| a.name.cmp(&b.name));
                form.connection.litellm = Some(conn.name.clone());
                Some(Restore::Create(Box::new(form)))
            }
            _ => None,
        };
        let create_restore = match new_conn.is_some() {
            true => restore_after_key,
            false => restore.clone().map(|form| Restore::Create(Box::new(form))),
        };
        let create = PendingJob {
            title: format!("Creating {label}"),
            steps,
            post: PostJob {
                register: Some(Registered {
                    name: name.clone(),
                    label,
                    clone,
                    skills,
                    start_dir: start_dir.clone(),
                    static_mcp: static_mcp.clone(),
                    extra_folders,
                }),
                auth: use_token.then(|| AuthRecord::named(&name, &token_name)),
                attach_start: attach
                    .then(|| Some(workspace.join(start_dir.as_ref()?)))
                    .flatten(),
                // The create drops any selection left under this name, so only the addons count.
                attach_skill_steps: match attach {
                    true => skill_source::install_steps_with(&name, &addons, false),
                    false => Vec::new(),
                },
                attach_config: attach
                    .then(|| {
                        let agent = crate::litellm::Agent::of_backend(&backend.kit.name)
                            .filter(|a| *a == crate::litellm::Agent::Claude)?;
                        crate::litellm::push_argv_in(
                            &self.litellm_root,
                            &name,
                            conn.as_ref()?,
                            agent,
                        )
                    })
                    .flatten(),
                attach: attach.then_some(name),
                // Remembered only once the create succeeded (`apply_post`).
                last_litellm: litellm,
                defaults: Some(defaults),
                restore: create_restore,
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        };
        let (Some((conn, key)), Some(agent)) = (new_conn, backend.litellm_agent()) else {
            self.start_job(create);
            return;
        };
        // The key first; `App::start_next` writes the file and kit, then starts the create.
        let placeholder = crate::litellm::placeholder(&conn.name);
        self.start_job(PendingJob {
            title: format!("Storing the key of LiteLLM connection {}", conn.name),
            steps: vec![Step::with_stdin(
                format!("sbx secret set-custom ({placeholder})"),
                sbx::secret_set_litellm_key_argv(crate::litellm::host(&conn.url), &placeholder),
                key,
            )],
            post: PostJob {
                save_connection: Some((conn, Some(agent))),
                then: Some(Box::new(create)),
                restore: restore.map(|form| Restore::Create(Box::new(form))),
                ..Default::default()
            },
            fallback: None,
            fallback_hint: None,
        });
    }

    fn submit_kits(&mut self) {
        let catalog = Arc::clone(&self.catalog);
        let Screen::Kits(form) = &self.screen else {
            return;
        };
        let name = form.sandbox.clone();
        let other_checkout = self
            .sandboxes
            .iter()
            .find(|s| s.name == name)
            .and_then(|s| s.other_checkout.clone());
        let effect = form.effect(&catalog, other_checkout.is_some());
        if effect == KitsEffect::NoChanges {
            self.screen = Screen::List;
            self.set_info("no kit changes");
            return;
        }
        let wanted: Vec<String> = catalog
            .addons
            .iter()
            .zip(&form.addons)
            .filter(|(_, on)| **on)
            .map(|(a, _)| a.kit.name.clone())
            .collect();
        let added: Vec<String> = catalog
            .addons
            .iter()
            .enumerate()
            .filter(|(i, _)| form.addons[*i] && !form.original[*i])
            .map(|(_, a)| a.kit.name.clone())
            .collect();
        let removed: Vec<String> = catalog
            .addons
            .iter()
            .enumerate()
            .filter(|(i, _)| !form.addons[*i] && form.original[*i])
            .map(|(_, a)| a.kit.name.clone())
            .collect();
        let models = form.models.clone();
        let models_changed = form.models_changed();
        let models_rebuild = models_changed && form.had_models();
        let models_added = models_changed && !form.had_models();

        self.screen = Screen::List;

        if let KitsEffect::Rebuild(_) = effect {
            let mut body = Vec::new();
            if let Some(dir) = &other_checkout {
                body.push(format!(
                    "Its kits come from {} — rebuilding re-points them at this checkout.",
                    model::tilde(dir)
                ));
            }
            if !removed.is_empty() {
                body.push(format!(
                    "Removing {} — sbx cannot detach a kit, so the sandbox is rebuilt without it.",
                    join_labels(&removed)
                ));
            }
            if !added.is_empty() {
                body.push(format!("Adding {}.", join_labels(&added)));
            }
            if models_rebuild {
                body.push(format!(
                    "Pinning Claude Code's models to {} — sbx bakes a kit's environment variables \
                     into the container when it builds one, so changing them rebuilds it.",
                    match models.is_empty() {
                        true => "Claude Code's own defaults".to_string(),
                        false => models.summary(),
                    }
                ));
            }
            match self.recreate_confirm(&name, &wanted, Some(&models), "Apply kit changes", body) {
                Some(c) => self.modal = Some(Modal::Confirm(c)),
                None => self.set_error(self.rebuild_refusal_text(&name)),
            }
            return;
        }

        // Tried even where sbx refuses today, so it works once sbx lifts the limit.
        let added_dirs: Vec<PathBuf> = added
            .iter()
            .filter_map(|a| catalog.addon(a))
            .map(|a| a.kit.dir.clone())
            .collect();
        self.approve_credentials(&added_dirs);
        let mut steps: Vec<Step> = added
            .iter()
            .filter_map(|a| catalog.addon(a))
            .map(|a| {
                Step::new(
                    format!("sbx kit add {name} {}", a.kit.name),
                    sbx::kit_add_argv(&name, &a.kit.dir),
                )
            })
            .collect();
        if models_added {
            // The editor opened on this sandbox's connection's models (`action_edit_kits`).
            let Some(dir) = self.models_kit(&name, &models, &catalog.models) else {
                return;
            };
            steps.push(Step::new(
                format!("sbx kit add {name} (Claude models)"),
                sbx::kit_add_argv(&name, &dir),
            ));
        }

        let fallback = self
            .recreate_job(&name, &wanted, Some(&models))
            .map(|mut j| {
                j.title = format!("Rebuilding {name} with the new kits");
                Box::new(j)
            });

        let what = match (added.is_empty(), models_added) {
            (true, _) => "the model pins".to_string(),
            (false, true) => format!("{} and the model pins", join_labels(&added)),
            (false, false) => join_labels(&added),
        };
        self.start_job(PendingJob {
            title: format!("Adding {what} to {name}"),
            steps,
            post: PostJob {
                rebaseline: Some(name.clone()),
                ..Default::default()
            },
            fallback_hint: Some("sbx refuses to add a kit that ships files.".into()),
            fallback,
        });
    }

    /// Every `--kit` a sandbox is built from, backend first: its backend, the addons it was given,
    /// and the generated kits for a named setup token, a LiteLLM connection or model pins.
    fn kit_dirs(
        &self,
        sandbox: &str,
        backend: &str,
        addons: &[String],
        token: Option<&str>,
        models: &Overrides,
        litellm: Option<&crate::litellm::Connection>,
    ) -> Option<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        // The catalog the pins are named from: the connection's, on a LiteLLM backend.
        let mut pin_catalog = None;
        if let Some(b) = self.catalog.backend(backend) {
            dirs.push(b.kit.dir.clone());
            // The connection's kit names the gateway; without a connection, nothing is built.
            if let Some(agent) = b.litellm_agent() {
                let Some(conn) = litellm else {
                    let _ = self.notice_tx.send((
                        true,
                        "its LiteLLM connection no longer exists — pick another, or add it with \
                         `aisbx litellm add <file> --key-stdin`"
                            .into(),
                    ));
                    return None;
                };
                pin_catalog = Some(self.connection_models(conn).0);
                // A connection still being made is named here and written once its key is stored.
                if !crate::litellm::exists_in(&self.litellm_root, &conn.name) {
                    dirs.push(crate::litellm::kit_dir_in(
                        &self.litellm_root,
                        &conn.name,
                        agent,
                    ));
                } else {
                    match crate::litellm::ensure_kit_in(&self.litellm_root, conn, agent) {
                        Ok(dir) => dirs.push(dir),
                        Err(e) => {
                            let _ = self.notice_tx.send((true, format!("{e:#}")));
                            return None;
                        }
                    }
                }
            }
        }
        for addon in self.catalog.all_addons() {
            if addons.iter().any(|a| a == &addon.kit.name) {
                dirs.push(addon.kit.dir.clone());
            }
        }
        if let Some(token) = token {
            match tokens::ensure_kit(token) {
                Ok(dir) => dirs.push(dir),
                // Without it the sandbox would authenticate as nobody, so build nothing.
                Err(e) => {
                    let _ = self.notice_tx.send((true, format!("{e:#}")));
                    return None;
                }
            }
        }
        // No pins: remove a stale model kit, so the rebuild drops the variables. Only Claude
        // Code's slots ride the kit; the other agents' are in their config.
        if models.entries().any(|(slot, _)| slot.is_claude()) {
            let catalog = pin_catalog.as_deref().unwrap_or(&self.catalog.models);
            dirs.push(self.models_kit(sandbox, models, catalog)?);
        } else if let Err(e) = models::remove_kit(sandbox) {
            let _ = self.notice_tx.send((true, format!("{e:#}")));
        }
        self.approve_credentials(&dirs);
        Some(dirs)
    }

    /// `dirs` and the kit that tells the agent of `sandbox` about its host folders and its
    /// read-only `.git` ([`folders::ensure_kit`]): without it they are mounts nobody explains.
    /// `None`: the kit could not be written, and the caller must build nothing.
    fn with_folder_kit(
        &self,
        mut dirs: Vec<PathBuf>,
        sandbox: &str,
        plan: &folders::Plan,
        git_guard: bool,
    ) -> Option<Vec<PathBuf>> {
        match folders::ensure_kit(sandbox, &plan.folders, git_guard) {
            Ok(Some(dir)) => dirs.push(dir),
            Ok(None) => {}
            Err(e) => {
                let _ = self.notice_tx.send((true, format!("{e:#}")));
                return None;
            }
        }
        Some(dirs)
    }

    /// Write a sandbox's model kit and hand back the directory to stack. `None`: nothing was
    /// written, and the caller must build nothing.
    fn models_kit(
        &self,
        sandbox: &str,
        models: &Overrides,
        catalog: &[models::Model],
    ) -> Option<PathBuf> {
        match models::ensure_kit(sandbox, models, catalog) {
            Ok(Some(dir)) => Some(dir),
            Ok(None) => {
                let _ = self.notice_tx.send((
                    true,
                    "no models are pinned, so there is nothing to apply".into(),
                ));
                None
            }
            Err(e) => {
                let _ = self.notice_tx.send((true, format!("{e:#}")));
                None
            }
        }
    }

    /// `sbx rm` + `sbx create` for an existing sandbox with a (possibly new) addon set. `models`:
    /// `None` keeps the pins the sandbox has today.
    fn recreate_job(
        &self,
        name: &str,
        addons: &[String],
        models: Option<&Overrides>,
    ) -> Option<PendingJob> {
        self.recreate_plan(name, addons, models, None)
            .map(|p| p.job)
    }

    /// [`App::recreate_job`], with the sandbox's backend and token kept, and — where the caller
    /// says so — its detached mode changed. `keep_running: None` keeps whatever it has.
    fn recreate_plan(
        &self,
        name: &str,
        addons: &[String],
        models: Option<&Overrides>,
        keep_running: Option<bool>,
    ) -> Option<RebuildPlan> {
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        let backend = sb.backend.clone()?;
        // Keep whatever account it is on: a kit edit is not a place to lose one.
        let token = sb
            .auth
            .as_ref()
            .and_then(|a| a.token_name())
            .map(str::to_string);
        let models = models.cloned().unwrap_or_else(|| sb.models.clone());
        self.rebuild_plan(
            name,
            &backend,
            addons,
            token.as_deref(),
            &models,
            keep_running,
            None,
        )
    }

    /// The same, onto a named backend and a named token — which is how changing a sandbox's
    /// Anthropic account works, since neither of those can be changed on a live container.
    fn recreate_job_as(
        &self,
        name: &str,
        backend: &str,
        addons: &[String],
        token: Option<&str>,
        models: &Overrides,
    ) -> Option<PendingJob> {
        self.rebuild_plan(name, backend, addons, token, models, None, None)
            .map(|p| p.job)
    }

    /// Why a rebuild of this sandbox must not start, if it must not: its workspace folder is gone
    /// from the host. A mountless sandbox has no folder to lose.
    pub fn rebuild_blocker(&self, name: &str) -> Option<&'static str> {
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        (sb.workspace_missing && !sb.mountless()).then_some(WORKSPACE_GONE)
    }

    /// Every reason a rebuild is refused, worded for the status line: the folder is gone, or the
    /// LiteLLM connection the sandbox is on no longer exists.
    pub fn rebuild_blocker_text(&self, name: &str) -> Option<String> {
        if let Some(reason) = self.rebuild_blocker(name) {
            return Some(reason.to_string());
        }
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        let litellm_backend = sb
            .backend
            .as_deref()
            .and_then(|b| self.catalog.backend(b))
            .is_some_and(|b| b.litellm_agent().is_some());
        if !litellm_backend {
            return None;
        }
        match sb.litellm.as_deref() {
            None => {
                Some("this sandbox is on no LiteLLM connection — nothing to rebuild it onto".into())
            }
            Some(conn) if !crate::litellm::exists_in(&self.litellm_root, conn) => Some(format!(
                "LiteLLM connection {conn} no longer exists — add it again with `aisbx litellm add`"
            )),
            Some(_) => None,
        }
    }

    /// [`App::rebuild_blocker_text`] for a move onto another connection: the workspace folder and
    /// an unmapped backend still refuse it; the sandbox's own connection being gone does not.
    fn move_blocker(&self, name: &str) -> Option<String> {
        self.rebuild_blocker(name).map(str::to_string)
    }

    /// The error for a rebuild that could not be planned — the blocker, where there is one.
    fn rebuild_refusal(&self, name: &str) -> &'static str {
        self.rebuild_blocker(name)
            .unwrap_or("could not build a rebuild plan for this sandbox")
    }

    /// [`App::rebuild_refusal`] with every reason [`App::rebuild_blocker_text`] knows.
    fn rebuild_refusal_text(&self, name: &str) -> String {
        self.rebuild_blocker_text(name)
            .unwrap_or_else(|| self.rebuild_refusal(name).to_string())
    }

    /// Everything a rebuild does, and what the confirmation in front of it has to say about it.
    /// `target`: the LiteLLM connection an `A` move rebuilds onto; `None` keeps the sandbox's.
    /// `None` when a blocker refuses the rebuild.
    #[allow(clippy::too_many_arguments)]
    fn rebuild_plan(
        &self,
        name: &str,
        backend: &str,
        addons: &[String],
        token: Option<&str>,
        models: &Overrides,
        keep_running: Option<bool>,
        target: Option<&crate::litellm::Connection>,
    ) -> Option<RebuildPlan> {
        // A move is wanted when the own connection is gone, so only the other blockers apply.
        let blocked = match target {
            None => self.rebuild_blocker_text(name),
            Some(_) => self.move_blocker(name),
        };
        if blocked.is_some() {
            return None;
        }
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        let backend_kit = self.catalog.backend(backend)?;
        let agent = backend_kit.agent.clone();
        // A workload backend is the agent positional (see `sbx::Create`).
        let agent_kit = backend_kit
            .kit
            .is_workload
            .then(|| backend_kit.kit.dir.clone());
        let model::KitMounts {
            mounts: mut ro_mounts,
            notes: mut mount_notes,
        } = model::kit_mounts(addons);
        // The host folders as the lists say now, those picked for this sandbox alone, and the
        // `.git` guard the sandbox has.
        let git_guard = folders::has_git_guard(&sb.workspace, &sb.ro_mounts);
        let mut folder_plan = self.folder_plan(&sb.workspace);
        let recorded = self.registry.extra_folders(name);
        let home = model::home_dir();
        let (extra, problems) = folders::extras_of(
            recorded.iter().map(String::as_str),
            &folder_plan,
            home.as_deref(),
        );
        folder_plan.folders.extend(extra);
        mount_notes.extend(problems);
        ro_mounts.splice(
            0..0,
            git_guard
                .then(|| folders::git_guard(&sb.workspace))
                .flatten(),
        );
        ro_mounts.extend(folder_plan.sbx_specs());
        mount_notes.extend(folder_plan.notes.iter().cloned());
        // Regenerate the connection kit from `connection.json`, or write the move target's.
        let conn = match target {
            Some(t) => Some(t.clone()),
            None => sb
                .litellm
                .as_deref()
                .and_then(|n| crate::litellm::load_in(&self.litellm_root, n)),
        };
        let kit_dirs = self.kit_dirs(name, backend, addons, token, models, conn.as_ref())?;
        let kit_dirs = self.with_folder_kit(kit_dirs, name, &folder_plan, git_guard)?;
        // Volumes in RAM stay in RAM: each kit goes in as its copy again, the backend first.
        let (kit_dirs, agent_kit) = match sb.tmpfs_volumes {
            false => (kit_dirs, agent_kit),
            true => match crate::tmpfs::copies(&kit_dirs) {
                Ok(copies) => {
                    let agent_kit = agent_kit.map(|_| copies[0].clone());
                    (copies, agent_kit)
                }
                Err(e) => {
                    let _ = self.notice_tx.send((true, format!("{e:#}")));
                    return None;
                }
            },
        };
        // Repeat the ports and the skills mode; an unrecorded mode passes no `--skills`.
        let ports = sbx::publish_specs(&sb.ports);
        let skills = sb.skills.and_then(model::Skills::mode);
        // A failed read means no rules, not a failed rebuild.
        let rules = sbx::policy_rules(name).unwrap_or_default();
        let deny_network = sbx::deny_specs(&rules, name);
        let readd = sbx::rules_to_readd(&rules, name);
        let approved = readd
            .iter()
            .filter(|r| r.provenance.created_via == "approval")
            .count();
        let rule_steps: Vec<Step> = readd
            .iter()
            .flat_map(|r| sbx::rule_argvs(r, name))
            .map(|argv| {
                Step::new(
                    format!("sbx policy {} network --sandbox {name}", argv[2]),
                    argv,
                )
            })
            .collect();
        let static_mcp = self.registry.static_mcp(name);
        let argv = sbx::create_argv(sbx::Create {
            name,
            agent: &agent,
            agent_kit: agent_kit.as_deref(),
            kit_dirs: &kit_dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
            workspace: &sb.workspace,
            mounts: &ro_mounts,
            clone: sb.clone,
            ports: &ports,
            skills,
            deny_network: &deny_network,
            static_mcp: &static_mcp,
            cpus: sb.cpus,
            memory: sb.memory_spec.as_deref(),
            template: None,
        });
        let keep = keep_running.unwrap_or(sb.keep_running);
        let mut steps = vec![
            Step::new("sbx rm --force", sbx::rm_argv(name)),
            Step::new(format!("sbx create {name}"), argv).noting(BUILD_NOTE),
        ];
        if keep {
            steps.push(Step::new(
                "sbx run --detach (keeps running)",
                sbx::keep_running_argv(name),
            ));
        }
        if !sb.runtime_mounts.is_empty() {
            if !keep {
                steps.push(Step::new(
                    "make sure it is running for sbx mount",
                    sbx::exec_argv(name, &["true"]),
                ));
            }
            steps.extend(
                sb.runtime_mounts.iter().map(|m| {
                    Step::new(format!("sbx mount {}", m.spec()), sbx::mount_argv(name, m))
                }),
            );
        }
        let readded = rule_steps.len();
        steps.extend(rule_steps);
        Some(RebuildPlan {
            job: PendingJob {
                title: format!("Rebuilding {name}"),
                steps,
                post: PostJob {
                    rebaseline: Some(name.to_string()),
                    ..Default::default()
                },
                fallback: None,
                fallback_hint: None,
            },
            denies: deny_network,
            readded,
            approved,
            keep_running: keep,
            mount_notes,
        })
    }

    fn recreate_confirm(
        &self,
        name: &str,
        addons: &[String],
        models: Option<&Overrides>,
        title: &str,
        body: Vec<String>,
    ) -> Option<Confirm> {
        self.recreate_confirm_with(name, addons, models, None, title, body)
    }

    /// The rebuild confirmation, with the detached mode changed where `keep_running` says so —
    /// which is how `b` takes a sandbox back out of it.
    fn recreate_confirm_with(
        &self,
        name: &str,
        addons: &[String],
        models: Option<&Overrides>,
        keep_running: Option<bool>,
        title: &str,
        body: Vec<String>,
    ) -> Option<Confirm> {
        let plan = self.recreate_plan(name, addons, models, keep_running)?;
        self.rebuild_confirm(name, plan, title, body)
    }

    /// The confirmation in front of a rebuild `plan`: `body`, then everything the rebuild keeps
    /// and loses.
    fn rebuild_confirm(
        &self,
        name: &str,
        plan: RebuildPlan,
        title: &str,
        mut body: Vec<String>,
    ) -> Option<Confirm> {
        let job = plan.job.clone();
        let sb = self.sandboxes.iter().find(|s| s.name == name)?;
        body.push(String::new());
        // One paragraph per entry: a sentence split over two entries reads as two.
        body.push(match sb.tmpfs_volumes {
            true => "The sandbox is rebuilt: what you installed in it and the agent's earlier \
                     sessions are lost. Its volumes are in RAM again."
                .into(),
            false if sb.history_in_volumes => {
                "The sandbox is rebuilt: what you installed in it is lost; the agent's earlier \
                 sessions stay."
                    .into()
            }
            false => "The sandbox is rebuilt: what you installed in it and the agent's earlier \
                      sessions are lost."
                .into(),
        });
        body.push(match sb.mountless() {
            true => format!(
                "It mounts no folder, so the files in {} inside it are lost too.",
                model::MOUNTLESS_WORKDIR
            ),
            false => format!("Your files in {} stay.", model::tilde(&sb.workspace)),
        });
        let ports = sbx::publish_specs(&sb.ports);
        if !ports.is_empty() {
            body.push(format!("Its ports come back: {}.", ports.join(", ")));
            body.push(
                "A port number sbx picked itself may be taken by then; the job says so if it is."
                    .into(),
            );
        } else if !sb.running {
            // sbx reports no ports for a stopped sandbox: nothing to carry over.
            body.push(
                "sbx shows no ports while a sandbox is stopped, so any it has are lost. Start it \
                 first to keep them."
                    .into(),
            );
        }
        match sb.skills {
            Some(model::Skills::Known(mode)) => body.push(format!(
                "The shared skills come back {}, as now.",
                match mode {
                    sbx::SkillsMode::Off => "off",
                    sbx::SkillsMode::Readonly => "read-only",
                    sbx::SkillsMode::Readwrite => "read-write",
                }
            )),
            Some(model::Skills::Unrecorded) => body.push(
                "How this sandbox gets the shared skills was never recorded, so it gets the \
                 default for new sandboxes (read-only unless you changed it)."
                    .into(),
            ),
            None => {}
        }
        // A limit sbx does not report becomes aisbx's default, which changes the sandbox: say so.
        let cpus_text = |n: u32| format!("{n} CPU{}", if n == 1 { "" } else { "s" });
        let default_cpus = cpus_text(sbx::default_cpus());
        let default_memory = format!("{} of memory", sbx::default_memory());
        match (sb.cpus, sb.memory.as_deref()) {
            (Some(cpus), Some(memory)) => body.push(format!(
                "It keeps its limits: {}, {memory} of memory.",
                cpus_text(cpus)
            )),
            (Some(cpus), None) => body.push(format!(
                "It keeps its limit of {}, and gets {default_memory} in place of sbx's automatic \
                 limit.",
                cpus_text(cpus)
            )),
            (None, Some(memory)) => body.push(format!(
                "It keeps its limit of {memory} of memory, and gets {default_cpus} in place of all \
                 of them."
            )),
            (None, None) => body.push(format!(
                "It gets {default_cpus} and {default_memory}, in place of sbx's automatic limits \
                 (all CPUs, half the memory)."
            )),
        }
        body.extend(
            plan.mount_notes
                .iter()
                .map(|note| format!("Not mounted: {note}.")),
        );
        if !sb.runtime_mounts.is_empty() {
            body.push(format!(
                "The folders added with sbx mount are mounted again: {}.",
                sb.runtime_mounts
                    .iter()
                    .map(|m| model::tilde(Path::new(&m.host_path)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !plan.denies.is_empty() || plan.readded > 0 {
            let mut parts = Vec::new();
            if !plan.denies.is_empty() {
                parts.push(format!("{} blocked host(s)", plan.denies.len()));
            }
            if plan.readded > plan.approved {
                parts.push(format!(
                    "{} rule(s) added by hand",
                    plan.readded - plan.approved
                ));
            }
            if plan.approved > 0 {
                parts.push(format!("{} approved destination(s)", plan.approved));
            }
            body.push(format!(
                "Its own network rules come back: {}.",
                parts.join(", ")
            ));
        }
        let static_mcp = self.registry.static_mcp(name);
        if sbx::is_no_mcp(&static_mcp) {
            body.push(
                "It comes back with no MCP servers, as it was created. A server added since with \
                 sbx mcp load is not kept."
                    .into(),
            );
        } else if !static_mcp.is_empty() {
            body.push(format!(
                "It comes back with the MCP servers it was created with: {}. A server added or \
                 removed since with sbx mcp is not kept.",
                static_mcp.join(", ")
            ));
        }
        match (sb.keep_running, plan.keep_running) {
            (true, true) => body.push("It keeps running after its sessions end, as now.".into()),
            (true, false) => body.push("It then stops after its last session.".into()),
            _ => {}
        }
        match &sb.auth {
            Some(auth) if auth.on_token() => {
                body.push(String::new());
                body.push(format!(
                    "It stays on the setup token “{}”.",
                    auth.token_name().unwrap_or(DEFAULT_TOKEN_NAME)
                ));
            }
            Some(_) => {
                body.push(String::new());
                body.push("It stays on the /login.".into());
            }
            None => {}
        }
        Some(Confirm {
            title: title.into(),
            body,
            danger: true,
            confirm_label: "Rebuild".into(),
            yes: false,
            job,
        })
    }

    fn start_job(&mut self, pending: PendingJob) {
        // Never run the steps under test: they would create sandboxes and overwrite host secrets.
        let steps = if cfg!(any(test, feature = "test-support")) {
            Vec::new()
        } else {
            pending.steps.clone()
        };
        let job = Job::spawn(pending.title.clone(), steps);
        self.modal = Some(Modal::Running(job, pending));
    }
}

#[cfg(any(test, feature = "test-support"))]
impl App {
    /// An app wired up without touching sbx, for rendering and input tests — and without the
    /// registry of whoever runs them: it starts empty ([`Registry::empty`]).
    pub fn for_test(catalog: Catalog, sandboxes: Vec<Sandbox>) -> Self {
        let (tx, rx) = channel();
        let (insight_tx, insight_rx) = channel();
        let (notice_tx, notice_rx) = channel();
        let (fetch_tx, fetch_rx) = channel();
        let mut app = Self {
            catalog: Arc::new(catalog),
            registry: Registry::empty(),
            cwd: PathBuf::from("/proj"),
            cells: vec![Cell::New],
            here: 0,
            elsewhere: 0,
            show_other: true,
            sandboxes,
            cursor: 0,
            find: None,
            screen: Screen::List,
            modal: None,
            status: None,
            loading: false,
            quit: false,
            handoff: None,
            sbx_version: "v0.51.0".into(),
            // The production default, so the tests exercise the path a real start takes.
            auto_update: true,
            // One connection, `acme`, in a scratch folder: never the user's.
            litellm_root: crate::testkit::litellm_root(),
            host_home: Some(crate::testkit::host_home()),
            mounts_state: mounts_state(),
            tab: Tab::Overview,
            detail_scroll: 0,
            detail_full: false,
            help_scroll: 0,
            insight: Insight::default(),
            histories: Histories::default(),
            grid_scroll: 0,
            grid_rows: 1,
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            policy_needs_init: None,
            approvals: Vec::new(),
            // `acme`'s key, stored for its host — so no form warns about a missing one.
            secrets: Some(sbx::Secrets {
                services: Vec::new(),
                custom: vec![crate::testkit::litellm_key("acme", "litellm.example.com")],
            }),
            // Fixed, not read from the host: renders must not depend on `sbx setup ssh` there.
            ssh_ready: true,
            mouse: true,
            hits: Vec::new(),
            last_click: None,
            create_draft: None,
            tx,
            rx,
            insight_tx,
            insight_rx,
            notice_tx,
            notice_rx,
            fetchers: Vec::new(),
            models_conn: None,
            due_fetches_done: false,
            fetch_tx,
            fetch_rx,
            background_fetches: Vec::new(),
            refreshing: false,
            refresh_pending: false,
            last_refresh: Instant::now(),
        };
        app.recompute_cells(None);
        app
    }

    pub fn open_create(&mut self) {
        self.open_new_form();
    }

    /// Open the kits editor on a sandbox, for the tests that assert on what applying it builds.
    pub fn open_kits(&mut self, name: &str) {
        let Some(i) = self
            .cells
            .iter()
            .position(|c| matches!(c, Cell::Sandbox(i) if self.sandboxes[*i].name == name))
        else {
            panic!("no such sandbox: {name}");
        };
        self.cursor = i;
        self.action_edit_kits();
    }

    /// Re-split the list after a test moved `cwd` or the sandboxes under it.
    pub fn refresh_cells(&mut self) {
        self.recompute_cells(None);
    }
}

/// What the approvals screen is about, said once for all its requests: why one is held, and
/// what each answer does with it — the TUI's strip under *Close*, the GUI's subtitle.
pub const APPROVALS_NOTE: &str = "A sandbox asks before it reaches a host that no rule allows. \
    Allow lets that sandbox reach the host from now on; the agent has to try again. Dismiss keeps it \
    blocked, and the sandbox asks again the next time.";

/// The approvals screen with nothing waiting: where a request shows when one comes.
pub const APPROVALS_EMPTY: &str = "Nothing waits for an answer. A request shows here, on its \
    sandbox's row (⚑) and in the header.";

/// Move a form's focus one row up (`delta < 0`) or down (`delta > 0`), treating the buttons a form
/// ends with as a **single** row.
/// The file system root above `start`, and `start` relative to it: where the host folder picker
/// browses from, and the level it opens on.
fn host_root(start: &Path) -> (PathBuf, PathBuf) {
    let root = start.ancestors().last().unwrap_or(start).to_path_buf();
    let rel = start
        .strip_prefix(&root)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    (root, rel)
}

fn step_form(focus: usize, len: usize, buttons: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let first = len.saturating_sub(buttons);
    if focus >= first {
        return match delta > 0 {
            true => 0,
            false => first.saturating_sub(1),
        };
    }
    match delta > 0 {
        true => focus + 1,
        false if focus == 0 => len - 1,
        false => focus - 1,
    }
}

/// Move within the line of buttons a form ends with — the ←→ half of [`step_form`].
/// `None` while the focus is on any other row: the key then does nothing.
fn step_button(focus: usize, len: usize, buttons: usize, delta: isize) -> Option<usize> {
    let first = len.checked_sub(buttons)?;
    if buttons < 2 || focus < first {
        return None;
    }
    let at = focus - first;
    Some(first + (at + buttons.wrapping_add_signed(delta)) % buttons)
}

/// Apply a key to a focused text field. Returns whether the field consumed it — anything it does
/// not consume is navigation and belongs to the form.
pub(crate) fn edit_text(field: &mut TextInput, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if !ctrl => field.insert(c),
        KeyCode::Char('a') => field.home(),
        KeyCode::Char('e') => field.end(),
        KeyCode::Char('u') => field.delete_to_start(),
        KeyCode::Char('k') => field.delete_to_end(),
        KeyCode::Char('w') => field.delete_word(),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Delete => field.delete(),
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        _ => return false,
    }
    true
}

/// Split the create form's port field into individual `-p` arguments. Commas and whitespace
/// both separate; neither appears in sbx's port grammar.
pub(crate) fn split_ports(typed: &str) -> Vec<String> {
    typed
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Does this look like `[[HOST_IP:]HOST_PORT:]SANDBOX_PORT[/PROTOCOL]`? A shape check that errs
/// towards accepting: sbx judges whether a mapping can be bound.
pub(crate) fn port_spec_ok(spec: &str) -> bool {
    let (addr, proto) = match spec.split_once('/') {
        Some((addr, proto)) => (addr, Some(proto)),
        None => (spec, None),
    };
    if proto.is_some_and(|p| {
        !["tcp", "tcp4", "tcp6", "udp", "udp4", "udp6"].contains(&p.to_ascii_lowercase().as_str())
    }) {
        return false;
    }
    let port_ok = |p: &str| p.parse::<u16>().is_ok_and(|n| n > 0);
    // Read only the tails as numbers: an IPv6 host address brings colons of its own.
    match addr.rsplit_once(':') {
        // A bare sandbox port: sbx assigns an ephemeral host port.
        None => port_ok(addr),
        Some((head, sandbox_port)) => {
            port_ok(sandbox_port)
                && match head.rsplit_once(':') {
                    // `HOST_IP:HOST_PORT:SANDBOX_PORT` — the address itself is sbx's to judge.
                    Some((ip, host_port)) => !ip.is_empty() && port_ok(host_port),
                    // `HOST_PORT:SANDBOX_PORT`.
                    None => port_ok(head),
                }
        }
    }
}

/// Is this something `--deny-network` will take? A host, a wildcard pattern, a CIDR or an IP,
/// optionally with `:port`. Permissive on purpose: sbx judges what a rule means.
pub(crate) fn deny_host_ok(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.contains("://")
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "*.-_:/[]".contains(c))
}

/// Is this sandbox on that named token?
fn on_token(sb: &Sandbox, token: &str) -> bool {
    sb.auth
        .as_ref()
        .is_some_and(|a| a.token_name() == Some(token))
}

/// "a", "a and b", "a, b and c" — for prose about the other sandboxes a shared token reaches.
fn join_names(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => format!("“{}”", names[0]),
        _ => {
            let quoted: Vec<String> = names.iter().map(|n| format!("“{n}”")).collect();
            format!(
                "{} and {}",
                quoted[..quoted.len() - 1].join(", "),
                quoted[quoted.len() - 1]
            )
        }
    }
}

fn join_labels(kits: &[String]) -> String {
    let labels: Vec<String> = kits
        .iter()
        .map(|k| crate::kits::short_kit_label(k))
        .collect();
    match labels.len() {
        0 => String::new(),
        1 => labels[0].clone(),
        _ => format!(
            "{} and {}",
            labels[..labels.len() - 1].join(", "),
            labels[labels.len() - 1]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kits::{Catalog, HOST_SKILLS};

    #[test]
    fn the_test_form_finds_every_host_cache_whatever_the_runner_has() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        assert_ne!(app.host_home, model::home_dir(), "never the runner's home");
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        let detected: Vec<(&str, bool)> = app
            .catalog
            .addons
            .iter()
            .zip(&form.kits.host)
            .filter_map(|(a, d)| d.as_ref().map(|d| (a.kit.name.as_str(), d.detected())))
            .collect();
        assert!(detected.len() >= 5, "{detected:?}");
        assert!(detected.iter().all(|(_, found)| *found), "{detected:?}");
    }

    /// An app whose refreshes start but never land, and the sender that lands one by hand.
    fn polling() -> (App, Sender<Result<Snapshot>>, Receiver<Result<Snapshot>>) {
        let mut a = App::for_test(Catalog::load().unwrap(), Vec::new());
        let (tx, held) = channel();
        let answer = std::mem::replace(&mut a.tx, tx);
        a.last_refresh = Instant::now() - AUTO_REFRESH - Duration::from_millis(1);
        a.tick();
        assert!(a.refreshing, "the poll started");
        (a, answer, held)
    }

    #[test]
    fn the_poll_shows_its_mark_only_once_it_is_slow() {
        let (mut a, _answer, _held) = polling();
        assert!(!a.loading, "a poll starts without the mark");
        a.tick();
        assert!(!a.loading, "nor shows it while it is quick");
        a.last_refresh = Instant::now() - SLOW_REFRESH - Duration::from_millis(1);
        a.tick();
        assert!(a.loading, "a slow poll shows it");
    }

    #[test]
    fn a_refresh_asked_for_during_a_poll_shows_its_mark_until_it_lands() {
        let (mut a, answer, _held) = polling();
        a.refresh();
        assert!(
            a.loading && a.refresh_pending,
            "it waits for the poll and says so"
        );
        answer.send(Err(anyhow::anyhow!("stale"))).unwrap();
        a.tick();
        assert!(
            a.refreshing && a.loading,
            "the asked-for refresh runs, shown"
        );
    }

    const SB: &str = "claude-proj";

    /// The global row that holds a named token's value.
    fn stored_token(token: &str) -> sbx::CustomSecret {
        sbx::CustomSecret {
            scope: sbx::GLOBAL_SCOPE.into(),
            targets: vec![sbx::TOKEN_HOST.into()],
            env: String::new(),
            placeholder: sbx::named_token_placeholder(token),
            masked: "sk-ant***".into(),
        }
    }

    fn on_named(token: &str, stored: bool) -> Auth {
        Auth::Token {
            name: token.into(),
            placeholder: sbx::named_token_placeholder(token),
            stored,
        }
    }

    /// A second sandbox already on a named token.
    fn peer_on(app: &mut App, name: &str, token: &str) {
        let mut sb = model::sample(name, Some("claude-token"), &[]);
        sb.workspace = PathBuf::from("/proj");
        sb.auth = Some(on_named(token, true));
        app.sandboxes.push(sb);
    }

    /// One Claude sandbox on the given account, with the secret store it implies.
    fn app_with_auth(current: Auth, custom: Vec<sbx::CustomSecret>) -> App {
        let backend = match current.on_token() {
            true => "claude-token",
            false => "claude-anthropic",
        };
        let mut sb = model::sample(SB, Some(backend), &[]);
        sb.workspace = PathBuf::from("/proj");
        sb.auth = Some(current);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.secrets = Some(sbx::Secrets {
            services: Vec::new(),
            custom,
        });
        app
    }

    /// Open the auth editor over the selected sandbox and fill it in.
    fn planned(app: &mut App, account: AccountChoice, token: &str, name: &str) -> AuthPlan {
        app.action_edit_auth();
        let Screen::AuthEdit(form) = &mut app.screen else {
            panic!("the auth editor is open")
        };
        form.account = account;
        form.token = TextInput::new(token);
        form.token_name = TextInput::new(name);
        let form = form.clone();
        app.auth_plan(&form)
    }

    fn step_argv(plan: &AuthPlan) -> Vec<String> {
        let job = match plan {
            AuthPlan::Apply(job) => job.as_ref(),
            AuthPlan::Confirm(confirm) => &confirm.job,
            other => panic!("expected a job, got {other:?}"),
        };
        job.steps.iter().map(|s| s.argv.join(" ")).collect()
    }

    #[test]
    fn changing_account_swaps_the_kit_and_rebuilds() {
        let mut app = app_with_auth(Auth::Login, Vec::new());
        let plan = planned(&mut app, AccountChoice::New, "sk-ant-oat01-x", "private");
        let steps = step_argv(&plan);
        assert!(matches!(plan, AuthPlan::Confirm(_)), "{plan:?}");
        // The token is stored first, then rm + create.
        assert!(steps[0].contains("secret set-custom --host"), "{steps:?}");
        assert!(
            !steps[0].contains("--sandbox"),
            "the named token is global: {steps:?}"
        );
        assert!(steps[1].contains("sbx rm"), "{steps:?}");
        assert!(steps[2].contains("sbx create"), "{steps:?}");
        assert!(steps[2].contains("kits/claude-token"), "{steps:?}");
        assert!(
            steps[2].contains(&format!("{}", tokens::kit_dir("private").display())),
            "{steps:?}"
        );

        assert!(matches!(
            planned(&mut app, AccountChoice::New, "", "private"),
            AuthPlan::Refuse(_)
        ));

        // The other direction rebuilds onto the /login kit and removes no secret.
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        peer_on(&mut app, "claude-token-other", "private");
        let plan = planned(&mut app, AccountChoice::Login, "", "private");
        let steps = step_argv(&plan);
        assert!(matches!(plan, AuthPlan::Confirm(_)), "{plan:?}");
        assert!(steps[0].contains("sbx rm"), "{steps:?}");
        assert!(steps[1].contains("claude-anthropic"), "{steps:?}");
        assert!(
            !steps.iter().any(|s| s.contains("secret")),
            "the token is untouched: {steps:?}"
        );
        let AuthPlan::Confirm(confirm) = &plan else {
            panic!("{plan:?}")
        };
        assert_eq!(
            confirm.job.post.auth.as_ref().unwrap().token_name,
            None,
            "the token name is forgotten with the wiring"
        );
        assert!(
            confirm.body.iter().any(|l| l.contains("stays on the host")),
            "and the confirmation says so: {:?}",
            confirm.body
        );
    }

    #[test]
    fn the_editors_of_a_sandbox_with_ram_volumes_say_its_history_goes() {
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        app.sandboxes[0].tmpfs_volumes = true;
        app.action_edit_kits();
        let Screen::Kits(kits) = &app.screen else {
            panic!("the kits editor is open")
        };
        assert!(kits.ram_volumes);
        app.screen = Screen::List;
        // token -> another token, then token -> /login: both confirmations say it.
        for (account, name) in [
            (AccountChoice::Named("work".into()), "work"),
            (AccountChoice::Login, ""),
        ] {
            app.screen = Screen::List;
            app.action_edit_auth();
            let Screen::AuthEdit(form) = &app.screen else {
                panic!("the auth editor is open")
            };
            assert!(form.ram_volumes);
            let plan = planned(&mut app, account, "sk-ant-oat01-x", name);
            let AuthPlan::Confirm(confirm) = plan else {
                panic!("a rebuild asks first: {plan:?}")
            };
            assert!(
                confirm.body.iter().any(|l| l == SWITCH_IN_RAM),
                "{:?}",
                confirm.body
            );
            assert!(
                !confirm.body.iter().any(|l| l.contains("history stays")),
                "{:?}",
                confirm.body
            );
        }
        // The note the form shows before Apply, for each kind of rebuild.
        let mut form = crate::testkit::auth_form(AccountChoice::New);
        form.ram_volumes = true;
        let lost = "earlier sessions are lost";
        let to_token = form.effect().join(" ");
        assert!(to_token.contains(lost), "{to_token}");
        form.current = on_named("work", true);
        form.account = AccountChoice::Login;
        let to_login = form.effect().join(" ");
        assert!(to_login.contains(lost), "{to_login}");
        form.account = AccountChoice::Named("private".into());
        let other_token = form.effect().join(" ");
        assert!(other_token.contains(lost), "{other_token}");
        for text in [&to_token, &to_login, &other_token] {
            assert!(!text.contains("stays"), "{text}");
        }
    }

    #[test]
    fn an_existing_token_needs_no_value() {
        let mut app = app_with_auth(Auth::Login, vec![stored_token("private")]);
        let plan = planned(&mut app, AccountChoice::Named("private".into()), "", "");
        let steps = step_argv(&plan);
        assert!(matches!(plan, AuthPlan::Confirm(_)), "{plan:?}");
        assert!(
            !steps
                .iter()
                .any(|s| s.contains("set-custom (sbx-cs-aisbx-tok-")),
            "the token is already stored — nothing is written to it: {steps:?}"
        );
        assert_eq!(steps.len(), 2, "only a rebuild: {steps:?}");
        assert!(steps[0].contains("sbx rm"), "{steps:?}");
        assert!(
            steps[1].contains(&format!("{}", tokens::kit_dir("private").display())),
            "the container is built with the kit that carries the token: {steps:?}"
        );

        // Moving between two named tokens: the same shape, plus dropping the old wiring.
        let mut app = app_with_auth(
            on_named("private", true),
            vec![stored_token("private"), stored_token("work")],
        );
        let plan = planned(&mut app, AccountChoice::Named("work".into()), "", "");
        let steps = step_argv(&plan);
        assert!(steps[0].contains("sbx rm"), "{steps:?}");
        assert!(
            steps[1].contains(&format!("{}", tokens::kit_dir("work").display())),
            "{steps:?}"
        );
        assert!(
            !steps.iter().any(|s| s.contains("set-custom")),
            "an existing token is not written to: {steps:?}"
        );
    }

    #[test]
    fn rotating_a_named_token_applies_in_place_and_says_who_it_reaches() {
        for stored in [true, false] {
            let rows = match stored {
                true => vec![stored_token("private")],
                false => Vec::new(),
            };
            let mut app = app_with_auth(on_named("private", stored), rows);
            let plan = planned(
                &mut app,
                AccountChoice::Named("private".into()),
                "sk-ant-oat01-new",
                "",
            );
            let steps = step_argv(&plan);
            assert!(
                matches!(plan, AuthPlan::Apply(_)),
                "stored={stored} → {plan:?}"
            );
            assert_eq!(steps.len(), 1, "{steps:?}");
            assert!(steps[0].contains("secret set-custom --host"), "{steps:?}");
            assert!(
                !steps[0].contains("--sandbox"),
                "the named token is global: {steps:?}"
            );
            assert!(
                steps[0].contains(&sbx::named_token_placeholder("private")),
                "{steps:?}"
            );
            assert!(
                !steps[0].contains("sk-ant"),
                "the token rides on stdin: {steps:?}"
            );
        }

        // With another sandbox on the same name, rotating is a decision about both of them.
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        peer_on(&mut app, "claude-token-other", "private");
        let plan = planned(
            &mut app,
            AccountChoice::Named("private".into()),
            "sk-ant-oat01-new",
            "",
        );
        let AuthPlan::Confirm(confirm) = &plan else {
            panic!("a shared rotation asks first: {plan:?}")
        };
        assert!(!confirm.danger, "nothing is destroyed: {confirm:?}");
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("claude-token-other")),
            "and it names who else it reaches: {:?}",
            confirm.body
        );
    }

    #[test]
    fn a_named_token_with_nothing_stored_is_offered_and_repairable() {
        let mut app = app_with_auth(on_named("private", false), Vec::new());
        app.action_edit_auth();
        let Screen::AuthEdit(form) = &app.screen else {
            panic!("the auth editor is open")
        };
        assert!(
            form.tokens.iter().any(|t| t == "private"),
            "the name the sandbox is on is in the list even with nothing stored: {:?}",
            form.tokens
        );
        assert_eq!(form.account, AccountChoice::Named("private".into()));
        assert!(form.rows().contains(&AuthRow::Value), "{:?}", form.rows());

        assert!(matches!(
            planned(&mut app, AccountChoice::Named("private".into()), "", ""),
            AuthPlan::Refuse(_)
        ));
        let plan = planned(
            &mut app,
            AccountChoice::Named("private".into()),
            "sk-ant-oat01-x",
            "",
        );
        let steps = step_argv(&plan);
        assert!(
            matches!(plan, AuthPlan::Apply(_)),
            "nothing to rebuild: {plan:?}"
        );
        assert!(steps[0].contains("secret set-custom --host"), "{steps:?}");
        assert!(
            !steps[0].contains("--sandbox"),
            "the named token is global: {steps:?}"
        );
    }

    #[test]
    fn the_create_form_preselects_the_last_used_token() {
        let catalog = Catalog::load().unwrap();
        let tokens = vec!["private".into(), "work".into()];
        let form = CreateForm::new(
            &catalog,
            Some(&crate::testkit::host_home()),
            tokens.clone(),
            Some("work"),
            Vec::new(),
            sbx::SkillsMode::default(),
        );
        assert_eq!(
            form.connection.account,
            AccountChoice::Login,
            "the default account is unchanged"
        );
        assert_eq!(form.token_default(), AccountChoice::Named("work".into()));
        // A remembered name that has since been deleted falls back to one that exists…
        let form = CreateForm::new(
            &catalog,
            Some(&crate::testkit::host_home()),
            tokens,
            Some("gone"),
            Vec::new(),
            sbx::SkillsMode::default(),
        );
        assert_eq!(form.token_default(), AccountChoice::Named("private".into()));
        // …and with no tokens at all there is only one thing the radio can mean.
        let form = CreateForm::new(
            &catalog,
            Some(&crate::testkit::host_home()),
            Vec::new(),
            Some("gone"),
            Vec::new(),
            sbx::SkillsMode::default(),
        );
        assert_eq!(form.token_default(), AccountChoice::New);
        assert_eq!(
            form.connection.token_name.value, DEFAULT_TOKEN_NAME,
            "a first token gets a name to start from; a later one must not reuse it"
        );
    }

    /// Put the create wizard on `step` with its cursor on `row`, the fold it stands under open.
    fn wizard_on(app: &mut App, step: CreateStep, row: WizardRow) {
        let catalog = Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.wizard_set_step(&catalog, step);
        if let WizardRow::Field(field) = row {
            form.reveal(field);
        }
        assert!(
            form.focus_wizard_row(&catalog, row),
            "{row:?} is on {step:?}"
        );
    }

    /// Press `Create only` on the create wizard's review step.
    fn press_create_only(app: &mut App) {
        wizard_on(app, CreateStep::Review, WizardRow::CreateOnly);
        app.on_key(KeyEvent::from(KeyCode::Enter));
    }

    #[test]
    fn the_wizard_takes_jkhl_where_nothing_is_typed() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let at = |app: &App| match &app.screen {
            Screen::Create(form) => form.wizard_rows(&app.catalog)[form.wizard_focus()],
            _ => unreachable!("the create form is open"),
        };
        let press = |app: &mut App, c| app.on_key(KeyEvent::from(KeyCode::Char(c)));
        wizard_on(&mut app, CreateStep::Harness, WizardRow::Agent(0));
        press(&mut app, 'j');
        assert_eq!(at(&app), WizardRow::Agent(1));
        press(&mut app, 'k');
        assert_eq!(at(&app), WizardRow::Agent(0));

        wizard_on(
            &mut app,
            CreateStep::Workspace,
            WizardRow::Field(CreateRow::Skills),
        );
        let skills = |app: &App| match &app.screen {
            Screen::Create(form) => form.workspace.skills,
            _ => unreachable!("the create form is open"),
        };
        let start = skills(&app);
        press(&mut app, 'l');
        assert_eq!(skills(&app), start.next());
        press(&mut app, 'h');
        assert_eq!(skills(&app), start);

        wizard_on(
            &mut app,
            CreateStep::Workspace,
            WizardRow::Field(CreateRow::Ports),
        );
        for c in "jkhl".chars() {
            press(&mut app, c);
        }
        assert_eq!(at(&app), WizardRow::Field(CreateRow::Ports));
        match &app.screen {
            Screen::Create(form) => assert_eq!(form.workspace.ports.value, "jkhl"),
            _ => unreachable!("the create form is open"),
        }
    }

    #[test]
    fn the_buttons_are_one_row_to_updown_and_move_with_leftright() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");
        app.open_create();
        wizard_on(&mut app, CreateStep::Workspace, WizardRow::Next);
        let (rows, buttons) = match &app.screen {
            Screen::Create(form) => (form.wizard_rows(&app.catalog), form.wizard_buttons()),
            _ => unreachable!("the create form is open"),
        };
        // The button count against the buttons drawn on the line, the main action first.
        assert_eq!(
            &rows[rows.len() - buttons..],
            &[WizardRow::Next, WizardRow::Back, WizardRow::Cancel]
        );
        let focus = |app: &App| match &app.screen {
            Screen::Create(form) => rows[form.wizard_focus()],
            _ => unreachable!("the create form is open"),
        };
        let press = |app: &mut App, code: KeyCode| app.on_key(KeyEvent::from(code));
        let set = |app: &mut App, row: CreateRow| {
            wizard_on(app, CreateStep::Workspace, WizardRow::Field(row));
        };

        // ←→ walks the line, and wraps inside it rather than escaping onto the form.
        press(&mut app, KeyCode::Right);
        assert_eq!(focus(&app), WizardRow::Back);
        press(&mut app, KeyCode::Right);
        assert_eq!(focus(&app), WizardRow::Cancel);
        press(&mut app, KeyCode::Right);
        assert_eq!(focus(&app), WizardRow::Next);
        press(&mut app, KeyCode::Left);
        assert_eq!(focus(&app), WizardRow::Cancel);

        press(&mut app, KeyCode::Up);
        assert_eq!(focus(&app), WizardRow::Field(CreateRow::AdvancedFold));
        press(&mut app, KeyCode::Down);
        assert_eq!(
            focus(&app),
            WizardRow::Next,
            "entered at the first button, the step's main action"
        );
        press(&mut app, KeyCode::Down);
        assert_eq!(focus(&app), rows[0], "and ↓ wraps to the top of the step");
        // Wrapping up lands on Cancel, so no wrap reaches a button that commits.
        press(&mut app, KeyCode::Up);
        assert_eq!(focus(&app), WizardRow::Cancel);

        // Left/Right belong to the button line alone; a checkbox ignores them.
        set(&mut app, CreateRow::Clone);
        press(&mut app, KeyCode::Right);
        assert_eq!(focus(&app), WizardRow::Field(CreateRow::Clone));
        set(&mut app, CreateRow::Name);
        if let Screen::Create(form) = &mut app.screen {
            form.workspace.name = TextInput::new("abc");
        }
        press(&mut app, KeyCode::Left);
        assert_eq!(focus(&app), WizardRow::Field(CreateRow::Name));
        match &app.screen {
            Screen::Create(form) => {
                assert_eq!(
                    form.workspace.name.cursor, 2,
                    "the caret moved, not the row"
                )
            }
            _ => unreachable!("the create form is open"),
        }
    }

    #[test]
    fn the_kits_and_auth_forms_move_between_their_buttons_too() {
        let mut app = app_with_auth(Auth::Login, Vec::new());
        app.action_edit_auth();
        let auth_rows = match &app.screen {
            Screen::AuthEdit(form) => form.rows(),
            _ => panic!("the auth editor is open"),
        };
        assert_eq!(
            &auth_rows[auth_rows.len() - AuthForm::BUTTONS..],
            &[AuthRow::Apply, AuthRow::Cancel]
        );
        if let Screen::AuthEdit(form) = &mut app.screen {
            form.focus = auth_rows.len() - AuthForm::BUTTONS;
        }
        app.on_key(KeyEvent::from(KeyCode::Right));
        match &app.screen {
            Screen::AuthEdit(form) => assert_eq!(auth_rows[form.focus], AuthRow::Cancel),
            _ => panic!("the auth editor is open"),
        }
        // …and the footer says which key that was, only while it is the one that moves.
        assert!(app.on_form_buttons());
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert!(!app.on_form_buttons());

        let sb = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.open_kits("claude-anthropic-proj");
        let kits_rows = match &app.screen {
            Screen::Kits(form) => form.rows(&app.catalog),
            _ => panic!("the kits editor is open"),
        };
        assert_eq!(
            &kits_rows[kits_rows.len() - KitsForm::BUTTONS..],
            &[KitsRow::Apply, KitsRow::Cancel]
        );
        if let Screen::Kits(form) = &mut app.screen {
            form.focus = kits_rows.len() - KitsForm::BUTTONS;
        }
        // `l` as well as →: this form has no text field to want them, and binds `j`/`k` already.
        app.on_key(KeyEvent::from(KeyCode::Char('l')));
        match &app.screen {
            Screen::Kits(form) => assert_eq!(kits_rows[form.focus], KitsRow::Cancel),
            _ => panic!("the kits editor is open"),
        }
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        match &app.screen {
            Screen::Kits(form) => assert_eq!(form.focus, 0, "↓ leaves the line, not steps in it"),
            _ => panic!("the kits editor is open"),
        }
    }

    #[test]
    fn a_new_token_name_is_checked_before_anything_runs() {
        let mut app = app_with_auth(Auth::Login, vec![stored_token("private")]);
        for name in ["", "My Token", "UPPER"] {
            assert!(
                matches!(
                    planned(&mut app, AccountChoice::New, "sk-ant-oat01-x", name),
                    AuthPlan::Refuse(_)
                ),
                "{name:?}"
            );
        }
        assert!(matches!(
            planned(&mut app, AccountChoice::New, "sk-ant-oat01-x", "private"),
            AuthPlan::Refuse(_)
        ));
    }

    #[test]
    fn the_create_form_builds_the_kit_the_account_radio_names() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");
        let subscription = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account() == Some(Account::Login))
            .expect("the /login backend is offered");
        let submit = |app: &mut App, account: AccountChoice| -> Vec<Vec<String>> {
            app.open_create();
            if let Screen::Create(form) = &mut app.screen {
                // The account radio exists only on a subscription backend.
                form.harness.backend = subscription;
                form.connection.account = account;
                form.connection.token_name = TextInput::new("private");
                form.connection.token = TextInput::new("sk-ant-oat01-x");
            }
            // `Create only`, so nothing is handed the terminal.
            press_create_only(app);
            let Some(Modal::Running(_, pending)) = &app.modal else {
                panic!("a job was started")
            };
            let steps = pending.steps.iter().map(|s| s.argv.clone()).collect();
            app.modal = None;
            steps
        };

        let login = submit(&mut app, AccountChoice::Login);
        assert_eq!(login.len(), 1, "no secret to store: {login:?}");
        let create = login[0].join(" ");
        assert!(create.contains("kits/claude-anthropic"), "{create}");
        assert!(create.contains("--name claude-anthropic-proj"), "{create}");
        assert!(
            create.ends_with("kits/claude-anthropic /proj"),
            "the workload is the positional: {create}"
        );

        let token = submit(&mut app, AccountChoice::New);
        assert_eq!(token.len(), 2, "store the token, then create: {token:?}");
        assert!(
            token[0].join(" ").contains("set-custom --host"),
            "{token:?}"
        );
        let create = token[1].join(" ");
        assert!(create.contains("kits/claude-token"), "{create}");
        assert!(
            create.contains(&format!("{}", tokens::kit_dir("private").display())),
            "the generated kit carries the placeholder into the container: {create}"
        );
        assert!(create.contains("--name claude-token-proj"), "{create}");
        // A workload is the agent positional: its kit folder, never its name or a `--kit` mixin.
        let kit_dir = app
            .catalog
            .backend("claude-token")
            .unwrap()
            .kit
            .dir
            .display()
            .to_string();
        assert!(
            create.ends_with(&format!(" {kit_dir} /proj")),
            "the kit dir is the agent positional: {create}"
        );
        assert!(!create.contains(" claude-token /proj"), "{create}");
        assert!(
            !create.contains(&format!("--kit {kit_dir}")),
            "a sandbox kit is not a mixin: {create}"
        );

        // An existing token stores nothing: the second sandbox on an account is wiring only.
        app.secrets = Some(sbx::Secrets {
            services: Vec::new(),
            custom: vec![stored_token("private")],
        });
        let reused = submit(&mut app, AccountChoice::Named("private".into()));
        assert_eq!(
            reused.len(),
            1,
            "nothing to store — just create: {reused:?}"
        );
        assert!(
            reused[0]
                .join(" ")
                .contains(&format!("{}", tokens::kit_dir("private").display())),
            "{reused:?}"
        );
    }

    #[test]
    fn no_host_skills_folder_still_offers_every_skills_mode() {
        let base = std::env::temp_dir().join(format!("aisbx-no-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");
        let catalog = Arc::clone(&app.catalog);
        let detect = |catalog: &Catalog| -> Vec<Option<model::HostDetect>> {
            catalog
                .addons
                .iter()
                .map(|a| model::host_cache_detection_in(&a.kit.name, Some(home.as_path())))
                .collect()
        };
        let skills_kit = catalog
            .addons
            .iter()
            .position(|a| a.kit.name == HOST_SKILLS)
            .unwrap();
        for mode in [
            sbx::SkillsMode::Off,
            sbx::SkillsMode::Readonly,
            sbx::SkillsMode::Readwrite,
        ] {
            let steps = submit_create_form(&mut app, |form| {
                form.kits.host = detect(&catalog);
                assert!(form.addon_unavailable(skills_kit), "no folder, no kit");
                assert!(form.skills_offered(&catalog), "the store is still shared");
                assert!(form.rows(&catalog).contains(&CreateRow::Skills));
                // Ticking the greyed kit does nothing.
                form.toggle_addon(skills_kit);
                assert!(!form.kits.addons[skills_kit]);
                form.workspace.skills = mode;
            });
            let create = steps[0].join(" ");
            assert!(
                create.contains(&format!("--skills {}", mode.flag())),
                "{create}"
            );
            assert!(!create.contains("kits/host-skills"), "{create}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Run the create form to `Create only` and hand back the argv of every step it built.
    fn submit_create_form(app: &mut App, edit: impl FnOnce(&mut CreateForm)) -> Vec<Vec<String>> {
        app.open_create();
        if let Screen::Create(form) = &mut app.screen {
            edit(form);
        }
        press_create_only(app);
        let steps = match &app.modal {
            Some(Modal::Running(_, pending)) => {
                pending.steps.iter().map(|s| s.argv.clone()).collect()
            }
            // A rejected form starts no job — the caller asserts on the status line instead.
            _ => Vec::new(),
        };
        app.modal = None;
        steps
    }

    #[test]
    fn a_create_mounts_the_host_folders_and_the_read_only_git_folder_and_tells_the_agent() {
        let root = std::env::temp_dir().join(format!("aisbx-tests-mounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["proj-dev/.git", "proj", "kb", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let ws = root.join("proj-dev");
        std::fs::write(ws.join(folders::COMPANION_FILE), r#"{"public": "../proj"}"#).unwrap();
        let state = root.join("state");
        let kb = folders::Entry {
            path: root.join("kb"),
            access: folders::Access::Read,
            purpose: "the knowledge base".into(),
        };
        folders::put(&state, kb).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = ws.clone();
        app.mounts_state = state;

        let steps = submit_create_form(&mut app, |form| form.workspace.git_guard = true);
        let create = &steps[0];
        let at = |spec: String| create.iter().position(|a| *a == spec);
        let workspace = at(ws.display().to_string()).expect("the workspace");
        let git = at(format!("{}:ro", ws.join(".git").display())).expect("the .git guard");
        let public = at(root.join("proj").display().to_string()).expect("the public repo");
        let kb = at(format!("{}:ro", root.join("kb").display())).expect("the host's folder");
        assert!(workspace < git && git < public && public < kb, "{create:?}");
        let kit = create
            .iter()
            .find(|a| folders::is_kit(a.rsplit(std::path::MAIN_SEPARATOR).next().unwrap()))
            .expect("the folder kit");
        let text = std::fs::read_to_string(crate::kits::descriptor_path(Path::new(kit))).unwrap();
        assert!(text.contains(&format!(
            "`{}` (read-only): the knowledge base",
            root.join("kb").display()
        )));
        assert!(text.contains("`.git` folder is mounted read-only"));

        // Clone mode mounts the workspace read-only already: no guard, though it was ticked.
        let steps = submit_create_form(&mut app, |form| {
            form.workspace.git_guard = true;
            form.workspace.clone = true;
        });
        assert!(
            !steps[0].iter().any(|a| a.ends_with(".git:ro")),
            "{:?}",
            steps[0]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_create_mounts_its_own_folders_at_their_targets_and_records_them() {
        let root = std::env::temp_dir().join(format!("aisbx-tests-extras-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["more-ws", "docs", "data", "kb", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let state = root.join("state");
        let kb = folders::Entry {
            path: root.join("kb"),
            access: folders::Access::Read,
            purpose: String::new(),
        };
        folders::put(&state, kb).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = root.join("more-ws");
        app.mounts_state = state;
        let docs = format!("{}:ro", root.join("docs").display());
        let data = format!("{}:/workspace/data", root.join("data").display());

        app.open_create();
        if let Screen::Create(form) = &mut app.screen {
            form.workspace.extra_folders = vec![docs.clone(), data.clone()];
        }
        press_create_only(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started: {:?}", app.status)
        };
        let create = &pending.steps[0].argv;
        let at = |spec: &str| create.iter().position(|a| a == spec);
        let kb = at(&format!("{}:ro", root.join("kb").display())).expect("the host's folder");
        let typed = (at(&docs).expect("docs"), at(&data).expect("data"));
        assert!(kb < typed.0 && typed.0 < typed.1, "{create:?}");
        let kit = create
            .iter()
            .find(|a| folders::is_kit(a.rsplit(std::path::MAIN_SEPARATOR).next().unwrap()))
            .expect("the folder kit");
        let text = std::fs::read_to_string(crate::kits::descriptor_path(Path::new(kit))).unwrap();
        let line = format!(
            "`/workspace/data` (read-write, the host's `{}`)",
            root.join("data").display()
        );
        assert!(
            text.contains(&line),
            "the agent learns where it sees the folder: {text}"
        );
        let name = (pending.post.register.as_ref()).map(|r| r.name.clone());
        app.apply_post(&pending.post);
        assert_eq!(
            app.registry.extra_folders(&name.expect("registered")),
            [docs, data],
            "recorded, so a rebuild mounts them again"
        );

        let gone = format!("{}:/gone", root.join("gone").display());
        let steps = submit_create_form(&mut app, |form| {
            form.workspace.extra_folders = vec![gone.clone()]
        });
        assert!(steps.is_empty(), "{steps:?}");
        let (text, err, _) = app.status.clone().expect("a reason is shown");
        assert!(err);
        assert_eq!(text, format!("“{gone}”: the folder does not exist"));
        let inside = format!("{}:{}/data", root.join("data").display(), app.cwd.display());
        let steps = submit_create_form(&mut app, |form| {
            form.workspace.extra_folders = vec![inside.clone()]
        });
        assert!(steps.is_empty(), "{steps:?}");
        let (text, _, _) = app.status.clone().expect("a reason is shown");
        let why = "the target is the workspace or inside it";
        assert_eq!(text, format!("“{inside}”: {why}"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_folders_help_says_what_each_access_means_and_nothing_a_button_says() {
        let form = FoldersForm {
            folders: vec![folders::Entry {
                path: "/srv/kb".into(),
                access: folders::Access::Read,
                purpose: String::new(),
            }],
            ..Default::default()
        };
        assert_eq!(
            form.help(FoldersRow::Folder(0)).text,
            "Read-only: nothing in the sandbox can change it."
        );
        let access = form.help(FoldersRow::Access).text;
        for value in folders::Access::ALL {
            assert!(access.contains(value.meaning()), "{access}");
        }
        for row in [FoldersRow::Save, FoldersRow::Close] {
            assert!(form.help(row).text.is_empty(), "{row:?}");
        }
        let browse = form.help(FoldersRow::Browse).text;
        assert!(!browse.contains('↵') && !browse.contains("Use"), "{browse}");
        assert!(!FOLDERS_NOTE.contains("companion.json"), "{FOLDERS_NOTE}");
    }

    #[test]
    fn the_folders_screen_adds_edits_and_deletes_the_hosts_folders() {
        let state =
            std::env::temp_dir().join(format!("aisbx-tests-folders-screen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.mounts_state = state.clone();
        let key = |app: &mut App, code| app.on_key(KeyEvent::from(code));
        let typed = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.on_key(KeyEvent::from(KeyCode::Char(c)));
            }
        };
        app.perform(Action::Folders);
        let Screen::Folders(form) = &app.screen else {
            panic!("the folders screen");
        };
        assert_eq!(
            form.focused(),
            FoldersRow::Path,
            "an empty list opens on the field"
        );
        typed(&mut app, "/srv/kb");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Enter);
        typed(&mut app, "the  knowledge base");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        let saved = folders::load(&folders::config_path(&state))
            .unwrap()
            .folders;
        let kb = folders::Entry {
            path: PathBuf::from("/srv/kb"),
            access: folders::Access::Write,
            purpose: "the knowledge base".into(),
        };
        assert_eq!(saved, [kb]);
        let Screen::Folders(form) = &app.screen else {
            panic!("still the folders screen");
        };
        assert_eq!(
            form.focused(),
            FoldersRow::Folder(0),
            "on the folder just saved"
        );
        // ↵ puts it in the fields; ↵ on the folder field skips Browse; Save changes it in place.
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char(' '));
        let Screen::Folders(form) = &mut app.screen else {
            panic!("the folders screen");
        };
        form.focus = form
            .rows()
            .iter()
            .position(|r| *r == FoldersRow::Save)
            .unwrap();
        key(&mut app, KeyCode::Enter);
        let saved = folders::load(&folders::config_path(&state))
            .unwrap()
            .folders;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].access, folders::Access::Read);
        // A relative path is refused and saves nothing.
        app.open_folders(None);
        typed(&mut app, "relative");
        let Screen::Folders(form) = &mut app.screen else {
            panic!("the folders screen");
        };
        form.focus = form
            .rows()
            .iter()
            .position(|r| *r == FoldersRow::Save)
            .unwrap();
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            folders::load(&folders::config_path(&state))
                .unwrap()
                .folders
                .len(),
            1
        );
        assert!(app.status.as_ref().is_some_and(|(_, error, _)| *error));
        // d on a folder drops it.
        app.open_folders(Some(Path::new("/srv/kb")));
        key(&mut app, KeyCode::Char('d'));
        assert!(folders::load(&folders::config_path(&state))
            .unwrap()
            .folders
            .is_empty());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.screen, Screen::List));
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn dot_dot_at_the_top_of_the_picker_mounts_the_folder_above() {
        let root = std::env::temp_dir().join(format!("aisbx-tests-widen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/api")).unwrap();
        std::fs::create_dir_all(root.join("repo/web")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = root.join("repo/api");
        app.open_create();
        app.action_create_start_dir();
        let Screen::StartDir(picker) = &mut app.screen else {
            panic!("the picker");
        };
        assert!(picker.widens());
        assert_eq!(picker.rows()[0], StartDirRow::Up);
        picker.up();
        assert_eq!(picker.root, root.join("repo"));
        assert_eq!(picker.dirs, ["api", "web"]);
        assert_eq!(
            picker.focused(),
            StartDirRow::Dir(0),
            "on the folder it came from"
        );
        picker.descend(0);
        app.submit_start_dir(false);
        let Screen::Create(form) = &app.screen else {
            panic!("back on the form");
        };
        assert_eq!(
            form.workspace.root.as_deref(),
            Some(root.join("repo").as_path())
        );
        assert_eq!(form.workspace.start_dir.as_deref(), Some(Path::new("api")));
        let review = form.review(&app);
        assert_eq!(review.mounts[0], model::tilde(&root.join("repo")));
        // An existing sandbox's picker never leaves its workspace.
        let existing = StartDirForm::open(
            StartDirTarget::Sandbox("s".into()),
            "s".into(),
            root.join("repo"),
            None,
        );
        assert!(!existing.widens() && !existing.rows().contains(&StartDirRow::Up));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_create_form_publishes_ports_and_can_opt_out_of_the_shared_skills_store() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");

        // The mode on screen is spelled out, also when it matches the host default.
        let steps = submit_create_form(&mut app, |_| {});
        let create = steps[0].join(" ");
        assert!(create.contains("--skills readonly"), "{create}");
        // On the argv, not the joined line: `-p` is a substring of `…-proj`.
        assert!(
            !steps[0].iter().any(|a| a == "-p"),
            "nothing published: {create}"
        );

        // Both options together, typed the way a list gets typed.
        let steps = submit_create_form(&mut app, |form| {
            form.workspace.skills = sbx::SkillsMode::Off;
            form.workspace.ports = TextInput::new("8080:3000, 5173");
        });
        let create = steps[0].join(" ");
        assert!(create.contains("--skills off"), "{create}");
        assert!(create.contains("-p 8080:3000"), "{create}");
        assert!(create.contains("-p 5173"), "{create}");
        // Flags first: cobra reads anything after the workload positional as an argument to it.
        let agent = steps[0]
            .iter()
            .position(|a| a.ends_with("kits/claude-litellm"))
            .unwrap();
        assert!(
            steps[0].iter().position(|a| a == "-p").unwrap() < agent,
            "{create}"
        );

        // A typo must be caught here, not after `sbx rm` has already run in a recreate.
        let steps = submit_create_form(&mut app, |form| {
            form.workspace.ports = TextInput::new("8080:not-a-port");
        });
        assert!(steps.is_empty(), "nothing ran: {steps:?}");
        assert!(app.status.as_ref().is_some_and(|(_, err, _)| *err));

        // The token workload gets the skills mode too, like the other Claude Code workloads.
        let token_backend = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account() == Some(Account::Login))
            .unwrap();
        let steps = submit_create_form(&mut app, |form| {
            form.harness.backend = token_backend;
            form.connection.account = AccountChoice::Named("private".into());
            form.workspace.skills = sbx::SkillsMode::Off;
        });
        let create = steps.last().unwrap().join(" ");
        assert!(create.contains("kits/claude-token"), "{create}");
        assert!(create.contains("--skills off"), "{create}");
    }

    #[test]
    fn a_recreate_puts_the_published_ports_and_the_skills_choice_back() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        // One publish, as sbx reports it: two rows for the two loopback families.
        for host_ip in ["127.0.0.1", "::1"] {
            sb.ports.push(sbx::Port {
                host_ip: host_ip.into(),
                host_port: 8080,
                sandbox_port: 3000,
                protocol: "tcp".into(),
                ephemeral: false,
            });
        }
        sb.skills = Some(model::Skills::Known(sbx::SkillsMode::Off));
        let name = sb.name.clone();
        let app = App::for_test(Catalog::load().unwrap(), vec![sb]);

        let job = app
            .recreate_job(&name, &[], None)
            .expect("a managed sandbox");
        let create = job.steps.last().unwrap().argv.join(" ");
        assert!(
            create.contains("-p 8080:3000"),
            "the port comes back, once: {create}"
        );
        assert_eq!(
            job.steps
                .last()
                .unwrap()
                .argv
                .iter()
                .filter(|a| *a == "-p")
                .count(),
            1,
            "{create}"
        );
        assert!(
            create.contains("--skills off"),
            "a sandbox kept out of the store stays out: {create}"
        );

        // An unrecorded mode is rebuilt with no `--skills`, so the host default answers.
        let mut old = model::sample("claude-litellm-old", Some("claude-litellm"), &[]);
        old.skills = Some(model::Skills::Unrecorded);
        let name = old.name.clone();
        let app = App::for_test(Catalog::load().unwrap(), vec![old]);
        let job = app
            .recreate_job(&name, &[], None)
            .expect("a managed sandbox");
        let create = job.steps.last().unwrap().argv.join(" ");
        assert!(!create.contains("--skills"), "{create}");
    }

    #[test]
    fn a_port_field_is_split_on_what_people_type_and_checked_for_shape() {
        assert_eq!(split_ports(" 8080:3000,  5173 "), ["8080:3000", "5173"]);
        assert!(split_ports("   ").is_empty());
        for good in [
            "3000",
            "8080:3000",
            "127.0.0.1:8080:3000",
            "8080:3000/udp",
            "0.0.0.0:9000:9000/tcp6",
        ] {
            assert!(port_spec_ok(good), "{good}");
        }
        for bad in ["", "0", "abc", "8080:", ":3000", "8080:3000/sctp", "70000"] {
            assert!(!port_spec_ok(bad), "{bad}");
        }
    }

    #[test]
    fn starting_a_sandbox_updates_its_agent_first() {
        let claude = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut opencode = model::sample("opencode-proj", Some("opencode-litellm"), &[]);
        opencode.agent = "opencode".into();
        // Not from this repo's kits, and an agent with no update command of its own.
        let mut stranger = model::sample("shell-lab", None, &[]);
        stranger.agent = "shell".into();

        let mut app = App::for_test(Catalog::load().unwrap(), vec![claude, opencode, stranger]);
        let start = |app: &mut App, cell: usize| {
            app.cursor = cell;
            app.handoff = None;
            app.action_start();
            app.handoff.take().expect("a session was handed over")
        };

        // The label always goes first: it costs nothing and is never skipped.
        let label = |n: &str| sbx::label_argv(n, n);

        // Claude Code reads its gateway and pins from its environment, and gets its model picker
        // merged into its settings.
        let handoff = start(&mut app, 0);
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", None)
        );
        let picker = app.config_push("claude-litellm-proj").expect("the picker");
        assert!(picker.contains(&crate::litellm::CLAUDE_SETTINGS_PATH.to_string()));
        assert_eq!(
            handoff.prelude(),
            [
                label("claude-litellm-proj"),
                picker.clone(),
                sbx::exec_argv("claude-litellm-proj", &["claude", "update"]),
            ],
            "the update runs as its own process, ahead of the session"
        );
        // OpenCode gets the connection's model list and picks into its config first.
        let acme = crate::litellm::load_in(&app.litellm_root, "acme").unwrap();
        let push = crate::litellm::push_argv_in(
            &app.litellm_root,
            "opencode-proj",
            &acme,
            crate::litellm::Agent::Opencode,
        )
        .unwrap();
        assert_eq!(
            start(&mut app, 1).prelude(),
            [
                label("opencode-proj"),
                push.clone(),
                sbx::exec_argv(
                    "opencode-proj",
                    crate::kits::agent_update("opencode").unwrap()
                ),
            ]
        );
        assert_eq!(
            start(&mut app, 2).prelude(),
            [label("shell-lab")],
            "an agent with no known update command is started after the label step alone"
        );

        // `--no-update`: no update step; the label and the config writes are local and stay.
        app.auto_update = false;
        let handoff = start(&mut app, 0);
        assert_eq!(handoff.prelude(), [label("claude-litellm-proj"), picker]);
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", None)
        );
        assert_eq!(start(&mut app, 1).prelude(), [label("opencode-proj"), push]);
    }

    #[test]
    fn a_host_skills_sandbox_syncs_before_the_session() {
        let skills = model::sample(
            "claude-anthropic-proj",
            Some("claude-anthropic"),
            &["host-skills"],
        );
        let plain = model::sample("claude-anthropic-other", Some("claude-anthropic"), &["rtk"]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![skills, plain]);
        let start = |app: &mut App, cell: usize| {
            app.cursor = cell;
            app.handoff = None;
            app.action_start();
            app.handoff.take().expect("a session was handed over")
        };
        let label = |n: &str| sbx::label_argv(n, n);

        let handoff = start(&mut app, 0);
        assert_eq!(
            handoff.prelude(),
            [
                label("claude-anthropic-proj"),
                sbx::host_skills_argv("claude-anthropic-proj"),
                sbx::exec_argv("claude-anthropic-proj", &["claude", "update"]),
            ],
            "label, the host-skills sync, then the update"
        );
        let hook = sbx::host_skills_argv("claude-anthropic-proj");
        assert_eq!(hook[..3], ["sbx", "exec", "claude-anthropic-proj"]);
        assert_eq!(
            hook[7..],
            ["/usr/local/lib/aisbx/host-skills/startup.sh"],
            "the kit's hook"
        );
        assert_eq!(
            start(&mut app, 1).prelude(),
            [
                label("claude-anthropic-other"),
                sbx::exec_argv("claude-anthropic-other", &["claude", "update"]),
            ],
            "no sync without the addon"
        );

        app.auto_update = false;
        assert_eq!(
            start(&mut app, 0).prelude(),
            [
                label("claude-anthropic-proj"),
                sbx::host_skills_argv("claude-anthropic-proj"),
            ],
            "--no-update drops the update, not the sync"
        );
        assert_eq!(
            start(&mut app, 1).prelude(),
            [label("claude-anthropic-other")]
        );
    }

    #[test]
    fn the_first_start_after_a_create_writes_the_label_and_claude_codes_picker() {
        let create_and_start = |app: &mut App, backend: &str| -> PostJob {
            app.open_create();
            let idx = app
                .catalog
                .backends
                .iter()
                .position(|b| b.kit.name == backend)
                .unwrap();
            let Screen::Create(form) = &mut app.screen else {
                panic!("the create form is open")
            };
            form.harness.backend = idx;
            wizard_on(app, CreateStep::Review, WizardRow::CreateStart);
            app.on_key(KeyEvent::from(KeyCode::Enter));
            let Some(Modal::Running(_, pending)) = app.modal.take() else {
                panic!("a create job was started")
            };
            pending.post
        };
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");

        // The create wrote OpenCode's config from the host's list, but nothing of Claude Code's
        // settings: only its picker is merged. No update.
        for (backend, picker) in [("claude-litellm", true), ("opencode-litellm", false)] {
            let post = create_and_start(&mut app, backend);
            let name = post.attach.clone().expect("a session follows the create");
            assert_eq!(post.attach_config.is_some(), picker, "{backend}");
            app.apply_post(&post);
            let handoff = app.handoff.take().expect("a session was handed over");
            let label = sbx::label_argv(&name, app.registry.display_name(&name).unwrap_or(&name));
            let want: Vec<Vec<String>> = [label].into_iter().chain(post.attach_config).collect();
            assert_eq!(handoff.prelude(), want, "{backend}");
            if picker {
                assert!(want[1].contains(&crate::litellm::CLAUDE_SETTINGS_PATH.to_string()));
            }
        }

        let post = create_and_start(&mut app, "claude-anthropic");
        assert!(
            post.attach_skill_steps.is_empty(),
            "no host-skills addon was ticked"
        );
        app.apply_post(&post);
        let handoff = app.handoff.take().unwrap();
        assert_eq!(handoff.prelude().len(), 1, "{:?}", handoff.prelude());
    }

    #[test]
    fn the_first_start_after_a_create_syncs_host_skills() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let name = "claude-litellm-proj".to_string();
        let post = PostJob {
            attach: Some(name.clone()),
            attach_skill_steps: vec![sbx::host_skills_argv(&name)],
            ..Default::default()
        };
        for auto_update in [true, false] {
            app.auto_update = auto_update;
            app.apply_post(&post);
            let handoff = app.handoff.take().expect("a session was handed over");
            assert_eq!(
                handoff.prelude(),
                [
                    sbx::label_argv(&name, app.registry.display_name(&name).unwrap_or(&name)),
                    sbx::host_skills_argv(&name),
                ],
                "auto_update: {auto_update}"
            );
        }
    }

    #[test]
    fn l_fetches_the_connections_list_through_its_fetcher() {
        let litellm = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let login = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![litellm, login]);
        let acme = crate::litellm::load_in(&app.litellm_root, "acme").unwrap();
        let kit = crate::model_fetch::kit_dir_in(&app.litellm_root, "acme");

        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('L')));
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("L started a job")
        };
        assert_eq!(pending.title, "Fetching the models of acme");
        let argvs = |steps: &[Step]| steps.iter().map(|s| s.argv.clone()).collect::<Vec<_>>();
        assert_eq!(
            argvs(&pending.steps),
            argvs(&crate::model_fetch::steps(&acme, &kit, false)),
            "no fetcher yet: it is created first"
        );
        assert_eq!(
            pending.post.harvest_models,
            Some(("aisbx-models-acme".to_string(), "acme".to_string()))
        );

        // Once the fetcher exists, the fetch is one step.
        app.fetchers = vec!["aisbx-models-acme".into()];
        app.on_key(KeyEvent::from(KeyCode::Char('L')));
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("L started a job")
        };
        assert_eq!(pending.steps.len(), 1);
        assert_eq!(pending.steps[0].argv, crate::model_fetch::fetch_argv(&acme));

        app.cursor = 1;
        app.on_key(KeyEvent::from(KeyCode::Char('L')));
        assert!(
            app.modal.is_none(),
            "no job on a sandbox without a connection"
        );
    }

    #[test]
    fn a_harvest_replaces_the_pin_list_only_when_it_is_newer() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let conn = format!("harvest-{}", std::process::id());
        let dir = app.litellm_root.join(&conn);
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("models.json");
        app.models_conn = Some(conn.clone());
        let now = 1_800_000_000;
        let cache = |at: i64, id: &str| {
            format!(
                r#"{{"version":1,"fetched_at":{at},"models":[{{"id":"{id}"}},{{"id":"claude-opus-5"}}]}}"#
            )
        };

        assert_eq!(
            app.store_models_harvest(&conn, "a", &cache(now - 60, "new-x"), now),
            Ok(true)
        );
        assert_eq!(
            app.catalog.models_source,
            crate::models::Source::Gateway {
                sandbox: "a".into(),
                fetched_at: now - 60
            }
        );
        assert!(app.catalog.models.iter().any(|m| m.id == "new-x"));
        let before = std::fs::read(&path).unwrap();

        // The same fetch read again (a fresh skip), and an older one from another sandbox.
        assert_eq!(
            app.store_models_harvest(&conn, "a", &cache(now - 60, "new-x"), now),
            Ok(false)
        );
        assert_eq!(
            app.store_models_harvest(&conn, "b", &cache(now - 900, "old-y"), now),
            Ok(false)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!app.catalog.models.iter().any(|m| m.id == "old-y"));
        assert!(matches!(
            app.catalog.models_source,
            crate::models::Source::Gateway { fetched_at, .. } if fetched_at == now - 60
        ));

        // Refused data changes nothing either.
        assert!(app
            .store_models_harvest(&conn, "b", "not json", now)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        assert_eq!(
            app.store_models_harvest(&conn, "b", &cache(now, "newer-z"), now),
            Ok(true)
        );
        assert!(app.catalog.models.iter().any(|m| m.id == "newer-z"));
        // A list of another connection than the one on offer is kept, not shown.
        app.models_conn = Some("acme".into());
        assert_eq!(
            app.store_models_harvest(&conn, "c", &cache(now + 1, "elsewhere"), now + 1),
            Ok(true)
        );
        assert!(!app.catalog.models.iter().any(|m| m.id == "elsewhere"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A workspace with a few subfolders: the picker reads the real filesystem.
    fn workspace_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aisbx-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for rel in ["notes/drafts", "notes/published", "aisbx", ".git"] {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        }
        root
    }

    fn app_in_tree(tag: &str) -> (App, PathBuf) {
        let root = workspace_tree(tag);
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.workspace = root.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        // Regroup after moving the cwd, or the sandbox sits under "Sandboxes in other folders".
        app.cwd = root.clone();
        app.refresh_cells();
        app.cursor = 0;
        (app, root)
    }

    #[test]
    fn a_start_folder_launches_the_agent_through_run_workdir() {
        let (mut app, root) = app_in_tree("launch");

        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", None),
            "no start folder keeps `sbx run --name`, which is what every sandbox did before"
        );

        app.registry
            .set_start_dir("claude-litellm-proj", Some(Path::new("notes")));
        app.sandboxes[0].start_dir = Some(PathBuf::from("notes"));
        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", Some(&root.join("notes"))),
        );
        // The workspace is mounted at its host path, so the argument is the host path verbatim.
        assert!(
            handoff
                .argv()
                .contains(&root.join("notes").display().to_string()),
            "{:?}",
            handoff.argv()
        );
    }

    #[test]
    fn a_start_folder_launches_the_kits_own_entrypoint() {
        let (mut app, root) = app_in_tree("yolo");
        app.sandboxes[0].start_dir = Some(PathBuf::from("notes"));
        app.action_start();
        let argv = app
            .handoff
            .take()
            .expect("a session was handed over")
            .argv();
        let dir = root.join("notes").display().to_string();
        assert_eq!(
            argv,
            [
                "sbx",
                "run",
                "--name",
                "claude-litellm-proj",
                "--workdir",
                &dir
            ],
            "sbx starts the kit's entrypoint, its flags included: no agent binary in the argv"
        );

        let mut other = model::sample("other-proj", None, &[]);
        other.agent = "some-foreign-agent".into();
        other.workspace = root.clone();
        assert!(
            other.can_pick_start_dir(),
            "an agent aisbx knows nothing about can open in a subfolder too"
        );
    }

    #[test]
    fn a_start_folder_adds_no_pre_start_step() {
        let (mut app, root) = app_in_tree("trust");
        app.auto_update = false;
        let dir = root.join("aisbx");

        app.sandboxes[0].start_dir = Some(PathBuf::from("aisbx"));
        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        let picker = app.config_push("claude-litellm-proj").expect("the picker");
        assert_eq!(
            handoff.prelude(),
            [
                sbx::label_argv("claude-litellm-proj", "claude-litellm-proj"),
                picker
            ],
            "the label step and nothing for the folder"
        );
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", Some(&dir))
        );
        assert!(
            handoff
                .prelude()
                .iter()
                .flatten()
                .all(|a| !a.contains(".claude.json")),
            "{:?}",
            handoff.prelude()
        );

        let mut oc = model::sample("opencode-proj", Some("opencode-litellm"), &[]);
        oc.agent = "opencode".into();
        oc.workspace = root.clone();
        oc.start_dir = Some(PathBuf::from("aisbx"));
        let mut app = App::for_test(Catalog::load().unwrap(), vec![oc]);
        app.auto_update = false;
        app.cwd = root.clone();
        app.refresh_cells();
        app.cursor = 0;
        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert_eq!(
            handoff.prelude()[0],
            sbx::label_argv("opencode-proj", "opencode-proj"),
        );
        assert_eq!(
            handoff.prelude().len(),
            2,
            "the label step, the model list, and nothing for the folder"
        );
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("opencode-proj", Some(&root.join("aisbx")))
        );
    }

    #[test]
    fn a_start_folder_that_has_gone_missing_falls_back_to_the_workspace() {
        let (mut app, _root) = app_in_tree("missing");
        app.sandboxes[0].start_dir = Some(PathBuf::from("deleted-since"));
        assert!(app.sandboxes[0].start_dir_missing());

        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert_eq!(
            handoff.argv(),
            sbx::attach_argv("claude-litellm-proj", None),
            "the start is not failed, only moved back to the root"
        );
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(m, _, _)| m.contains("gone")),
            "{:?}",
            app.status
        );
        // A shell follows the agent, so it must fall back the same way rather than fail.
        app.action_shell();
        assert_eq!(
            app.handoff.take().expect("a shell was handed over").argv(),
            sbx::shell_argv("claude-litellm-proj", None)
        );
    }

    #[test]
    fn a_shell_opens_in_the_same_folder_as_the_agent() {
        let (mut app, root) = app_in_tree("shell");
        app.sandboxes[0].start_dir = Some(PathBuf::from("notes/drafts"));
        app.action_shell();
        assert_eq!(
            app.handoff.take().expect("a shell was handed over").argv(),
            sbx::shell_argv(
                "claude-litellm-proj",
                Some(&root.join("notes").join("drafts"))
            )
        );
    }

    #[test]
    fn the_picker_walks_in_and_out_of_the_workspace_tree() {
        let (mut app, root) = app_in_tree("walk");
        app.on_key(KeyEvent::from(KeyCode::Char('w')));

        let dirs = |app: &App| match &app.screen {
            Screen::StartDir(f) => f.dirs.clone(),
            _ => panic!("the picker is open"),
        };
        let rel = |app: &App| match &app.screen {
            Screen::StartDir(f) => f.rel.clone(),
            _ => panic!("the picker is open"),
        };
        let focused = |app: &App| match &app.screen {
            Screen::StartDir(f) => f.focused(),
            _ => panic!("the picker is open"),
        };

        // Hidden directories are never the project someone means to open, so `.git` is not listed.
        assert_eq!(dirs(&app), ["aisbx", "notes"]);
        assert_eq!(rel(&app), Path::new(""), "it opens at the workspace root");
        assert_eq!(
            focused(&app),
            StartDirRow::Dir(0),
            "on the first folder, not on `..`"
        );

        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(focused(&app), StartDirRow::Dir(1));
        app.on_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(rel(&app), Path::new("notes"));
        assert_eq!(dirs(&app), ["drafts", "published"]);

        // Deeper still, which is the whole point of a tree rather than one list of children.
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(rel(&app), Path::new("notes/drafts"));
        assert!(dirs(&app).is_empty());
        assert_eq!(
            focused(&app),
            StartDirRow::Start,
            "with nowhere left to go the cursor lands on the button that commits"
        );

        // On the button line Left belongs to the buttons, so Up steps back into the list first.
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(focused(&app), StartDirRow::Up);
        app.on_key(KeyEvent::from(KeyCode::Left));
        assert_eq!(rel(&app), Path::new("notes"));
        // …and the cursor is back on the folder just left, so a wrong turn costs one key.
        assert_eq!(
            focused(&app),
            StartDirRow::Dir(0),
            "drafts, where we came from"
        );
        app.on_key(KeyEvent::from(KeyCode::Left));
        assert_eq!(rel(&app), Path::new(""));
        assert_eq!(
            focused(&app),
            StartDirRow::Dir(1),
            "back on notes, which is where we came from"
        );

        // Committing writes the registry and nothing else — no container is touched.
        match &mut app.screen {
            Screen::StartDir(f) => f.descend(0),
            _ => panic!("the picker is open"),
        }
        app.submit_start_dir(false);
        assert_eq!(
            app.registry.start_dir("claude-litellm-proj"),
            Some(PathBuf::from("aisbx"))
        );
        assert!(matches!(app.screen, Screen::List));
        assert!(app.handoff.is_none(), "“use, don't start” starts nothing");

        // And the root is reachable the same way, which is how a start folder is taken back off.
        app.on_key(KeyEvent::from(KeyCode::Char('w')));
        app.on_key(KeyEvent::from(KeyCode::Left));
        app.submit_start_dir(false);
        assert_eq!(app.registry.start_dir("claude-litellm-proj"), None);
        assert_eq!(app.sandboxes[0].start_path(), root);
    }

    #[test]
    fn the_create_form_picks_a_start_folder_without_losing_what_was_typed() {
        let (mut app, _root) = app_in_tree("create");
        app.open_create();
        if let Screen::Create(form) = &mut app.screen {
            form.workspace.name = TextInput::new("My KI sandbox");
        }
        let start_dir = WizardRow::Field(CreateRow::StartDir);
        wizard_on(&mut app, CreateStep::Workspace, start_dir);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        // Back on step 4 of the wizard, with the cursor on the row the picker was opened from.
        let on_step_four = |app: &App| match &app.screen {
            Screen::Create(form) => {
                assert_eq!(form.wizard_step(), CreateStep::Workspace);
                let rows = form.wizard_rows(&app.catalog);
                assert_eq!(rows[form.wizard_focus()], start_dir);
            }
            _ => panic!("back on the create form"),
        };

        match &app.screen {
            Screen::StartDir(f) => {
                assert!(matches!(f.target, StartDirTarget::New(_)));
                assert!(
                    !f.rows().contains(&StartDirRow::Start),
                    "nothing to start before the sandbox exists"
                );
            }
            _ => panic!("the picker is open"),
        }

        // Descend and commit; the form comes back with the folder and everything typed on it.
        app.on_key(KeyEvent::from(KeyCode::Right));
        app.submit_start_dir(false);
        match &app.screen {
            Screen::Create(form) => {
                assert_eq!(form.workspace.start_dir, Some(PathBuf::from("aisbx")));
                assert_eq!(form.workspace.name.value, "My KI sandbox");
            }
            _ => panic!("back on the create form"),
        }
        on_step_four(&app);

        // Escaping out of the picker gives the form back too.
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(app.screen, Screen::StartDir(_)),
            "the picker again"
        );
        app.on_key(KeyEvent::from(KeyCode::Esc));
        match &app.screen {
            Screen::Create(form) => assert_eq!(form.workspace.name.value, "My KI sandbox"),
            _ => panic!("back on the create form"),
        }
        on_step_four(&app);
    }

    #[test]
    fn the_start_folder_is_offered_wherever_a_host_folder_is_mounted() {
        let claude = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut stranger = model::sample("gemini-lab", None, &[]);
        stranger.agent = "gemini".into();
        let mut scratch = model::sample("claude-litellm-scratch", Some("claude-litellm"), &[]);
        scratch.workspace = PathBuf::new();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![claude, stranger, scratch]);

        app.cursor = 0;
        assert!(app.can_pick_start_dir());
        app.cursor = 1;
        assert!(
            app.can_pick_start_dir(),
            "`sbx run --workdir` needs nothing from the agent"
        );
        app.cursor = 2;
        assert!(!app.can_pick_start_dir());
        app.on_key(KeyEvent::from(KeyCode::Char('w')));
        assert!(
            matches!(app.screen, Screen::List),
            "no picker without a host folder"
        );
    }

    #[test]
    fn a_start_folder_outside_the_workspace_is_ignored() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let registry = &mut app.registry;
        registry.set_start_dir("s", Some(Path::new("../../etc")));
        assert_eq!(registry.start_dir("s"), None, "never written");

        registry.set_start_dir("s", Some(Path::new("apps/web")));
        assert_eq!(registry.start_dir("s"), Some(PathBuf::from("apps/web")));
        registry.set_start_dir("s", None);
        assert_eq!(
            registry.start_dir("s"),
            None,
            "the root stores nothing at all"
        );
    }

    #[test]
    fn starting_a_sandbox_hands_it_the_label_the_dashboard_knows_it_by() {
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.registry.set_display_name(
            "claude-litellm-proj",
            Some("Bob's \"weird\" $label `here`".into()),
        );

        app.cursor = 0;
        app.action_start();
        let prelude = app.handoff.take().expect("a session was handed over");
        let argv = &prelude.prelude()[0];

        // A positional, never interpolated: quotes, `$` and backticks in the label stay data.
        assert_eq!(argv.last().unwrap(), "Bob's \"weird\" $label `here`");
        assert!(
            argv[5].contains("$0") && !argv[5].contains("Bob"),
            "the script reads the label from $0: {}",
            argv[5]
        );
    }

    #[test]
    fn connecting_before_ssh_is_set_up_explains_itself_instead_of_failing_later() {
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.screen = Screen::Connect(ConnectForm {
            sandbox: name.clone(),
            ssh_ready: false,
            focus: 0,
        });
        // Row 0 is the setup button while SSH is unconfigured, so the targets shift down by one.
        let rows = match &app.screen {
            Screen::Connect(form) => form.rows(),
            _ => unreachable!(),
        };
        assert_eq!(rows[0], ConnectRow::SetupSsh);
        app.submit_connect(0);
        assert!(app.handoff.is_none(), "nothing was launched");
        assert!(app.status.as_ref().is_some_and(|(_, err, _)| *err));

        // Once it is set up, the terminal target hands over `ssh <name>.sbx`.
        app.screen = Screen::Connect(ConnectForm {
            sandbox: name.clone(),
            ssh_ready: true,
            focus: 0,
        });
        app.submit_connect(0);
        let handoff = app.handoff.take().expect("a connection was handed over");
        assert_eq!(handoff.argv(), vec!["ssh", &sbx::ssh_host(&name)]);

        // Gateway is set up inside the app: nothing runs, and the screen stays open.
        app.screen = Screen::Connect(ConnectForm {
            sandbox: name,
            ssh_ready: true,
            focus: 0,
        });
        let jetbrains = crate::remote::Target::ALL
            .iter()
            .position(|t| *t == crate::remote::Target::Jetbrains)
            .unwrap();
        app.submit_connect(jetbrains);
        assert!(app.handoff.is_none());
        assert!(matches!(app.screen, Screen::Connect(_)));
    }

    #[test]
    fn a_no_op_on_the_account_form_says_so() {
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        assert!(matches!(
            planned(&mut app, AccountChoice::Named("private".into()), "", ""),
            AuthPlan::Nothing(_)
        ));
        assert!(matches!(
            planned(
                &mut app_with_auth(Auth::Login, Vec::new()),
                AccountChoice::Login,
                "",
                ""
            ),
            AuthPlan::Nothing(_)
        ));
        // A token typed, then abandoned by picking /login again, is no change.
        assert!(matches!(
            planned(
                &mut app_with_auth(Auth::Login, Vec::new()),
                AccountChoice::Login,
                "sk-ant-oat01-x",
                "private"
            ),
            AuthPlan::Nothing(_)
        ));
    }

    #[test]
    fn a_gateway_sandbox_is_never_offered_the_auth_editor() {
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        assert!(app.can_edit_auth());
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert!(matches!(app.screen, Screen::ConnSwitch(_)));
        assert!(
            app.status.is_none(),
            "no dead end to explain: {:?}",
            app.status
        );

        // A sandbox that is not from this repo's kits at all has nothing to say either.
        let mut app = App::for_test(
            Catalog::load().unwrap(),
            vec![model::sample("ts-lab", None, &[])],
        );
        assert!(!app.can_edit_auth());
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert!(app.status.is_none());

        // …and on a subscription sandbox it opens, which is the whole point of the distinction.
        let mut app = app_with_auth(Auth::Login, Vec::new());
        assert!(app.can_edit_auth());
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert!(matches!(app.screen, Screen::AuthEdit(_)));
    }

    #[test]
    fn deleting_a_sandbox_keeps_its_named_token() {
        let delete = |app: &mut App| -> Confirm {
            app.action_delete();
            let Some(Modal::Confirm(confirm)) = &app.modal else {
                panic!("delete asks first")
            };
            confirm.clone()
        };
        let argv =
            |c: &Confirm| -> Vec<String> { c.job.steps.iter().map(|s| s.argv.join(" ")).collect() };

        // A named token stays: the other sandboxes on it keep their account.
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        peer_on(&mut app, "claude-token-other", "private");
        let confirm = delete(&mut app);
        let steps = argv(&confirm);
        assert_eq!(steps.len(), 1, "the container, and nothing else: {steps:?}");
        assert!(
            !steps.iter().any(|s| s.contains("secret rm")),
            "the token itself is stored once for the host: {steps:?}"
        );
        assert!(
            confirm.body.iter().any(|l| l.contains("stays on the host")),
            "and the confirmation says so: {:?}",
            confirm.body
        );
    }

    // --- the secrets screen ------------------------------------------------------------------

    /// The dashboard on a host holding one of everything, with a sandbox on the named token.
    fn app_with_secrets() -> App {
        let mut sb = model::sample(SB, Some("claude-token"), &[]);
        sb.workspace = PathBuf::from("/proj");
        sb.auth = Some(on_named("private", true));
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.secrets = Some(crate::testkit::sample_secrets());
        app
    }

    // --- the skills store, ports, prune and mountless sandboxes ----------------------------

    fn store(skills: &[&str]) -> sbx::SkillsStore {
        sbx::SkillsStore {
            store: "/Users/u/Library/Application Support/com.docker.sandboxes/sandboxes/\
                    agent-skills"
                .into(),
            skills: skills.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn open_skills(app: &mut App) -> Vec<SkillRow> {
        app.on_key(KeyEvent::from(KeyCode::Char('K')));
        assert!(
            matches!(app.screen, Screen::Skills(_)),
            "K opens the skills screen"
        );
        app.skill_rows()
    }

    fn focus_skill(app: &mut App, row: &SkillRow) {
        let at = app
            .skill_rows()
            .iter()
            .position(|r| r == row)
            .unwrap_or_else(|| panic!("{row:?} is on the screen"));
        let Screen::Skills(form) = &mut app.screen else {
            panic!("the skills screen is open")
        };
        form.focus = at;
    }

    #[test]
    fn the_skills_screen_lists_the_store_and_the_ways_to_change_it() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.skills = Some(store(&["pdf", "frontend-design"]));
        let rows = open_skills(&mut app);
        assert_eq!(
            rows,
            vec![
                SkillRow::Skill("pdf".into()),
                SkillRow::Skill("frontend-design".into()),
                SkillRow::Import,
                SkillRow::AddRepo,
                SkillRow::Update,
                SkillRow::RepoSkills,
                SkillRow::Close,
            ]
        );
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::List));

        // An empty store: nothing to update, so the row is not offered — and `u` says so.
        app.skills = Some(store(&[]));
        let rows = open_skills(&mut app);
        assert_eq!(
            rows,
            vec![
                SkillRow::Import,
                SkillRow::AddRepo,
                SkillRow::RepoSkills,
                SkillRow::Close
            ]
        );
        app.on_key(KeyEvent::from(KeyCode::Char('u')));
        assert!(app.modal.is_none());
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("nothing to update")));
        // …and the path is the one sbx reported, not the platform guess.
        assert!(app.skills_store_path().unwrap().ends_with("agent-skills"));
    }

    #[test]
    fn adding_skills_from_a_repository_builds_the_add_command() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.skills = Some(store(&[]));
        open_skills(&mut app);
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        let Some(Modal::Prompt(prompt)) = &mut app.modal else {
            panic!("a opens the repository prompt")
        };
        assert!(matches!(prompt.ask, Ask::AddSkills));
        assert!(!prompt.masked(), "a repository is not a secret");
        prompt.input = TextInput::new("anthropics/skills pdf docx");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Modal::Running(_, pending)) = &app.modal else {
            panic!("the add runs as a job")
        };
        assert_eq!(
            pending.steps[0].argv,
            sbx::skills_add_argv("anthropics/skills", &["pdf".into(), "docx".into()])
        );
        app.modal = None;

        // Not a repository at all: refused with the grammar, and nothing runs.
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        if let Some(Modal::Prompt(prompt)) = &mut app.modal {
            prompt.input = TextInput::new("skills");
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none(), "{:?}", app.modal.is_some());
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("owner/repo")));
    }

    #[test]
    fn removing_and_updating_skills_ask_first() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.skills = Some(store(&["pdf"]));
        open_skills(&mut app);
        focus_skill(&mut app, &SkillRow::Skill("pdf".into()));
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("d asks first")
        };
        assert!(confirm.danger);
        assert_eq!(
            confirm.job.steps[0].argv,
            sbx::skills_rm_argv(&["pdf".into()])
        );
        app.modal = None;

        focus_skill(&mut app, &SkillRow::Update);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("update asks first")
        };
        assert!(!confirm.danger, "nothing is destroyed, only replaced");
        assert_eq!(confirm.job.steps[0].argv, sbx::skills_update_argv(&[]));
        assert!(
            confirm.body.iter().any(|l| l.contains("import")),
            "it says imported skills are not touched: {:?}",
            confirm.body
        );
        app.modal = None;

        // `d` on anything but a skill does nothing.
        focus_skill(&mut app, &SkillRow::AddRepo);
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(app.modal.is_none());
    }

    #[test]
    fn a_confirmation_answers_with_the_name_of_its_action() {
        for action in [
            Action::Rebuild,
            Action::Delete,
            Action::KeepRunning,
            Action::Prune,
        ] {
            let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
            sb.running = action != Action::Prune;
            let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
            app.cursor = 0;
            app.perform(action);
            let Some(Modal::Confirm(confirm)) = &app.modal else {
                panic!("{action:?} asks first: {:?}", app.status)
            };
            assert_eq!(confirm.confirm_label, action.title(), "{action:?}");
            assert!(
                confirm.title.starts_with(action.title()),
                "{action:?}: {}",
                confirm.title
            );
        }
    }

    #[test]
    fn a_recreate_keeps_the_named_token() {
        let mut app = app_with_auth(on_named("private", true), vec![stored_token("private")]);
        app.cursor = 0;
        app.action_recreate();
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("a recreate asks first")
        };
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("stays on the setup token")),
            "{:?}",
            confirm.body
        );
    }

    #[test]
    fn unpublishing_one_port_fills_the_prompt_with_it() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.insight = crate::insight::Insight::sample("box");
        let ports = app.insight.ports.value.clone().unwrap();
        assert_eq!(ports.len(), 2);
        let prefill = |app: &App| match &app.modal {
            Some(Modal::Prompt(p)) if matches!(p.ask, Ask::UnpublishPort) => p.input.value.clone(),
            other => panic!("no unpublish prompt: {other:?}"),
        };
        app.on_key(KeyEvent::from(KeyCode::Char('u')));
        assert_eq!(prefill(&app), "", "two ports: the key cannot tell which");
        app.modal = None;
        app.unpublish(&ports[1]);
        assert_eq!(prefill(&app), ports[1].spec());

        app.modal = None;
        app.cursor = app.here;
        app.unpublish(&ports[0]);
        assert!(app.modal.is_none(), "nothing to unpublish on the New row");
        assert!(app.status.is_none(), "and, as for `u`, nothing to say");
    }

    #[test]
    fn publishing_a_port_is_offered_on_a_stopped_sandbox() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.running = false;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('p')));
        let Some(Modal::Prompt(prompt)) = &app.modal else {
            panic!("the port prompt opens on a stopped sandbox")
        };
        assert!(matches!(prompt.ask, Ask::PublishPort));
        assert!(
            prompt.help.iter().any(|l| l.contains("started first")),
            "and says the sandbox is started: {:?}",
            prompt.help
        );
        assert!(
            prompt.help.iter().any(|l| l.contains("tcp4")),
            "and what a bare spec binds since 0.42: {:?}",
            prompt.help
        );
    }

    #[test]
    fn the_prune_list_names_a_sandbox_once_where_its_label_is_its_name() {
        let mut named = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
        named.label = "My private Claude Code".into();
        let bare = model::sample("ts-lab", None, &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![named, bare]);
        app.action_prune();
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("prune asks first")
        };
        assert_eq!(
            confirm.body[..2],
            ["My private Claude Code  (claude-anthropic-proj)", "ts-lab"],
            "{:?}",
            confirm.body
        );
        // The history stays only where a kit keeps it in volumes, so the text says where.
        let text = confirm.body.join(" ");
        assert!(
            text.contains("Claude Code's earlier sessions stay"),
            "{text}"
        );
        assert!(
            text.contains("other agents' sessions are deleted"),
            "{text}"
        );
    }

    #[test]
    fn a_request_is_answered_by_its_id_without_the_approvals_screen() {
        for (allow, option) in [(true, "allow"), (false, "dismiss")] {
            let mut app = App::for_test(
                Catalog::load().unwrap(),
                vec![model::sample("box", Some("claude-anthropic"), &[])],
            );
            app.approvals = vec![
                crate::testkit::held("box", "one.example:443"),
                crate::testkit::held("box", "two.example:443"),
            ];
            let id = app.approvals[1].id.clone();
            app.answer_approval(&id, allow);
            assert!(matches!(app.screen, Screen::List), "no screen opens");
            let titles: Vec<&str> = (app.approvals_of("box").into_iter())
                .map(|a| a.title.as_str())
                .collect();
            assert_eq!(titles, ["one.example:443"]);
            let Some(Modal::Running(_, job)) = &app.modal else {
                panic!("{option}: a job runs")
            };
            assert_eq!(job.steps[0].argv, sbx::approval_respond_argv(&id, option));
            assert!(job.title.contains("two.example:443"), "{}", job.title);

            app.modal = None;
            app.answer_approval(&id, allow);
            assert!(app.modal.is_none(), "{option}: answered once");
        }
    }

    #[test]
    fn a_mountless_sandbox_is_rebuilt_mountless() {
        let mut sb = model::sample("claude-litellm-scratch", Some("claude-litellm"), &[]);
        sb.workspace = PathBuf::new();
        assert!(sb.mountless());
        assert_eq!(sb.folder_label(), "no host folder (mountless)");
        assert!(sb.mounts().is_empty());
        assert!(!sb.can_pick_start_dir());
        assert_eq!(
            sb.remote_workspace(),
            PathBuf::from(model::MOUNTLESS_WORKDIR)
        );
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let job = app
            .recreate_job("claude-litellm-scratch", &[], None)
            .expect("a rebuild plan");
        let create = job.steps[1].argv.join(" ");
        assert!(
            create.ends_with("kits/claude-litellm"),
            "no workspace positional after the workload: {create}"
        );
        assert!(!create.contains("/proj"), "{create}");
        // Mountless, so it belongs to no folder: it sits in the second section, past + New.
        app.cursor = app.here + 1;
        assert_eq!(
            app.selected().map(|s| s.name.as_str()),
            Some("claude-litellm-scratch")
        );
        app.action_delete();
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("delete asks first")
        };
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains(model::MOUNTLESS_WORKDIR)),
            "its files live inside it, and go with it: {:?}",
            confirm.body
        );
    }

    fn open_secrets(app: &mut App) -> Vec<SecretRow> {
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        assert!(
            matches!(app.screen, Screen::Secrets(_)),
            "S opens the screen"
        );
        app.secret_rows()
    }

    fn focus_on(app: &mut App, row: &SecretRow) {
        let at = app
            .secret_rows()
            .iter()
            .position(|r| r == row)
            .unwrap_or_else(|| panic!("{row:?} is on the screen"));
        let Screen::Secrets(form) = &mut app.screen else {
            panic!("the secrets screen is open")
        };
        form.focus = at;
    }

    #[test]
    fn the_secrets_screen_lists_every_credential_the_host_holds() {
        let mut app = app_with_secrets();
        let rows = open_secrets(&mut app);
        assert_eq!(rows[0], SecretRow::Oauth);
        assert!(
            rows.contains(&SecretRow::Token("private".into())),
            "{rows:?}"
        );
        assert!(rows.contains(&SecretRow::Token("work".into())), "{rows:?}");
        // Stored, so listed though no kit declares it.
        assert!(
            rows.contains(&SecretRow::global_service("litellm")),
            "{rows:?}"
        );
        // A connection comes from its `connection.json`; a key no file names is an orphan.
        assert!(
            rows.contains(&SecretRow::Connection("acme".into())),
            "{rows:?}"
        );
        assert!(
            rows.contains(&SecretRow::LitellmOrphan("gone".into())),
            "{rows:?}"
        );
        assert_eq!(rows.last(), Some(&SecretRow::Close));

        // Someone else's custom secret is shown on the screen but is not a row anything acts on.
        assert!(
            !rows.iter().any(
                |r| matches!(r, SecretRow::Service { name, .. } if name == "someone-elses-secret")
            ),
            "{rows:?}"
        );

        // An empty host still offers the two rows that can be fixed from here.
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.secrets = Some(sbx::Secrets::default());
        let rows = open_secrets(&mut app);
        assert_eq!(rows[0], SecretRow::Oauth);
        assert!(
            rows.contains(&SecretRow::Connection("acme".into())),
            "listed with its key missing: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, SecretRow::LitellmOrphan(_))),
            "{rows:?}"
        );
    }

    #[test]
    fn a_sandbox_scoped_service_secret_is_its_own_row() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.secrets = Some(sbx::Secrets {
            services: vec![
                sbx::Service::sample("litellm"),
                sbx::Service {
                    scope: "claude-ki".into(),
                    ..sbx::Service::sample("litellm")
                },
            ],
            custom: Vec::new(),
        });
        let scoped = SecretRow::Service {
            name: "litellm".into(),
            scope: Some("claude-ki".into()),
        };
        let rows = open_secrets(&mut app);
        assert!(
            rows.contains(&SecretRow::global_service("litellm")),
            "{rows:?}"
        );
        assert!(rows.contains(&scoped), "{rows:?}");

        focus_on(&mut app, &scoped);
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("{:?}", app.modal)
        };
        assert_eq!(
            confirm.job.steps[0].argv.join(" "),
            "sbx secret rm litellm --sandbox claude-ki -f",
            "without --sandbox this would delete the global secret instead"
        );
    }

    #[test]
    fn storing_a_secret_passes_the_value_on_stdin() {
        let mut app = app_with_secrets();
        let cases = [
            (
                SecretRow::Token("private".into()),
                "secret set-custom --host",
                sbx::named_token_placeholder("private"),
            ),
            (
                SecretRow::global_service("litellm"),
                "secret set litellm -f",
                "litellm".to_string(),
            ),
        ];
        for (row, expected, needle) in cases {
            open_secrets(&mut app);
            focus_on(&mut app, &row);
            app.on_key(KeyEvent::from(KeyCode::Enter));
            let Some(Modal::Prompt(prompt)) = &app.modal else {
                panic!("{row:?} asks for a value: {:?}", app.modal)
            };
            assert!(prompt.masked(), "a pasted secret is never echoed back");
            for c in "sk-ant-oat01-x".chars() {
                app.on_key(KeyEvent::from(KeyCode::Char(c)));
            }
            app.on_key(KeyEvent::from(KeyCode::Enter));
            let Some(Modal::Running(_, pending)) = &app.modal else {
                panic!("{row:?} stores it: {:?}", app.modal)
            };
            let argv = pending.steps[0].argv.join(" ");
            assert!(argv.contains(expected), "{argv}");
            assert!(argv.contains(&needle), "{argv}");
            assert!(!argv.contains("sk-ant"), "the value rides on stdin: {argv}");
            assert_eq!(pending.steps[0].stdin.as_deref(), Some("sk-ant-oat01-x"));
            app.modal = None;
        }

        // `/login` is an OAuth exchange inside a sandbox: nothing here can write it.
        open_secrets(&mut app);
        focus_on(&mut app, &SecretRow::Oauth);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none(), "{:?}", app.modal);
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("/login")));
    }

    #[test]
    fn deleting_a_secret_names_what_it_reaches() {
        let mut app = app_with_secrets();
        let confirm = |app: &mut App, row: &SecretRow| -> Confirm {
            open_secrets(app);
            focus_on(app, row);
            app.on_key(KeyEvent::from(KeyCode::Char('d')));
            let Some(Modal::Confirm(confirm)) = &app.modal else {
                panic!("{row:?} asks first: {:?}", app.modal)
            };
            let confirm = confirm.clone();
            app.modal = None;
            confirm
        };

        let token = confirm(&mut app, &SecretRow::Token("private".into()));
        let argv = token.job.steps[0].argv.join(" ");
        assert!(argv.contains("secret rm --placeholder"), "{argv}");
        assert!(
            argv.contains(&sbx::named_token_placeholder("private")),
            "{argv}"
        );
        assert!(
            token.body.iter().any(|l| l.contains(SB)),
            "the sandbox that breaks is named: {:?}",
            token.body
        );
        assert_eq!(
            token.job.post.drop_token_kit.as_deref(),
            Some("private"),
            "the generated kit goes with the token it wired"
        );

        // Nothing is on "work", and the confirmation says so rather than implying casualties.
        let unused = confirm(&mut app, &SecretRow::Token("work".into()));
        assert!(unused.body[0].contains("No sandbox"), "{:?}", unused.body);

        let oauth = confirm(&mut app, &SecretRow::Oauth);
        assert_eq!(
            oauth.job.steps[0].argv.join(" "),
            "sbx secret rm anthropic -f"
        );
        assert!(
            oauth.body.iter().any(|l| l.contains("whole machine")),
            "{:?}",
            oauth.body
        );

        let service = confirm(&mut app, &SecretRow::global_service("litellm"));
        assert_eq!(
            service.job.steps[0].argv.join(" "),
            "sbx secret rm litellm -f"
        );

        // A connection: its key by placeholder, then — only once that succeeded — its folder.
        let mut app = App::for_test(
            Catalog::load().unwrap(),
            vec![model::sample(
                "claude-litellm-proj",
                Some("claude-litellm"),
                &[],
            )],
        );
        app.secrets = Some(crate::testkit::sample_secrets());
        let conn = confirm(&mut app, &SecretRow::Connection("acme".into()));
        assert_eq!(
            conn.job.steps[0].argv.join(" "),
            "sbx secret rm --placeholder sbx-cs-aisbx-llm-acme -f"
        );
        assert!(
            conn.body[0].contains("claude-litellm-proj") && conn.body[0].contains("stop"),
            "the sandboxes on it are named, and what they lose: {:?}",
            conn.body
        );
        assert_eq!(conn.job.post.drop_connection.as_deref(), Some("acme"));
        // A connection with no key stored: no sbx call, only its folder.
        let mut keyless = crate::testkit::sample_secrets();
        keyless
            .custom
            .retain(|c| c.placeholder != crate::litellm::placeholder("acme"));
        app.secrets = Some(keyless);
        let bare = confirm(&mut app, &SecretRow::Connection("acme".into()));
        assert!(bare.job.steps.is_empty(), "{:?}", bare.job.steps);
        assert_eq!(bare.job.post.drop_connection.as_deref(), Some("acme"));
        // Secrets not read yet: the removal runs, so a real failure still shows.
        app.secrets = None;
        let unread = confirm(&mut app, &SecretRow::Connection("acme".into()));
        assert_eq!(unread.job.steps.len(), 1);
        app.secrets = Some(crate::testkit::sample_secrets());
        // An orphan key: only the key, there is no folder.
        let orphan = confirm(&mut app, &SecretRow::LitellmOrphan("gone".into()));
        assert_eq!(
            orphan.job.steps[0].argv.join(" "),
            "sbx secret rm --placeholder sbx-cs-aisbx-llm-gone -f"
        );
        assert_eq!(orphan.job.post.drop_connection, None);
        assert!(
            orphan
                .body
                .iter()
                .any(|l| l.contains("nothing uses the key")),
            "{:?}",
            orphan.body
        );
        // A connection.json removed by hand leaves the sandboxes built on it: they are named.
        app.sandboxes[0].litellm = Some("gone".into());
        let used = confirm(&mut app, &SecretRow::LitellmOrphan("gone".into()));
        assert!(
            used.body
                .iter()
                .any(|l| l.contains("claude-litellm-proj") && l.contains("stop authenticating")),
            "{:?}",
            used.body
        );
        assert!(
            !used.body.iter().any(|l| l.contains("nothing uses")),
            "{:?}",
            used.body
        );

        // A service with nothing stored has nothing to delete, and says so instead of running.
        let mut empty = App::for_test(Catalog::load().unwrap(), Vec::new());
        empty.secrets = Some(sbx::Secrets::default());
        open_secrets(&mut empty);
        empty.delete_secret(&SecretRow::global_service("litellm"));
        assert!(empty.modal.is_none(), "{:?}", empty.modal);
    }

    /// A host with the agy backend in its catalog, and — or not — Antigravity's shared Google
    /// sign-in stored, the way sbx reports one: global scope, `(oauth configured)`.
    fn app_with_agy_login(stored: bool) -> App {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let mut secrets = sbx::Secrets::default();
        if stored {
            secrets.services.push(sbx::Service {
                masked: "(oauth configured)".into(),
                ..sbx::Service::sample(sbx::AGY_OAUTH_SERVICE)
            });
        }
        app.secrets = Some(secrets);
        app
    }

    #[test]
    fn agy_sign_in_is_one_dedicated_secrets_row() {
        for stored in [false, true] {
            let mut app = app_with_agy_login(stored);
            let rows = open_secrets(&mut app);
            let agy = rows.iter().filter(|r| **r == SecretRow::AgyLogin).count();
            assert_eq!(agy, 1, "stored: {stored}: {rows:?}");
            assert!(
                !rows.contains(&SecretRow::global_service("antigravity")),
                "stored: {stored}: {rows:?}"
            );
        }
    }

    #[test]
    fn agy_sign_in_row_refuses_to_ask_for_a_value() {
        for stored in [false, true] {
            let mut app = app_with_agy_login(stored);
            open_secrets(&mut app);
            focus_on(&mut app, &SecretRow::AgyLogin);
            app.on_key(KeyEvent::from(KeyCode::Enter));
            assert!(app.modal.is_none(), "stored: {stored}: {:?}", app.modal);
            let (msg, _, _) = app.status.as_ref().expect("an info line");
            assert!(msg.contains("agy sandbox"), "{msg}");
            assert!(msg.contains("d removes"), "{msg}");
        }
    }

    #[test]
    fn agy_sign_in_row_deletes_the_global_secret_after_asking() {
        let mut app = app_with_agy_login(true);
        open_secrets(&mut app);
        focus_on(&mut app, &SecretRow::AgyLogin);
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("asks first: {:?}", app.modal)
        };
        assert_eq!(
            confirm.job.steps[0].argv,
            ["sbx", "secret", "rm", "antigravity", "-f"]
        );
        let body = confirm.body.join("\n");
        assert!(body.contains("one Google account"), "{body}");
        assert!(body.contains("switches accounts"), "{body}");
        assert!(body.contains("does not revoke"), "{body}");
        assert!(!body.contains("signs every agy sandbox out"), "{body}");
        assert!(!body.contains("only ever"), "{body}");

        // Nothing stored, nothing to confirm: an info line instead of a modal.
        let mut empty = app_with_agy_login(false);
        open_secrets(&mut empty);
        focus_on(&mut empty, &SecretRow::AgyLogin);
        empty.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(empty.modal.is_none(), "{:?}", empty.modal);
        assert!(empty.status.is_some());
    }

    #[test]
    fn text_input_is_char_indexed() {
        let mut input = TextInput::new("äb");
        assert_eq!(input.cursor, 2);
        input.left();
        input.insert('x');
        assert_eq!(input.value, "äxb");
        input.home();
        input.delete();
        assert_eq!(input.value, "xb");
        input.backspace();
        assert_eq!(input.value, "xb");
    }

    #[test]
    fn a_text_field_takes_the_shell_line_editing_keys() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut input = TextInput::new("8080:3000, äpi  5173");
        assert!(edit_text(&mut input, ctrl('w')));
        assert_eq!(
            (input.value.as_str(), input.cursor),
            ("8080:3000, äpi  ", 16)
        );
        edit_text(&mut input, ctrl('w'));
        assert_eq!((input.value.as_str(), input.cursor), ("8080:3000, ", 11));
        edit_text(&mut input, ctrl('a'));
        assert_eq!(input.cursor, 0);
        edit_text(&mut input, KeyEvent::from(KeyCode::Right));
        edit_text(&mut input, ctrl('k'));
        assert_eq!(input.value, "8");
        edit_text(&mut input, ctrl('e'));
        input.insert('0');
        assert_eq!(input.value, "80");
        input.left();
        edit_text(&mut input, ctrl('u'));
        assert_eq!((input.value.as_str(), input.cursor), ("0", 0));
        // Another chord is the form's: Ctrl-S still creates from a text row.
        assert!(!edit_text(&mut input, ctrl('s')));
        assert_eq!(input.value, "0");
    }

    // --- model aliases ----------------------------------------------------------------------

    /// The gateway sandboxes these tests act on, one name per test: a model kit is keyed by
    /// sandbox name, so tests sharing a name would race under `cargo test`'s parallelism.
    const GATEWAY_CREATE: &str = "claude-litellm-aisbx-models-create";
    const GATEWAY_EDIT: &str = "claude-litellm-aisbx-models-edit";

    fn gateway_app(name: &str) -> App {
        let sb = model::sample(name, Some("claude-litellm"), &[]);
        App::for_test(Catalog::load().unwrap(), vec![sb])
    }

    fn first_model(app: &App) -> String {
        app.catalog
            .models
            .first()
            .expect("the opencode-litellm kit declares the gateway's models")
            .id
            .clone()
    }

    #[test]
    fn the_model_rows_belong_to_the_gateway_backend_only() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        // The form opens on the gateway backend, so the rows are there from the start.
        assert!(form.models_offered(&app.catalog));
        let rows = form.rows(&app.catalog);
        for slot in Slot::ALL {
            assert!(
                rows.contains(&CreateRow::Model(slot)),
                "{slot:?} in {rows:?}"
            );
        }

        let subscription = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account() == Some(Account::Login))
            .expect("the /login backend is offered");
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = subscription;
        }
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert!(!form.models_offered(&app.catalog));
        assert!(
            !form
                .rows(&app.catalog)
                .iter()
                .any(|r| matches!(r, CreateRow::Model(_))),
            "a subscription backend has no gateway model to pin"
        );

        // ...and the section stays hidden through the account radio, which swaps the kit.
        if let Screen::Create(form) = &mut app.screen {
            form.connection.account = AccountChoice::New;
        }
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert!(!form.models_offered(&app.catalog));
    }

    #[test]
    fn picking_a_model_pins_the_alias_and_closes_the_list() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.open_create();
        let want = first_model(&app);

        let press = |app: &mut App, row: CreateRow| {
            wizard_on(app, CreateStep::Models, WizardRow::Field(row));
            app.on_key(KeyEvent::from(KeyCode::Enter));
        };

        // ↵ on the alias opens its list and steps onto the first entry, so ↵↵ is "leave it alone".
        press(&mut app, CreateRow::Model(Slot::Opus));
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(form.connection.model_pick, Some(Slot::Opus));
        assert_eq!(
            form.wizard_rows(&app.catalog)[form.wizard_focus()],
            WizardRow::Field(CreateRow::ModelDefault(Slot::Opus)),
            "the cursor steps into what opened"
        );

        press(&mut app, CreateRow::ModelPick(Slot::Opus, 0));
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(form.connection.models.get(Slot::Opus), Some(want.as_str()));
        assert_eq!(
            form.connection.model_pick, None,
            "the answer is given; the list closes"
        );
        assert_eq!(
            form.wizard_rows(&app.catalog)[form.wizard_focus()],
            WizardRow::Field(CreateRow::Model(Slot::Opus)),
            "and the cursor is back on the alias"
        );
        // Only the alias that was picked — the other two stay on Claude Code's defaults.
        assert_eq!(form.connection.models.get(Slot::Sonnet), None);
        assert_eq!(form.connection.models.get(Slot::Haiku), None);

        // "(default)" is how a pin is taken back off.
        press(&mut app, CreateRow::Model(Slot::Opus));
        press(&mut app, CreateRow::ModelDefault(Slot::Opus));
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert!(
            form.connection.models.is_empty(),
            "{:?}",
            form.connection.models
        );
    }

    #[test]
    fn the_create_form_stacks_the_model_kit_only_when_something_is_pinned() {
        let create = |models: Overrides| -> String {
            let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
            app.cwd = PathBuf::from("/aisbx-models-create");
            app.open_create();
            if let Screen::Create(form) = &mut app.screen {
                form.connection.models = models;
            }
            press_create_only(&mut app);
            let Some(Modal::Running(_, pending)) = &app.modal else {
                panic!("a job was started")
            };
            pending.steps.last().unwrap().argv.join(" ")
        };

        let plain = create(Overrides::default());
        assert!(plain.contains("kits/claude-litellm"), "{plain}");
        assert!(
            !plain.contains(&format!("{}", models::kit_dir(GATEWAY_CREATE).display())),
            "nothing pinned, nothing to wire in: {plain}"
        );

        // The models a sandbox on the connection the form is on, `acme`, harvested.
        let catalog = crate::testkit::harvested("acme");
        let mut pinned = Overrides::default();
        pinned.set(Slot::Haiku, Some(catalog[0].clone()));
        let with_models = create(pinned);
        assert!(
            with_models.contains(&format!("{}", models::kit_dir(GATEWAY_CREATE).display())),
            "the generated kit carries the variables into the container: {with_models}"
        );
        // …and it really does set the variable Claude Code reads.
        let spec = std::fs::read_to_string(crate::kits::descriptor_path(&models::kit_dir(
            GATEWAY_CREATE,
        )))
        .unwrap();
        assert!(spec.contains(Slot::Haiku.env()), "{spec}");
        assert!(spec.contains(&catalog[0]), "{spec}");

        models::remove_kit(GATEWAY_CREATE).unwrap();
    }

    #[test]
    fn a_first_pin_is_added_live_and_changing_one_rebuilds() {
        let apply = |models: Overrides, original: Overrides| -> App {
            let mut app = gateway_app(GATEWAY_EDIT);
            app.open_kits(GATEWAY_EDIT);
            if let Screen::Kits(form) = &mut app.screen {
                assert!(form.models_offered, "a gateway sandbox can pin its models");
                form.original_models = original;
                form.models = models;
            }
            app.submit_kits();
            app
        };

        // The models a sandbox on the sandbox's connection, `acme`, harvested.
        let catalog = crate::testkit::harvested("acme");
        let mut first = Overrides::default();
        first.set(Slot::Opus, Some(catalog[0].clone()));
        let mut second = Overrides::default();
        second.set(Slot::Opus, Some(catalog.last().unwrap().clone()));

        // Nothing pinned before → `sbx kit add`, which recreates the container in place.
        let app = apply(first.clone(), Overrides::default());
        let Some(Modal::Running(_, pending)) = &app.modal else {
            panic!("a first pin applies without asking: {:?}", app.modal)
        };
        let steps: Vec<String> = pending.steps.iter().map(|s| s.argv.join(" ")).collect();
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert!(steps[0].starts_with("sbx kit add"), "{steps:?}");
        assert!(
            steps[0].contains(&format!("{}", models::kit_dir(GATEWAY_EDIT).display())),
            "{steps:?}"
        );
        // The rebuild is still offered as the fallback, since `kit add` can refuse.
        assert!(pending.fallback.is_some());

        // Already pinned → a rebuild, confirmed first because it destroys the container.
        let app = apply(second, first.clone());
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("changing a pin asks first: {:?}", app.modal)
        };
        assert!(confirm.danger);
        let steps: Vec<String> = confirm.job.steps.iter().map(|s| s.argv.join(" ")).collect();
        assert!(steps[0].starts_with("sbx rm"), "{steps:?}");
        assert!(
            steps[1].contains(&format!("{}", models::kit_dir(GATEWAY_EDIT).display())),
            "{steps:?}"
        );

        // Clearing every pin is the same rebuild, and leaves the container without the variables.
        let app = apply(Overrides::default(), first);
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("clearing a pin asks first: {:?}", app.modal)
        };
        let create = confirm.job.steps.last().unwrap().argv.join(" ");
        assert!(
            !create.contains(&format!("{}", models::kit_dir(GATEWAY_EDIT).display())),
            "back on Claude Code's defaults, with no kit left to set them: {create}"
        );
        assert!(
            !models::kit_dir(GATEWAY_EDIT).exists(),
            "and the stale kit is gone rather than waiting to be re-stacked"
        );

        // Neither costs anything when nothing changed.
        let app = apply(Overrides::default(), Overrides::default());
        assert!(app.modal.is_none(), "{:?}", app.modal);
        assert!(matches!(app.screen, Screen::List));

        models::remove_kit(GATEWAY_EDIT).unwrap();
    }

    #[test]
    fn joins_kit_labels_for_prose() {
        assert_eq!(join_labels(&["rtk".into()]), "RTK");
        assert_eq!(join_labels(&["rtk".into(), "web".into()]), "RTK and Web");
        assert_eq!(
            join_labels(&["jdk25".into(), "rtk".into(), "web".into()]),
            "JDK25, RTK and Web"
        );
    }

    /// Print what a rebuild of a real sandbox would run, without running it:
    /// `AISBX_PLAN_SANDBOX=<name> cargo test -- --ignored --nocapture live_rebuild_plan`.
    #[test]
    #[ignore = "reads the local sbx daemon; run explicitly"]
    fn live_rebuild_plan() {
        let Ok(name) = std::env::var("AISBX_PLAN_SANDBOX") else {
            println!("set AISBX_PLAN_SANDBOX to a sandbox name");
            return;
        };
        let catalog = Catalog::load().unwrap();
        let snapshot = model::gather(&catalog).expect("sbx ls/inspect");
        let mut registry = Registry::load();
        let sandboxes = model::resolve(&snapshot, &catalog, &mut registry);
        let mut app = App::for_test(catalog, sandboxes);
        // Only the real registry and connections folder know this sandbox.
        app.registry = registry;
        app.litellm_root = crate::litellm::dir();
        let sb = app
            .sandboxes
            .iter()
            .find(|s| s.name == name)
            .expect("that sandbox");
        let addons = sb.addons.clone();
        let confirm = app
            .recreate_confirm(&name, &addons, None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        for line in &confirm.body {
            println!("| {line}");
        }
        for step in &confirm.job.steps {
            println!("$ {}", step.argv.join(" "));
        }
    }

    /// A request held for approval, the way `sbx policy approval ls --json` reports it.
    fn held(sandbox: &str, title: &str) -> sbx::Approval {
        sbx::Approval {
            id: format!("network:{sandbox}-{title}"),
            sandbox: sandbox.into(),
            title: title.into(),
            detail: "Protocol: TCP\nResource type: domain\nno matching allow rule (default deny)"
                .into(),
            options: vec![
                sbx::ApprovalOption {
                    id: "allow".into(),
                    label: "Allow".into(),
                },
                sbx::ApprovalOption {
                    id: "dismiss".into(),
                    label: "Dismiss".into(),
                },
            ],
        }
    }

    /// The job a key press started, if it started one.
    fn running_argvs(app: &App) -> Vec<Vec<String>> {
        match &app.modal {
            Some(Modal::Running(_, pending)) => {
                pending.steps.iter().map(|s| s.argv.clone()).collect()
            }
            other => panic!("expected a job, got {other:?}"),
        }
    }

    #[test]
    fn the_approvals_screen_answers_the_request_under_the_cursor() {
        let here = model::sample("claude-here", Some("claude-anthropic"), &[]);
        let mut far = model::sample("claude-far", Some("claude-anthropic"), &[]);
        far.workspace = PathBuf::from("/elsewhere");
        let mut app = App::for_test(Catalog::load().unwrap(), vec![far, here]);
        app.approvals = vec![
            held("claude-far", "example.net:80"),
            held("claude-here", "example.org:443"),
        ];
        assert_eq!(app.approvals_for("claude-here"), 1);

        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(matches!(app.screen, Screen::Approvals(_)));
        let rows = app.approval_rows();
        assert_eq!(
            rows,
            vec![
                ApprovalRow::Request("network:claude-here-example.org:443".into()),
                ApprovalRow::Request("network:claude-far-example.net:80".into()),
                ApprovalRow::Close,
            ],
            "this folder's sandbox first"
        );

        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(
            running_argvs(&app),
            vec![sbx::approval_respond_argv(
                "network:claude-here-example.org:443",
                "allow"
            )]
        );
        // Out of the list at once, so a second press cannot answer it twice.
        assert_eq!(app.approvals.len(), 1);

        app.modal = None;
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(
            running_argvs(&app),
            vec![sbx::approval_respond_argv(
                "network:claude-far-example.net:80",
                "dismiss"
            )]
        );
        app.modal = None;
        assert!(app.approvals.is_empty());
        // With nothing left, ↵ is the Close button.
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::List));
    }

    #[test]
    fn keeping_a_sandbox_running_is_read_back_and_survives_a_rebuild() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.running = true;
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        app.on_key(KeyEvent::from(KeyCode::Char('b')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("b asks first: {:?}", app.modal);
        };
        assert_eq!(confirm.job.steps[0].argv, sbx::keep_running_argv(&name));
        assert!(confirm.job.steps[0].argv.iter().any(|a| a == "--detach"));
        assert!(
            confirm.job.post.register.is_none(),
            "nothing is remembered about it"
        );
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("A stop and a start keep it so")),
            "{:?}",
            confirm.body
        );

        // Once sbx reports it on, a rebuild repeats it straight after the create.
        sb.keep_running = true;
        sb.runtime_mounts = vec![sbx::RuntimeMount {
            host_path: "/h/docs".into(),
            container_target: "/h/docs".into(),
            read_only: true,
        }];
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let job = app
            .recreate_job(&name, &[], None)
            .expect("a managed sandbox");
        let labels: Vec<&str> = job.steps.iter().map(|s| s.label.as_str()).collect();
        let create = labels
            .iter()
            .position(|l| l.starts_with("sbx create"))
            .unwrap();
        assert_eq!(job.steps[create + 1].argv, sbx::keep_running_argv(&name));
        // No `exec true` in front of the mount: the detached switch already started it.
        assert_eq!(
            job.steps[create + 2].argv,
            vec!["sbx", "mount", &name, "/h/docs:/h/docs:ro"]
        );

        // And `b` on it now offers the only way back sbx has: a rebuild without it.
        app.on_key(KeyEvent::from(KeyCode::Char('b')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!(
                "b on a kept-running sandbox offers the rebuild: {:?}",
                app.modal
            );
        };
        assert!(confirm
            .job
            .steps
            .iter()
            .all(|s| s.argv != sbx::keep_running_argv(&name)));
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("then stops after its last session")),
            "{:?}",
            confirm.body
        );
    }

    #[test]
    fn a_sandbox_whose_folder_is_gone_is_never_rebuilt() {
        let mut sb = model::sample("claude-litellm-gone", Some("claude-litellm"), &["web"]);
        sb.workspace_missing = true;
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        assert_eq!(app.rebuild_blocker(&name), Some(WORKSPACE_GONE));
        assert!(
            WORKSPACE_GONE.contains("on this computer") && !WORKSPACE_GONE.contains("host"),
            "{WORKSPACE_GONE}"
        );
        assert!(app.recreate_job(&name, &[], None).is_none());
        app.on_key(KeyEvent::from(KeyCode::Char('R')));
        assert!(
            app.modal.is_none(),
            "no rebuild is offered: {:?}",
            app.modal
        );
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(m, err, _)| *err && m.contains("no longer exists")),
            "{:?}",
            app.status
        );
        // A mountless sandbox has no folder to lose, and one whose folder is there rebuilds.
        sb.workspace = PathBuf::new();
        let app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        assert_eq!(app.rebuild_blocker(&name), None);
        sb.workspace = PathBuf::from("/proj");
        sb.workspace_missing = false;
        let app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        assert!(app.recreate_job(&name, &[], None).is_some());
    }

    #[test]
    fn a_rebuild_keeps_the_git_guard_and_mounts_the_host_folders_listed_now() {
        let root = std::env::temp_dir().join(format!(
            "aisbx-tests-rebuild-folders-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["proj/.git", "kb", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.workspace = root.join("proj");
        sb.ro_mounts = vec![root.join("proj/.git"), PathBuf::from("/gone/from/the/list")];
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.mounts_state = root.join("state");
        let kb = folders::Entry {
            path: root.join("kb"),
            access: folders::Access::Write,
            purpose: String::new(),
        };
        folders::put(&app.mounts_state, kb).unwrap();
        let confirm = app
            .recreate_confirm(&name, &[], None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        let create = (confirm.job.steps.iter())
            .find(|s| s.argv.get(1).is_some_and(|a| a == "create"))
            .unwrap();
        let has = |spec: String| create.argv.contains(&spec);
        assert!(
            has(format!("{}:ro", root.join("proj/.git").display())),
            "{:?}",
            create.argv
        );
        assert!(
            has(root.join("kb").display().to_string()),
            "{:?}",
            create.argv
        );
        assert!(!create
            .argv
            .iter()
            .any(|a| a.contains("/gone/from/the/list")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_rebuild_mounts_the_folders_typed_for_it_again_and_names_one_that_is_gone() {
        let root =
            std::env::temp_dir().join(format!("aisbx-tests-rebuild-extras-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["proj", "data", "state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let mut sb = model::sample("claude-litellm-more", Some("claude-litellm"), &[]);
        sb.workspace = root.join("proj");
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.mounts_state = root.join("state");
        let data = format!("{}:/workspace/data:ro", root.join("data").display());
        let gone = root.join("gone").display().to_string();
        app.registry
            .set_extra_folders(&name, vec![data.clone(), gone.clone()]);
        let confirm = app
            .recreate_confirm(&name, &[], None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        let create = (confirm.job.steps.iter())
            .find(|s| s.argv.get(1).is_some_and(|a| a == "create"))
            .unwrap();
        assert!(create.argv.contains(&data), "{:?}", create.argv);
        assert!(
            !create.argv.iter().any(|a| a.starts_with(&gone)),
            "{:?}",
            create.argv
        );
        let body = confirm.body.join("\n");
        let note = format!("Not mounted: “{gone}”: the folder does not exist.");
        assert!(body.contains(&note), "{body}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_rebuild_repeats_the_recorded_limits_and_the_sbx_mount_mounts() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.cpus = Some(2);
        sb.memory = Some("4g".into());
        sb.memory_spec = Some("4096m".into());
        sb.runtime_mounts = vec![sbx::RuntimeMount {
            host_path: "/h/data".into(),
            container_target: "/workspace/data".into(),
            read_only: false,
        }];
        let name = sb.name.clone();
        let app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let confirm = app
            .recreate_confirm(&name, &[], None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        let argvs: Vec<String> = confirm.job.steps.iter().map(|s| s.argv.join(" ")).collect();
        let create = argvs.iter().find(|a| a.starts_with("sbx create")).unwrap();
        assert!(
            create.contains("--cpus 2") && create.contains("-m 4096m"),
            "{create}"
        );
        let at = argvs
            .iter()
            .position(|a| a.starts_with("sbx create"))
            .unwrap();
        assert_eq!(argvs[at + 1], format!("sbx exec {name} true"));
        assert_eq!(
            argvs[at + 2],
            format!("sbx mount {name} /h/data:/workspace/data")
        );
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("2 CPUs, 4g of memory")),
            "{:?}",
            confirm.body
        );
        // One CPU is one, not "1 CPUs" nor "1 CPU(s)".
        let mut one = app.sandboxes[0].clone();
        one.cpus = Some(1);
        one.memory = None;
        let app = App::for_test(Catalog::load().unwrap(), vec![one]);
        let confirm = app
            .recreate_confirm(&name, &[], None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("keeps its limit of 1 CPU, and")),
            "{:?}",
            confirm.body
        );
        assert!(
            confirm.body.iter().any(|l| l.contains("sbx mount")),
            "{:?}",
            confirm.body
        );
    }
    #[test]
    fn a_sandbox_with_ram_volumes_keeps_them_in_ram() {
        let mut sb = model::sample(
            "claude-anthropic-proj",
            Some("claude-anthropic"),
            &["host-npm", "rtk"],
        );
        sb.tmpfs_volumes = true;
        let name = sb.name.clone();
        let catalog = Catalog::load().unwrap();
        let here = catalog.kits_dir.clone();
        let mut app = App::for_test(catalog, vec![sb]);
        let confirm = app
            .recreate_confirm(
                &name,
                &["host-npm".into(), "rtk".into()],
                None,
                "Recreate",
                Vec::new(),
            )
            .expect("a managed sandbox");
        let create: Vec<String> = confirm
            .job
            .steps
            .iter()
            .find(|s| s.argv[..2] == ["sbx".to_string(), "create".to_string()])
            .unwrap()
            .argv
            .clone();
        let ram = crate::tmpfs::kits_dir();
        assert!(
            create.contains(&ram.join("claude-anthropic").display().to_string()),
            "{create:?}"
        );
        assert!(
            create.contains(&ram.join("host-npm").display().to_string()),
            "{create:?}"
        );
        assert!(
            create.contains(&here.join("rtk").display().to_string()),
            "{create:?}"
        );
        assert!(
            !create.contains(&here.join("claude-anthropic").display().to_string()),
            "{create:?}"
        );
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("Its volumes are in RAM again")),
            "{:?}",
            confirm.body
        );
        app.action_delete();
        let Some(Modal::Confirm(delete)) = &app.modal else {
            panic!("delete asks first")
        };
        assert!(
            delete.body.iter().any(|l| l == VOLUMES_IN_RAM),
            "{:?}",
            delete.body
        );
    }

    #[test]
    fn a_rebuild_without_recorded_limits_gets_the_default_and_says_so() {
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let name = sb.name.clone();
        let app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let confirm = app
            .recreate_confirm(&name, &[], None, "Recreate", Vec::new())
            .expect("a managed sandbox");
        let create = confirm
            .job
            .steps
            .iter()
            .map(|s| s.argv.join(" "))
            .find(|a| a.starts_with("sbx create"))
            .unwrap();
        assert!(create.contains("--cpus 4 -m 4g"), "{create}");
        assert!(
            confirm
                .body
                .iter()
                .any(|l| l.contains("gets 4 CPUs and 4g of memory, in place of")),
            "{:?}",
            confirm.body
        );
    }

    fn repo_view(id: &str, skills: &[&str]) -> repo_skills_ui::RepoView {
        repo_skills_ui::RepoView {
            id: id.into(),
            source: format!("o/{id}"),
            git_ref: None,
            commit: Some("0123456789abcdef".into()),
            problem: None,
            skills: skills.iter().map(|s| s.to_string()).collect(),
            skipped: Vec::new(),
        }
    }

    fn repo_form(app: &mut App) -> &mut RepoSkillsForm {
        match &mut app.screen {
            Screen::RepoSkills(form) => form,
            other => panic!("the repo skills screen is open, not {other:?}"),
        }
    }

    fn focus_repo_row(app: &mut App, row: &RepoSkillRow) {
        let form = repo_form(app);
        form.focus = form
            .rows()
            .iter()
            .position(|r| r == row)
            .unwrap_or_else(|| panic!("{row:?} is on the screen"));
    }

    #[test]
    fn repo_skills_open_in_both_scopes() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.on_key(KeyEvent::from(KeyCode::Char('i')));
        assert!(matches!(app.screen, Screen::List));
        assert!(app.status.as_ref().is_some_and(|(_, err, _)| *err));

        open_skills(&mut app);
        focus_skill(&mut app, &SkillRow::RepoSkills);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(repo_form(&mut app).scope, RepoScope::Global);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::Skills(_)));

        let mut sb = model::sample(SB, Some("claude-anthropic"), &[]);
        sb.workspace = PathBuf::from("/proj");
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.on_key(KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(repo_form(&mut app).scope, RepoScope::Sandbox(SB.into()));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::List));
    }

    #[test]
    fn the_per_sandbox_screen_saves_its_ticks_and_syncs() {
        use crate::skill_select::{Pick, Selection, Skills};
        let (r, s) = ("github-com-o-r-0123abcd", "github-com-o-s-89abcdef");
        let mut sb = model::sample(SB, Some("claude-anthropic"), &[]);
        sb.workspace = PathBuf::from("/proj");
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let mut sel = Selection {
            global: vec![Pick {
                repo: r.into(),
                skills: Skills::Named(vec!["alpha".into()]),
            }],
            ..Selection::default()
        };
        sel.sandboxes.insert(
            SB.into(),
            vec![Pick {
                repo: s.into(),
                skills: Skills::Named(vec!["gamma".into()]),
            }],
        );
        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            RepoScope::Sandbox(SB.into()),
            vec![repo_view(r, &["alpha", "beta"]), repo_view(s, &["gamma"])],
            Ok(sel),
        ));
        let alpha = RepoSkillRow::Skill {
            repo: r.into(),
            name: "alpha".into(),
            locked: true,
        };
        focus_repo_row(&mut app, &alpha);
        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("ticked for all sandboxes")));
        assert!(!repo_form(&mut app).dirty);

        // ↓ onto beta, tick it; then gamma off.
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        focus_repo_row(
            &mut app,
            &RepoSkillRow::Skill {
                repo: s.into(),
                name: "gamma".into(),
                locked: false,
            },
        );
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            repo_form(&mut app).picks(),
            [Pick {
                repo: r.into(),
                skills: Skills::Named(vec!["beta".into()]),
            }]
        );

        // Unsaved: Sync now asks for the save first.
        focus_repo_row(&mut app, &RepoSkillRow::SyncNow);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none());
        focus_repo_row(&mut app, &RepoSkillRow::Save);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(!repo_form(&mut app).dirty);
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, err, _)| !err && m.contains("next start")));
        focus_repo_row(&mut app, &RepoSkillRow::SyncNow);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Modal::Running(_, pending)) = &app.modal else {
            panic!("Sync now starts a job")
        };
        assert_eq!(&pending.steps[0].argv[1..], ["skill", "sync", SB]);
    }

    #[test]
    fn an_unreadable_selection_is_not_saved() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            RepoScope::Global,
            vec![repo_view("github-com-o-r-0123abcd", &["alpha"])],
            Err("skill-selection.json does not parse".into()),
        ));
        assert_eq!(repo_form(&mut app).rows(), [RepoSkillRow::Close]);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, err, _)| *err && m.starts_with("nothing saved")));
    }

    // --- LiteLLM connections ----------------------------------------------------------------

    #[test]
    fn a_test_app_starts_from_an_empty_registry() {
        let app = App::for_test(Catalog::load().unwrap(), Vec::new());
        assert_eq!(
            format!("{:?}", app.registry),
            format!("{:?}", Registry::empty())
        );
    }

    /// A test app on its own folder of connections, holding `conns`.
    fn app_on(conns: &[crate::litellm::Connection], sandboxes: Vec<Sandbox>) -> App {
        let mut app = App::for_test(Catalog::load().unwrap(), sandboxes);
        app.litellm_root = crate::testkit::litellm_root_with(conns);
        app
    }

    /// `beta`, on another gateway; a sandbox on it harvested models `acme` does not have
    /// ([`crate::testkit::harvested`]).
    fn beta() -> crate::litellm::Connection {
        crate::testkit::connection("beta", "https://gw.example.org")
    }

    /// Fill in *new connection…* on an open create form.
    fn type_new_connection(app: &mut App, name: &str, key: &str) {
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.connection.litellm = None;
        let typed = &mut form.connection.new_conn;
        typed.name = TextInput::new(name);
        typed.url = TextInput::new("https://gw.example.org");
        typed.key = TextInput::new(key);
    }

    fn create_form(app: &mut App) -> &mut CreateForm {
        match &mut app.screen {
            Screen::Create(form) => form,
            other => panic!("the create form is open, not {other:?}"),
        }
    }

    /// Put the create form on the backend of that kit.
    fn on_backend(app: &mut App, kit: &str) {
        let at = (app.catalog.backends.iter())
            .position(|b| b.kit.name == kit)
            .unwrap();
        create_form(app).harness.backend = at;
    }

    #[test]
    fn a_connection_asks_for_no_model() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        let catalog = Arc::clone(&app.catalog);
        for kit in ["claude-litellm", "opencode-litellm", "pi-litellm"] {
            on_backend(&mut app, kit);
            for pick in [Some("acme"), None] {
                create_form(&mut app).connection.litellm = pick.map(str::to_string);
                let rows = create_form(&mut app).rows(&catalog);
                let connection: Vec<&CreateRow> = rows
                    .iter()
                    .filter(|r| r.step() == crate::create::CreateStep::Connection)
                    .collect();
                let want: &[CreateRow] = match pick {
                    Some(_) => &[CreateRow::LitellmConn],
                    None => &[
                        CreateRow::LitellmConn,
                        CreateRow::LitellmName,
                        CreateRow::LitellmUrl,
                        CreateRow::LitellmKey,
                    ],
                };
                assert_eq!(
                    connection.into_iter().copied().collect::<Vec<_>>(),
                    want,
                    "{kit} on {pick:?}"
                );
            }
        }
        // The pins: the harvested list on `acme`, nothing on a connection no sandbox has read.
        app.load_models_for(Some("acme"));
        let ids: Vec<&str> = app.catalog.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(matches!(
            app.catalog.models_source,
            crate::models::Source::Gateway { .. }
        ));
        app.load_models_for(None);
        assert!(app.catalog.models.is_empty());
        let fresh = crate::testkit::litellm_root_with(&[]);
        crate::litellm::save_in(&fresh, &beta()).unwrap();
        app.litellm_root = fresh;
        app.load_models_for(Some("beta"));
        assert!(
            app.catalog.models.is_empty(),
            "no sandbox on beta has read its list"
        );
        assert_eq!(app.catalog.models_source, crate::models::Source::NotYet);
        // `S` counts the harvested list, and leaves the count out before there is one.
        let note =
            |app: &App, name: &str| app.secret_view(&SecretRow::Connection(name.into())).note;
        assert_eq!(note(&app, "beta"), "gw.example.org");
        crate::testkit::harvest_in(
            &app.litellm_root,
            "beta",
            &crate::testkit::harvested("beta"),
        );
        assert_eq!(note(&app, "beta"), "gw.example.org · 2 models");
    }

    #[test]
    fn the_create_form_opens_on_the_last_used_connection() {
        let opened = |app: &mut App| -> (Option<String>, Option<String>) {
            app.open_create();
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            (
                form.connection.litellm.clone(),
                form.connection.last_litellm.clone(),
            )
        };
        let mut app = app_on(&[crate::testkit::acme(), beta()], Vec::new());
        assert_eq!(opened(&mut app).0.as_deref(), Some("acme"));
        app.registry.set_last_litellm("beta");
        assert_eq!(opened(&mut app), (Some("beta".into()), Some("beta".into())));
        // The pins offer the connection's own models.
        let mut ids: Vec<&str> = app.catalog.models.iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["beta-large", "beta-small"]);
        app.registry.set_last_litellm("gone");
        assert_eq!(opened(&mut app).0.as_deref(), Some("acme"));
        let mut empty = app_on(&[], Vec::new());
        assert_eq!(opened(&mut empty).0, None, "new connection…");
        // ←→ walk every connection, then *new connection…*.
        let Screen::Create(form) = &mut app.screen else {
            unreachable!()
        };
        form.cycle_litellm(true);
        assert_eq!(form.connection.litellm.as_deref(), Some("beta"));
        form.cycle_litellm(true);
        assert_eq!(form.connection.litellm, None);
        form.cycle_litellm(true);
        assert_eq!(form.connection.litellm.as_deref(), Some("acme"));
    }

    /// The create form on `step`, on the backend `kit`.
    fn wizard_for(app: &mut App, kit: &str, step: CreateStep) {
        let catalog = Arc::clone(&app.catalog);
        let backend = catalog
            .backends
            .iter()
            .position(|b| b.kit.name == kit)
            .unwrap();
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.harness.backend = backend;
        form.wizard_set_step(&catalog, step);
        app.after_wizard_input();
    }

    #[test]
    fn a_config_file_row_opens_the_file_in_an_editor_and_delete_resets_it() {
        use crate::litellm::{Agent, FileState};
        let file_help = |app: &App| {
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            form.help(app, WizardRow::Field(CreateRow::ConfigFile(0)))
        };
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        wizard_for(&mut app, "opencode-litellm", CreateStep::Models);
        let file = "opencode-litellm-acme.json";
        assert_eq!(
            app.config_file_row(0),
            Some((file.to_string(), FileState::Generated))
        );
        let (_, _, files) = app.wizard_config_files().expect("the form's files");
        let catalog = Arc::clone(&app.catalog);
        let form = create_form(&mut app);
        let picked = form.connection.models.get(Slot::Main).map(str::to_string);
        form.focus_field(&catalog, CreateRow::ConfigFile(0));
        assert_eq!(
            files[0].value.get("model").and_then(|v| v.as_str()),
            picked.map(|id| format!("litellm-acme/{id}")).as_deref(),
            "the file follows the form's picks"
        );
        let help = file_help(&app);
        assert_eq!(help.title, file);
        assert!(
            help.text.contains("every OpenCode sandbox"),
            "{}",
            help.text
        );

        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Handoff::Edit { file: named, path }) = app.handoff.take() else {
            panic!("↵ hands the file to an editor")
        };
        assert_eq!(named, file);
        assert!(path.starts_with(crate::litellm::edits_dir(
            &app.litellm_root,
            "acme",
            Agent::Opencode
        )));
        assert_eq!(
            app.edit_ended(file).0,
            format!("{file}: unchanged, the generated file is used")
        );

        std::fs::write(&path, r#"{"model": "litellm-acme/mine"}"#).unwrap();
        assert_eq!(app.config_file_row(0).unwrap().1, FileState::Edited);
        assert!(!app.edit_ended(file).1);
        let help = file_help(&app);
        assert!(help.text.contains("Your copy"), "{}", help.text);

        std::fs::write(&path, "{").unwrap();
        assert!(
            app.edit_ended(file).1,
            "a copy that does not parse is an error"
        );
        let help = file_help(&app);
        assert!(help.warn.is_some_and(|w| w.contains("not valid JSON")));

        app.on_key(KeyEvent::from(KeyCode::Delete));
        assert_eq!(app.config_file_row(0).unwrap().1, FileState::Generated);
        assert!(!path.exists());
    }

    #[test]
    fn the_models_step_offers_the_agents_own_slots_and_every_model() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        wizard_for(&mut app, "opencode-litellm", CreateStep::Models);
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        let rows: Vec<CreateRow> = form.rows_of(CreateStep::Models, &app.catalog);
        assert_eq!(
            rows,
            [
                CreateRow::ModelsRefresh,
                CreateRow::ModelsFold,
                CreateRow::Model(Slot::Main),
                CreateRow::Model(Slot::Small),
                CreateRow::ModelOn(0),
                CreateRow::ModelOn(1),
                CreateRow::FilesFold,
                CreateRow::ConfigFile(0),
            ]
        );
        // No Models step for a harness that picks its own models: Next passes it by.
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        wizard_for(&mut app, "claude-anthropic", CreateStep::Harness);
        let catalog = Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        assert!(form.rows_of(CreateStep::Models, &catalog).is_empty());
        assert_eq!(
            form.next_step(CreateStep::Harness, &catalog),
            Some(CreateStep::Kits)
        );
        assert_eq!(
            form.prev_step(CreateStep::Kits, &catalog),
            Some(CreateStep::Harness)
        );
        form.wizard_goto(&catalog, CreateStep::Models).unwrap();
        assert_eq!(
            form.wizard_step(),
            CreateStep::Kits,
            "its number goes to the next step"
        );
    }

    #[test]
    fn a_model_switched_off_is_offered_for_no_slot() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        wizard_for(&mut app, "claude-litellm", CreateStep::Models);
        let catalog = Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        let first = catalog.models[0].id.clone();
        form.connection.models.set(Slot::Opus, Some(first.clone()));
        let space = KeyEvent::from(KeyCode::Char(' '));
        form.field_key(&catalog, CreateRow::ModelOn(0), space);
        assert!(form.connection.disabled.contains(&first));
        assert_eq!(
            form.connection.models.get(Slot::Opus),
            None,
            "its pick goes too"
        );
        form.connection.model_pick = Some(Slot::Opus);
        let rows = form.rows_of(CreateStep::Models, &catalog);
        assert!(
            !rows.contains(&CreateRow::ModelPick(Slot::Opus, 0)),
            "{rows:?}"
        );
        assert!(
            rows.contains(&CreateRow::ModelPick(Slot::Opus, 1)),
            "{rows:?}"
        );
        assert!(
            rows.contains(&CreateRow::ModelOn(0)),
            "it can be switched on again"
        );
        let picks = form.picks(&catalog);
        assert_eq!(picks.disabled.iter().collect::<Vec<_>>(), [&first]);
        form.field_key(&catalog, CreateRow::ModelOn(0), space);
        assert!(form.connection.disabled.is_empty());
    }

    /// Next from the Connection step fetches the list of a connection that has none, and comes
    /// back on the Harness step; once per form, so a failed fetch is not started at every Next.
    /// A list younger than a day is reused.
    #[test]
    fn next_past_the_connection_fetches_a_stale_list_once() {
        let mut app = app_on(&[crate::testkit::acme(), beta()], Vec::new());
        // beta has a key but no list yet.
        let _ = std::fs::remove_file(app.litellm_root.join("beta/models.json"));
        if let Some(secrets) = &mut app.secrets {
            secrets
                .custom
                .push(crate::testkit::litellm_key("beta", "gw.example.org"));
        }
        app.open_create();
        create_form(&mut app).connection.litellm = Some("beta".into());
        app.load_models_for(Some("beta"));
        wizard_on(&mut app, CreateStep::Connection, WizardRow::Next);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Some(Modal::Running(_, job)) = app.modal.take() else {
            panic!("the fetch started: {:?}", app.status)
        };
        assert_eq!(job.title, "Fetching the models of beta");
        let Some(Restore::Create(back)) = &job.post.resume else {
            panic!("the form comes back")
        };
        assert_eq!(back.wizard_step(), CreateStep::Harness);
        assert_eq!(back.wizard.auto_fetched.as_deref(), Some("beta"));
        // Back with still no list: Back and Next again start no second fetch.
        app.restore_screen(Restore::Create(back.clone()));
        wizard_on(&mut app, CreateStep::Connection, WizardRow::Next);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none(), "a failed fetch is not started again");
        assert_eq!(create_form(&mut app).wizard_step(), CreateStep::Harness);

        // acme's list was fetched just now: Next goes on with no fetch.
        app.open_create();
        create_form(&mut app).connection.litellm = Some("acme".into());
        app.load_models_for(Some("acme"));
        wizard_on(&mut app, CreateStep::Connection, WizardRow::Next);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none(), "a list younger than a day is reused");
        assert_eq!(create_form(&mut app).wizard_step(), CreateStep::Harness);
    }

    #[test]
    fn a_create_keeps_its_answers_and_the_next_form_opens_on_them() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        wizard_for(&mut app, "opencode-litellm", CreateStep::Workspace);
        let rtk = app
            .catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "rtk")
            .unwrap();
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.kits.addons[rtk] = true;
        form.workspace.clone = true;
        form.workspace.ports = TextInput::new("8080");
        press_create_only(&mut app);
        let Some(Modal::Running(_, job)) = app.modal.take() else {
            panic!("a create was started: {:?}", app.status)
        };
        let defaults = job.post.defaults.clone().expect("the answers are kept");
        assert_eq!(defaults.backend, "opencode-litellm");
        assert_eq!(defaults.addons["opencode-litellm"], ["rtk"]);
        assert!(defaults.clone);
        assert_eq!(defaults.ports, "8080");

        app.apply_post(&job.post);
        app.screen = Screen::List;
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(
            form.wizard_step(),
            CreateStep::Review,
            "a create is one key away"
        );
        let rows = form.wizard_rows(&app.catalog);
        assert_eq!(rows[form.wizard_focus()], WizardRow::CreateStart);
        assert!(form.wizard.prefilled);
        let (heading, _) = form.step_heading(CreateStep::Review, &app.catalog);
        assert!(heading.contains("last sandbox"), "{heading}");
        let status = app.status.as_ref().map(|(text, _, _)| text.as_str());
        assert_eq!(status, Some(crate::create::PREFILLED));
        let backend = &app.catalog.backends[form.harness.backend];
        assert_eq!(backend.kit.name, "opencode-litellm");
        assert!(form.kits.addons[rtk]);
        assert!(form.workspace.clone);
        assert_eq!(form.workspace.ports.value, "8080");
        assert_eq!(
            form.workspace.name.value, "OpenCode · LiteLLM acme (RTK)",
            "the label is this folder's own"
        );
    }

    #[test]
    fn defaults_that_no_longer_pass_open_the_step_they_fail_on() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.registry
            .set_create_defaults(crate::state::CreateDefaults {
                backend: "claude-litellm".into(),
                ports: "not a port".into(),
                ..Default::default()
            });
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Workspace);
        // A backend this checkout no longer ships leaves the form as it opens.
        app.registry
            .set_create_defaults(crate::state::CreateDefaults {
                backend: "gone-kit".into(),
                ..Default::default()
            });
        app.screen = Screen::List;
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Provider);
    }

    #[test]
    fn no_mcp_comes_back_with_the_last_creates_answers() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.mcp = vec![sbx::McpServer {
            name: "notion".into(),
            status: "ready".into(),
            ..sbx::McpServer::default()
        }];
        app.open_create();
        let catalog = Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.toggle_no_mcp();
        let defaults = form.defaults(&catalog);
        assert_eq!(defaults.mcp, ["none"]);

        app.registry.set_create_defaults(defaults);
        app.screen = Screen::List;
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert!(form.workspace.no_mcp);
        assert_eq!(form.workspace.static_mcp, [false]);
        assert_eq!(form.selected_mcp(), ["none"]);
    }

    #[test]
    fn a_rebuild_repeats_no_mcp_and_says_so() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.registry.set_static_mcp("box", vec!["none".into()]);
        app.cursor = 0;
        app.action_recreate();
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("a rebuild asks first: {:?}", app.status)
        };
        let create = (confirm.job.steps.iter())
            .find(|s| s.argv.get(1).map(String::as_str) == Some("create"))
            .expect("a create step");
        let at = (create.argv.iter())
            .position(|a| a == "--static-mcp")
            .expect("the fixed set");
        assert_eq!(create.argv[at + 1], "none");
        let body = confirm.body.join(" ");
        assert!(body.contains(crate::create::NO_MCP), "{body}");
        assert!(!body.contains("fixed set it was created with"), "{body}");
    }

    #[test]
    fn the_start_fetch_runs_once_for_each_connection_with_a_key() {
        let mut app = app_on(&[crate::testkit::acme(), beta()], Vec::new());
        app.start_due_fetches();
        assert_eq!(app.background_fetches, ["acme"], "beta has no key");
        app.start_due_fetches();
        assert_eq!(app.background_fetches, ["acme"], "once per start");
        // A list asked for today is not due.
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        crate::model_fetch::mark_in(&app.litellm_root, "acme", crate::state::now_epoch()).unwrap();
        app.start_due_fetches();
        assert!(app.background_fetches.is_empty());
        // No answer from `sbx secret ls` yet: wait for it.
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.secrets = None;
        app.start_due_fetches();
        assert!(app.background_fetches.is_empty() && !app.due_fetches_done);
    }

    #[test]
    fn a_background_fetch_is_kept_under_its_connection() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        // A second newer than the fixture's list.
        let at = crate::state::now_epoch() as i64 + 1;
        let cache = format!(
            r#"{{"version":1,"fetched_at":{at},"models":[{{"id":"fresh-1","context":64000}}]}}"#
        );
        let fetcher = crate::model_fetch::sandbox_name("acme");
        app.fetch_tx
            .send(("acme".into(), fetcher, Some(cache)))
            .unwrap();
        app.tick_fetches();
        let file = app.litellm_root.join("acme/models.json");
        let (models, _) = crate::models::catalog_for(Some(&file));
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["fresh-1"]
        );
        assert_eq!(models[0].limits.context, Some(64000), "the limits are kept");
        let fetcher = crate::model_fetch::sandbox_name("acme");
        app.fetch_tx.send(("acme".into(), fetcher, None)).unwrap();
        app.tick_fetches();
        assert!(
            matches!(&app.status, Some((text, true, _)) if text.contains("could not be fetched"))
        );
    }

    #[test]
    fn the_picks_follow_the_connection() {
        let pinned = || {
            let mut models = Overrides::default();
            models.set(Slot::Opus, Some("claude-sonnet-5".into()));
            models
        };
        let press_on_row = |app: &mut App, code: KeyCode| {
            let catalog = Arc::clone(&app.catalog);
            let Screen::Create(form) = &mut app.screen else {
                panic!("the create form is open")
            };
            form.wizard_set_step(&catalog, CreateStep::Connection);
            assert!(form.focus_wizard_row(&catalog, WizardRow::Field(CreateRow::LitellmConn)));
            app.on_key(KeyEvent::from(code));
        };
        let form = |app: &App| match &app.screen {
            Screen::Create(form) => form.clone(),
            _ => panic!("the create form is open"),
        };
        let mut app = app_on(&[crate::testkit::acme(), beta()], Vec::new());
        app.open_create();
        assert_eq!(
            form(&app).connection.picks_for,
            Some(("acme".into(), crate::litellm::Agent::Claude)),
            "the form opens on the saved picks of its connection"
        );
        if let Screen::Create(form) = &mut app.screen {
            form.connection.models = pinned();
        }
        // acme → beta: beta's own picks, none saved.
        press_on_row(&mut app, KeyCode::Right);
        assert_eq!(form(&app).connection.litellm.as_deref(), Some("beta"));
        assert!(
            form(&app).connection.models.is_empty(),
            "another connection's ids mean nothing"
        );
        // beta → new connection…: no models to pick, so nothing is picked.
        press_on_row(&mut app, KeyCode::Right);
        assert_eq!(form(&app).connection.litellm, None);
        assert!(form(&app).picks(&app.catalog).slots.is_empty());
    }

    #[test]
    fn a_new_connection_is_written_only_after_its_key_is_stored() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        type_new_connection(&mut app, "beta", "not-a-real-key");
        press_create_only(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started: {:?}", app.status)
        };
        assert!(pending.steps[0].argv.contains(&"set-custom".to_string()));
        assert!(!crate::litellm::exists_in(&app.litellm_root, "beta"));
        assert_eq!(app.registry.last_litellm(), None);

        app.apply_post(&pending.post);
        app.start_next(&pending.post);
        let saved = crate::litellm::load_in(&app.litellm_root, "beta").expect("saved");
        assert_eq!(saved.url, "https://gw.example.org");
        assert!(crate::kits::descriptor_path(&crate::litellm::kit_dir_in(
            &app.litellm_root,
            "beta",
            crate::litellm::Agent::Claude
        ))
        .is_file());
        let text = std::fs::read_to_string(app.litellm_root.join("beta/connection.json")).unwrap();
        assert!(!text.contains("not-a-real-key"), "{text}");
        assert_eq!(app.registry.last_litellm(), Some("beta"));
        let Some(Modal::Running(_, create)) = &app.modal else {
            panic!("the create follows: {:?}", app.modal)
        };
        assert!(create.title.starts_with("Creating "), "{}", create.title);

        // A name `aisbx litellm add` took meanwhile is refused at Create, and nothing runs.
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        crate::litellm::save_in(&app.litellm_root, &beta()).unwrap();
        type_new_connection(&mut app, "beta", "not-a-real-key");
        press_create_only(&mut app);
        assert!(app.modal.is_none());
        assert_eq!(
            app.status.as_ref().map(|(m, _, _)| m.as_str()),
            Some("connection beta exists")
        );
    }

    #[test]
    fn a_failed_key_brings_the_form_back_without_the_key() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        type_new_connection(&mut app, "beta", "not-a-real-key");
        press_create_only(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started")
        };
        assert!(matches!(app.screen, Screen::List));
        // Stands in for `sbx secret set-custom` failing, e.g. with the daemon stopped.
        let job = Job::spawn("fails", vec![Step::new("fail", vec!["false".into()])]);
        for _ in 0..500 {
            if job.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(job.status(), Status::Failed(_)));
        app.modal = Some(Modal::Running(job, pending));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        let Screen::Create(form) = &app.screen else {
            panic!("the form is back")
        };
        assert_eq!(form.connection.litellm, None);
        assert_eq!(form.connection.new_conn.name.value, "beta");
        assert_eq!(form.connection.new_conn.url.value, "https://gw.example.org");
        assert!(form.connection.new_conn.key.value.is_empty());
        assert!(!crate::litellm::exists_in(&app.litellm_root, "beta"));
        assert_eq!(app.registry.last_litellm(), None);
    }

    #[test]
    fn closing_a_finished_key_job_still_saves_the_connection() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_create();
        type_new_connection(&mut app, "beta", "not-a-real-key");
        press_create_only(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started")
        };
        // Stands in for `sbx secret set-custom` succeeding.
        let job = Job::spawn("works", vec![Step::new("ok", vec!["true".into()])]);
        for _ in 0..500 {
            if job.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(job.status(), Status::Ok));
        app.modal = Some(Modal::Running(job, pending));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(app.modal, Some(Modal::Running(..))),
            "the finished job waits for tick"
        );
        app.tick();
        assert!(crate::litellm::exists_in(&app.litellm_root, "beta"));
        let Some(Modal::Running(_, create)) = &app.modal else {
            panic!("the create started")
        };
        assert!(
            create
                .steps
                .iter()
                .any(|s| s.argv.iter().any(|a| a == "create")),
            "{:?}",
            create.steps.iter().map(|s| &s.argv).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&app.litellm_root);
    }

    #[test]
    fn the_kits_editor_lists_the_host_folder_kits_first() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.open_kits("box");
        let Screen::Kits(form) = &app.screen else {
            panic!("the editor is open");
        };
        let names: Vec<&str> = form
            .rows(&app.catalog)
            .into_iter()
            .filter_map(|row| match row {
                KitsRow::Addon(i) => Some(app.catalog.addons[i].kit.name.as_str()),
                _ => None,
            })
            .collect();
        let hosts = names
            .iter()
            .take_while(|n| model::is_host_folder_kit(n))
            .count();
        for kit in [
            "host-gradle",
            "host-maven",
            "host-npm",
            "host-pip",
            "host-skills",
        ] {
            assert!(
                names[..hosts].contains(&kit),
                "{kit} is a host cache: {names:?}"
            );
        }
        assert!(
            names[hosts..].iter().all(|n| !model::is_host_folder_kit(n)),
            "one block of host caches, then the rest: {names:?}"
        );
        let catalog_order = |part: &[&str]| {
            let at = |n: &&str| app.catalog.addons.iter().position(|a| a.kit.name == *n);
            part.windows(2).all(|w| at(&w[0]) < at(&w[1]))
        };
        assert!(catalog_order(&names[..hosts]) && catalog_order(&names[hosts..]));
        // The same rule as the wizard's: every kit the create form detects host folders for.
        let mut create = CreateForm::new(
            &app.catalog,
            Some(&crate::testkit::host_home()),
            Vec::new(),
            None,
            Vec::new(),
            sbx::SkillsMode::default(),
        );
        create.kits.host = (app.catalog.addons.iter())
            .map(|a| model::host_cache_detection_in(&a.kit.name, None))
            .collect();
        let wizard: Vec<&str> = (create.addon_order(&app.catalog).into_iter())
            .map(|i| app.catalog.addons[i].kit.name.as_str())
            .collect();
        assert_eq!(names, wizard);
        // The cursor starts on the first row the editor draws.
        assert!(matches!(
            form.rows(&app.catalog)[form.focus],
            KitsRow::Addon(i) if app.catalog.addons[i].kit.name == names[0]
        ));
    }

    #[test]
    fn a_kit_for_another_harness_is_not_offered_nor_built() {
        let catalog = &Catalog::load().unwrap();
        let at = |name: &str| catalog.backends.iter().position(|b| b.kit.name == name);
        let mut form = CreateForm::new(
            catalog,
            Some(&crate::testkit::host_home()),
            Vec::new(),
            None,
            Vec::new(),
            sbx::SkillsMode::default(),
        );
        let statusline = (catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "statusline"))
        .unwrap();
        form.kits.addons[statusline] = true;
        let offered = |form: &CreateForm| form.addon_order(catalog).contains(&statusline);
        form.harness.backend = at("claude-anthropic").unwrap();
        assert!(offered(&form));
        assert_eq!(form.selected_addons(catalog), vec!["statusline"]);
        form.harness.backend = at("codex-openai").unwrap();
        assert!(!offered(&form));
        assert!(form.selected_addons(catalog).is_empty());
        assert!(!form.rows(catalog).contains(&CreateRow::Addon(statusline)));
        // The kits editor leaves it out too, unless the sandbox already has it.
        let mut kits = KitsForm {
            sandbox: "codex-openai-proj".into(),
            addons: vec![false; catalog.addons.len()],
            original: vec![false; catalog.addons.len()],
            models_offered: false,
            models: Default::default(),
            original_models: Default::default(),
            model_pick: None,
            focus: 0,
            ram_volumes: false,
            history_in_volumes: false,
            harness: Some("codex".into()),
        };
        assert!(!kits.addon_order(catalog).contains(&statusline));
        kits.original[statusline] = true;
        assert!(kits.addon_order(catalog).contains(&statusline));
        let jdk = (catalog.addons.iter().position(|a| a.kit.name == "jdk25")).unwrap();
        assert!(
            kits.addon_order(catalog).contains(&jdk),
            "no list: every harness"
        );
    }

    #[test]
    fn the_kits_editor_offers_the_sandboxs_connection_models() {
        let mut on_beta = model::sample("claude-litellm-beta", Some("claude-litellm"), &[]);
        on_beta.litellm = Some("beta".into());
        let on_acme = model::sample("claude-litellm-acme", Some("claude-litellm"), &[]);
        let mut app = app_on(&[crate::testkit::acme(), beta()], vec![on_acme, on_beta]);
        let ids = |app: &App| -> Vec<String> {
            let mut ids: Vec<String> = app.catalog.models.iter().map(|m| m.id.clone()).collect();
            ids.sort_unstable();
            ids
        };
        app.open_kits("claude-litellm-beta");
        assert!(matches!(&app.screen, Screen::Kits(f) if f.models_offered));
        assert_eq!(ids(&app), ["beta-large", "beta-small"]);
        app.screen = Screen::List;
        app.open_create();
        assert_eq!(
            ids(&app),
            ["claude-sonnet-5", "gpt-6"],
            "the form is on acme"
        );
        app.screen = Screen::List;
        app.open_kits("claude-litellm-beta");
        assert_eq!(ids(&app), ["beta-large", "beta-small"]);
    }

    /// Does a prelude write the agent's config ([`sbx::write_files_argv`])?
    fn pushes(handoff: &Handoff) -> bool {
        (handoff.prelude().iter()).any(|step| step.iter().any(|a| a.contains(".aisbx-tmp")))
    }

    #[test]
    fn a_pi_sandbox_gets_its_list_through_its_connection_only() {
        let mut pi = model::sample("pi-litellm-proj", Some("pi-litellm"), &[]);
        pi.agent = "pi-litellm".into();
        let mut bare = pi.clone();
        bare.name = "pi-litellm-bare".into();
        bare.label = bare.name.clone();
        bare.litellm = None;
        let mut app = app_on(&[crate::testkit::acme()], vec![pi, bare]);
        let acme = crate::litellm::load_in(&app.litellm_root, "acme").unwrap();

        app.cursor = 0;
        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert_eq!(
            handoff.prelude()[1],
            crate::litellm::push_argv_in(
                &app.litellm_root,
                "pi-litellm-proj",
                &acme,
                crate::litellm::Agent::Pi
            )
            .unwrap()
        );
        assert_eq!(
            handoff.prelude().last().unwrap(),
            &sbx::exec_argv(
                "pi-litellm-proj",
                crate::kits::agent_update("pi-litellm").unwrap()
            )
        );

        app.cursor = 1;
        app.action_start();
        let handoff = app.handoff.take().expect("a session was handed over");
        assert!(!pushes(&handoff), "{:?}", handoff.prelude());
        app.on_key(KeyEvent::from(KeyCode::Char('L')));
        assert!(app.modal.is_none(), "L fetches nothing for it");
    }

    #[test]
    fn r_needs_the_connection_the_sandbox_is_on() {
        // On a connection that does not exist: refused with the reason.
        let mut mapped = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        mapped.litellm = Some("gone".into());
        let mut app = App::for_test(Catalog::load().unwrap(), vec![mapped.clone()]);
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('R')));
        assert!(app.modal.is_none());
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(m, _, _)| m.starts_with("LiteLLM connection gone no longer exists")),
            "{:?}",
            app.status
        );
        // On one that does: rebuilt onto the backend plus that connection's kit.
        mapped.litellm = Some("acme".into());
        let app = App::for_test(Catalog::load().unwrap(), vec![mapped]);
        let job = app
            .recreate_job("claude-litellm-proj", &[], None)
            .expect("a plan");
        let create = job.steps.last().unwrap().argv.join(" ");
        assert!(
            create.ends_with("kits/claude-litellm /proj")
                && create.contains("--kit ")
                && create.contains("acme/litellm-acme-claude"),
            "{create}"
        );
    }

    // --- connection management: S -> n and Enter, A ----------------------------------------

    /// A job that has finished with `status`, standing in for `sbx secret set-custom`.
    fn finished_job(ok: bool) -> Job {
        let argv = vec![if ok { "true" } else { "false" }.to_string()];
        let job = Job::spawn("stand-in", vec![Step::new("stand-in", argv)]);
        for _ in 0..500 {
            if job.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        job
    }

    fn conn_form(app: &mut App) -> &mut ConnForm {
        match &mut app.screen {
            Screen::Connection(form) => form,
            other => panic!("the connection dialog is open, not {other:?}"),
        }
    }

    fn type_conn(app: &mut App, name: &str, url: &str, key: &str) {
        let form = conn_form(app);
        form.fields.name = TextInput::new(name);
        form.fields.url = TextInput::new(url);
        form.fields.key = TextInput::new(key);
    }

    fn press_save(app: &mut App) {
        let form = conn_form(app);
        form.focus = form
            .rows()
            .iter()
            .position(|r| *r == ConnRow::Save)
            .unwrap();
        app.on_key(KeyEvent::from(KeyCode::Enter));
    }

    fn status_text(app: &App) -> String {
        app.status
            .as_ref()
            .map(|(m, _, _)| m.clone())
            .unwrap_or_default()
    }

    #[test]
    fn n_on_secrets_stores_the_key_first_and_then_writes_the_connection() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(conn_form(&mut app).mode, ConnMode::New);
        assert_eq!(conn_form(&mut app).rows()[0], ConnRow::Name);
        type_conn(&mut app, "beta", "https://gw.example.org", "not-a-real-key");
        press_save(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started: {:?}", app.status)
        };
        assert!(matches!(app.screen, Screen::Secrets(_)), "back on S");
        assert_eq!(pending.steps.len(), 1);
        assert_eq!(
            pending.steps[0].argv,
            sbx::secret_set_litellm_key_argv("gw.example.org", "sbx-cs-aisbx-llm-beta")
        );
        assert!(!pending.steps[0]
            .argv
            .iter()
            .any(|a| a.contains("not-a-real")));
        assert_eq!(pending.steps[0].stdin.as_deref(), Some("not-a-real-key"));
        let fetch = pending
            .post
            .then
            .as_deref()
            .expect("the model list follows the key");
        assert_eq!(fetch.title, "Fetching the models of beta");
        assert!(fetch.post.resume.is_none(), "back on S, as after any job");
        let (conn, agent) = pending
            .post
            .save_connection
            .clone()
            .expect("saved afterwards");
        assert_eq!((conn.name.as_str(), agent), ("beta", None));
        assert!(!crate::litellm::exists_in(&app.litellm_root, "beta"));

        app.apply_post(&pending.post);
        app.start_next(&pending.post);
        assert!(crate::litellm::exists_in(&app.litellm_root, "beta"));
        assert_eq!(app.registry.last_litellm(), Some("beta"));
        assert!(!crate::litellm::kit_dir_in(
            &app.litellm_root,
            "beta",
            crate::litellm::Agent::Claude
        )
        .exists());
        let text = std::fs::read_to_string(app.litellm_root.join("beta/connection.json")).unwrap();
        assert!(!text.contains("not-a-real-key"), "{text}");
        assert!(
            crate::model_fetch::kit_dir_in(&app.litellm_root, "beta").exists(),
            "the fetcher's kit, written with the connection"
        );
        assert!(
            matches!(&app.modal, Some(Modal::Running(_, next)) if next.title == "Fetching the models of beta"),
            "the model list is fetched next"
        );
    }

    #[test]
    fn a_failed_key_brings_the_connection_dialog_back_without_the_key() {
        let mut app = app_on(&[], Vec::new());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        type_conn(&mut app, "beta", "https://gw.example.org", "not-a-real-key");
        press_save(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a job was started")
        };
        let job = finished_job(false);
        assert!(matches!(job.status(), Status::Failed(_)));
        app.modal = Some(Modal::Running(job, pending));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        let form = conn_form(&mut app);
        assert_eq!(form.mode, ConnMode::New);
        assert_eq!(form.fields.name.value, "beta");
        assert_eq!(form.fields.url.value, "https://gw.example.org");
        assert!(form.fields.key.value.is_empty());
        assert!(!crate::litellm::exists_in(&app.litellm_root, "beta"));
        assert_eq!(app.registry.last_litellm(), None);
    }

    #[test]
    fn n_refuses_an_existing_name_when_opened_and_when_saved() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_connection(ConnMode::New);
        type_conn(&mut app, "acme", "https://gw.example.org", "not-a-real-key");
        press_save(&mut app);
        assert!(app.modal.is_none());
        assert!(status_text(&app).starts_with("connection acme exists"));
        assert!(
            matches!(app.screen, Screen::Connection(_)),
            "the dialog stays"
        );

        app.open_connection(ConnMode::New);
        crate::litellm::save_in(&app.litellm_root, &beta()).unwrap();
        type_conn(&mut app, "beta", "https://gw.example.org", "not-a-real-key");
        press_save(&mut app);
        assert!(app.modal.is_none());
        assert_eq!(status_text(&app), "connection beta exists");
        // ...and a refused URL, the same rule as the create form.
        type_conn(&mut app, "gamma", "http://gw.example.org", "not-a-real-key");
        press_save(&mut app);
        assert!(app.modal.is_none());
        assert_eq!(status_text(&app), "the URL must start with https://");
    }

    #[test]
    fn enter_on_a_connection_row_opens_it_for_editing() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.secrets = Some(sbx::Secrets {
            custom: vec![crate::testkit::litellm_key("gone", "gw.example.org")],
            ..Default::default()
        });
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        let rows = app.secret_rows();
        let at = |row: SecretRow| rows.iter().position(|r| *r == row).unwrap();
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = at(SecretRow::Connection("acme".into()));
        }
        assert!(app.on_connection_row());
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let form = conn_form(&mut app);
        assert_eq!(form.mode, ConnMode::Edit("acme".into()));
        assert!(
            !form.rows().contains(&ConnRow::Name),
            "the name never changes"
        );
        assert_eq!(form.fields.url.value, crate::testkit::ACME_URL);
        assert!(form.fields.key.value.is_empty());
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.on_connection_row(), "back on its row");

        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = at(SecretRow::LitellmOrphan("gone".into()));
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::Secrets(_)));
        assert!(status_text(&app).contains("d removes it"));
    }

    #[test]
    fn the_edit_plan_costs_what_changed() {
        let users = vec![model::sample(
            "claude-litellm-proj",
            Some("claude-litellm"),
            &[],
        )];
        let plan_for = |users: Vec<Sandbox>, url: &str, key: &str| {
            let mut app = app_on(&[crate::testkit::acme()], users);
            app.open_connection(ConnMode::Edit("acme".into()));
            let form = conn_form(&mut app);
            form.fields.url = TextInput::new(url);
            form.fields.key = TextInput::new(key);
            let form = form.clone();
            app.conn_plan(&form)
        };
        let url = crate::testkit::ACME_URL;

        assert!(matches!(
            plan_for(Vec::new(), url, ""),
            ConnPlan::Nothing(m) if m == "no connection changes"
        ));
        // The path only: the file, no job.
        let path = format!("{url}/v2");
        assert!(matches!(
            plan_for(Vec::new(), &path, ""),
            ConnPlan::Write(c) if c.url == path
        ));
        // A key alone: one set-custom on the same host, no file.
        let ConnPlan::Apply(job) = plan_for(Vec::new(), url, "not-a-real-key") else {
            panic!("a key job")
        };
        assert_eq!(
            job.steps[0].argv,
            sbx::secret_set_litellm_key_argv("litellm.example.com", "sbx-cs-aisbx-llm-acme")
        );
        assert_eq!(job.steps[0].stdin.as_deref(), Some("not-a-real-key"));
        assert!(job.post.rewrite_connection.is_none() && job.post.save_connection.is_none());
        assert!(
            matches!(job.post.restore, Some(Restore::Connection(ref f)) if f.fields.key.value.is_empty())
        );
        // A new host needs the key again…
        assert!(matches!(
            plan_for(Vec::new(), "https://gw.example.org", ""),
            ConnPlan::Refuse(m) if m.contains("paste the key again")
        ));
        // …and with it, the set-custom targets the new host, then the file.
        let ConnPlan::Apply(job) = plan_for(Vec::new(), "https://gw.example.org", "k-2") else {
            panic!("a key job")
        };
        assert_eq!(
            job.steps[0].argv,
            sbx::secret_set_litellm_key_argv("gw.example.org", "sbx-cs-aisbx-llm-acme")
        );
        assert_eq!(
            job.post.rewrite_connection.as_ref().map(|c| c.url.as_str()),
            Some("https://gw.example.org")
        );

        // With a sandbox on it, every change is asked about first, naming it and what needs R.
        let body = |plan: ConnPlan| match plan {
            ConnPlan::Confirm(c) => (c.body.join("\n"), c.job),
            other => panic!("a confirmation, not {other:?}"),
        };
        let (text, job) = body(plan_for(users.clone(), &path, ""));
        assert!(
            text.contains("“claude-litellm-proj” is on connection “acme”"),
            "{text}"
        );
        assert!(job.steps.is_empty() && job.post.rewrite_connection.is_some());
        let (text, _) = body(plan_for(users.clone(), url, "not-a-real-key"));
        assert!(
            text.contains("reaches them on their next request"),
            "{text}"
        );
        let (text, _) = body(plan_for(users.clone(), "https://gw.example.org", "k-2"));
        assert!(
            text.contains("stop authenticating until rebuilt (R)"),
            "{text}"
        );
        let (text, _) = body(plan_for(users, &path, ""));
        assert!(
            text.contains("URL path reaches them only when rebuilt"),
            "{text}"
        );
    }

    #[test]
    fn an_edit_never_becomes_the_last_used() {
        let mut app = app_on(&[crate::testkit::acme()], Vec::new());
        app.open_connection(ConnMode::Edit("acme".into()));
        conn_form(&mut app).fields.url = TextInput::new(format!("{}/v2", crate::testkit::ACME_URL));
        press_save(&mut app);
        assert!(app.modal.is_none());
        assert!(matches!(app.screen, Screen::Secrets(_)));
        let saved = crate::litellm::load_in(&app.litellm_root, "acme").unwrap();
        assert!(saved.url.ends_with("/v2"));
        assert_eq!(app.registry.last_litellm(), None);

        app.open_connection(ConnMode::Edit("acme".into()));
        let form = conn_form(&mut app);
        form.fields.url = TextInput::new("https://gw.example.org");
        form.fields.key = TextInput::new("not-a-real-key");
        press_save(&mut app);
        let Some(Modal::Running(_, pending)) = app.modal.take() else {
            panic!("a key job: {:?}", app.status)
        };
        assert!(
            crate::litellm::load_in(&app.litellm_root, "acme")
                .unwrap()
                .url
                .ends_with("/v2"),
            "nothing is written before the key is stored"
        );
        app.apply_post(&pending.post);
        app.start_next(&pending.post);
        assert_eq!(
            crate::litellm::load_in(&app.litellm_root, "acme")
                .unwrap()
                .url,
            "https://gw.example.org"
        );
        assert_eq!(app.registry.last_litellm(), None);
    }

    fn switch_form(app: &mut App) -> &mut ConnSwitchForm {
        match &mut app.screen {
            Screen::ConnSwitch(form) => form,
            other => panic!("the connection form is open, not {other:?}"),
        }
    }

    fn press_apply(app: &mut App) {
        let form = switch_form(app);
        form.focus = form
            .rows()
            .iter()
            .position(|r| *r == ConnSwitchRow::Apply)
            .unwrap();
        app.on_key(KeyEvent::from(KeyCode::Enter));
    }

    /// The `sbx create` of a job, and its first `--kit` — the connection kit, stacked right
    /// after the backend workload, which is the positional.
    fn kit_after_backend(job: &PendingJob, backend: &str) -> String {
        let create = &job
            .steps
            .iter()
            .find(|s| s.argv.get(1).is_some_and(|a| a == "create"))
            .expect("a create")
            .argv;
        assert!(
            create
                .iter()
                .any(|a| a.ends_with(&format!("kits/{backend}"))),
            "the backend workload: {create:?}"
        );
        assert_eq!(create[2], "--kit", "{create:?}");
        create[3].clone()
    }

    #[test]
    fn a_moves_a_litellm_sandbox_to_another_connection() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.models.set(Slot::Opus, Some("claude-sonnet-5".into()));
        let mut app = app_on(&[crate::testkit::acme(), beta()], vec![sb]);
        app.cursor = 0;
        assert!(app.can_edit_auth());
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert_eq!(
            switch_form(&mut app).pick,
            crate::litellm::LitellmPick::Existing("acme".into())
        );
        press_apply(&mut app);
        assert!(app.modal.is_none());
        assert_eq!(status_text(&app), "no connection changes");

        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        app.on_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(
            switch_form(&mut app).pick,
            crate::litellm::LitellmPick::Existing("beta".into())
        );
        press_apply(&mut app);
        let Some(Modal::Confirm(confirm)) = app.modal.take() else {
            panic!("a confirmation: {:?}", app.status)
        };
        let text = confirm.body.join("\n");
        assert!(
            text.contains("moves from LiteLLM connection “acme” to “beta”"),
            "{text}"
        );
        assert!(text.contains("lists no “claude-sonnet-5”"), "{text}");
        // The same workload under the same name: sbx hands its kit volumes back.
        assert!(text.contains("earlier sessions stay"), "{text}");
        assert_eq!(
            confirm.job.steps[0].argv,
            sbx::rm_argv("claude-litellm-proj")
        );
        assert!(
            kit_after_backend(&confirm.job, "claude-litellm").ends_with("beta/litellm-beta-claude")
        );
        assert_eq!(confirm.job.post.last_litellm.as_deref(), Some("beta"));
        assert_eq!(app.registry.last_litellm(), None, "only once it succeeded");
    }

    #[test]
    fn a_onto_a_new_connection_stores_its_key_before_the_rebuild() {
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = app_on(&[crate::testkit::acme()], vec![sb]);
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        app.on_key(KeyEvent::from(KeyCode::Right));
        let form = switch_form(&mut app);
        assert_eq!(form.pick, crate::litellm::LitellmPick::New);
        assert!(form.rows().contains(&ConnSwitchRow::Key));
        form.new_conn.name = TextInput::new("gamma");
        form.new_conn.url = TextInput::new("https://gw.example.org");
        form.new_conn.key = TextInput::new("not-a-real-key");
        press_apply(&mut app);
        let Some(Modal::Confirm(confirm)) = app.modal.take() else {
            panic!("a confirmation: {:?}", app.status)
        };
        let job = confirm.job;
        assert_eq!(
            job.steps[0].argv,
            sbx::secret_set_litellm_key_argv("gw.example.org", "sbx-cs-aisbx-llm-gamma")
        );
        assert_eq!(job.steps[0].stdin.as_deref(), Some("not-a-real-key"));
        let (conn, agent) = job.post.save_connection.clone().unwrap();
        assert_eq!(
            (conn.name.as_str(), agent),
            ("gamma", Some(crate::litellm::Agent::Claude))
        );
        let rebuild = job.post.then.as_deref().expect("the rebuild follows");
        assert!(
            kit_after_backend(rebuild, "claude-litellm").ends_with("gamma/litellm-gamma-claude")
        );
        assert_eq!(rebuild.post.last_litellm.as_deref(), Some("gamma"));
        assert!(!crate::litellm::exists_in(&app.litellm_root, "gamma"));

        app.modal = Some(Modal::Running(finished_job(false), job));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        let form = switch_form(&mut app);
        assert_eq!(form.new_conn.name.value, "gamma");
        assert!(form.new_conn.key.value.is_empty());
        assert!(!crate::litellm::exists_in(&app.litellm_root, "gamma"));
    }

    #[test]
    fn a_moves_a_sandbox_whose_connection_is_gone_but_not_one_whose_folder_is() {
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        sb.litellm = Some("gone".into());
        let mut app = app_on(&[crate::testkit::acme()], vec![sb]);
        assert!(app.rebuild_blocker_text("claude-litellm-proj").is_some());
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert_eq!(
            switch_form(&mut app).pick,
            crate::litellm::LitellmPick::Existing("acme".into())
        );
        press_apply(&mut app);
        let Some(Modal::Confirm(confirm)) = app.modal.take() else {
            panic!("a confirmation: {:?}", app.status)
        };
        assert!(
            confirm.body[0].contains("“gone”, which no longer exists"),
            "{:?}",
            confirm.body
        );
        assert!(
            kit_after_backend(&confirm.job, "claude-litellm").ends_with("acme/litellm-acme-claude")
        );

        app.sandboxes[0].workspace_missing = true;
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        press_apply(&mut app);
        assert!(app.modal.is_none());
        assert_eq!(status_text(&app), WORKSPACE_GONE);
    }
}

#[cfg(test)]
mod grid_tests {
    use super::*;
    use crate::kits::Catalog;

    /// Two sandboxes in this folder (`/proj`), three in others.
    fn mixed() -> App {
        let mut sandboxes = vec![
            model::sample("here-1", Some("claude-anthropic"), &[]),
            model::sample("here-2", Some("claude-anthropic"), &[]),
        ];
        for (i, dir) in ["/work/api", "/work/api", "/work/ui"].iter().enumerate() {
            let mut sb = model::sample(&format!("far-{i}"), Some("claude-anthropic"), &[]);
            sb.workspace = PathBuf::from(dir);
            sandboxes.push(sb);
        }
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    #[test]
    fn select_clamps_to_the_last_cell_and_resets_the_detail_scroll() {
        let mut app = mixed();
        assert_eq!(app.slots(), 6);
        app.detail_scroll = 7;
        app.select(3);
        assert_eq!((app.cursor, app.detail_scroll), (3, 0));

        app.detail_scroll = 7;
        app.select(99);
        assert_eq!((app.cursor, app.detail_scroll), (5, 0));

        // The same cell again is no move: the details pane keeps its place.
        app.detail_scroll = 7;
        app.select(5);
        assert_eq!(app.detail_scroll, 7);
    }

    #[test]
    fn toggling_other_folders_flips_the_section_and_the_remembered_flag() {
        let mut app = mixed();
        assert_eq!((app.here, app.elsewhere), (2, 3));
        assert!(app.show_other);

        app.select(4);
        app.toggle_other_folders();
        assert!(!app.show_other);
        assert!(app.registry.hide_other_folders());
        // Collapsed: only this folder's cells and the New row are left to stand on.
        assert_eq!(app.slots(), app.here + 1);
        assert!(app.cursor <= app.here);

        app.select(0);
        app.toggle_other_folders();
        assert!(app.show_other);
        assert!(!app.registry.hide_other_folders());
        assert_eq!(
            app.cursor,
            app.here + 1,
            "expanding moves the cursor into the section it revealed"
        );
        assert!(!app.cell_is_here(app.cursor));
    }

    #[test]
    fn other_folders_cannot_be_folded_while_none_holds_a_sandbox() {
        let here = vec![model::sample("here-1", Some("claude-anthropic"), &[])];
        let mut app = App::for_test(Catalog::load().unwrap(), here);
        assert_eq!(app.elsewhere, 0);
        assert!(matches!(
            app.can(Action::ToggleOther),
            Err(Refusal::Info(text)) if text.contains("No other folder")
        ));
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(app.show_other, "nothing folded");
        assert!(!app.registry.hide_other_folders(), "nothing remembered");
        let status = app.status.as_ref().map(|(t, e, _)| (t.as_str(), *e));
        assert_eq!(
            status,
            Some(("No other folder has a sandbox to show or hide", false))
        );

        assert_eq!(mixed().can(Action::ToggleOther), Ok(()));
    }

    fn find_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    fn ctrl(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    #[test]
    fn slash_jumps_between_matches_and_esc_goes_back() {
        let mut app = mixed();
        app.select(0);
        find_keys(&mut app, "/far");
        assert_eq!(app.cursor, 3, "far-0, the first match after here-1");
        assert_eq!(app.find_position(), (Some(1), 3));
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!((app.cursor, app.find_position()), (5, (Some(3), 3)));
        ctrl(&mut app, 'n');
        assert_eq!(app.cursor, 3, "past the last match is the first");
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.cursor, 5, "before the first is the last");
        find_keys(&mut app, "q");
        assert!(!app.quit, "q is text while a search is typed");
        assert_eq!(app.find.as_ref().unwrap().query, "farq");
        // Back on a match, away from where the search began: Esc goes back there.
        app.on_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.cursor, 3);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!((app.find.clone(), app.cursor), (None, 0));
    }

    #[test]
    fn typing_narrows_from_the_origin_and_no_match_goes_back() {
        let mut app = mixed();
        app.select(1);
        find_keys(&mut app, "/here");
        assert_eq!(app.cursor, 1, "here-2 matches, and the search began on it");
        find_keys(&mut app, "-1");
        assert_eq!(app.cursor, 0);
        find_keys(&mut app, "z");
        assert_eq!((app.cursor, app.find_position()), (1, (None, 0)));
        app.on_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.cursor, 0);
        ctrl(&mut app, 'u');
        assert_eq!(app.cursor, 1, "an empty query is where the search began");
        app.on_key(KeyEvent::from(KeyCode::Backspace));
        assert!(app.find.is_none());
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn a_match_in_the_folded_section_unfolds_it_for_the_search() {
        let mut app = mixed();
        app.toggle_other_folders();
        assert!(!app.show_other);
        app.select(0);
        find_keys(&mut app, "/work/ui");
        assert!(app.show_other);
        assert_eq!(app.selected().unwrap().name, "far-2");
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.show_other);
        assert_eq!(app.cursor, 0);

        find_keys(&mut app, "/far-1");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.find.is_none());
        assert!(app.show_other);
        assert_eq!(app.selected().unwrap().name, "far-1");
        assert!(app.registry.hide_other_folders(), "the fold is not saved");
    }

    #[test]
    fn a_search_from_the_new_row_starts_after_it() {
        let mut app = mixed();
        app.select(app.here);
        find_keys(&mut app, "/-");
        assert_eq!(app.selected().unwrap().name, "far-0");
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.selected().unwrap().name, "here-2");
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.on_new_card());
    }

    #[test]
    fn a_click_ends_the_search_where_it_lands() {
        let mut app = mixed();
        find_keys(&mut app, "/far");
        app.click(Hit::Press(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.cursor, 4, "the hint stepped to the next match");
        assert!(app.find.is_some());
        app.click(Hit::Card(1));
        assert_eq!((app.find.clone(), app.cursor), (None, 1));
    }

    /// A scratch tree with two workspace folders and a plain file, under the system temp dir.
    fn folder_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aisbx-folders-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("file"), "").unwrap();
        root
    }

    /// `a-1`, `a-2` in `<root>/a`, `b-1` in `<root>/b`, `p-1` in `/proj` — the test app's folder.
    fn app_over(root: &Path) -> App {
        let mut sandboxes = Vec::new();
        for (name, dir) in [("a-1", "a"), ("a-2", "a"), ("b-1", "b")] {
            let mut sb = model::sample(name, Some("claude-anthropic"), &[]);
            sb.workspace = root.join(dir);
            sandboxes.push(sb);
        }
        sandboxes.push(model::sample("p-1", Some("claude-anthropic"), &[]));
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    fn names(app: &App) -> Vec<Option<String>> {
        (0..app.slots())
            .map(|i| app.sandbox_at(i).map(|sb| sb.name.clone()))
            .collect()
    }

    #[test]
    fn set_folder_refuses_what_is_not_an_existing_absolute_folder_and_changes_nothing() {
        let root = folder_tree("refuse");
        let mut app = app_over(&root);
        app.select(2);
        app.grid_scroll = 3;
        app.detail_scroll = 4;
        let before = (names(&app), app.cursor, app.here, app.elsewhere);
        for (dir, says) in [
            (PathBuf::from("relative/dir"), "is not an absolute path"),
            (root.join("missing"), "does not exist"),
            (root.join("file"), "is not a folder"),
        ] {
            let err = app.set_folder(dir.clone()).unwrap_err().to_string();
            assert!(err.contains(&dir.display().to_string()), "{err}");
            assert!(err.contains(says), "{err}");
            assert_eq!(app.cwd, PathBuf::from("/proj"));
            assert_eq!((names(&app), app.cursor, app.here, app.elsewhere), before);
            assert_eq!((app.grid_scroll, app.detail_scroll), (3, 4));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn set_folder_regroups_the_grid_and_keeps_the_selected_sandbox() {
        let root = folder_tree("regroup");
        let mut app = app_over(&root);
        let registry = format!("{:?}", app.registry);
        let process_dir = std::env::current_dir().unwrap();
        assert_eq!((app.here, app.elsewhere), (1, 3));
        let b = (0..app.slots())
            .find(|i| app.sandbox_at(*i).is_some_and(|sb| sb.name == "b-1"))
            .unwrap();
        app.select(b);
        app.grid_scroll = 2;
        app.detail_scroll = 5;

        app.set_folder(root.join("b")).unwrap();
        assert_eq!(app.cwd, root.join("b"));
        assert_eq!((app.here, app.elsewhere), (1, 3));
        assert_eq!(app.selected().map(|sb| sb.name.as_str()), Some("b-1"));
        assert_eq!(app.cursor, 0, "b-1 now opens the first section");
        assert!(app.cell_is_here(app.cursor));
        assert_eq!((app.grid_scroll, app.detail_scroll), (0, 0));
        // Only the grouping changed: nothing is filtered, stored, or moved in the process.
        assert_eq!(app.slots(), 5);
        assert!(app.show_other);
        assert_eq!(format!("{:?}", app.registry), registry);
        assert_eq!(std::env::current_dir().unwrap(), process_dir);

        // A cursor on the New row follows the new New row.
        app.select(app.here);
        assert!(app.on_new_card());
        app.set_folder(root.join("a")).unwrap();
        assert_eq!(app.here, 2);
        assert_eq!(app.cursor, 2);
        assert!(app.on_new_card());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_on_the_folders_screen_picks_a_host_folder_for_the_field() {
        let tmp = std::env::temp_dir().join(format!("aisbx-tests-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("kb").join("notes")).unwrap();
        let tmp = std::fs::canonicalize(&tmp).unwrap();
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.mounts_state = tmp.join("state");
        let key = |app: &mut App, code| app.on_key(KeyEvent::from(code));
        app.perform(Action::Folders);
        for c in tmp.display().to_string().chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let browse = |app: &mut App| {
            let Screen::Folders(form) = &mut app.screen else {
                panic!("the folders screen");
            };
            form.focus = form
                .rows()
                .iter()
                .position(|r| *r == FoldersRow::Browse)
                .unwrap();
            app.on_key(KeyEvent::from(KeyCode::Enter));
        };
        browse(&mut app);
        let Screen::StartDir(picker) = &app.screen else {
            panic!("the picker");
        };
        assert!(matches!(picker.target, StartDirTarget::HostFolder(_)));
        assert_eq!(picker.here(), tmp, "opens on the typed folder");
        assert!(picker.root.parent().is_none(), "browses from the root");
        assert_eq!(picker.dirs, ["kb"]);
        assert_eq!(picker.buttons(), 2, "no Start");
        // Esc hands the screen back unchanged.
        key(&mut app, KeyCode::Esc);
        let Screen::Folders(form) = &app.screen else {
            panic!("back on the folders screen");
        };
        assert_eq!(form.path.value, tmp.display().to_string());
        assert_eq!(form.focused(), FoldersRow::Browse);
        // Into kb, then Use: the field holds it, the cursor goes on to Access.
        browse(&mut app);
        let Screen::StartDir(picker) = &mut app.screen else {
            panic!("the picker");
        };
        picker.descend(0);
        picker.focus = picker
            .rows()
            .iter()
            .position(|r| *r == StartDirRow::Use)
            .unwrap();
        key(&mut app, KeyCode::Enter);
        let Screen::Folders(form) = &app.screen else {
            panic!("back on the folders screen");
        };
        assert_eq!(
            folders::typed_path(&form.path.value, model::home_dir().as_deref()).unwrap(),
            tmp.join("kb")
        );
        assert_eq!(form.focused(), FoldersRow::Access);
        assert!(
            !folders::config_path(&app.mounts_state).exists(),
            "nothing saved before Save"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_host_folder_picker_refuses_the_home_folder() {
        let Some(home) = model::home_dir().filter(|h| h.is_dir()) else {
            return;
        };
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let (root, rel) = host_root(&home);
        app.screen = Screen::StartDir(StartDirForm::open(
            StartDirTarget::HostFolder(Box::default()),
            "host folder".into(),
            root,
            Some(rel),
        ));
        let Screen::StartDir(picker) = &mut app.screen else {
            panic!("the picker");
        };
        picker.focus = picker
            .rows()
            .iter()
            .position(|r| *r == StartDirRow::Use)
            .unwrap();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::StartDir(_)), "stays open");
        assert!(app.status.as_ref().is_some_and(|(_, error, _)| *error));
    }

    #[test]
    fn host_root_splits_a_path_into_its_root_and_the_rest() {
        let start = std::env::temp_dir().join("a").join("b");
        let (root, rel) = host_root(&start);
        assert!(root.parent().is_none());
        assert!(rel.is_relative());
        assert_eq!(root.join(rel), start);
    }

    #[test]
    fn folders_lists_each_mounted_folder_once_in_the_other_sections_order() {
        let mut sandboxes = Vec::new();
        for (i, dir) in ["/work/UI", "/work/api", "/work/api", ""]
            .iter()
            .enumerate()
        {
            let mut sb = model::sample(&format!("s-{i}"), Some("claude-anthropic"), &[]);
            sb.workspace = PathBuf::from(dir);
            sandboxes.push(sb);
        }
        let mut app = App::for_test(Catalog::load().unwrap(), sandboxes);
        assert_eq!(app.here, 0, "nothing in /proj");
        let listed = |app: &App| -> Vec<(PathBuf, usize, bool)> {
            let folders = app.folders().into_iter();
            folders.map(|f| (f.path, f.sandboxes, f.current)).collect()
        };
        // Deduplicated, mountless left out, the current folder there with none.
        let p = PathBuf::from;
        assert_eq!(
            listed(&app),
            [
                (p("/proj"), 0, true),
                (p("/work/api"), 2, false),
                (p("/work/UI"), 1, false)
            ]
        );
        // The same order as the list's second section.
        let mut grid: Vec<PathBuf> = Vec::new();
        for sb in (app.here + 1..app.slots()).filter_map(|i| app.sandbox_at(i)) {
            if !sb.mountless() && grid.last() != Some(&sb.workspace) {
                grid.push(sb.workspace.clone());
            }
        }
        assert_eq!(grid, [p("/work/api"), p("/work/UI")]);

        // The current folder is marked where sandboxes mount it, not added twice.
        app.cwd = p("/work/api");
        assert_eq!(
            listed(&app),
            [(p("/work/api"), 2, true), (p("/work/UI"), 1, false)]
        );
    }

    #[test]
    fn only_a_pinning_backend_reports_its_pinned_models() {
        let mut sb = model::sample("gw", Some("claude-litellm"), &[]);
        sb.models.set(Slot::Opus, Some("big-1".into()));
        sb.models.set(Slot::Haiku, Some("small-1".into()));
        let mut login = model::sample("login", Some("claude-anthropic"), &[]);
        login.models.set(Slot::Opus, Some("big-1".into()));
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone(), login.clone()]);
        Arc::make_mut(&mut app.catalog).models = vec![models::Model {
            id: "big-1".into(),
            name: "Big One".into(),
            ..Default::default()
        }];
        assert!(app.backend_of(&sb).is_some());
        assert_eq!(
            app.pinned_models(&sb),
            [
                ("opus", "Big One".to_string()),
                ("haiku", "small-1".to_string())
            ]
        );
        assert!(app.pinned_models(&login).is_empty());
        let extern_ = model::sample("x", None, &[]);
        assert!(app.backend_of(&extern_).is_none());
        assert!(app.pinned_models(&extern_).is_empty());
    }

    #[test]
    fn a_credential_is_missing_only_once_the_host_has_answered() {
        let sb = model::sample("login", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
        let catalog = Arc::make_mut(&mut app.catalog);
        let backend = catalog
            .backends
            .iter_mut()
            .find(|b| b.kit.name == "claude-anthropic")
            .unwrap();
        backend.kit.credentials = vec!["svc-a".into(), "svc-b".into()];
        app.secrets = None;
        assert!(app.missing_credentials(&sb).is_empty(), "not answered yet");
        app.secrets = Some(sbx::Secrets {
            services: vec![sbx::Service::sample("svc-b")],
            custom: Vec::new(),
        });
        assert_eq!(app.missing_credentials(&sb), ["svc-a".to_string()]);
        assert!(app
            .missing_credentials(&model::sample("x", None, &[]))
            .is_empty());
    }

    #[test]
    fn dismissing_the_status_clears_it() {
        let mut app = mixed();
        app.set_error("boom");
        assert!(app.status.is_some());
        app.dismiss_status();
        assert!(app.status.is_none());
    }
}

/// The list screen's actions through [`App::can`] / [`App::perform`], the modal answers both
/// frontends share, and [`App::open_session`].
#[cfg(test)]
mod action_tests {
    use super::*;
    use crate::kits::Catalog;

    /// `box` here (stopped), `run` here (running), the New row, then `far` in another folder.
    fn app() -> App {
        let mut running = model::sample("run", Some("claude-anthropic"), &[]);
        running.running = true;
        let mut far = model::sample("far", Some("claude-anthropic"), &[]);
        far.workspace = PathBuf::from("/elsewhere");
        let sandboxes = vec![
            model::sample("box", Some("claude-anthropic"), &[]),
            running,
            far,
        ];
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    /// Select the sandbox `name`, or the New row for `new`.
    fn on(app: &mut App, name: &str) {
        let cell = match name {
            "new" => app.here,
            name => app.cell_of(name).unwrap(),
        };
        app.select(cell);
    }

    const NEW_CARD: &str = "the cursor is on + New sandbox — press ↵ to create one";

    /// The status line: its text, and whether it is an error.
    fn said(app: &App) -> Option<(&str, bool)> {
        app.status
            .as_ref()
            .map(|(text, error, _)| (text.as_str(), *error))
    }

    /// What an action can change on the list screen, for comparing two apps.
    fn state(app: &App) -> String {
        let modal = match &app.modal {
            None => "none".to_string(),
            Some(Modal::Help) => "help".into(),
            Some(Modal::Confirm(c)) => format!("confirm {} {:?}", c.title, c.body),
            Some(Modal::Prompt(p)) => format!("prompt {} {}", p.title(), p.subject),
            Some(Modal::Running(job, pending)) => format!("running {} {:?}", job.title, pending),
        };
        format!(
            "{:?} | {modal} | {:?} | {:?} | {} {} {} {} {}",
            std::mem::discriminant(&app.screen),
            app.status.as_ref().map(|(t, e, _)| (t, e)),
            app.handoff,
            app.show_other,
            app.cursor,
            app.quit,
            app.detail_full,
            app.mouse,
        )
    }

    #[test]
    fn esc_on_the_list_backs_out_and_never_quits() {
        let mut esc = app();
        esc.on_key(KeyEvent::from(KeyCode::Char('m')));
        assert!(esc.detail_full);
        for _ in 0..3 {
            esc.on_key(KeyEvent::from(KeyCode::Esc));
            assert!(!esc.detail_full && !esc.quit);
        }
        for key in [
            KeyEvent::from(KeyCode::Char('q')),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut quit = app();
            quit.on_key(key);
            assert!(quit.quit, "{key:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn leaving_a_running_job_says_why_it_stays() {
        let keys = [
            KeyEvent::from(KeyCode::Esc),
            KeyEvent::from(KeyCode::Enter),
            KeyEvent::from(KeyCode::Char('q')),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ];
        for key in keys {
            let mut app = app();
            let sleep = vec!["sleep".to_string(), "5".to_string()];
            let job = Job::spawn("Waiting", vec![Step::new("sleep", sleep)]);
            let pending = PendingJob {
                title: "Waiting".into(),
                steps: Vec::new(),
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            };
            app.modal = Some(Modal::Running(job, pending));
            app.on_key(key);
            assert!(matches!(app.modal, Some(Modal::Running(..))), "{key:?}");
            assert!(!app.quit, "{key:?}");
            assert_eq!(said(&app), Some((JOB_STILL_RUNNING, false)), "{key:?}");
        }
    }

    #[test]
    fn t_opens_the_settings_and_a_change_is_one_settings_job() {
        let mut app = app();
        on(&mut app, "box");
        app.on_key(KeyEvent::from(KeyCode::Char('T')));
        let Screen::Settings(form) = &mut app.screen else {
            panic!("T opens the settings: {:?}", said(&app))
        };
        let rows = form.rows();
        assert_eq!(
            rows,
            [
                SettingsRow::Setting(0),
                SettingsRow::Setting(1),
                SettingsRow::Setting(2),
                SettingsRow::Setting(3),
                SettingsRow::SkillsDefault,
                SettingsRow::Close
            ]
        );
        assert_eq!(form.view(0).0, "unknown", "nothing is read under test");

        // Unknown: nothing is changed blind.
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none());
        assert!(said(&app).is_some_and(|(text, error)| error && text.contains("F5")));

        let (telemetry, copy) = (sbx::TELEMETRY_DISABLED, sbx::CLIPBOARD_TEXT_COPY);
        let paste = sbx::CLIPBOARD_IMAGE_PASTE;
        for (i, value, key, set) in [
            (0, false, telemetry, "true"),
            (0, true, telemetry, "false"),
            (1, false, copy, "true"),
            (2, false, paste, "true"),
            (2, true, paste, "false"),
            (3, true, sbx::SANDBOX_FUSE, "false"),
        ] {
            app.modal = None;
            let Screen::Settings(form) = &mut app.screen else {
                panic!("still on the settings")
            };
            form.values[i] = Some(crate::testkit::bool_setting(key, value));
            form.focus = i;
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
            let Some(Modal::Running(_, pending)) = &app.modal else {
                panic!("a change runs a job: {:?}", said(&app))
            };
            assert_eq!(pending.steps[0].argv, sbx::setting_set_argv(key, set));
            assert!(pending.post.reread_settings, "and reads the values back");
        }

        // A click on a setting only moves the cursor; one on Close closes.
        app.modal = None;
        app.click(Hit::FormRow(1));
        assert!(app.modal.is_none(), "no job from a click on the row");
        app.click(Hit::FormRow(5));
        assert!(matches!(app.screen, Screen::List));
    }

    #[test]
    fn the_skills_default_walks_its_three_modes_one_job_each() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Char('T')));
        let skills_row = SETTINGS.len();
        let job = |app: &App| match &app.modal {
            Some(Modal::Running(_, pending)) => Some(pending.steps[0].argv.clone()),
            _ => None,
        };
        let set = |mode: &str| sbx::setting_set_argv(sbx::SKILLS_DEFAULT_MODE, mode);
        let at = |app: &mut App, mode: &str| {
            app.modal = None;
            let Screen::Settings(form) = &mut app.screen else {
                panic!("on the settings")
            };
            form.skills = Some(crate::testkit::skills_default(mode));
            form.focus = skills_row;
        };

        // Unknown: refused.
        let Screen::Settings(form) = &mut app.screen else {
            panic!("T opens the settings")
        };
        form.focus = skills_row;
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(job(&app).is_none());
        assert!(said(&app).is_some_and(|(text, error)| error && text.contains("F5")));

        // ↵ and → walk forward, ← back, tightest first and wrapping.
        for (from, key, to) in [
            ("readonly", KeyCode::Enter, "readwrite"),
            ("readonly", KeyCode::Right, "readwrite"),
            ("readwrite", KeyCode::Char(' '), "off"),
            ("readonly", KeyCode::Left, "off"),
            ("off", KeyCode::Left, "readwrite"),
        ] {
            at(&mut app, from);
            app.on_key(KeyEvent::from(key));
            assert_eq!(job(&app), Some(set(to)), "{key:?} from {from}");
        }

        // A click on a mode sets it; one on the current mode does nothing.
        at(&mut app, "readonly");
        app.click(Hit::FormChoice(skills_row, 0));
        assert_eq!(job(&app), Some(set("off")));
        at(&mut app, "readonly");
        app.click(Hit::FormChoice(skills_row, 1));
        assert!(job(&app).is_none(), "already read-only");
    }

    #[test]
    fn the_skills_default_says_what_new_sandboxes_get() {
        let mut form = SettingsForm::default();
        assert_eq!(form.skills_view().0, "unknown");
        for (mode, word, tone) in [
            ("off", "off", SettingTone::Calm),
            ("readonly", "read-only", SettingTone::Plain),
            ("readwrite", "read-write", SettingTone::Warn),
        ] {
            form.skills = Some(crate::testkit::skills_default(mode));
            let (state, meaning, got) = form.skills_view();
            assert_eq!((state, got), (word, tone), "{mode}");
            assert!(meaning.starts_with("new sandboxes"), "{meaning}");
        }
        assert!(SKILLS_DEFAULT_NOTE.contains(sbx::SKILLS_DEFAULT_MODE));
    }

    #[test]
    fn a_setting_the_environment_sets_says_so_and_is_not_changed() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Char('T')));
        let Screen::Settings(form) = &mut app.screen else {
            panic!("T opens the settings")
        };
        let mut set = crate::testkit::bool_setting(sbx::CLIPBOARD_IMAGE_PASTE, true);
        set.source = "envvar".into();
        form.values[2] = Some(set);
        form.focus = 2;
        let (state, meaning) = form.view(2);
        assert_eq!(state, "on");
        assert!(
            meaning.contains("set by DOCKER_SANDBOXES_CLIPBOARD_IMAGE_PASTE"),
            "{meaning}"
        );
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.modal.is_none(), "a stored value would not apply");
        assert!(said(&app).is_some_and(|(text, error)| {
            error && text.contains("DOCKER_SANDBOXES_CLIPBOARD_IMAGE_PASTE")
        }));
    }

    #[test]
    fn each_setting_says_what_its_value_means() {
        let mut form = SettingsForm::default();
        assert_eq!(form.view(0).0, "unknown");
        form.error = Some("daemon down".into());
        assert!(form.view(1).1.contains("daemon down"));
        let copy = |value| crate::testkit::bool_setting(sbx::CLIPBOARD_TEXT_COPY, value);
        let telemetry = |value| crate::testkit::bool_setting(sbx::TELEMETRY_DISABLED, value);
        let paste = |value| crate::testkit::bool_setting(sbx::CLIPBOARD_IMAGE_PASTE, value);
        let fuse = |value| crate::testkit::bool_setting(sbx::SANDBOX_FUSE, value);
        form.values = vec![
            Some(telemetry(true)),
            Some(copy(true)),
            Some(paste(true)),
            Some(fuse(true)),
        ];
        assert_eq!(form.view(0), ("off", SETTINGS[0].meanings[0].to_string()));
        assert_eq!(form.view(1), ("on", SETTINGS[1].meanings[0].to_string()));
        assert_eq!(form.view(2), ("on", SETTINGS[2].meanings[0].to_string()));
        assert_eq!(form.view(3), ("on", SETTINGS[3].meanings[0].to_string()));
        let tones: Vec<SettingTone> = (0..SETTINGS.len())
            .map(|i| SETTINGS[i].tone(form.value(i)))
            .collect();
        assert_eq!(
            tones,
            [
                SettingTone::Calm,
                SettingTone::Warn,
                SettingTone::Warn,
                SettingTone::Plain
            ],
            "telemetry off is calm, an opened clipboard a warning, FUSE neither"
        );
        assert_eq!(SETTINGS[0].tone(None), SettingTone::Unknown);
        form.values = vec![
            Some(telemetry(false)),
            Some(copy(false)),
            Some(paste(false)),
            Some(fuse(false)),
        ];
        assert_eq!(
            form.view(0).0,
            "on",
            "telemetry.disabled false: telemetry on"
        );
        assert_eq!(form.view(1).0, "off");
        assert_eq!(form.view(2).0, "off");
        assert_eq!(form.view(3).0, "off");
        assert_eq!(SETTINGS[3].tone(Some(false)), SettingTone::Plain);
        for spec in SETTINGS {
            assert!(
                spec.note.starts_with("When on, ") && spec.note.contains(" Otherwise, "),
                "{} says what on and off do",
                spec.label
            );
        }
        assert!(
            SETTINGS[0].note.contains("sbx daemon restart"),
            "what telemetry needs"
        );
    }

    #[test]
    fn each_action_key_is_perform() {
        for cell in ["box", "run", "new"] {
            for action in Action::ALL {
                let (mut by_key, mut by_call) = (app(), app());
                on(&mut by_key, cell);
                on(&mut by_call, cell);
                by_key.on_key(action.key());
                by_call.perform(action);
                assert_eq!(state(&by_key), state(&by_call), "{action:?} on {cell}");
            }
        }
    }

    #[test]
    fn the_actions_cover_every_list_key() {
        let primary: Vec<KeyEvent> = Action::ALL.iter().map(|a| a.key()).collect();
        let not_actions = "qhjklgG1234567mM";
        for c in ' '..='~' {
            let key = KeyEvent::from(KeyCode::Char(c));
            if primary.contains(&key) {
                continue;
            }
            assert_eq!(Action::from_key(key), None, "{c}");
            if not_actions.contains(c) {
                continue;
            }
            let mut a = app();
            on(&mut a, "box");
            let before = state(&a);
            a.on_key(key);
            assert_eq!(state(&a), before, "{c} does something but is no action");
        }
        // The aliases (see `actions::tests`): Delete is `d`.
        let (mut by_alias, mut by_call) = (app(), app());
        by_alias.on_key(KeyCode::Delete.into());
        by_call.perform(Action::Delete);
        assert_eq!(state(&by_alias), state(&by_call));
    }

    #[test]
    fn start_on_the_new_card_opens_the_create_form() {
        for by_key in [false, true] {
            let mut a = app();
            a.select(a.here);
            assert_eq!(a.can(Action::Start), Err(Refusal::Error(NEW_CARD.into())));
            match by_key {
                true => a.on_key(KeyCode::Enter.into()),
                false => a.perform(Action::Start),
            }
            assert!(matches!(a.screen, Screen::Create(_)) && a.handoff.is_none());
        }
    }

    #[test]
    fn each_refusal_keeps_its_kind_and_text() {
        let mut a = app();
        a.select(a.here);
        assert_eq!(a.can(Action::Shell), Err(Refusal::Error(NEW_CARD.into())));
        for action in [Action::Stop, Action::Rename, Action::Delete] {
            assert_eq!(a.can(action), Err(Refusal::Silent), "{action:?}");
            a.perform(action);
            assert!(a.status.is_none() && a.modal.is_none(), "{action:?}");
        }
        a.perform(Action::Shell);
        assert_eq!(said(&a), Some((NEW_CARD, true)));

        on(&mut a, "box");
        let stopped = Refusal::Info("box is already stopped".into());
        assert_eq!(a.can(Action::Stop), Err(stopped));
        a.perform(Action::Stop);
        assert_eq!(said(&a), Some(("box is already stopped", false)));
        assert!(a.modal.is_none());

        a.sandboxes[0].workspace_missing = true;
        let gone = Refusal::Error("the folder /proj no longer exists on this computer".into());
        for action in [
            Action::Start,
            Action::Shell,
            Action::Connect,
            Action::KeepRunning,
        ] {
            assert_eq!(a.can(action), Err(gone.clone()), "{action:?}");
        }
        a.perform(Action::Start);
        assert!(a.handoff.is_none());
        assert_eq!(said(&a).map(|s| s.1), Some(true));
    }

    #[test]
    fn perform_opens_the_tuis_modals_and_handoffs() {
        let mut a = app();
        on(&mut a, "box");
        a.perform(Action::Delete);
        let confirm = |c: &Confirm| c.title == "Delete sandbox" && c.danger && !c.yes;
        assert!(
            matches!(&a.modal, Some(Modal::Confirm(c)) if confirm(c)),
            "Cancel first"
        );
        a.modal = None;
        a.perform(Action::Rename);
        let rename = |p: &Prompt| matches!(p.ask, Ask::Rename) && !p.masked();
        assert!(matches!(&a.modal, Some(Modal::Prompt(p)) if rename(p)));
        a.modal = None;
        on(&mut a, "run");
        a.perform(Action::Stop);
        let stop = |p: &PendingJob| p.steps[0].argv == sbx::stop_argv("run");
        assert!(matches!(&a.modal, Some(Modal::Running(_, p)) if stop(p)));

        a.modal = None;
        a.perform(Action::Start);
        assert!(matches!(&a.handoff, Some(Handoff::Agent { name, .. }) if name == "run"));
    }

    #[test]
    fn every_prompt_names_what_answering_does() {
        let asks = [
            (Ask::Rename, "rename"),
            (Ask::PublishPort, "publish"),
            (Ask::UnpublishPort, "unpublish"),
            (Ask::CheckHost, "check"),
            (Ask::SetSecret(SecretTarget::Token("work".into())), "store"),
            (Ask::AddSkills, "add"),
        ];
        for (ask, verb) in asks {
            let prompt = Prompt {
                ask: ask.clone(),
                subject: "box".into(),
                input: TextInput::new(""),
                help: Vec::new(),
            };
            assert_eq!(prompt.verb(), verb, "{ask:?}");
        }
    }

    #[test]
    fn a_confirmation_is_answered_through_one_path() {
        for (key, yes) in [
            (None, true),
            (None, false),
            (Some(KeyCode::Char('y')), true),
            (Some(KeyCode::Esc), false),
            (Some(KeyCode::Enter), false),
        ] {
            let mut a = app();
            on(&mut a, "box");
            a.perform(Action::Delete);
            match key {
                None => a.confirm_answer(yes),
                Some(code) => a.on_key(code.into()),
            }
            let rm = |p: &PendingJob| p.steps[0].argv == sbx::rm_argv("box");
            match yes {
                true => assert!(
                    matches!(&a.modal, Some(Modal::Running(_, p)) if rm(p)),
                    "{key:?}"
                ),
                false => assert!(a.modal.is_none(), "{key:?}: closed"),
            }
        }
    }

    #[test]
    fn a_prompt_submits_and_cancels_through_one_path() {
        let mut a = app();
        on(&mut a, "box");
        a.perform(Action::Rename);
        if let Some(Modal::Prompt(prompt)) = &mut a.modal {
            prompt.input.set("  Renamed  ");
        }
        a.prompt_submit();
        assert!(a.modal.is_none());
        assert_eq!(a.registry.display_name("box"), Some("Renamed"));

        a.perform(Action::Rename);
        a.on_key(KeyCode::Char('X').into());
        a.prompt_cancel();
        assert!(a.modal.is_none());
        assert_eq!(a.registry.display_name("box"), Some("Renamed"));

        // The keys reach the same methods (the prompt opens on the label the row shows).
        a.perform(Action::Rename);
        a.on_key(KeyCode::Char('2').into());
        a.on_key(KeyCode::Enter.into());
        assert_eq!(a.registry.display_name("box"), Some("box2"));
        a.perform(Action::Rename);
        a.on_key(KeyCode::Esc.into());
        assert!(a.modal.is_none());
    }

    fn failed_job() -> Job {
        let job = Job::spawn("fails", vec![Step::new("fail", Vec::new())]);
        while !job.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(job.status(), Status::Failed(_)));
        job
    }

    fn pending(title: &str) -> PendingJob {
        PendingJob {
            title: title.into(),
            steps: Vec::new(),
            post: PostJob::default(),
            fallback: None,
            fallback_hint: None,
        }
    }

    #[test]
    fn a_failed_job_closes_with_its_restore_or_runs_its_fallback() {
        for by_key in [false, true] {
            let mut a = app();
            a.open_create();
            let Screen::Create(form) = std::mem::replace(&mut a.screen, Screen::List) else {
                panic!()
            };
            let mut job = pending("create");
            job.post.restore = Some(Restore::Create(Box::new(form)));
            a.modal = Some(Modal::Running(failed_job(), job));
            match by_key {
                false => a.job_close(),
                true => a.on_key(KeyCode::Char('q').into()),
            }
            assert!(a.modal.is_none());
            assert!(matches!(a.screen, Screen::Create(_)), "restored");

            let mut job = pending("kit add");
            job.fallback = Some(Box::new(pending("rebuild instead")));
            a.modal = Some(Modal::Running(failed_job(), job));
            match by_key {
                false => a.job_fallback(),
                true => a.on_key(KeyCode::Char('r').into()),
            }
            let fallback = |p: &PendingJob| p.title == "rebuild instead";
            assert!(matches!(&a.modal, Some(Modal::Running(_, p)) if fallback(p)));
        }
    }

    #[test]
    fn a_succeeded_job_is_left_to_tick() {
        let mut a = app();
        a.modal = Some(Modal::Running(Job::spawn("ok", Vec::new()), pending("ok")));
        while !matches!(&a.modal, Some(Modal::Running(job, _)) if job.is_finished()) {
            std::thread::sleep(Duration::from_millis(5));
        }
        a.job_close();
        assert!(a.modal.is_some(), "tick closes it and runs what follows");
    }

    /// An environment with nothing on PATH but, maybe, `AISBX_TERMINAL`.
    fn env(aisbx_terminal: Option<&str>) -> terminal::TermEnv<'static> {
        terminal::TermEnv {
            aisbx_terminal: aisbx_terminal.map(str::to_string),
            path: Some("/usr/bin".into()),
            pathext: None,
            exists: &|_| false,
            editor: None,
        }
    }

    fn launch(name: &str) -> terminal::Launch {
        terminal::Launch::start(Handoff::Shell {
            name: name.into(),
            workdir: None,
        })
    }

    #[test]
    fn a_session_opens_in_a_terminal_and_says_so() {
        let mut a = app();
        a.registry.set_display_name("run", Some("My run".into()));
        a.sandboxes[1].label = "My run".into();
        let env = env(Some("xterm -e"));
        let session = launch("run");
        assert!(session.is_finished());
        a.open_session_in(session, terminal::Os::Linux, &env)
            .unwrap();
        let opened_line = "My run: shell opened in a terminal window";
        assert_eq!(said(&a), Some((opened_line, false)));
        assert!(a.loading, "a refresh was requested");
        let opened = terminal::opened();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0][..2], ["xterm".to_string(), "-e".into()]);
        let shell = sbx::shell_argv("run", None);
        assert_eq!(opened[0][opened[0].len() - shell.len()..], shell[..]);
    }

    #[test]
    fn no_terminal_is_returned_and_a_control_character_is_an_error() {
        let mut a = app();
        let env = env(None);
        assert_eq!(
            a.open_session_in(launch("run"), terminal::Os::Linux, &env),
            Err(terminal::LaunchError::NoTerminal)
        );
        assert!(terminal::opened().is_empty());

        let bad = terminal::Launch::start(Handoff::Connect {
            name: "run".into(),
            what: "editor",
            argv: vec!["code".into(), "line\nbreak".into()],
            terminal: true,
        });
        assert_eq!(a.open_session_in(bad, terminal::Os::Linux, &env), Ok(()));
        let (text, error, _) = a.status.clone().unwrap();
        assert!(error && text.contains("argument 1"), "{text}");
        assert!(!text.contains("line"), "{text}");
        assert!(terminal::opened().is_empty());
    }

    fn editor(argv: &[&str]) -> terminal::Launch {
        terminal::Launch::start(Handoff::Connect {
            name: "run".into(),
            what: "connection",
            argv: argv.iter().map(|s| s.to_string()).collect(),
            terminal: false,
        })
    }

    #[test]
    fn an_editor_starts_detached_and_says_so() {
        let mut a = app();
        let argv = ["code", "--remote", "ssh-remote+run.sbx", "/proj"];
        let env = env(None);
        assert_eq!(
            a.open_session_in(editor(&argv), terminal::Os::Linux, &env),
            Ok(())
        );
        assert_eq!(terminal::opened(), vec![argv.map(String::from).to_vec()]);
        assert_eq!(said(&a), Some(("run: `code` started", false)));
        assert!(a.loading, "a refresh was requested");

        let bad = ["code", "--remote", "ssh-remote+run.sbx", "/a\u{7}b"];
        assert_eq!(
            a.open_session_in(editor(&bad), terminal::Os::Linux, &env),
            Ok(())
        );
        let (text, error, _) = a.status.clone().unwrap();
        assert!(error && text.contains("argument 3"), "{text}");
        assert!(terminal::opened().is_empty());
        let windows = ["code", "/say \"hi\""];
        a.open_session_in(editor(&windows), terminal::Os::Windows, &env)
            .unwrap();
        assert!(said(&a).is_some_and(|(t, e)| e && t.contains("argument 1")));
        assert!(terminal::opened().is_empty());
    }

    #[test]
    fn submit_connect_marks_whether_the_target_wants_a_terminal() {
        let mut a = app();
        for (i, target) in crate::remote::Target::ALL.iter().enumerate() {
            a.screen = Screen::Connect(ConnectForm {
                sandbox: "run".into(),
                ssh_ready: true,
                focus: 0,
            });
            a.submit_connect(i);
            match a.handoff.take() {
                Some(Handoff::Connect { terminal, argv, .. }) => {
                    assert_eq!(terminal, target.in_terminal(), "{target:?}");
                    assert_eq!(Some(argv), target.command("run", Path::new("/proj")));
                }
                // Set up inside the app, or its launcher is not on this host's PATH.
                None => assert!(!target.in_terminal() || !target.available(), "{target:?}"),
                Some(other) => panic!("{other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod connect_view_tests {
    use super::*;
    use crate::kits::Catalog;
    use crate::remote::{Target, REMOTE_DEV_KIT};

    fn on_connect(sb: Sandbox, ssh_ready: bool) -> App {
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.screen = Screen::Connect(ConnectForm {
            sandbox: name,
            ssh_ready,
            focus: 0,
        });
        app
    }

    fn target(view: &ConnectView, target: Target) -> &TargetView {
        let at = Target::ALL.iter().position(|t| *t == target).unwrap();
        &view.targets[at]
    }

    #[test]
    fn the_endpoint_says_whether_connecting_starts_the_sandbox() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        let app = on_connect(sb.clone(), true);
        let view = app.connect_view_with(|_| true).unwrap();
        assert_eq!(
            (view.label.as_str(), view.host.as_str()),
            ("box", "box.sbx")
        );
        assert_eq!(view.state, "stopped: connecting starts it");
        assert!(!view.ssh_missing && view.ssh_warning.is_empty() && view.ssh_notes.is_empty());
        let rows: Vec<ConnectRow> = view.targets.iter().map(|t| t.row).collect();
        assert_eq!(
            rows,
            (0..Target::ALL.len())
                .map(ConnectRow::Target)
                .collect::<Vec<_>>()
        );

        let app = on_connect(
            Sandbox {
                running: true,
                ..sb
            },
            false,
        );
        let view = app.connect_view_with(|_| true).unwrap();
        assert_eq!(view.state, "running");
        assert!(view.ssh_missing);
        assert_eq!(
            view.ssh_warning,
            ["SSH is not set up yet, so no app can connect."]
        );
        let notes = view.ssh_notes.join(" ");
        assert!(notes.contains("~/.ssh/config"), "{notes}");
        for inner in ["Idempotent", "socket", "Host *.sbx"] {
            assert!(!notes.contains(inner), "{inner}: {notes}");
        }

        // No Connect screen, or its sandbox gone: nothing to draw.
        let mut app = app;
        app.sandboxes.clear();
        assert_eq!(app.connect_view(), None);
        app.screen = Screen::List;
        assert_eq!(app.connect_view(), None);
    }

    #[test]
    fn a_mountless_sandbox_opens_the_images_own_folder() {
        let mut sb = model::sample("bare", Some("claude-anthropic"), &[]);
        sb.workspace = PathBuf::new();
        let view = on_connect(sb, true).connect_view_with(|_| true).unwrap();
        for t in &view.targets {
            let steps = t.steps.join(" ");
            assert!(
                steps.contains(model::MOUNTLESS_WORKDIR),
                "{}: {steps}",
                t.title
            );
        }
    }

    #[test]
    fn each_target_says_how_it_is_reached() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        let app = on_connect(sb, true);
        let view = app.connect_view_with(|t| t != Target::VsCode).unwrap();
        let code = target(&view, Target::VsCode);
        assert_eq!(code.launch, TargetLaunch::NotOnPath("code"));
        assert_eq!(code.status, "`code` command not found");
        let ssh = target(&view, Target::Terminal);
        assert_eq!(
            (ssh.launch, ssh.status.as_str()),
            (TargetLaunch::Here, ""),
            "a target that opens says nothing: Open and the footer say it"
        );
        assert_eq!(ssh.steps, Target::Terminal.steps("box", Path::new("/proj")));
        for t in [Target::Jetbrains, Target::ClaudeDesktop] {
            let t = target(&view, t);
            assert_eq!(t.launch, TargetLaunch::InApp);
            assert_eq!(t.status, "set up in the app");
        }
    }

    #[test]
    fn the_kit_warning_needs_a_target_that_downloads_and_a_kit_to_add() {
        let bare = model::sample("box", Some("claude-anthropic"), &[]);
        let view = on_connect(bare.clone(), true)
            .connect_view_with(|_| true)
            .unwrap();
        for (t, view) in Target::ALL.iter().zip(&view.targets) {
            assert_eq!(!view.kit_warning.is_empty(), t.needs_kit(), "{t:?}");
        }
        let warning = target(&view, Target::VsCode).kit_warning.join(" ");
        assert!(warning.contains(REMOTE_DEV_KIT), "{warning}");
        assert!(warning.contains("Edit kits"), "names the action: {warning}");

        let kitted = model::sample("box", Some("claude-anthropic"), &[REMOTE_DEV_KIT]);
        let view = on_connect(kitted, true)
            .connect_view_with(|_| true)
            .unwrap();
        assert!(view.targets.iter().all(|t| t.kit_warning.is_empty()));

        let mut app = on_connect(bare, true);
        let catalog = std::sync::Arc::make_mut(&mut app.catalog);
        catalog.addons.retain(|a| a.kit.name != REMOTE_DEV_KIT);
        let view = app.connect_view_with(|_| true).unwrap();
        assert!(view.targets.iter().all(|t| t.kit_warning.is_empty()));
    }
}

#[cfg(test)]
mod form_decision_tests {

    use super::*;
    use crate::create::{CreateStep, WizardRow};
    use crate::kits::Catalog;

    fn addon(app: &App, name: &str) -> usize {
        let at = app.catalog.addons.iter().position(|a| a.kit.name == name);
        at.unwrap_or_else(|| panic!("the checkout ships {name}"))
    }

    /// The kits editor on `box` (carrying `has`), with `change` applied to its form.
    fn editor(has: &[&str], other_checkout: bool, change: impl FnOnce(&App, &mut KitsForm)) -> App {
        let mut sb = model::sample("box", Some("claude-anthropic"), has);
        if other_checkout {
            sb.other_checkout = Some(PathBuf::from("/elsewhere"));
        }
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.open_kits("box");
        let Screen::Kits(mut form) = std::mem::replace(&mut app.screen, Screen::List) else {
            panic!("the kits editor is open")
        };
        change(&app, &mut form);
        app.screen = Screen::Kits(form);
        app
    }

    fn effect(app: &App) -> KitsEffect {
        let Screen::Kits(form) = &app.screen else {
            panic!("the kits editor is open")
        };
        let other = app.sandboxes[0].other_checkout.is_some();
        form.effect(&app.catalog, other)
    }

    /// What `submit_kits` did: nothing, a live `kit add`, or the rebuild (its confirmation, or the
    /// refusal to rebuild).
    fn submitted(mut app: App) -> &'static str {
        app.submit_kits();
        match &app.modal {
            None if app
                .status
                .as_ref()
                .is_some_and(|(t, ..)| t == "no kit changes") =>
            {
                "none"
            }
            Some(Modal::Running(_, pending))
                if pending
                    .steps
                    .iter()
                    .all(|s| s.argv[..3] == ["sbx", "kit", "add"]) =>
            {
                "kit add"
            }
            Some(Modal::Confirm(c)) if c.title.contains("Apply kit changes") => "rebuild",
            None if app.status.as_ref().is_some_and(|(_, error, _)| *error) => "rebuild",
            other => panic!("unexpected: {other:?} {:?}", app.status),
        }
    }

    #[test]
    fn the_kits_effect_is_what_apply_does() {
        let cases: Vec<(App, KitsEffect, &str)> = vec![
            (editor(&[], false, |_, _| {}), KitsEffect::NoChanges, "none"),
            (editor(&[], true, |_, _| {}), KitsEffect::NoChanges, "none"),
            (
                editor(&[], false, |app, f| f.addons[addon(app, "jdk25")] = true),
                KitsEffect::KitAdd,
                "kit add",
            ),
            // A v3 kit of image content and an install hook: `sbx kit add` applies it.
            (
                editor(&[], false, |app, f| {
                    f.addons[addon(app, "statusline")] = true
                }),
                KitsEffect::KitAdd,
                "kit add",
            ),
            // A network grant needs sbx's terminal prompt: tried, expected to fail.
            (
                editor(&[], false, |app, f| f.addons[addon(app, "web")] = true),
                KitsEffect::KitAddRefused("network rules"),
                "kit add",
            ),
            (
                editor(&["jdk25"], false, |app, f| {
                    f.addons[addon(app, "jdk25")] = false
                }),
                KitsEffect::Rebuild(RebuildCause::Kits),
                "rebuild",
            ),
            (
                editor(&[], false, |app, f| f.addons[addon(app, "host-npm")] = true),
                KitsEffect::Rebuild(RebuildCause::Kits),
                "rebuild",
            ),
            (
                editor(&[], true, |app, f| f.addons[addon(app, "jdk25")] = true),
                KitsEffect::Rebuild(RebuildCause::OtherCheckout),
                "rebuild",
            ),
            (
                editor(&[], false, |_, f| {
                    f.original_models.set(Slot::Opus, Some("m-1".into()));
                    f.models.set(Slot::Opus, Some("m-2".into()));
                }),
                KitsEffect::Rebuild(RebuildCause::Models),
                "rebuild",
            ),
        ];
        for (app, want, does) in cases {
            assert_eq!(effect(&app), want);
            assert_eq!(submitted(app), does, "{want:?}");
        }
        // A removal outranks the other checkout and a re-pin, as `submit_kits` always ranked them.
        let app = editor(&["jdk25"], true, |app, f| {
            f.addons[addon(app, "jdk25")] = false
        });
        assert_eq!(effect(&app), KitsEffect::Rebuild(RebuildCause::Kits));
    }

    #[test]
    fn each_effect_has_its_note() {
        let note = KitsEffect::Rebuild(RebuildCause::OtherCheckout)
            .note(History::Volumes)
            .join(" ");
        assert_eq!(
            note,
            "Applying this rebuilds the sandbox: its kits come from another checkout. The \
             agent's earlier sessions stay."
        );
        let refused = KitsEffect::KitAddRefused("network rules").note(History::Volumes);
        assert!(
            refused[0].starts_with("Applying this tries to add the kits without a rebuild."),
            "{refused:?}"
        );
        for cause in [
            RebuildCause::Models,
            RebuildCause::Kits,
            RebuildCause::OtherCheckout,
        ] {
            let text = KitsEffect::Rebuild(cause).note(History::Volumes).join(" ");
            assert!(text.contains("earlier sessions stay"), "{cause:?}: {text}");
        }
        assert_eq!(
            KitsEffect::NoChanges.note(History::Volumes),
            ["No changes."]
        );
    }

    #[test]
    fn with_ram_volumes_every_note_says_the_history_is_lost() {
        let effects = [
            KitsEffect::Rebuild(RebuildCause::Models),
            KitsEffect::Rebuild(RebuildCause::Kits),
            KitsEffect::Rebuild(RebuildCause::OtherCheckout),
            KitsEffect::KitAddRefused("volumes"),
            KitsEffect::KitAdd,
        ];
        for effect in effects {
            let ram = effect.note(History::Ram).join(" ");
            assert!(
                !ram.contains("stays") && !ram.contains("survive:"),
                "{effect:?}: {ram}"
            );
            assert!(
                ram.contains("earlier sessions are lost"),
                "{effect:?}: {ram}"
            );
            let disk = effect.note(History::Volumes).join(" ");
            assert!(!disk.contains("sessions are lost"), "{effect:?}: {disk}");
            assert_ne!(ram, disk, "{effect:?}");
        }
        let kit_add = KitsEffect::KitAdd.note(History::Ram).join(" ");
        assert!(kit_add.contains("what you installed stay"), "{kit_add}");
        assert_eq!(KitsEffect::NoChanges.note(History::Ram), ["No changes."]);
    }

    #[test]
    fn the_session_history_stays_only_in_kit_volumes() {
        let texts = || {
            let sb = model::sample("box", Some("claude-anthropic"), &[]);
            let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
            let rebuild = app
                .recreate_confirm("box", &[], None, "Rebuild", Vec::new())
                .expect("a plan")
                .body
                .join(" ");
            app.cursor = 0;
            app.action_delete();
            let Some(Modal::Confirm(delete)) = app.modal.take() else {
                panic!("a delete confirmation")
            };
            (rebuild, delete.body.join(" "))
        };
        let (rebuild, delete) = texts();
        assert!(rebuild.contains("earlier sessions stay"), "{rebuild}");
        assert!(
            delete.contains("earlier sessions stay, and come back"),
            "{delete}"
        );

        // A kit without volumes keeps the history only in the container, which a rebuild and a
        // delete remove.
        let sb = model::sample("box", Some("agy"), &[]);
        assert!(!sb.history_in_volumes);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        let rebuild = app
            .recreate_confirm("box", &[], None, "Rebuild", Vec::new())
            .expect("a plan")
            .body
            .join(" ");
        assert!(
            rebuild.contains("and the agent's earlier sessions are lost"),
            "{rebuild}"
        );
        assert!(!rebuild.contains("stays"), "{rebuild}");
        app.cursor = 0;
        app.action_delete();
        let Some(Modal::Confirm(delete)) = app.modal.take() else {
            panic!("a delete confirmation")
        };
        let delete = delete.body.join(" ");
        assert!(
            delete.contains("earlier sessions are deleted with it"),
            "{delete}"
        );
        assert!(!delete.contains("sessions stay"), "{delete}");
        let kits = KitsEffect::Rebuild(RebuildCause::Kits)
            .note(History::Container)
            .join(" ");
        assert!(
            kits.ends_with("The agent's earlier sessions are lost."),
            "{kits}"
        );
        let refused = KitsEffect::KitAddRefused("volumes")
            .note(History::Container)
            .join(" ");
        assert!(
            refused.ends_with("The agent's earlier sessions are lost."),
            "{refused}"
        );
    }

    #[test]
    fn every_paragraph_of_a_rebuild_confirmation_starts_a_sentence() {
        for ram in [false, true] {
            let mut sb = model::sample("box", Some("claude-anthropic"), &["jdk25"]);
            sb.tmpfs_volumes = ram;
            let app = App::for_test(Catalog::load().unwrap(), vec![sb]);
            let body = app
                .recreate_confirm("box", &[], None, "Rebuild", Vec::new())
                .expect("a plan")
                .body;
            for line in body.iter().filter(|l| !l.is_empty()) {
                let first = line.chars().next().unwrap();
                let named = line.starts_with("sbx ");
                assert!(!first.is_lowercase() || named, "ram {ram}: {line:?}");
            }
        }
    }

    #[test]
    fn missing_secrets_names_only_what_the_host_lacks() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let codex = app.catalog.backend("codex-openai").cloned().unwrap();
        app.secrets = None;
        assert!(app.missing_secrets(&codex).is_empty(), "not read yet");
        app.secrets = Some(sbx::Secrets::default());
        assert_eq!(app.missing_secrets(&codex), ["openai"]);
    }

    #[test]
    fn a_click_focuses_a_text_row_and_presses_any_other() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.click(Hit::NewCard);
        let rows = |app: &App| match &app.screen {
            Screen::Create(form) => (form.wizard_step(), form.wizard_rows(&app.catalog)),
            _ => panic!("the create form is open"),
        };
        let (step, providers) = rows(&app);
        assert_eq!(step, CreateStep::Provider);
        assert!(matches!(providers[0], WizardRow::Provider(_)));
        app.click(Hit::FormRow(0));
        assert_eq!(rows(&app).0, CreateStep::Harness, "↵ on a provider goes on");

        if let Screen::Create(form) = &mut app.screen {
            form.wizard_set_step(&app.catalog, CreateStep::Workspace);
        }
        let (_, ws) = rows(&app);
        let name = ws
            .iter()
            .position(|r| *r == WizardRow::Field(CreateRow::Name));
        app.click(Hit::FormRow(name.unwrap()));
        let Screen::Create(form) = &app.screen else {
            panic!("still open")
        };
        assert_eq!(
            (form.wizard_step(), form.wizard_focus()),
            (CreateStep::Workspace, name.unwrap())
        );
        assert!(CreateRow::Name.is_text() && !CreateRow::Clone.is_text());
        assert!(CreateRow::Token.masked() && CreateRow::LitellmKey.masked());
        assert!(!CreateRow::TokenName.masked() && !CreateRow::LitellmUrl.masked());
    }

    #[test]
    fn a_form_focus_click_moves_the_cursor_only() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.secrets = Some(crate::testkit::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        let token = app
            .secret_rows()
            .iter()
            .position(|r| matches!(r, SecretRow::Token(_)))
            .unwrap();
        app.click(Hit::FormFocus(token));
        assert!(app.modal.is_none(), "no prompt: {:?}", app.modal);
        assert!(matches!(&app.screen, Screen::Secrets(f) if f.focus == token));
        app.click(Hit::FormFocus(usize::MAX));
        assert!(matches!(&app.screen, Screen::Secrets(f) if f.focus == token));
        app.click(Hit::FormRow(token));
        assert!(
            matches!(app.modal, Some(Modal::Prompt(_))),
            "FormRow presses"
        );

        app.modal = None;
        app.screen = Screen::List;
        app.click(Hit::NewCard);
        app.click(Hit::FormFocus(1));
        let Screen::Create(form) = &app.screen else {
            panic!("the wizard is open")
        };
        assert_eq!(
            (form.wizard_step(), form.wizard_focus()),
            (CreateStep::Provider, 1)
        );
    }

    #[test]
    fn the_review_lists_every_answer_in_order() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = PathBuf::from("/proj");
        app.open_create();
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        let review = form.review(&app);
        // Each answer, the step a click goes to, and whether it is a default (drawn muted).
        let lines: Vec<(&str, CreateStep, bool)> = review
            .answers
            .iter()
            .map(|a| (a.label, a.step, a.default))
            .collect();
        assert_eq!(
            lines,
            [
                ("Provider", CreateStep::Provider, false),
                ("Connection", CreateStep::Connection, false),
                ("Harness", CreateStep::Harness, false),
                ("Models", CreateStep::Models, true),
                ("Kits", CreateStep::Kits, true),
                ("Label", CreateStep::Workspace, false),
                ("Starts in", CreateStep::Workspace, true),
                ("Clone mode", CreateStep::Workspace, true),
                ("Skills", CreateStep::Workspace, true),
            ],
            "the seldom used answers have no line while they are the form's defaults"
        );
        assert_eq!(review.answers[0].value, "LiteLLM");
        assert_eq!(review.answers[1].value, "acme");
        assert_eq!(review.answers[2].value, "Claude Code");
        assert_eq!(review.answers[3].value, crate::create::AGENT_PICKS);
        assert_eq!(review.answers[4].value, "none");
        assert_eq!(review.answers[5].value, "Claude Code · LiteLLM acme");
        assert_eq!(review.sbx_name, "claude-litellm-proj");
        assert_eq!(review.sbx_name, form.sbx_name(&app));
        assert_eq!(review.mounts, ["/proj"]);
        assert!(!review.policy_warning);
        assert_eq!(
            crate::create::account_notes(&AccountChoice::Named("work".into()))[0],
            "Stored on this computer. Sandboxes on the same token share its account."
        );
        assert!(crate::kits::headroom_warning("claude").starts_with("⚠ headroom is unmeasured"));
        assert!(crate::kits::headroom_warning("opencode").contains("idle proxy"));
    }

    #[test]
    fn a_changed_answer_is_no_longer_a_default() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.mcp = vec![sbx::McpServer {
            name: "notion".into(),
            status: "ready".into(),
            ..sbx::McpServer::default()
        }];
        app.open_create();
        let catalog = Arc::clone(&app.catalog);
        let default_of = |app: &App, label: &str| {
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            let review = form.review(app);
            let found = review.answers.iter().find(|a| a.label == label);
            found
                .unwrap_or_else(|| panic!("{label} is reviewed"))
                .default
        };
        let reviewed = |app: &App, label: &str| {
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            form.review(app).answers.iter().any(|a| a.label == label)
        };
        type Change = fn(&mut CreateForm, &Catalog);
        let changes: [(&str, Change); 6] = [
            ("Models", |f, c| {
                f.connection
                    .models
                    .set(Slot::Opus, Some(c.models[0].id.clone()))
            }),
            ("Kits", |f, _| f.kits.addons[0] = true),
            ("Starts in", |f, _| {
                f.workspace.start_dir = Some("sub".into())
            }),
            ("Clone mode", |f, _| f.workspace.clone = true),
            ("Skills", |f, _| {
                f.workspace.skills = f.workspace.skills.next()
            }),
            ("Label", |f, _| f.workspace.name = TextInput::new("mine")),
        ];
        for (label, change) in changes {
            app.open_create();
            let label_line = label == "Label";
            assert_eq!(
                default_of(&app, label),
                !label_line,
                "{label} opens as a default"
            );
            let Screen::Create(form) = &mut app.screen else {
                panic!("the create form is open")
            };
            change(form, &catalog);
            assert!(!default_of(&app, label), "{label} changed");
        }
        let seldom: [(&str, Change); 6] = [
            ("Offered", |f, c| f.toggle_model(c, 1)),
            ("Also mounts", |f, _| {
                f.workspace.extra_folders = vec!["/srv/docs:ro".into()]
            }),
            ("Ports", |f, _| f.workspace.ports = TextInput::new("8080")),
            ("Blocked hosts", |f, _| {
                f.workspace.deny_network = TextInput::new("example.com")
            }),
            ("MCP servers", |f, _| f.workspace.static_mcp[0] = true),
            ("MCP servers", |f, _| f.workspace.no_mcp = true),
        ];
        for (label, change) in seldom {
            app.open_create();
            assert!(!reviewed(&app, label), "{label} has no line at first");
            let Screen::Create(form) = &mut app.screen else {
                panic!("the create form is open")
            };
            change(form, &catalog);
            assert!(!default_of(&app, label), "{label} changed");
        }
    }
}

/// The decisions the account, secrets, skills and approvals screens draw — one copy, in core,
/// for both frontends.
#[cfg(test)]
mod screen_decision_tests {
    use super::*;
    use crate::kits::Catalog;
    use crate::testkit;

    fn app(sandboxes: Vec<Sandbox>) -> App {
        let mut app = App::for_test(Catalog::load().unwrap(), sandboxes);
        app.secrets = Some(testkit::sample_secrets());
        app
    }

    #[test]
    fn secret_rows_show_the_value_matched_to_them_and_who_uses_it() {
        let mut box_ = model::sample("box", Some("claude-anthropic"), &[]);
        box_.auth = Some(Auth::Login);
        let mut app = app(vec![box_]);
        let view = app.secret_view(&SecretRow::Oauth);
        assert_eq!(view.name, "/login (OAuth)");
        assert_eq!(view.stored.as_deref(), Some("(oauth configured)"));
        assert_eq!(view.note, "1 sandbox");

        // The connection's key counts only for its host, and its file gives the model count.
        let view = app.secret_view(&SecretRow::Connection("acme".into()));
        assert_eq!(view.stored.as_deref(), Some("sk-l********"));
        assert_eq!(view.note, "litellm.example.com · 2 models");
        let secrets = app.secrets.as_mut().unwrap();
        secrets.custom[2] = testkit::litellm_key("acme", "elsewhere.example.org");
        let view = app.secret_view(&SecretRow::Connection("acme".into()));
        assert_eq!(view.stored, None);
        assert_eq!(
            view.note,
            "litellm.example.com · key stored for another host"
        );

        // A service row is matched on name and scope: the global value is not a scoped one.
        let scoped = SecretRow::Service {
            name: "litellm".into(),
            scope: Some("box".into()),
        };
        let view = app.secret_view(&scoped);
        assert_eq!((view.stored, view.empty), (None, Empty::NotStored));
        assert_eq!(view.note, "this sandbox only: box");
        let global = app.secret_view(&SecretRow::global_service("litellm"));
        assert!(global.stored.is_some());

        let mcp = app.secret_view(&SecretRow::global_service("mcp:slack:client_secret"));
        assert_eq!(mcp.note, "MCP gateway: slack");

        // Nothing read yet, and an Antigravity sign-in sbx does not list.
        assert_eq!(
            app.secret_view(&SecretRow::AgyLogin).empty,
            Empty::NotListed
        );
        app.secrets = None;
        assert_eq!(app.secret_view(&SecretRow::Oauth).empty, Empty::Unknown);
    }

    #[test]
    fn the_agy_sign_in_is_read_off_the_agy_sandboxes_auth_mode() {
        let agy = |name: &str, mode: &str| {
            let mut sb = model::sample(name, Some("agy"), &[]);
            sb.auth_mode = mode.into();
            sb
        };
        let mut app = app(Vec::new());
        app.secrets = Some(sbx::Secrets::default());
        let empty = |app: &App| app.secret_view(&SecretRow::AgyLogin).empty;
        assert_eq!((app.agy_signed_in(), empty(&app)), (None, Empty::NotListed));

        // Another backend's mode, an unread one and another service say nothing.
        let mut claude = model::sample("claude-anthropic-x", Some("claude-anthropic"), &[]);
        claude.auth_mode = "oauth · antigravity".into();
        app.sandboxes = vec![claude, agy("agy-a", ""), agy("agy-b", "oauth · google")];
        assert_eq!((app.agy_signed_in(), empty(&app)), (None, Empty::NotListed));

        app.sandboxes.push(agy("agy-c", "none · antigravity"));
        assert_eq!(
            (app.agy_signed_in(), empty(&app)),
            (Some(false), Empty::NotSignedIn)
        );
        app.sandboxes.push(agy("agy-d", "oauth · antigravity"));
        assert_eq!(
            (app.agy_signed_in(), empty(&app)),
            (Some(true), Empty::SignedIn)
        );
        let view = app.secret_view(&SecretRow::AgyLogin);
        assert_eq!(view.stored, None, "no Sign out on inspect's word alone");
    }

    #[test]
    fn secret_notes_headings_and_foreign_lines_come_unindented() {
        let app = app(Vec::new());
        let mcp = app.secret_notes(&SecretRow::global_service("mcp:slack:client_secret"));
        assert!(
            mcp[0].starts_with("The secret of the MCP server “slack”."),
            "{mcp:?}"
        );
        let service = app.secret_notes(&SecretRow::global_service("litellm"));
        assert!(
            service[0].ends_with("gets the change at once — no sandbox here does yet."),
            "{service:?}"
        );
        assert!(app
            .secret_notes(&SecretRow::Token("work".into()))
            .iter()
            .all(|l| !l.starts_with(' ')));
        assert!(app.secret_notes(&SecretRow::Close).is_empty());

        assert_eq!(
            App::secret_heading(&SecretRow::Token("work".into())),
            Some("Anthropic subscription")
        );
        assert_eq!(
            App::secret_heading(&SecretRow::global_service("mcp:acme:api-key")),
            Some(MCP_SECRETS)
        );
        assert_eq!(App::secret_heading(&SecretRow::Close), None);

        let foreign = app.foreign_secret_lines();
        assert_eq!(foreign.len(), 2, "{foreign:?}");
        assert!(foreign[0].starts_with("someone-elses-secret"));
        assert!(foreign[1].starts_with("change these with"));
        let mut none = App::for_test(Catalog::load().unwrap(), Vec::new());
        none.secrets = Some(sbx::Secrets::default());
        assert!(none.foreign_secret_lines().is_empty());
    }

    #[test]
    fn row_predicates_name_the_text_and_the_secret_rows() {
        assert!(AuthRow::Value.masked() && AuthRow::Value.is_text());
        assert!(AuthRow::Name.is_text() && !AuthRow::Name.masked());
        assert!(!AuthRow::Apply.is_text() && !AuthRow::TokenNew.is_text());
        assert!(ConnRow::Key.masked() && ConnRow::Url.is_text() && !ConnRow::Save.is_text());
        assert!(ConnSwitchRow::Key.masked() && ConnSwitchRow::Name.is_text());
        assert!(!ConnSwitchRow::Conn.is_text() && !ConnSwitchRow::Url.masked());
    }

    #[test]
    fn the_account_effect_follows_what_apply_does() {
        use crate::app::AccountChoice;
        let effect = |account| testkit::auth_form(account).effect()[0];
        assert!(effect(AccountChoice::Login).starts_with("No changes."));
        assert!(effect(AccountChoice::New).starts_with("Applying this rebuilds the sandbox."));
        let mut form = testkit::auth_form(AccountChoice::Login);
        form.current = Auth::Token {
            name: "work".into(),
            placeholder: sbx::named_token_placeholder("work"),
            stored: true,
        };
        assert!(form.effect()[0].contains("rebuilds the sandbox on /login"));
        form.account = AccountChoice::Named("private".into());
        assert!(form.effect()[0].contains("rebuilds the sandbox on the other token"));
        form.account = AccountChoice::Named("work".into());
        form.token = TextInput::new("sk-ant-oat01-rotated");
        assert!(form.effect()[0].starts_with("Applying this stores the token"));
    }

    #[test]
    fn moving_agrees_with_the_switch_plan() {
        let beta = testkit::connection("beta", "https://gw.example.org");
        let sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.litellm_root = testkit::litellm_root_with(&[testkit::acme(), beta]);
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        let Screen::ConnSwitch(form) = &app.screen else {
            panic!("the move form is open")
        };
        let mut form = form.clone();
        for pick in [
            crate::litellm::LitellmPick::Existing("acme".into()),
            crate::litellm::LitellmPick::Existing("beta".into()),
            crate::litellm::LitellmPick::New,
        ] {
            form.pick = pick.clone();
            let nothing = matches!(app.switch_plan(&form), ConnPlan::Nothing(_));
            assert_eq!(form.moving(), !nothing, "{pick:?}");
        }
        assert_eq!(app.litellm_users("acme").len(), 1);
    }

    #[test]
    fn approval_notes_say_what_allow_records() {
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        let mut request = testkit::held("box", "example.org:443");
        app.approvals = vec![request.clone()];
        let notes = app.approval_notes(&request.id);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("Allow lets example.org:443 through for this sandbox"));
        assert!(notes[0].contains("Dismiss keeps it blocked"), "{notes:?}");
        request.options.push(sbx::ApprovalOption {
            id: "allow-all".into(),
            label: "Allow all".into(),
        });
        app.approvals = vec![request.clone()];
        let notes = app.approval_notes(&request.id);
        assert_eq!(
            notes.len(),
            1,
            "sbx's own option ids are not the user's: {notes:?}"
        );
        for inner in ["sbx policy", "`", "allow-all"] {
            assert!(!notes[0].contains(inner), "{inner}: {notes:?}");
        }
        assert!(app.approval_notes("network:gone").is_empty());

        assert_eq!(approval_reason(&request), NO_RULE_YET);
        request.detail = "Protocol: TCP\nblocked by the organization's policy".into();
        assert_eq!(
            approval_reason(&request),
            "blocked by the organization's policy"
        );
    }

    #[test]
    fn skill_rows_say_what_they_do() {
        let app = App::for_test(Catalog::load().unwrap(), Vec::new());
        assert_eq!(
            app.skill_view(&SkillRow::Skill("pdf".into())),
            ("pdf".to_string(), String::new())
        );
        assert_eq!(
            app.skill_view(&SkillRow::AddRepo).0,
            "Add from a Git repository"
        );
        assert!(
            app.skill_notes(&SkillRow::Skill("pdf".into()))[0].starts_with("Remove deletes pdf")
        );
        assert!(app.skill_notes(&SkillRow::Update)[0].contains("0 sandboxes here"));
        assert!(app.skill_notes(&SkillRow::Close).is_empty());
        assert_eq!(
            App::skill_heading(&SkillRow::Update),
            Some("Change the store")
        );
        assert_eq!(App::skill_heading(&SkillRow::Close), None);
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::insight::Fetched;
    use crate::stats::Sample;

    fn reading(uptime_s: f64, busy: u64) -> Sample {
        let mut s = Sample::parse(
            "cpu  10 0 10 80 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\nMemAvailable: 524288 kB\n\
             100.0 100.0\nx 1000 500 500 50% /\n",
        )
        .unwrap();
        s.uptime_s = uptime_s;
        s.cpu_total = 100 + uptime_s as u64 * 100;
        s.cpu_idle = s.cpu_total - busy;
        s
    }

    fn app() -> App {
        let sandboxes = ["a", "b"].map(|n| {
            let mut sb = model::sample(n, Some("claude-anthropic"), &[]);
            sb.running = true;
            sb
        });
        let mut app = App::for_test(Catalog::load().unwrap(), sandboxes.into());
        app.tab = Tab::Resources;
        app
    }

    fn deliver(app: &mut App, name: &str, sample: Sample) {
        let payload = Payload::Resources(Ok(sample));
        let name = name.to_string();
        app.insight_tx.send(Fetched { name, payload }).unwrap();
        app.tick();
    }

    fn cpu(app: &App, name: &str) -> Vec<Option<f32>> {
        let ring = app.histories.get(name).unwrap();
        ring.points().map(|p| p.cpu).collect()
    }

    #[test]
    fn readings_are_recorded_and_survive_a_selection_away_and_back() {
        let mut app = app();
        app.tick();
        assert_eq!(app.insight.name, "a");
        deliver(&mut app, "a", reading(100.0, 0));
        deliver(&mut app, "a", reading(102.5, 50));
        assert_eq!(cpu(&app, "a"), [None, Some(0.25)]);
        assert_eq!(app.insight.cpu, Some(0.25), "the same number as the chart");

        app.select(1);
        app.tick();
        assert_eq!(app.insight.name, "b");
        assert!(
            app.insight.resources.value.is_none(),
            "the tab's data is reset"
        );
        // A late answer for `a` still lands in its own ring, never in `b`'s.
        deliver(&mut app, "a", reading(105.0, 200));
        assert!(app.histories.get("b").is_none());
        app.select(0);
        app.tick();
        assert_eq!(cpu(&app, "a"), [None, Some(0.25), Some(0.5)]);
    }

    fn listed(name: &str, status: &str) -> model::Raw {
        model::Raw {
            ls: sbx::LsEntry {
                name: name.into(),
                agent: "claude".into(),
                status: status.into(),
                workspaces: vec!["/proj".into()],
                ..Default::default()
            },
            inspect: None,
        }
    }

    #[test]
    fn a_snapshot_drops_the_rings_of_stopped_and_missing_sandboxes() {
        let mut app = app();
        app.tick();
        for name in ["a", "b", "c"] {
            app.histories.record(name, &reading(100.0, 0));
        }
        let snapshot = Snapshot {
            sandboxes: vec![listed("a", "running"), listed("b", "stopped")],
            kit_hashes: Default::default(),
            secrets: None,
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            approvals: None,
            policy_needs_init: None,
            fetchers: Vec::new(),
        };
        app.tx.send(Ok(snapshot)).unwrap();
        app.tick();
        assert!(app.sandboxes.iter().any(|s| s.name == "a" && s.running));
        assert!(app.histories.get("a").is_some());
        assert!(app.histories.get("b").is_none(), "stopped");
        assert!(app.histories.get("c").is_none(), "not listed");
    }

    #[test]
    fn a_late_reading_does_not_bring_back_a_dropped_ring() {
        let mut app = app();
        app.tick();
        for name in ["a", "b"] {
            app.histories.record(name, &reading(100.0, 0));
        }
        let snapshot = Snapshot {
            sandboxes: vec![listed("a", "stopped")],
            kit_hashes: Default::default(),
            secrets: None,
            mcp: Vec::new(),
            skills: None,
            skills_default: sbx::SkillsMode::default(),
            approvals: None,
            policy_needs_init: None,
            fetchers: Vec::new(),
        };
        app.tx.send(Ok(snapshot)).unwrap();
        for name in ["a", "b"] {
            let payload = Payload::Resources(Ok(reading(102.5, 50)));
            let name = name.to_string();
            app.insight_tx.send(Fetched { name, payload }).unwrap();
        }
        app.tick();
        assert!(app.histories.get("a").is_none(), "stopped");
        assert!(app.histories.get("b").is_none(), "not listed");
    }
}

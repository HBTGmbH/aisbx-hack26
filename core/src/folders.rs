//! Host folders a sandbox mounts beside its workspace: the host's own list of folders for every
//! sandbox (`state_dir()/mounts.json`) and the folders a workspace's `companion.json` names. Each
//! one is an extra workspace at its own host path, read-only or read-write, and a generated kit
//! tells the agent where they are and what they are for.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The host's list, in the state folder.
pub const CONFIG_FILE: &str = "mounts.json";
/// A workspace's list of the repos that belong to it.
pub const COMPANION_FILE: &str = "companion.json";
const VERSION: u32 = 1;
/// The generated kit that lists a sandbox's folders for its agent.
const KIT_PREFIX: &str = "folders-";

/// Whether the sandbox may change a folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    #[default]
    Read,
    Write,
}

impl Access {
    pub const ALL: [Access; 2] = [Access::Read, Access::Write];

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Access> {
        Access::ALL.into_iter().find(|a| a.as_str() == s)
    }

    /// How the forms and the agent's list say it.
    pub fn label(self) -> &'static str {
        match self {
            Access::Read => "read-only",
            Access::Write => "read-write",
        }
    }

    /// What the access means for the folder, as the forms say it.
    pub fn meaning(self) -> &'static str {
        match self {
            Access::Read => "nothing in the sandbox can change it",
            Access::Write => "the agent can change and delete what is in it",
        }
    }
}

/// One folder of the host's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub purpose: String,
}

/// The host's list as `mounts.json` holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub folders: Vec<Entry>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: VERSION,
            folders: Vec::new(),
        }
    }
}

pub fn config_path(state: &Path) -> PathBuf {
    state.join(CONFIG_FILE)
}

/// The host's list. A missing file is an empty list; a file that does not parse or has another
/// `version` is an error naming the file, so the next save cannot write over it.
pub fn load(path: &Path) -> Result<Config> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let config: Config = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} does not parse; fix or remove it (it is left as it is)",
            path.display()
        )
    })?;
    if config.version != VERSION {
        bail!(
            "{} has version {}, this aisbx reads version {VERSION}; it is left as it is",
            path.display(),
            config.version
        );
    }
    for (i, entry) in config.folders.iter().enumerate() {
        if let Err(why) = check_entry(entry) {
            bail!("{}: entry {}: {why}", path.display(), i + 1);
        }
    }
    Ok(config)
}

/// Change the host's list under an exclusive lock on `<path>.lock`: load, apply `f`, then write
/// through a temp file and a rename. A list that does not load is not written.
pub fn update<T>(path: &Path, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let dir = path
        .parent()
        .context("the mount list path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _lock = crate::skill_repos::lock_file(&path.with_extension("json.lock"))?;
    let mut config = load(path)?;
    let out = f(&mut config)?;
    for entry in &config.folders {
        check_entry(entry).map_err(anyhow::Error::msg)?;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, serde_json::to_vec_pretty(&config)?)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map(|()| out)
}

/// What the host's list accepts: an absolute path and a purpose on one line.
pub fn check_entry(entry: &Entry) -> Result<(), String> {
    if !entry.path.is_absolute() {
        return Err(format!("{} is not an absolute path", entry.path.display()));
    }
    if entry.purpose.chars().any(char::is_control) {
        return Err("the purpose must be one line of text".into());
    }
    Ok(())
}

/// A typed path for the host's list: `~` and `~/…` expand to `home`; the result must be absolute.
pub fn typed_path(raw: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("enter a folder".into());
    }
    let path = match (raw.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(raw),
    };
    match path.is_absolute() {
        true => Ok(path),
        false => Err(format!("{raw} is not an absolute path")),
    }
}

/// Add a folder to the host's list, or replace the entry with the same path.
pub fn put(state: &Path, entry: Entry) -> Result<()> {
    update(&config_path(state), |config| {
        match config.folders.iter_mut().find(|e| e.path == entry.path) {
            Some(old) => *old = entry,
            None => config.folders.push(entry),
        }
        Ok(())
    })
}

/// Replace the folder at `index` of the host's list. A stale index is an error.
pub fn replace(state: &Path, index: usize, entry: Entry) -> Result<()> {
    update(&config_path(state), |config| {
        if config
            .folders
            .iter()
            .enumerate()
            .any(|(i, e)| i != index && e.path == entry.path)
        {
            bail!("{} is already in the list", entry.path.display());
        }
        let slot = config
            .folders
            .get_mut(index)
            .context("the list changed meanwhile; open it again")?;
        *slot = entry;
        Ok(())
    })
}

/// Remove the folder with this path from the host's list; whether one was there.
pub fn remove(state: &Path, path: &Path) -> Result<bool> {
    update(&config_path(state), |config| {
        let before = config.folders.len();
        config.folders.retain(|e| e.path != path);
        Ok(config.folders.len() != before)
    })
}

// --- companion.json -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct Companion {
    #[serde(default)]
    public: Option<String>,
    #[serde(default)]
    related: Vec<Related>,
}

#[derive(Deserialize)]
struct Related {
    path: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    access: Access,
}

/// What the agent is told of a companion's public repo, which `companion.json` gives no purpose.
pub const PUBLIC_PURPOSE: &str =
    "The public repo of this project: its product code, tests and user README. Change the code there.";

/// Where a folder comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The host's list, for every sandbox.
    Host,
    /// The workspace's `companion.json`.
    Companion,
    /// Typed into the create wizard for this one sandbox ([`extras`]).
    Wizard,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Host => "every sandbox",
            Origin::Companion => COMPANION_FILE,
            Origin::Wizard => "this sandbox",
        }
    }
}

/// A folder a sandbox mounts beside its workspace: at its own absolute host path, or at `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    pub access: Access,
    pub purpose: String,
    pub origin: Origin,
    /// Where the sandbox sees it, when not at its host path.
    pub target: Option<String>,
}

impl Folder {
    /// The extra workspace as `sbx create` takes it: `PATH[:TARGET][:ro]`.
    pub fn sbx_spec(&self) -> String {
        self.spec(self.path.display().to_string())
    }

    /// [`Folder::sbx_spec`] with `~` for the home folder, for the Review.
    pub fn shown_spec(&self) -> String {
        self.spec(crate::model::tilde(&self.path))
    }

    fn spec(&self, host: String) -> String {
        let mut spec = host;
        if let Some(target) = &self.target {
            spec.push(':');
            spec.push_str(target);
        }
        if self.access == Access::Read {
            spec.push_str(":ro");
        }
        spec
    }

    /// Where the sandbox sees it.
    pub fn sandbox_path(&self) -> String {
        match &self.target {
            Some(target) => target.clone(),
            None => crate::sbx::sandbox_path(&self.path),
        }
    }
}

/// One typed entry as sbx reads it: the host path, the target after its last colon where that
/// starts with `/`, and `:ro` at the end. A drive letter (`C:/data`) stays part of the path.
fn entry_parts(entry: &str) -> (&str, Option<&str>, Access) {
    let (rest, access) = match entry.strip_suffix(":ro") {
        Some(rest) => (rest, Access::Read),
        None => (entry, Access::Write),
    };
    let drive = |p: &str| p.len() == 1 && p.chars().all(|c| c.is_ascii_alphabetic());
    match rest.rsplit_once(':') {
        Some((path, target)) if !path.is_empty() && !drive(path) && target.starts_with('/') => {
            (path, Some(target), access)
        }
        _ => (rest, None, access),
    }
}

/// The host folder of an extra workspace as sbx writes it (`PATH[:TARGET][:ro]`), and whether
/// it is read-only.
pub fn spec_host(spec: &str) -> (&str, bool) {
    let (path, _, access) = entry_parts(spec);
    (path, access == Access::Read)
}

/// The extra workspace `spec` (`PATH[:TARGET][:ro]`) with its access set to `access`: `:ro`
/// added for read-only, dropped for read-write. Path and target stay as they are.
pub fn spec_with_access(spec: &str, access: Access) -> String {
    let (path, target, _) = entry_parts(spec);
    let mut out = path.to_string();
    if let Some(target) = target {
        out.push(':');
        out.push_str(target);
    }
    if access == Access::Read {
        out.push_str(":ro");
    }
    out
}

/// `path` with every link resolved, without the `\\?\` prefix `fs::canonicalize` puts on a Windows
/// drive path: sbx would mount `\\?\C:\data` at `/?/C:/data` instead of `/c/data`.
fn real_path(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(crate::sbx::plain_path)
}

/// Where sbx mounts a clone-mode sandbox's source, a target no folder may take.
pub const CLONE_SOURCE: &str = "/run/sandbox/source";

/// The folders one sandbox mounts beside `plan`'s, each `PATH`, `PATH:ro`, `PATH:TARGET` or
/// `PATH:TARGET:ro`, with TARGET an absolute path inside the sandbox. Each is checked like the
/// host's list ([`plan`]: it must exist, be a folder outside the workspace and pass
/// [`crate::model::mount_refusal`]), must not be one `plan` mounts already, and its target must
/// follow sbx's rules: absolute, not `/`, none twice, not the workspace or a path inside it, not
/// [`CLONE_SOURCE`], not where another folder is mounted. `~/` is the home folder. A problem is one
/// message per entry.
pub fn extras_of<'a>(
    entries: impl IntoIterator<Item = &'a str>,
    plan: &Plan,
    home: Option<&Path>,
) -> (Vec<Folder>, Vec<String>) {
    let (workspace, planned) = (plan.workspace.as_path(), plan.folders.as_slice());
    let mut folders: Vec<Folder> = Vec::new();
    let mut problems = Vec::new();
    let workspace_real = real_path(workspace).ok();
    let home_real = home.map(|h| real_path(h).unwrap_or_else(|_| h.to_path_buf()));
    let in_workspace = |p: &str| {
        let p = Path::new(p);
        !workspace.as_os_str().is_empty()
            && (p.starts_with(workspace)
                || workspace_real.as_ref().is_some_and(|w| p.starts_with(w)))
    };
    for entry in entries.into_iter().map(str::trim).filter(|e| !e.is_empty()) {
        let (path, target, access) = entry_parts(entry);
        let target = target.map(str::to_string);
        let expanded = match (path.strip_prefix("~/"), home) {
            (Some(below), Some(home)) => home.join(below),
            _ => PathBuf::from(path),
        };
        let problem = |why: &str| format!("“{entry}”: {why}");
        if !expanded.is_absolute() {
            problems.push(problem("give the host folder as an absolute path or ~/…"));
            continue;
        }
        let Ok(real) = real_path(&expanded) else {
            problems.push(problem("the folder does not exist"));
            continue;
        };
        if !real.is_dir() {
            problems.push(problem("it is not a folder"));
            continue;
        }
        if workspace_real
            .as_ref()
            .is_some_and(|ws| real.starts_with(ws))
        {
            problems.push(problem("it is in the workspace, which is mounted already"));
            continue;
        }
        if let Some(why) =
            (home_real.as_deref()).and_then(|h| crate::model::mount_refusal(&real, h))
        {
            problems.push(problem(&why));
            continue;
        }
        if planned.iter().any(|f| f.path == real) {
            problems.push(problem("it is a host folder every sandbox mounts already"));
            continue;
        }
        if folders.iter().any(|f| f.path == real && f.target == target) {
            problems.push(problem("listed twice"));
            continue;
        }
        let at = target
            .clone()
            .unwrap_or_else(|| crate::sbx::sandbox_path(&real));
        let taken = |f: &Folder| f.sandbox_path() == at;
        let why = match &target {
            Some(t) if t.trim_end_matches('/').is_empty() => Some("the target cannot be /"),
            Some(t) if t.contains('\0') => Some("the target holds a NUL character"),
            Some(t) if in_workspace(t) => Some("the target is the workspace or inside it"),
            Some(t) if t == CLONE_SOURCE || t.starts_with(&format!("{CLONE_SOURCE}/")) => {
                Some("the target is where clone mode keeps its source")
            }
            _ if planned.iter().any(taken) || folders.iter().any(taken) => {
                Some("another folder is mounted at that path in the sandbox")
            }
            _ => None,
        };
        if let Some(why) = why {
            problems.push(problem(why));
            continue;
        }
        folders.push(Folder {
            path: real,
            access,
            purpose: String::new(),
            origin: Origin::Wizard,
            target,
        });
    }
    (folders, problems)
}

/// The folders a sandbox on `workspace` mounts, and why any listed folder is left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The workspace the plan is for; empty for a mountless sandbox.
    pub workspace: PathBuf,
    pub folders: Vec<Folder>,
    pub notes: Vec<String>,
}

impl Plan {
    pub fn sbx_specs(&self) -> Vec<String> {
        self.folders.iter().map(Folder::sbx_spec).collect()
    }
}

/// The folders for a sandbox on `workspace`: those its `companion.json` names (the public repo
/// read-write, each related repo with its own access), then the host's list. A folder that is
/// missing, is the workspace or inside it, or that [`crate::model::mount_refusal`] refuses is left
/// out with a note; a folder listed twice is mounted once, read-write if either says so. A
/// mountless sandbox (an empty `workspace`) reads no `companion.json`.
pub fn plan(workspace: &Path, host: &[Entry], home: Option<&Path>) -> Plan {
    let mut wanted: Vec<(PathBuf, Access, String, Origin)> = Vec::new();
    let mut notes = Vec::new();
    if !workspace.as_os_str().is_empty() {
        match companion(workspace) {
            Ok(Some(c)) => {
                let at = |p: &str| workspace.join(p);
                if let Some(public) = c.public.filter(|p| !p.trim().is_empty()) {
                    let purpose = PUBLIC_PURPOSE.to_string();
                    wanted.push((at(&public), Access::Write, purpose, Origin::Companion));
                }
                for r in c.related {
                    wanted.push((
                        at(&r.path),
                        r.access,
                        one_line(&r.purpose),
                        Origin::Companion,
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => notes.push(format!("{e:#}")),
        }
    }
    for e in host {
        wanted.push((e.path.clone(), e.access, one_line(&e.purpose), Origin::Host));
    }
    let workspace_real = real_path(workspace).ok();
    let home_real = home.map(|h| real_path(h).unwrap_or_else(|_| h.to_path_buf()));
    let mut folders: Vec<Folder> = Vec::new();
    for (path, access, purpose, origin) in wanted {
        let shown = path.display().to_string();
        let Ok(real) = real_path(&path) else {
            notes.push(format!("{shown}: not mounted, the folder does not exist"));
            continue;
        };
        if !real.is_dir() {
            notes.push(format!("{shown}: not mounted, it is not a folder"));
            continue;
        }
        if workspace_real
            .as_ref()
            .is_some_and(|ws| real.starts_with(ws))
        {
            notes.push(format!(
                "{shown}: not mounted again, it is in the workspace"
            ));
            continue;
        }
        if let Some(why) =
            (home_real.as_deref()).and_then(|h| crate::model::mount_refusal(&real, h))
        {
            notes.push(format!("{shown}: not mounted, {why}"));
            continue;
        }
        match folders.iter_mut().find(|f| f.path == real) {
            Some(f) => {
                f.access = f.access.max(access);
                if f.purpose.is_empty() {
                    f.purpose = purpose;
                }
            }
            None => folders.push(Folder {
                path: real,
                access,
                purpose,
                origin,
                target: None,
            }),
        }
    }
    Plan {
        workspace: workspace.to_path_buf(),
        folders,
        notes,
    }
}

/// The workspace's `companion.json`, `None` without one.
fn companion(workspace: &Path) -> Result<Option<Companion>> {
    let path = workspace.join(COMPANION_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let parsed = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} does not parse; no folder of it is mounted",
            path.display()
        )
    })?;
    Ok(Some(parsed))
}

fn one_line(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ")
}

/// The extra workspace that mounts the workspace's `.git` folder read-only, so nothing in the
/// sandbox can change the repo's hooks, config or history; `None` where the workspace has no
/// `.git` folder.
pub fn git_guard(workspace: &Path) -> Option<String> {
    if workspace.as_os_str().is_empty() {
        return None;
    }
    let git = workspace.join(".git");
    git.is_dir().then(|| format!("{}:ro", git.display()))
}

/// Whether a sandbox's read-only mounts, as `sbx ls` reports them, carry its workspace's
/// [`git_guard`].
pub fn has_git_guard(workspace: &Path, ro_mounts: &[PathBuf]) -> bool {
    !workspace.as_os_str().is_empty() && ro_mounts.iter().any(|m| *m == workspace.join(".git"))
}

// --- the generated kit -------------------------------------------------------------------------

/// Where the generated kits live; under the test guard, a scratch folder.
pub fn kits_dir() -> PathBuf {
    if cfg!(any(test, feature = "test-support")) {
        std::env::temp_dir().join("aisbx-tests-folders")
    } else {
        crate::state::state_dir().join("folders")
    }
}

pub fn kit_dir(sandbox: &str) -> PathBuf {
    kits_dir().join(format!("{KIT_PREFIX}{}", crate::models::slug(sandbox)))
}

/// Is this kit directory name a generated folder kit?
pub fn is_kit(dir_name: &str) -> bool {
    dir_name
        .strip_prefix(KIT_PREFIX)
        .is_some_and(|rest| !rest.is_empty())
}

/// Write the kit that tells the agent of `sandbox` about its folders and its read-only `.git`,
/// and return the directory to pass to `--kit`; `None`, and no kit, when there is nothing to tell.
pub fn ensure_kit(sandbox: &str, folders: &[Folder], git_guard: bool) -> Result<Option<PathBuf>> {
    if folders.is_empty() && !git_guard {
        remove_kit(sandbox)?;
        return Ok(None);
    }
    let dir = kit_dir(sandbox);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating the folder kit directory for {sandbox}"))?;
    fs::write(
        crate::kits::descriptor_path(&dir),
        descriptor(sandbox, folders, git_guard),
    )
    .with_context(|| format!("writing the folder kit for {sandbox}"))?;
    Ok(Some(dir))
}

/// Drop a sandbox's generated kit. A missing directory is not an error.
pub fn remove_kit(sandbox: &str) -> Result<()> {
    let dir = kit_dir(sandbox);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
    }
}

/// What the agent reads: one line per folder, then the `.git` rule.
pub fn context(folders: &[Folder], git_guard: bool) -> String {
    let mut out = String::new();
    if !folders.is_empty() {
        out.push_str(
            "Host folders are mounted in this sandbox beside the workspace, each at its own host \
             path unless another one is named. Look there for what each one is for; never write \
             into a read-only one:\n\n",
        );
        for f in folders {
            let purpose = match f.purpose.as_str() {
                "" => String::new(),
                p => format!(": {p}"),
            };
            let from = match &f.target {
                Some(_) => format!(", the host's `{}`", f.path.display()),
                None => String::new(),
            };
            out.push_str(&format!(
                "- `{}` ({}{from}){purpose}\n",
                f.sandbox_path(),
                f.access.label()
            ));
        }
    }
    if git_guard {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            "The workspace's `.git` folder is mounted read-only: `git status`, `git log` and \
             `git diff` work, but a commit, a checkout or any other write to `.git` fails, also \
             with sudo. Leave commits to the user.\n",
        );
    }
    out
}

/// The kit's description, on one line: sbx lists it in the agent's context file itself, where
/// the kit's own file is read only when the agent thinks it relevant, so the folders are named
/// here. A list too long for one line names only the paths.
pub fn summary(folders: &[Folder], git_guard: bool) -> String {
    let mut parts = Vec::new();
    if !folders.is_empty() {
        let each = |with_purpose: bool| {
            let listed: Vec<String> = (folders.iter())
                .map(|f| match (with_purpose, f.purpose.as_str()) {
                    (true, p) if !p.is_empty() => {
                        format!(
                            "{} ({}): {}",
                            f.sandbox_path(),
                            f.access.label(),
                            p.trim_end_matches('.')
                        )
                    }
                    _ => format!("{} ({})", f.sandbox_path(), f.access.label()),
                })
                .collect();
            listed.join("; ")
        };
        let full = each(true);
        let listed = match full.chars().count() <= SUMMARY_MAX {
            true => full,
            false => format!("{}; see this kit's file for what each is for", each(false)),
        };
        parts.push(format!(
            "Host folders mounted beside the workspace: {listed}."
        ));
    }
    if git_guard {
        parts.push("The workspace's .git folder is read-only: leave commits to the user.".into());
    }
    parts.join(" ")
}

/// The longest folder list [`summary`] writes with the purposes.
const SUMMARY_MAX: usize = 900;

fn descriptor(sandbox: &str, folders: &[Folder], git_guard: bool) -> String {
    let content: String = context(folders, git_guard)
        .replace("${{", "$ {{")
        .lines()
        .map(|l| match l {
            "" => "\n".to_string(),
            l => format!("        {l}\n"),
        })
        .collect();
    let description = serde_json::Value::from(summary(folders, git_guard).replace("${{", "$ {{"));
    format!(
        "# syntax=docker/sandbox-kit:3\n\
         # Generated by aisbx - do not edit; rewritten whenever {sandbox} is built.\n\
         schemaVersion: \"3\"\n\
         kind: mixin\n\
         displayName: \"Host folders\"\n\
         description: {description}\n\
         \n\
         build: |\n\
         \x20 FROM scratch\n\
         \x20 ENV SBX_KIT_AISBX_FOLDERS=1\n\
         \n\
         capabilities:\n\
         \x20 - type: com.docker.sandbox/agent-context@1\n\
         \x20   config:\n\
         \x20     content: |\n\
         {content}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras(text: &str, plan: &Plan, home: Option<&Path>) -> (Vec<Folder>, Vec<String>) {
        extras_of(text.split(','), plan, home)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aisbx-tests-folders-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        real_path(&dir).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_planned_windows_folder_is_mounted_at_its_drive_path_not_a_verbatim_one() {
        let root = scratch("verbatim");
        let data = root.join("data");
        fs::create_dir_all(&data).unwrap();
        let host = [Entry {
            path: data.clone(),
            access: Access::Read,
            purpose: String::new(),
        }];
        let planned = plan(Path::new(""), &host, None);
        let folder = &planned.folders[0];
        assert!(
            !folder.sbx_spec().starts_with(r"\\?\"),
            "{}",
            folder.sbx_spec()
        );
        assert!(
            folder.sandbox_path().starts_with('/'),
            "{}",
            folder.sandbox_path()
        );
        assert!(
            !folder.sandbox_path().contains('?'),
            "{}",
            folder.sandbox_path()
        );
        let (extras, problems) = extras_of([data.to_str().unwrap()], &Plan::default(), None);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(
            !extras[0].sbx_spec().starts_with(r"\\?\"),
            "{}",
            extras[0].sbx_spec()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn switching_a_specs_access_keeps_its_path_and_target() {
        for (spec, access, want) in [
            ("/data", Access::Read, "/data:ro"),
            ("/data:ro", Access::Write, "/data"),
            ("/data:ro", Access::Read, "/data:ro"),
            ("/data:/mnt/d", Access::Read, "/data:/mnt/d:ro"),
            ("C:/data:ro", Access::Write, "C:/data"),
        ] {
            let got = spec_with_access(spec, access);
            assert_eq!(got, want, "{spec}");
            assert_eq!(spec_host(&got).1, access == Access::Read, "{spec}");
        }
    }

    fn entry(path: &Path, access: Access, purpose: &str) -> Entry {
        Entry {
            path: path.to_path_buf(),
            access,
            purpose: purpose.into(),
        }
    }

    #[test]
    fn the_host_list_round_trips_and_refuses_a_relative_path() {
        let state = scratch("list");
        let path = config_path(&state);
        assert_eq!(load(&path).unwrap(), Config::default());
        put(
            &state,
            entry(Path::new("/srv/docs"), Access::Read, "the docs"),
        )
        .unwrap();
        put(
            &state,
            entry(Path::new("/srv/out"), Access::Write, "output"),
        )
        .unwrap();
        put(
            &state,
            entry(Path::new("/srv/docs"), Access::Write, "the docs, writable"),
        )
        .unwrap();
        let config = load(&path).unwrap();
        assert_eq!(config.folders.len(), 2);
        assert_eq!(config.folders[0].access, Access::Write);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"access\": \"write\""), "{text}");
        assert!(put(&state, entry(Path::new("rel"), Access::Read, "")).is_err());
        assert!(replace(&state, 1, entry(Path::new("/srv/docs"), Access::Read, "")).is_err());
        replace(&state, 1, entry(Path::new("/srv/new"), Access::Read, "new")).unwrap();
        assert!(remove(&state, Path::new("/srv/new")).unwrap());
        assert!(!remove(&state, Path::new("/srv/new")).unwrap());
        assert_eq!(load(&path).unwrap().folders.len(), 1);
        fs::write(&path, "{not json").unwrap();
        assert!(put(&state, entry(Path::new("/x"), Access::Read, "")).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{not json",
            "left as it is"
        );
        fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn a_typed_path_expands_the_home_folder() {
        let home = Path::new("/home/me");
        assert_eq!(typed_path("~/docs", Some(home)), Ok(home.join("docs")));
        assert_eq!(typed_path(" /srv ", Some(home)), Ok(PathBuf::from("/srv")));
        assert!(typed_path("docs", Some(home)).is_err());
        assert!(typed_path("", Some(home)).is_err());
    }

    #[test]
    fn the_plan_mounts_the_companion_repos_and_the_host_list() {
        let root = scratch("plan");
        let ws = root.join("proj-dev");
        for d in ["proj-dev/.git", "proj", "kb", "guide", "shared"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(
            ws.join(COMPANION_FILE),
            r#"{"public": "../proj", "related": [
                {"path": "../kb", "purpose": "the knowledge base", "access": "write"},
                {"path": "../guide", "purpose": "coding\nguidelines"},
                {"path": "../gone", "purpose": "missing"},
                {"path": ".", "purpose": "itself"}]}"#,
        )
        .unwrap();
        let host = [
            entry(&root.join("shared"), Access::Read, "shared notes"),
            entry(&root.join("guide"), Access::Write, "again"),
        ];
        let plan = plan(&ws, &host, Some(Path::new("/nonexistent-home")));
        let got: Vec<(String, Access, &str, Origin)> = (plan.folders.iter())
            .map(|f| (f.sbx_spec(), f.access, f.purpose.as_str(), f.origin))
            .collect();
        let at = |d: &str| root.join(d).display().to_string();
        assert_eq!(
            got,
            [
                (at("proj"), Access::Write, PUBLIC_PURPOSE, Origin::Companion),
                (
                    at("kb"),
                    Access::Write,
                    "the knowledge base",
                    Origin::Companion
                ),
                (
                    at("guide"),
                    Access::Write,
                    "coding guidelines",
                    Origin::Companion
                ),
                (
                    format!("{}:ro", at("shared")),
                    Access::Read,
                    "shared notes",
                    Origin::Host
                ),
            ]
        );
        assert_eq!(plan.notes.len(), 2, "{:?}", plan.notes);
        assert!(plan.notes[0].contains("does not exist"));
        assert!(plan.notes[1].contains("in the workspace"));
        // A home folder or a credential place is never mounted.
        let kb = [entry(&root.join("kb"), Access::Read, "")];
        let refused = super::plan(&root.join("proj"), &kb, Some(&root.join("kb")));
        assert!(refused.folders.is_empty() && refused.notes[0].contains("home folder"));
        fs::write(ws.join(COMPANION_FILE), "{").unwrap();
        let broken = super::plan(&ws, &[], None);
        assert!(broken.folders.is_empty() && broken.notes[0].contains("does not parse"));
        assert!(super::plan(Path::new(""), &[], None).folders.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_git_guard_mounts_only_an_existing_git_folder() {
        let root = scratch("git");
        assert_eq!(git_guard(&root), None);
        fs::create_dir_all(root.join(".git")).unwrap();
        let spec = git_guard(&root).unwrap();
        assert_eq!(spec, format!("{}:ro", root.join(".git").display()));
        assert!(has_git_guard(&root, &[root.join(".git")]));
        assert!(!has_git_guard(&root, &[root.join("other")]));
        assert_eq!(git_guard(Path::new("")), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn own_folders_parse_with_their_targets_and_follow_sbxs_rules() {
        let root = std::env::temp_dir().join(format!("aisbx-extras-{}", std::process::id()));
        let (ws, data, docs, home) = (
            root.join("ws"),
            root.join("data"),
            root.join("docs"),
            root.join("home"),
        );
        for dir in [&ws, &data, &docs, &home, &ws.join("inner")] {
            fs::create_dir_all(dir).unwrap();
        }
        let real = |p: &Path| real_path(p).unwrap();
        let text = format!(
            "{}, {}:ro, {}:/workspace/data:ro",
            data.display(),
            docs.display(),
            docs.display()
        );
        let plan = |folders: Vec<Folder>| Plan {
            workspace: ws.clone(),
            folders,
            notes: Vec::new(),
        };
        let (folders, problems) = extras(&text, &plan(Vec::new()), Some(&home));
        assert!(problems.is_empty(), "{problems:?}");
        let specs: Vec<String> = folders.iter().map(Folder::sbx_spec).collect();
        assert_eq!(
            specs,
            [
                real(&data).display().to_string(),
                format!("{}:ro", real(&docs).display()),
                format!("{}:/workspace/data:ro", real(&docs).display()),
            ]
        );
        assert_eq!(folders[2].sandbox_path(), "/workspace/data");
        assert!(folders.iter().all(|f| f.origin == Origin::Wizard));

        let refused = |text: &str, planned: &[Folder]| {
            let (folders, problems) = extras(text, &plan(planned.to_vec()), Some(&home));
            assert!(folders.is_empty(), "{text}: {folders:?}");
            problems.join(" | ")
        };
        let at = |target: &str| format!("{}:{target}", data.display());
        let refused_last = |text: &str| {
            let (folders, problems) = extras(text, &plan(Vec::new()), Some(&home));
            assert_eq!(
                (folders.len(), problems.len()),
                (1, 1),
                "{text}: {problems:?}"
            );
            problems[0].clone()
        };
        assert!(refused("relative/path", &[]).contains("absolute"));
        assert!(refused(&format!("{}/nope", root.display()), &[]).contains("does not exist"));
        assert!(refused(&ws.join("inner").display().to_string(), &[]).contains("in the workspace"));
        assert!(refused(&home.display().to_string(), &[]).contains("home folder"));
        assert!(refused(&at("/"), &[]).contains("cannot be /"));
        let inside = at(&format!("{}/sub", ws.display()));
        assert!(refused(&inside, &[]).contains("workspace or inside it"));
        assert!(refused(&at(CLONE_SOURCE), &[]).contains("clone mode"));
        let planned = Folder {
            path: real(&docs),
            access: Access::Read,
            purpose: String::new(),
            origin: Origin::Host,
            target: None,
        };
        let docs_text = docs.display().to_string();
        assert!(
            refused(&docs_text, std::slice::from_ref(&planned)).contains("every sandbox mounts")
        );
        let clash = at(&real(&docs).display().to_string());
        assert!(refused(&clash, &[planned]).contains("another folder"));
        let at_docs = format!(
            "{}, {}",
            at(&real(&docs).display().to_string()),
            docs.display()
        );
        assert!(refused_last(&at_docs).contains("another folder is mounted at that path"));
        for twice in [
            format!("{0}:/t, {0}:/t", data.display()),
            format!("{0}, {0}", data.display()),
        ] {
            let (_, problems) = extras(&twice, &plan(Vec::new()), Some(&home));
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].ends_with(": listed twice"), "{problems:?}");
        }
        let tilde = extras("~/", &plan(Vec::new()), Some(&home)).1;
        assert!(tilde[0].contains("home folder"), "{tilde:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_entry_splits_at_the_targets_colon_and_keeps_a_drive_letter() {
        let (r, w) = (Access::Read, Access::Write);
        let cases = [
            ("/srv/a", ("/srv/a", None, w)),
            ("/srv/a:ro", ("/srv/a", None, r)),
            ("/srv/a:/t", ("/srv/a", Some("/t"), w)),
            ("~/a:/t/b:ro", ("~/a", Some("/t/b"), r)),
            ("/srv/a:relative", ("/srv/a:relative", None, w)),
            ("C:/data", ("C:/data", None, w)),
            ("C:/data:ro", ("C:/data", None, r)),
            (
                "C:/data:/workspace/data:ro",
                ("C:/data", Some("/workspace/data"), r),
            ),
            (r"C:\data:ro", (r"C:\data", None, r)),
        ];
        for (entry, want) in cases {
            assert_eq!(entry_parts(entry), want, "{entry}");
        }
    }

    #[test]
    fn the_kit_lists_each_folder_with_its_purpose_for_the_agent() {
        let folders = [
            Folder {
                path: PathBuf::from("/srv/kb"),
                access: Access::Write,
                purpose: "the knowledge base ${{ x }}".into(),
                origin: Origin::Companion,
                target: None,
            },
            Folder {
                path: PathBuf::from("/srv/docs"),
                access: Access::Read,
                purpose: String::new(),
                origin: Origin::Host,
                target: None,
            },
            Folder {
                path: PathBuf::from("/srv/data"),
                access: Access::Read,
                purpose: String::new(),
                origin: Origin::Wizard,
                target: Some("/workspace/data".into()),
            },
        ];
        let dir = ensure_kit("my.proj", &folders, true).unwrap().unwrap();
        assert!(is_kit(&dir.file_name().unwrap().to_string_lossy()));
        let text = fs::read_to_string(crate::kits::descriptor_path(&dir)).unwrap();
        let doc = crate::kits::load_descriptor(&text).unwrap();
        assert_eq!(doc["kind"].as_str(), Some("mixin"));
        let caps = &doc["capabilities"][0];
        assert_eq!(
            caps["type"].as_str(),
            Some("com.docker.sandbox/agent-context@1")
        );
        let content = caps["config"]["content"].as_str().unwrap();
        assert!(content.contains("- `/srv/kb` (read-write): the knowledge base $ {{ x }}\n"));
        assert!(content.contains("- `/srv/docs` (read-only)\n"));
        assert!(
            content.contains("- `/workspace/data` (read-only, the host's `/srv/data`)\n"),
            "a folder at its own target names where the sandbox sees it: {content}"
        );
        assert!(content.contains("`.git` folder is mounted read-only"));
        assert!(!text.contains("${{"));
        assert_eq!(
            doc["description"].as_str(),
            Some(
                "Host folders mounted beside the workspace: /srv/kb (read-write): the knowledge \
                 base $ {{ x }}; /srv/docs (read-only); /workspace/data (read-only). The \
                 workspace's .git folder is read-only: leave commits to the user."
            )
        );
        let many: Vec<Folder> = (0..40)
            .map(|i| Folder {
                path: PathBuf::from(format!("/srv/f{i}")),
                purpose: "a purpose long enough to push the list past one line".into(),
                ..folders[1].clone()
            })
            .collect();
        let short = summary(&many, false);
        assert!(!short.contains("a purpose") && short.contains("/srv/f39 (read-only)"));
        assert!(doc["build"].as_str().unwrap().contains("FROM scratch"));
        // Nothing to tell: no kit.
        assert_eq!(ensure_kit("my.proj", &[], false).unwrap(), None);
        assert!(!dir.exists());
    }

    #[test]
    #[ignore = "live: runs `sbx kit validate` on the generated kit"]
    fn live_sbx_validates_the_generated_kit() {
        let folders = [Folder {
            path: PathBuf::from("/srv/kb"),
            access: Access::Write,
            purpose: "the knowledge base: \"quoted\", $HOME and `code`".into(),
            origin: Origin::Companion,
            target: None,
        }];
        let dir = ensure_kit("aisbx-selftest-folders", &folders, true)
            .unwrap()
            .unwrap();
        let out = std::process::Command::new("sbx")
            .args(["kit", "validate", "--json"])
            .arg(&dir)
            .output()
            .expect("running `sbx kit validate`");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        println!("sbx kit validate {} ->\n{text}", dir.display());
        remove_kit("aisbx-selftest-folders").ok();
        assert!(out.status.success(), "{text}");
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(report["valid"], serde_json::json!(true), "{text}");
        assert_eq!(report["warnings"], serde_json::json!([]), "{text}");
    }
}

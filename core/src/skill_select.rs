//! Skills from the cached skill repos ([`skill_repos`]) in sandboxes: the selection file (which
//! skills each sandbox gets), [`discover`] (which skills a cached commit holds), the [`sync`] that
//! copies them in, and the change report of a fetch ([`fetch_report`]).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{sbx, skill_repos};

pub const FILE: &str = "skill-selection.json";
const VERSION: u32 = 1;

/// The sandbox side of a sync: `put` and `finish` (see the script's header).
pub const SCRIPT: &str = include_str!("repo_skills_sync.sh");

/// How deep below the repo root a skill folder may sit.
pub const MAX_DEPTH: usize = 4;
/// A skill whose blobs add up to more than this is skipped.
pub const MAX_SKILL_BYTES: u64 = 20 * 1024 * 1024;
/// The store's bookkeeping file, so never a skill name.
const RESERVED: &str = "installed";

// --- the selection -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub version: u32,
    /// Every sandbox's picks, merged with its own at each sync ([`effective`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global: Vec<Pick>,
    #[serde(default)]
    pub sandboxes: BTreeMap<String, Vec<Pick>>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            version: VERSION,
            global: Vec::new(),
            sandboxes: BTreeMap::new(),
        }
    }
}

/// One repo's part of a sandbox's selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    /// A [`skill_repos`] id.
    pub repo: String,
    pub skills: Skills,
}

/// `"all"` — whatever the cached commit holds at sync time — or a list of names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SkillsRepr", into = "SkillsRepr")]
pub enum Skills {
    All,
    Named(Vec<String>),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SkillsRepr {
    Word(String),
    Names(Vec<String>),
}

impl TryFrom<SkillsRepr> for Skills {
    type Error = String;

    fn try_from(raw: SkillsRepr) -> Result<Self, String> {
        match raw {
            SkillsRepr::Word(w) if w == "all" => Ok(Skills::All),
            SkillsRepr::Word(_) => Err("skills is \"all\" or a list of names".into()),
            SkillsRepr::Names(names) => Ok(Skills::Named(names)),
        }
    }
}

impl From<Skills> for SkillsRepr {
    fn from(skills: Skills) -> Self {
        match skills {
            Skills::All => SkillsRepr::Word("all".into()),
            Skills::Named(names) => SkillsRepr::Names(names),
        }
    }
}

pub fn selection_path(state: &Path) -> PathBuf {
    state.join(FILE)
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, and not the store's `installed` file.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && name != RESERVED
}

/// A sandbox name as sbx spells them (letters, digits, `.`, `-`), as a key of the file.
fn valid_sandbox(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// The selection. A missing file is an empty selection; a file that does not parse, has another
/// `version` or an entry this build refuses is an error naming the file.
pub fn load(path: &Path) -> Result<Selection> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Selection::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let selection: Selection = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} does not parse; fix or remove it (it is left as it is)",
            path.display()
        )
    })?;
    if selection.version != VERSION {
        bail!(
            "{} has version {}, this aisbx reads version {VERSION}; it is left as it is",
            path.display(),
            selection.version
        );
    }
    if let Err(why) = check_picks(GLOBAL, &selection.global) {
        bail!("{}: {why}; it is left as it is", path.display());
    }
    for (sandbox, picks) in &selection.sandboxes {
        if let Err(why) = check_entry(sandbox, picks) {
            bail!("{}: {why}; it is left as it is", path.display());
        }
    }
    Ok(selection)
}

/// How messages name the `global` list.
const GLOBAL: &str = "global";

fn check_entry(sandbox: &str, picks: &[Pick]) -> Result<(), String> {
    if !valid_sandbox(sandbox) {
        return Err("a key is not a sandbox name".into());
    }
    check_picks(sandbox, picks)
}

fn check_picks(sandbox: &str, picks: &[Pick]) -> Result<(), String> {
    let mut repos: Vec<&str> = Vec::new();
    for pick in picks {
        if !skill_repos::is_id(&pick.repo) {
            return Err(format!("{sandbox}: a repo is not a skill-repo id"));
        }
        if repos.contains(&pick.repo.as_str()) {
            return Err(format!("{sandbox}: {} is listed twice", pick.repo));
        }
        repos.push(&pick.repo);
        if let Skills::Named(names) = &pick.skills {
            if names.is_empty() {
                return Err(format!("{sandbox}: {} lists no skills", pick.repo));
            }
            for (i, name) in names.iter().enumerate() {
                if !valid_name(name) {
                    return Err(format!("{sandbox}: {} names a bad skill name", pick.repo));
                }
                if names[..i].contains(name) {
                    return Err(format!("{sandbox}: {} lists {name} twice", pick.repo));
                }
            }
        }
    }
    Ok(())
}

/// Change the selection under an exclusive lock on `<path>.lock`: load, apply `f`, then — only
/// when the document changed — write through a temp file and a rename. A selection that does not
/// load is not written.
pub fn update<T>(path: &Path, f: impl FnOnce(&mut Selection) -> Result<T>) -> Result<T> {
    let dir = path
        .parent()
        .context("the skill selection path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _lock = skill_repos::lock_file(&path.with_extension("json.lock"))?;
    let mut selection = load(path)?;
    let before = selection.clone();
    let out = f(&mut selection)?;
    if selection == before {
        return Ok(out);
    }
    selection.sandboxes.retain(|_, picks| !picks.is_empty());
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, serde_json::to_vec_pretty(&selection)?)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map(|()| out)
}

/// The picks a sync of the sandbox takes: the global ones, then the sandbox's own merged in — per
/// repo `all` wins and named lists are unioned (global names first). Pure.
pub fn effective(sel: &Selection, sandbox: &str) -> Vec<Pick> {
    let mut out = sel.global.clone();
    for own in sel.sandboxes.get(sandbox).into_iter().flatten() {
        let Some(pick) = out.iter_mut().find(|p| p.repo == own.repo) else {
            out.push(own.clone());
            continue;
        };
        match (&mut pick.skills, &own.skills) {
            (Skills::All, _) => {}
            (skills, Skills::All) => *skills = Skills::All,
            (Skills::Named(names), Skills::Named(more)) => {
                for n in more {
                    if !names.contains(n) {
                        names.push(n.clone());
                    }
                }
            }
        }
    }
    out
}

/// Whether the sandbox has a non-empty [`effective`] selection (any global pick counts). An
/// unreadable file counts too, so the sync runs and reports it instead of being skipped.
pub fn has_selection(state: &Path, sandbox: &str) -> bool {
    let path = selection_path(state);
    if !path.exists() {
        return false;
    }
    match load(&path) {
        Ok(selection) => !effective(&selection, sandbox).is_empty(),
        Err(_) => true,
    }
}

/// Drop the sandbox's selection (its delete, or a new sandbox of the same name); `global` stays.
pub fn forget(state: &Path, sandbox: &str) -> Result<()> {
    let path = selection_path(state);
    if !path.exists() {
        return Ok(());
    }
    update(&path, |selection| {
        selection.sandboxes.remove(sandbox);
        Ok(())
    })
}

// --- specs ---------------------------------------------------------------------------------------

/// `REPO` (all its skills) or `REPO#SKILL`; `REPO` is an id or a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub repo: String,
    pub skill: Option<String>,
}

/// Check a spec's spelling. No message quotes a refused repo: it may be a pasted token.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let (repo, skill) = match raw.split_once('#') {
        Some((repo, skill)) => (repo, Some(skill)),
        None => (raw, None),
    };
    if repo.is_empty() {
        return Err("a skill spec is REPO or REPO#SKILL, with REPO an id or a source".into());
    }
    if !skill_repos::is_id(repo) && skill_repos::parse_source(repo).is_err() {
        return Err("a skill spec's repo is neither a skill-repo id nor an accepted source".into());
    }
    if let Some(skill) = skill {
        if !valid_name(skill) {
            return Err(format!(
                "{} is not a skill name: a-z, 0-9, . _ -, at most 64, starting with a letter or digit",
                skill.escape_debug()
            ));
        }
    }
    Ok(Spec {
        repo: repo.to_string(),
        skill: skill.map(str::to_string),
    })
}

/// A command refused before anything was written (exit 2), or one that failed (exit 1).
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Refused(String),
    Failed(String),
}

fn failed(e: anyhow::Error) -> Error {
    Error::Failed(format!("{e:#}"))
}

/// The configured repo a spec's handle names, as its id.
fn resolve(config: &skill_repos::Config, handle: &str) -> Result<String, Error> {
    match skill_repos::matching(config, handle).as_slice() {
        [] => Err(Error::Refused(format!(
            "no configured skill repo is {handle}; add it with `aisbx skill-repo add`"
        ))),
        [i] => config.repos[*i].id().map_err(Error::Refused),
        more => Err(Error::Refused(format!(
            "{handle} matches {} configured skill repos (one per ref); name one by its id",
            more.len()
        ))),
    }
}

// --- discovery -----------------------------------------------------------------------------------

/// A skill in a cached commit: its name, its folder's path in the repo and its content id — the
/// git tree object id of that folder, so an untouched skill keeps it from commit to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub path: String,
    pub content: String,
}

/// A skill folder left out, and why: `bad-name`, `bad-path`, `symlink`, `submodule`, `too-big` or
/// `dup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub why: &'static str,
}

impl Skipped {
    /// One line, the name escaped (it may hold anything git allows in a path).
    pub fn line(&self, repo: &str) -> String {
        format!("{repo}: skipped {}: {}", self.name.escape_debug(), self.why)
    }
}

struct TreeEntry {
    mode: String,
    object: String,
    size: u64,
    path: String,
}

/// The mode git gives a folder.
const TREE_MODE: &str = "040000";

/// `git ls-tree -r -t -l -z` output: `<mode> <type> <object> <size>\t<path>\0` (a folder's size
/// is `-`, read as 0).
fn parse_tree(raw: &[u8]) -> Vec<TreeEntry> {
    raw.split(|b| *b == 0)
        .filter_map(|record| {
            let tab = record.iter().position(|b| *b == b'\t')?;
            let meta = String::from_utf8_lossy(&record[..tab]);
            let mut fields = meta.split_whitespace();
            let mode = fields.next()?.to_string();
            let object = fields.nth(1)?.to_string();
            let size = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let path = String::from_utf8_lossy(&record[tab + 1..]).into_owned();
            Some(TreeEntry {
                mode,
                object,
                size,
                path,
            })
        })
        .collect()
}

/// The entries below a folder (`prefix` ends in `/`).
fn under<'a>(
    entries: &'a [TreeEntry],
    prefix: &'a str,
) -> impl Iterator<Item = &'a TreeEntry> + 'a {
    entries.iter().filter(move |e| e.path.starts_with(prefix))
}

/// The skills of a tree (see [`discover`] for the rules), in path order.
fn skills_in(entries: &[TreeEntry]) -> (Vec<Found>, Vec<Skipped>) {
    let mut dirs: Vec<&str> = entries
        .iter()
        .filter(|e| e.mode == "100644" || e.mode == "100755")
        .filter_map(|e| e.path.strip_suffix("/SKILL.md"))
        .filter(|dir| {
            let parts: Vec<&str> = dir.split('/').collect();
            parts.len() <= MAX_DEPTH && !parts.iter().any(|p| p.starts_with('.'))
        })
        .collect();
    dirs.sort_unstable();
    dirs.dedup();
    let inside_another = |dir: &str| {
        dirs.iter()
            .any(|outer| dir.len() > outer.len() && dir.starts_with(&format!("{outer}/")))
    };
    let mut found: Vec<Found> = Vec::new();
    let mut skipped = Vec::new();
    for dir in dirs.iter().copied() {
        if inside_another(dir) {
            continue;
        }
        let name = dir.rsplit('/').next().unwrap_or(dir).to_string();
        let prefix = format!("{dir}/");
        let why = if !valid_name(&name) {
            Some("bad-name")
        } else if dir
            .chars()
            .any(|c| c.is_control() || c == char::REPLACEMENT_CHARACTER)
        {
            Some("bad-path")
        } else if under(entries, &prefix).any(|e| e.mode == "120000") {
            Some("symlink")
        } else if under(entries, &prefix).any(|e| e.mode == "160000") {
            Some("submodule")
        } else if under(entries, &prefix).map(|e| e.size).sum::<u64>() > MAX_SKILL_BYTES {
            Some("too-big")
        } else if found.iter().any(|f| f.name == name) {
            Some("dup")
        } else {
            None
        };
        match why {
            Some(why) => skipped.push(Skipped { name, why }),
            None => found.push(Found {
                name,
                path: dir.to_string(),
                content: entries
                    .iter()
                    .find(|e| e.mode == TREE_MODE && e.path == dir)
                    .map(|e| e.object.clone())
                    .unwrap_or_default(),
            }),
        }
    }
    (found, skipped)
}

/// The skills of `commit` in the cache at `cache_dir`, from git's committed tree, never the
/// working tree: folders with a regular `SKILL.md`, at most [`MAX_DEPTH`] levels down, outside dot
/// folders and other skills, named after the folder. `-t` lists the folders for the content ids.
pub fn discover(cache_dir: &Path, commit: &str) -> Result<(Vec<Found>, Vec<Skipped>)> {
    if commit.is_empty() || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a commit id");
    }
    let out = skill_repos::local_git(cache_dir, &["ls-tree", "-r", "-t", "-l", "-z", commit])
        .stderr(Stdio::null())
        .output()
        .context("running git ls-tree")?;
    if !out.status.success() {
        bail!("git cannot list the cached commit");
    }
    Ok(skills_in(&parse_tree(&out.stdout)))
}

/// A repo's cache as git reads it now.
pub struct Cached {
    pub dir: PathBuf,
    pub commit: String,
    pub found: Vec<Found>,
    pub skipped: Vec<Skipped>,
}

impl Cached {
    fn names(&self) -> Vec<String> {
        self.found.iter().map(|f| f.name.clone()).collect()
    }
}

/// The repo's cache, or why there is none git can read — with the fetch hint. Never fetches.
pub fn read_cache(state: &Path, id: &str) -> Result<Cached, String> {
    let hint = format!("run `aisbx skill-repo fetch {id}`");
    let dir = skill_repos::cache_root(state).join(id);
    let Some((commit, _)) = skill_repos::cached_commit(&dir) else {
        return Err(format!("{id} has no cache git can read; {hint}"));
    };
    let (found, skipped) = discover(&dir, &commit).map_err(|e| format!("{id}: {e:#}; {hint}"))?;
    Ok(Cached {
        dir,
        commit,
        found,
        skipped,
    })
}

/// The names a cached repo gives now; none without a readable cache.
fn cached_names(state: &Path, id: &str) -> Vec<String> {
    read_cache(state, id).map(|c| c.names()).unwrap_or_default()
}

// --- the change report ---------------------------------------------------------------------------

/// A skill a fetch took away: gone from the repo, or still there but now skipped (and why).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Removed {
    pub name: String,
    pub skipped: Option<&'static str>,
}

/// What a fetch changed in a repo's skills, keyed by name; every list sorted by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillChanges {
    pub added: Vec<String>,
    pub removed: Vec<Removed>,
    pub changed: Vec<String>,
}

impl SkillChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// `+a,b  -c (skipped: too-big)  ~d`, names escaped as in [`Skipped::line`]; `None` when
    /// nothing changed.
    pub fn summary(&self) -> Option<String> {
        let removed: Vec<String> = self
            .removed
            .iter()
            .map(|r| match r.skipped {
                Some(why) => format!("{} (skipped: {why})", r.name.escape_debug()),
                None => r.name.escape_debug().to_string(),
            })
            .collect();
        let parts: Vec<String> = [
            ("+", escaped(&self.added)),
            ("-", removed),
            ("~", escaped(&self.changed)),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(sign, names)| format!("{sign}{}", names.join(",")))
        .collect();
        (!parts.is_empty()).then(|| parts.join("  "))
    }
}

fn escaped(names: &[String]) -> Vec<String> {
    names.iter().map(|n| n.escape_debug().to_string()).collect()
}

/// Compare a repo's skills before a fetch with those after it: new names are `added`, names gone
/// (or now skipped, with the reason) `removed`, and a name whose content id moved `changed`.
/// Without a cache before, every skill is `added`.
pub fn diff(before: Option<&Cached>, after: &Cached) -> SkillChanges {
    let mut changes = SkillChanges::default();
    let old: &[Found] = before.map(|b| b.found.as_slice()).unwrap_or_default();
    for f in &after.found {
        match old.iter().find(|o| o.name == f.name) {
            None => changes.added.push(f.name.clone()),
            Some(o) if o.content != f.content => changes.changed.push(f.name.clone()),
            Some(_) => {}
        }
    }
    for o in old {
        if !after.found.iter().any(|f| f.name == o.name) {
            changes.removed.push(Removed {
                name: o.name.clone(),
                skipped: after
                    .skipped
                    .iter()
                    .find(|s| s.name == o.name)
                    .map(|s| s.why),
            });
        }
    }
    changes.added.sort();
    changes.changed.sort();
    changes.removed.sort();
    changes
}

/// Whether a pick takes this name of its repo: `all` takes every name, a list its own names. The
/// one rule the sync ([`plan`]) and the fetch report ([`forecast`]) share.
pub fn selects(skills: &Skills, name: &str) -> bool {
    match skills {
        Skills::All => true,
        Skills::Named(names) => names.iter().any(|n| n == name),
    }
}

/// What a sandbox's next start does with a repo's changes; every list sorted by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forecast {
    pub installs: Vec<String>,
    pub updates: Vec<String>,
    pub removes: Vec<String>,
    /// Kept as installed (auto-update off): `aisbx skill sync NAME` moves them.
    pub held: Vec<String>,
}

impl Forecast {
    /// One line, such as `box: next start installs a,b; held c,d (run …)`, the hint naming
    /// `aisbx skill sync box`.
    pub fn line(&self, sandbox: &str) -> String {
        let mut parts = Vec::new();
        for (verb, names) in [
            ("installs", &self.installs),
            ("updates", &self.updates),
            ("removes", &self.removes),
        ] {
            if !names.is_empty() {
                parts.push(format!("{verb} {}", names.join(",")));
            }
        }
        if !self.held.is_empty() {
            parts.push(format!(
                "held {} (run `aisbx skill sync {sandbox}` to update)",
                self.held.join(",")
            ));
        }
        format!("{sandbox}: next start {}", parts.join("; "))
    }
}

/// A sandbox's next start, for a pick of the changed repo — `None` when the pick selects none of
/// the changed names. With auto-update off, new skills still arrive and the rest is held. A
/// forecast from the selection only: the sandbox's own `installed` file decides at the sync.
pub fn forecast(skills: &Skills, changes: &SkillChanges, auto_update: bool) -> Option<Forecast> {
    let picked = |names: Vec<&String>| -> Vec<String> {
        names
            .into_iter()
            .filter(|n| selects(skills, n))
            .cloned()
            .collect()
    };
    let installs = picked(changes.added.iter().collect());
    let changed = picked(changes.changed.iter().collect());
    let removed = picked(changes.removed.iter().map(|r| &r.name).collect());
    if installs.is_empty() && changed.is_empty() && removed.is_empty() {
        return None;
    }
    Some(if auto_update {
        Forecast {
            installs,
            updates: changed,
            removes: removed,
            held: Vec::new(),
        }
    } else {
        let mut held = changed;
        held.extend(removed);
        held.sort();
        Forecast {
            installs,
            held,
            ..Forecast::default()
        }
    })
}

/// The sandboxes whose selection a repo's changes touch, by name, each with its [`forecast`].
/// Reads the selection file only.
pub fn affected(
    state: &Path,
    id: &str,
    changes: &SkillChanges,
    auto_update: bool,
) -> Result<Vec<(String, Forecast)>> {
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let selection = load(&selection_path(state))?;
    Ok(selection
        .sandboxes
        .keys()
        .filter_map(|sandbox| {
            let picks = effective(&selection, sandbox);
            let pick = picks.iter().find(|p| p.repo == id)?;
            forecast(&pick.skills, changes, auto_update).map(|f| (sandbox.clone(), f))
        })
        .collect())
}

/// How a fetch went for one entry, as `skill-repo fetch --json` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStatus {
    Unchanged,
    Updated,
    Cloned,
    Failed,
}

impl FetchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FetchStatus::Unchanged => "unchanged",
            FetchStatus::Updated => "updated",
            FetchStatus::Cloned => "cloned",
            FetchStatus::Failed => "failed",
        }
    }
}

/// One entry's fetch: its commits before and after, the skill changes and who they reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchReport {
    pub id: String,
    pub old_commit: Option<String>,
    pub commit: Option<String>,
    pub status: FetchStatus,
    pub changes: SkillChanges,
    pub sandboxes: Vec<(String, Forecast)>,
}

impl FetchReport {
    /// The lines `skill-repo fetch` adds to its per-entry line on stderr: one summary, then one
    /// per affected sandbox.
    pub fn lines(&self) -> Vec<String> {
        let id = &self.id;
        let short = |c: &Option<String>| c.as_deref().map_or("-", skill_repos::short).to_string();
        let summary = self.changes.summary();
        let head = match self.status {
            FetchStatus::Failed => return Vec::new(),
            FetchStatus::Unchanged => format!("{id}: unchanged"),
            FetchStatus::Cloned => format!(
                "{id}: new cache at {}: {}",
                short(&self.commit),
                summary.as_deref().unwrap_or("no skills")
            ),
            FetchStatus::Updated => format!(
                "{id}: {} → {}: {}",
                short(&self.old_commit),
                short(&self.commit),
                summary.as_deref().unwrap_or("no skill changed")
            ),
        };
        let mut lines = vec![head];
        lines.extend(
            self.sandboxes
                .iter()
                .map(|(name, f)| format!("{id}:   {}", f.line(name))),
        );
        lines
    }

    pub fn to_json(&self) -> serde_json::Value {
        let sandboxes: Vec<serde_json::Value> = self
            .sandboxes
            .iter()
            .map(|(name, f)| {
                serde_json::json!({
                    "name": name,
                    "installs": f.installs,
                    "updates": f.updates,
                    "removes": f.removes,
                    "held": f.held,
                })
            })
            .collect();
        let removed: Vec<serde_json::Value> = self
            .changes
            .removed
            .iter()
            .map(|r| serde_json::json!({ "name": r.name, "skipped": r.skipped }))
            .collect();
        serde_json::json!({
            "id": self.id,
            "old_commit": self.old_commit,
            "commit": self.commit,
            "status": self.status.as_str(),
            "added": self.changes.added,
            "removed": removed,
            "changed": self.changes.changed,
            "sandboxes": sandboxes,
        })
    }
}

/// Report one entry's fetch: `before` is its cache read just before the fetch, `fetched` whether
/// the fetch worked; the cache is read again here. Warnings (an unreadable selection or cache)
/// are handed back for the caller to print. Reads files and git only.
pub fn fetch_report(
    state: &Path,
    id: &str,
    before: Option<&Cached>,
    fetched: bool,
    auto_update: bool,
) -> (FetchReport, Vec<String>) {
    let mut report = FetchReport {
        id: id.to_string(),
        old_commit: before.map(|b| b.commit.clone()),
        commit: None,
        status: FetchStatus::Failed,
        changes: SkillChanges::default(),
        sandboxes: Vec::new(),
    };
    let mut warnings = Vec::new();
    if !fetched {
        return (report, warnings);
    }
    let after = match read_cache(state, id) {
        Ok(after) => after,
        Err(why) => {
            warnings.push(format!("{why}; no skill report"));
            return (report, warnings);
        }
    };
    report.commit = Some(after.commit.clone());
    report.status = match before {
        None => FetchStatus::Cloned,
        Some(b) if b.commit == after.commit => FetchStatus::Unchanged,
        Some(_) => FetchStatus::Updated,
    };
    if report.status == FetchStatus::Unchanged {
        return (report, warnings);
    }
    report.changes = diff(before, &after);
    match affected(state, id, &report.changes, auto_update) {
        Ok(sandboxes) => report.sandboxes = sandboxes,
        Err(e) => warnings.push(format!("{e:#}; no sandboxes named")),
    }
    (report, warnings)
}

/// `skill-repo fetch --json`: the switch and one element per fetched entry.
pub fn fetch_json(auto_update: bool, reports: &[FetchReport]) -> serde_json::Value {
    let repos: Vec<serde_json::Value> = reports.iter().map(FetchReport::to_json).collect();
    serde_json::json!({ "auto_update": auto_update, "repos": repos })
}

// --- add, rm, replace ----------------------------------------------------------------------------

/// `(repo id, skill)` pairs; `None` is all of the repo.
type Wanted = Vec<(String, Option<String>)>;

/// Resolve and check specs against the configured repos and their caches: a repo that is not
/// configured or is ambiguous, a malformed spec, and a named skill the current cache lacks are
/// refused. Hands back `(id, skill)` pairs in order, plus notes for the caller to print.
fn checked(state: &Path, raw: &[String]) -> Result<(Wanted, Vec<String>), Error> {
    let specs = raw
        .iter()
        .map(|s| parse_spec(s).map_err(Error::Refused))
        .collect::<Result<Vec<_>, _>>()?;
    let config = skill_repos::load(&skill_repos::config_path(state)).map_err(failed)?;
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for spec in specs {
        let id = resolve(&config, &spec.repo)?;
        match &spec.skill {
            Some(skill) => {
                let cached = read_cache(state, &id).map_err(Error::Refused)?;
                if !cached.found.iter().any(|f| &f.name == skill) {
                    let why = cached
                        .skipped
                        .iter()
                        .find(|s| &s.name == skill)
                        .map(|s| format!(" (skipped: {})", s.why))
                        .unwrap_or_default();
                    return Err(Error::Refused(format!(
                        "{id} has no skill {skill} in its cached commit {}{why}",
                        skill_repos::short(&cached.commit)
                    )));
                }
            }
            None => {
                if let Err(why) = read_cache(state, &id) {
                    notes.push(format!("{why}; until then it gives no skills"));
                }
            }
        }
        out.push((id, spec.skill));
    }
    Ok((out, notes))
}

/// The names an entry gives now: its list, or what the cache holds for `all`.
fn pick_names(state: &Path, pick: &Pick) -> Vec<String> {
    match &pick.skills {
        Skills::Named(names) => names.clone(),
        Skills::All => cached_names(state, &pick.repo),
    }
}

/// Merge one `(id, skill)` into a sandbox's picks; refused when a skill it adds would come from
/// another repo of the same selection as well.
fn merge(state: &Path, picks: &mut Vec<Pick>, id: &str, skill: Option<&str>) -> Result<(), String> {
    let adds: Vec<String> = match skill {
        Some(s) => vec![s.to_string()],
        None => cached_names(state, id),
    };
    for other in picks.iter().filter(|p| p.repo != id) {
        let theirs = pick_names(state, other);
        if let Some(name) = adds.iter().find(|n| theirs.contains(*n)) {
            return Err(format!(
                "{name} would come from both {id} and {}; take it from one of them",
                other.repo
            ));
        }
    }
    if let Some(pick) = picks.iter_mut().find(|p| p.repo == id) {
        match (skill, &mut pick.skills) {
            (None, skills) => *skills = Skills::All,
            (Some(s), Skills::Named(names)) => {
                if !names.iter().any(|n| n == s) {
                    names.push(s.to_string());
                }
            }
            // Already all of it.
            (Some(_), Skills::All) => {}
        }
        return Ok(());
    }
    picks.push(Pick {
        repo: id.to_string(),
        skills: match skill {
            Some(s) => Skills::Named(vec![s.to_string()]),
            None => Skills::All,
        },
    });
    Ok(())
}

/// The picks a list of specs makes on its own (for a new sandbox), checked as [`add`] checks them.
pub fn prepare(state: &Path, raw: &[String]) -> Result<(Vec<Pick>, Vec<String>), Error> {
    if raw.is_empty() {
        // Nothing to check, so nothing is read: a broken skill-repo list never stops an `up`.
        return Ok((Vec::new(), Vec::new()));
    }
    let (wanted, notes) = checked(state, raw)?;
    let mut picks = Vec::new();
    for (id, skill) in &wanted {
        merge(state, &mut picks, id, skill.as_deref()).map_err(Error::Refused)?;
    }
    Ok((picks, notes))
}

/// Whose picks a change edits: every sandbox's (`global`) or one sandbox's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Global,
    Sandbox(&'a str),
}

impl Scope<'_> {
    /// How messages name it.
    fn label(&self) -> &str {
        match self {
            Scope::Global => GLOBAL,
            Scope::Sandbox(name) => name,
        }
    }

    /// The scope's own picks (for a sandbox: without the global ones).
    pub fn picks(&self, selection: &Selection) -> Vec<Pick> {
        match self {
            Scope::Global => selection.global.clone(),
            Scope::Sandbox(name) => selection.sandboxes.get(*name).cloned().unwrap_or_default(),
        }
    }

    fn set(&self, selection: &mut Selection, picks: Vec<Pick>) {
        match self {
            Scope::Global => selection.global = picks,
            Scope::Sandbox(name) if picks.is_empty() => {
                selection.sandboxes.remove(*name);
            }
            Scope::Sandbox(name) => {
                selection.sandboxes.insert(name.to_string(), picks);
            }
        }
    }

    /// The other picks a sync merges with this scope's: the global ones for a sandbox, every
    /// sandbox's for `global`.
    fn others(&self, selection: &Selection) -> Vec<(String, Vec<Pick>)> {
        match self {
            Scope::Global => selection
                .sandboxes
                .iter()
                .map(|(name, picks)| (name.clone(), picks.clone()))
                .collect(),
            Scope::Sandbox(_) => vec![(GLOBAL.to_string(), selection.global.clone())],
        }
    }

    /// `rm`'s command for this scope, for the refusal of a name of an `all` entry.
    fn rm_command(&self, id: &str) -> String {
        match self {
            Scope::Global => format!("aisbx skill rm --global {id}"),
            Scope::Sandbox(name) => format!("aisbx skill rm {name} {id}"),
        }
    }
}

/// The names each repo gives in `after` that it did not give in `before`.
fn grown(state: &Path, before: &[Pick], after: &[Pick]) -> Vec<(String, Vec<String>)> {
    after
        .iter()
        .filter_map(|pick| {
            let old = before
                .iter()
                .find(|p| p.repo == pick.repo)
                .map(|p| pick_names(state, p))
                .unwrap_or_default();
            let new: Vec<String> = pick_names(state, pick)
                .into_iter()
                .filter(|n| !old.contains(n))
                .collect();
            (!new.is_empty()).then(|| (pick.repo.clone(), new))
        })
        .collect()
}

/// Refuse names a change adds when another repo of a list a sync merges them with gives them
/// too. Only what the change adds is checked, so a clash already in the file never blocks a
/// removal.
fn refuse_clash(
    state: &Path,
    adds: &[(String, Vec<String>)],
    against: &[(String, Vec<Pick>)],
) -> Result<(), String> {
    for (id, names) in adds {
        for (label, picks) in against {
            for other in picks.iter().filter(|p| &p.repo != id) {
                let theirs = pick_names(state, other);
                if let Some(name) = names.iter().find(|n| theirs.contains(*n)) {
                    return Err(format!(
                        "{name} would come from both {id} and {} ({label}); take it from one of them",
                        other.repo
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Add specs to a sandbox's selection; nothing is written when one is refused.
pub fn add(state: &Path, sandbox: &str, raw: &[String]) -> Result<Vec<String>, Error> {
    add_to(state, Scope::Sandbox(sandbox), raw)
}

/// Add specs to the global selection, every sandbox's; refused (nothing written) when a skill
/// would come from another repo in any sandbox's own picks as well.
pub fn add_global(state: &Path, raw: &[String]) -> Result<Vec<String>, Error> {
    add_to(state, Scope::Global, raw)
}

fn add_to(state: &Path, scope: Scope, raw: &[String]) -> Result<Vec<String>, Error> {
    let (wanted, notes) = checked(state, raw)?;
    let outcome = update(&selection_path(state), |selection| {
        let before = scope.picks(selection);
        let mut picks = before.clone();
        for (id, skill) in &wanted {
            if let Err(why) = merge(state, &mut picks, id, skill.as_deref()) {
                return Ok(Err(why));
            }
        }
        let adds = grown(state, &before, &picks);
        if let Err(why) = refuse_clash(state, &adds, &scope.others(selection)) {
            return Ok(Err(why));
        }
        scope.set(selection, picks);
        Ok(Ok(()))
    })
    .map_err(failed)?;
    outcome.map_err(Error::Refused)?;
    Ok(notes)
}

/// Make `picks` the scope's whole list in one write (the dashboard's save), refused — nothing
/// written — when a skill it adds would come from two repos in a sandbox's [`effective`]
/// selection.
pub fn store(state: &Path, scope: Scope, picks: Vec<Pick>) -> Result<(), Error> {
    let label = scope.label().to_string();
    if let Err(why) = check_picks(&label, &picks) {
        return Err(Error::Refused(why));
    }
    let outcome = update(&selection_path(state), |selection| {
        let before = scope.picks(selection);
        let adds = grown(state, &before, &picks);
        let mut against = vec![(label.clone(), picks.clone())];
        against.extend(scope.others(selection));
        if let Err(why) = refuse_clash(state, &adds, &against) {
            return Ok(Err(why));
        }
        scope.set(selection, picks);
        Ok(Ok(()))
    })
    .map_err(failed)?;
    outcome.map_err(Error::Refused)
}

/// Make `picks` the sandbox's whole selection (none: drop its entry). `global` stays.
pub fn replace(state: &Path, sandbox: &str, picks: Vec<Pick>) -> Result<()> {
    if picks.is_empty() {
        return forget(state, sandbox);
    }
    update(&selection_path(state), |selection| {
        selection.sandboxes.insert(sandbox.to_string(), picks);
        Ok(())
    })
}

/// Remove specs from a sandbox's selection: `REPO` drops the repo's entry, `REPO#SKILL` one name
/// of a list (an `all` entry is refused). Works on the file alone; a repo no longer configured is
/// named by its id. A global pick is not the sandbox's to remove ([`remove_global`]).
pub fn remove(state: &Path, sandbox: &str, raw: &[String]) -> Result<(), Error> {
    remove_from(state, Scope::Sandbox(sandbox), raw)
}

/// Remove specs from the global selection, as [`remove`] does from a sandbox's.
pub fn remove_global(state: &Path, raw: &[String]) -> Result<(), Error> {
    remove_from(state, Scope::Global, raw)
}

fn remove_from(state: &Path, scope: Scope, raw: &[String]) -> Result<(), Error> {
    let specs = raw
        .iter()
        .map(|s| parse_spec(s).map_err(Error::Refused))
        .collect::<Result<Vec<_>, _>>()?;
    let mut targets = Vec::new();
    let mut config: Option<skill_repos::Config> = None;
    for spec in specs {
        let id = if skill_repos::is_id(&spec.repo) {
            spec.repo.clone()
        } else {
            if config.is_none() {
                config = Some(skill_repos::load(&skill_repos::config_path(state)).map_err(failed)?);
            }
            resolve(config.as_ref().expect("loaded above"), &spec.repo)?
        };
        targets.push((id, spec.skill));
    }
    let label = scope.label().to_string();
    let path = selection_path(state);
    let outcome = update(&path, |selection| {
        let mut picks = scope.picks(selection);
        for (id, skill) in &targets {
            let Some(at) = picks.iter().position(|p| &p.repo == id) else {
                return Ok(Err(format!("{label} has nothing selected from {id}")));
            };
            let Some(s) = skill else {
                picks.remove(at);
                continue;
            };
            let Skills::Named(names) = &mut picks[at].skills else {
                return Ok(Err(format!(
                    "{label} takes all of {id}; drop the whole repo with \
                     `{}`, then add the skills it should keep",
                    scope.rm_command(id)
                )));
            };
            let Some(i) = names.iter().position(|n| n == s) else {
                return Ok(Err(format!("{label} has no {s} from {id} selected")));
            };
            names.remove(i);
            if names.is_empty() {
                picks.remove(at);
            }
        }
        scope.set(selection, picks);
        Ok(Ok(()))
    })
    .map_err(failed)?;
    outcome.map_err(Error::Refused)
}

// --- ls ------------------------------------------------------------------------------------------

/// The selection: the global entries, and each sandbox's [`effective`] entries (one sandbox's,
/// with `only` — listed even when only global picks reach it), each with its cache's commit, the
/// skills it resolves to now and its `origin`: `global`, `sandbox` or `both`. Reads files and git
/// only.
pub fn list_json(state: &Path, only: Option<&str>) -> Result<serde_json::Value> {
    let selection = load(&selection_path(state))?;
    let config = skill_repos::load(&skill_repos::config_path(state))?;
    let entry = |pick: &Pick, origin: Option<&str>| -> serde_json::Value {
        let source = config
            .repos
            .iter()
            .find(|r| r.id().as_deref() == Ok(pick.repo.as_str()))
            .map(|r| r.source.clone());
        let cached = read_cache(state, &pick.repo);
        let (commit, resolves, problem) = match (&source, &cached) {
            (None, _) => (
                None,
                Vec::new(),
                Some("not a configured skill repo".to_string()),
            ),
            (Some(_), Err(why)) => (None, Vec::new(), Some(why.clone())),
            (Some(_), Ok(c)) => {
                let names = c.names();
                let resolves: Vec<String> = match &pick.skills {
                    Skills::All => names,
                    Skills::Named(want) => want
                        .iter()
                        .filter(|n| names.contains(*n))
                        .cloned()
                        .collect(),
                };
                let missing: Vec<&String> = match &pick.skills {
                    Skills::All => Vec::new(),
                    Skills::Named(want) => want.iter().filter(|n| !resolves.contains(*n)).collect(),
                };
                let problem = (!missing.is_empty()).then(|| {
                    let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
                    format!("not in the cached commit: {}", names.join(", "))
                });
                (Some(c.commit.clone()), resolves, problem)
            }
        };
        let mut doc = serde_json::json!({
            "repo": pick.repo,
            "source": source,
            "skills": pick.skills,
            "commit": commit,
            "resolves_to": resolves,
            "problem": problem,
        });
        if let Some(origin) = origin {
            doc["origin"] = origin.into();
        }
        doc
    };
    let global: Vec<serde_json::Value> = selection.global.iter().map(|p| entry(p, None)).collect();
    let mut names: Vec<&str> = selection.sandboxes.keys().map(String::as_str).collect();
    if let Some(only) = only {
        names.retain(|n| *n == only);
        if names.is_empty() && !selection.global.is_empty() {
            names.push(only);
        }
    }
    let mut sandboxes = serde_json::Map::new();
    for sandbox in names {
        let own = selection.sandboxes.get(sandbox);
        let entries: Vec<serde_json::Value> = effective(&selection, sandbox)
            .iter()
            .map(|pick| {
                let mine = own.is_some_and(|o| o.iter().any(|p| p.repo == pick.repo));
                let theirs = selection.global.iter().any(|p| p.repo == pick.repo);
                let origin = match (theirs, mine) {
                    (true, true) => "both",
                    (true, false) => "global",
                    _ => "sandbox",
                };
                entry(pick, Some(origin))
            })
            .collect();
        sandboxes.insert(sandbox.to_string(), serde_json::Value::Array(entries));
    }
    Ok(serde_json::json!({ "global": global, "sandboxes": sandboxes }))
}

// --- sync ----------------------------------------------------------------------------------------

/// What a sync sends: one upload per repo with wanted skills, then the `finish` call.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub uploads: Vec<Upload>,
    pub finish_args: Vec<String>,
    pub warnings: Vec<String>,
}

/// One repo's wanted skills at its cached commit, as `(path in the repo, name)`.
#[derive(Debug, PartialEq, Eq)]
pub struct Upload {
    pub repo_id: String,
    pub commit: String,
    pub cache: PathBuf,
    pub paths: Vec<(String, String)>,
    /// A held sync: the script leaves a stored skill this repo installed as it is.
    pub skip_installed: bool,
}

impl Upload {
    /// `git archive` of exactly those folders (run with literal pathspecs).
    pub fn archive_args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["archive", "--format=tar", self.commit.as_str(), "--"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.extend(self.paths.iter().map(|(path, _)| path.clone()));
        args
    }

    /// The script's `put` call that takes that archive on stdin.
    pub fn put_args(&self) -> Vec<String> {
        let mut args = vec!["put".to_string()];
        if self.skip_installed {
            args.push("--skip-installed".into());
        }
        args.push(self.repo_id.clone());
        args.push(self.commit.clone());
        args.extend(
            self.paths
                .iter()
                .map(|(path, name)| format!("{path}={name}")),
        );
        args
    }
}

/// Work out a sync: the sandbox's [`effective`] selection ∩ the current caches. Reads files and git only. With `hold`
/// (a start with auto-update off) every selected skill of a readable cache is a hold candidate —
/// `--hold-repo <id>=<commit>` for an `all` pick, `--hold <id>/<name>=<commit>` per listed name —
/// and the script keeps the ones its `installed` file records from that repo as they are.
pub fn plan(state: &Path, sandbox: &str, hold: bool) -> Result<Plan> {
    let selection = load(&selection_path(state))?;
    let config = skill_repos::load(&skill_repos::config_path(state))?;
    let picks = effective(&selection, sandbox);
    let mut plan = Plan::default();
    let mut keep: Vec<String> = Vec::new();
    let mut holds: Vec<String> = Vec::new();
    let mut entries: Vec<String> = Vec::new();
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    for pick in &picks {
        let id = &pick.repo;
        if !config
            .repos
            .iter()
            .any(|r| r.id().as_deref() == Ok(id.as_str()))
        {
            plan.warnings.push(format!(
                "{id} is no longer a configured skill repo; its skills are removed from {sandbox}"
            ));
            continue;
        }
        let cached = match read_cache(state, id) {
            Ok(c) => c,
            Err(why) => {
                plan.warnings.push(format!(
                    "{why}; the skills installed from it stay as they are"
                ));
                keep.push(id.clone());
                continue;
            }
        };
        match &pick.skills {
            Skills::All => {
                plan.warnings
                    .extend(cached.skipped.iter().map(|s| s.line(id)));
                if hold {
                    holds.push("--hold-repo".into());
                    holds.push(format!("{id}={}", cached.commit));
                }
            }
            Skills::Named(names) => {
                for n in names {
                    if hold {
                        holds.push("--hold".into());
                        holds.push(format!("{id}/{n}={}", cached.commit));
                    }
                    if cached.found.iter().any(|f| &f.name == n) {
                        continue;
                    }
                    let why = match cached.skipped.iter().find(|s| &s.name == n) {
                        Some(s) => format!("skipped: {}", s.why),
                        None => "not in the cached commit".to_string(),
                    };
                    plan.warnings.push(format!("{id}: {n}: {why}"));
                }
            }
        }
        let wanted = cached
            .found
            .iter()
            .filter(|f| selects(&pick.skills, &f.name));
        let mut paths = Vec::new();
        for f in wanted {
            if let Some(first) = taken.get(&f.name) {
                plan.warnings
                    .push(format!("{id}: dup {}: {first} gives it first", f.name));
                continue;
            }
            taken.insert(f.name.clone(), id.clone());
            paths.push((f.path.clone(), f.name.clone()));
            entries.push(format!("{}\t{id}\t{}", f.name, cached.commit));
        }
        if !paths.is_empty() {
            plan.uploads.push(Upload {
                repo_id: id.clone(),
                commit: cached.commit.clone(),
                cache: cached.dir.clone(),
                paths,
                skip_installed: hold,
            });
        }
    }
    plan.finish_args.push("finish".into());
    for id in keep {
        plan.finish_args.push("--keep-repo".into());
        plan.finish_args.push(id);
    }
    plan.finish_args.extend(holds);
    plan.finish_args.extend(entries);
    Ok(plan)
}

/// `sbx` for the sync: no host SSH agent and no token variable in its environment.
fn sbx_command(argv: &[String]) -> Command {
    let (program, args) = argv.split_first().expect("an argv names its program");
    let mut cmd = Command::new(program);
    cmd.args(args);
    for v in ["SSH_AUTH_SOCK", "GITHUB_TOKEN", "GH_TOKEN"] {
        cmd.env_remove(v);
    }
    cmd
}

/// Stream one repo's archive into the script's `put`.
fn upload(sandbox: &str, up: &Upload) -> Result<(), String> {
    let archive_args = up.archive_args();
    let args: Vec<&str> = archive_args.iter().map(String::as_str).collect();
    let mut git = skill_repos::local_git(&up.cache, &args)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{}: git archive: {e}", up.repo_id))?;
    let tar = git.stdout.take().expect("stdout is piped");
    let put = sbx_command(&sbx::repo_skills_argv(sandbox, &up.put_args()))
        .stdin(Stdio::from(tar))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output();
    let archived = git.wait();
    let put = put.map_err(|e| format!("{}: could not run sbx: {e}", up.repo_id))?;
    match archived {
        Ok(s) if s.success() => {}
        Ok(s) => return Err(format!("{}: git archive failed ({s})", up.repo_id)),
        Err(e) => return Err(format!("{}: git archive: {e}", up.repo_id)),
    }
    if !put.status.success() {
        return Err(format!("{}: sbx exec failed ({})", up.repo_id, put.status));
    }
    let said = String::from_utf8_lossy(&put.stdout);
    match said.lines().find(|l| l.starts_with("put ")) {
        Some(line) if !line.contains(" failed") => Ok(()),
        Some(line) => Err(line.to_string()),
        None => Err(format!("{}: the sandbox did not answer", up.repo_id)),
    }
}

/// Which sync: the owner's `aisbx skill sync NAME` brings every selected skill to the cached
/// commit; the pre-start one (`--start`) does that only while auto-update is on, and otherwise
/// holds the installed skills (new ones still arrive, deselected ones still go).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Full,
    Start { auto_update: bool },
}

impl SyncMode {
    fn holds(self) -> bool {
        self == SyncMode::Start { auto_update: false }
    }
}

/// The count and names of a `held <n> <a,b>` status line.
fn held_line(status: &str) -> Option<(&str, &str)> {
    status.lines().find_map(|l| {
        let mut words = l.split(' ');
        match (words.next(), words.next(), words.next(), words.next()) {
            (Some("held"), Some(n), Some(names), None) => Some((n, names)),
            _ => None,
        }
    })
}

/// Sync the sandbox's skills from the caches: the plan's warnings, any failed step and the hint
/// for held skills go to stderr, the script's status lines to `status_out`. `Ok(true)` when every
/// step worked.
pub fn sync(
    state: &Path,
    sandbox: &str,
    mode: SyncMode,
    status_out: &mut dyn io::Write,
) -> Result<bool> {
    let plan = plan(state, sandbox, mode.holds())?;
    for warning in &plan.warnings {
        eprintln!("aisbx: warning: {warning}");
    }
    let mut ok = true;
    for up in &plan.uploads {
        if let Err(why) = upload(sandbox, up) {
            eprintln!("aisbx: {why}");
            ok = false;
        }
    }
    let finish = sbx_command(&sbx::repo_skills_argv(sandbox, &plan.finish_args))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output();
    if let Ok(out) = &finish {
        let _ = io::Write::write_all(&mut *status_out, &out.stdout);
        let _ = io::Write::flush(&mut *status_out);
        if let Some((n, names)) = held_line(&String::from_utf8_lossy(&out.stdout)) {
            eprintln!(
                "aisbx: auto-update is off: {sandbox} keeps {n} skill(s) as installed ({names}); \
                 `aisbx skill sync {sandbox}` updates them"
            );
        }
    }
    match finish.map(|out| out.status) {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("aisbx: the skill links in {sandbox} were not updated ({s})");
            ok = false;
        }
        Err(e) => {
            eprintln!("aisbx: could not run sbx: {e}");
            ok = false;
        }
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aisbx-skill-select-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Plain git for building the fixture, away from the host's own configuration.
    fn sh_git(dir: &Path, args: &[&str]) -> String {
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
            .args(["-c", "protocol.file.allow=always"])
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

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn skill(work: &Path, dir: &str) {
        write(&work.join(dir).join("SKILL.md"), &format!("name: {dir}\n"));
    }

    /// A configured repo `o/<name>` whose cache is a git repo made in place, holding these
    /// skill folders, committed. Hands back its id.
    fn cached_repo(state: &Path, name: &str, skills: &[&str]) -> String {
        let id =
            skill_repos::add(state, &format!("o/{name}"), None, skill_repos::Method::Auto).unwrap();
        let work = skill_repos::cache_root(state).join(&id);
        fs::create_dir_all(&work).unwrap();
        sh_git(&work, &["init", "-q"]);
        write(&work.join("README.md"), "readme\n");
        for s in skills {
            skill(&work, s);
        }
        sh_git(&work, &["add", "-A"]);
        sh_git(&work, &["commit", "-qm", "skills"]);
        id
    }

    fn names(found: &[Found]) -> Vec<&str> {
        found.iter().map(|f| f.name.as_str()).collect()
    }

    fn whys(skipped: &[Skipped]) -> Vec<(&str, &str)> {
        skipped.iter().map(|s| (s.name.as_str(), s.why)).collect()
    }

    #[test]
    fn skill_names_follow_the_pattern() {
        let (long, longer) = ("a".repeat(64), "a".repeat(65));
        for ok in ["a", "0", "my-skill", "a.b_c-d", long.as_str()] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            ".a",
            "_a",
            "A",
            "a b",
            "a/b",
            "ä",
            longer.as_str(),
            "installed",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn discovery_reads_the_committed_tree_and_applies_every_rule() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let base = scratch("discover");
        let work = base.join("repo");
        fs::create_dir_all(&work).unwrap();
        sh_git(&work, &["init", "-q"]);
        write(&work.join("SKILL.md"), "the root is never a skill\n");
        skill(&work, "top");
        skill(&work, "skills/alpha");
        skill(&work, "skills/alpha/nested"); // inside another skill
        skill(&work, "a/b/c/deep4"); // 4 levels: fine
        skill(&work, "a/b/c/d/deep5"); // 5 levels: too deep
        skill(&work, ".hidden/secret"); // in a dot folder
        skill(&work, "skills/.dot"); // a dot folder itself
        skill(&work, "skills/Bad_Name");
        skill(&work, "skills/linked");
        std::os::unix::fs::symlink("../top", work.join("skills/linked/up")).unwrap();
        skill(&work, "skills/big");
        let big = vec![b'x'; (MAX_SKILL_BYTES + 1) as usize];
        fs::write(work.join("skills/big/blob.bin"), big).unwrap();
        skill(&work, "other/alpha"); // dup of skills/alpha
        fs::create_dir_all(work.join("noskill")).unwrap();
        write(&work.join("noskill/README.md"), "x\n");
        // A submodule entry, as a gitlink in the index.
        skill(&work, "skills/withsub");
        sh_git(&work, &["add", "-A"]);
        sh_git(&work, &["commit", "-qm", "one"]);
        let commit = sh_git(&work, &["rev-parse", "HEAD"]);
        sh_git(
            &work,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{commit},skills/withsub/vendored"),
            ],
        );
        sh_git(&work, &["commit", "-qm", "sub"]);
        let commit = sh_git(&work, &["rev-parse", "HEAD"]);
        // Only in the working tree: never seen.
        skill(&work, "skills/uncommitted");

        let (found, skipped) = discover(&work, &commit).unwrap();
        assert_eq!(names(&found), ["deep4", "alpha", "top"]);
        assert_eq!(
            found.iter().find(|f| f.name == "alpha").unwrap().path,
            "other/alpha",
            "path order: other/ before skills/"
        );
        assert_eq!(
            whys(&skipped),
            [
                ("Bad_Name", "bad-name"),
                ("alpha", "dup"),
                ("big", "too-big"),
                ("linked", "symlink"),
                ("withsub", "submodule"),
            ]
        );
        assert!(discover(&work, "not-a-commit").is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn specs_are_ids_or_sources_with_an_optional_skill() {
        let id = "github-com-o-r-0123abcd";
        assert_eq!(
            parse_spec(id).unwrap(),
            Spec {
                repo: id.into(),
                skill: None
            }
        );
        assert_eq!(
            parse_spec("o/r#my-skill").unwrap(),
            Spec {
                repo: "o/r".into(),
                skill: Some("my-skill".into())
            }
        );
        assert_eq!(
            parse_spec("git@github.com:o/r.git#x").unwrap(),
            Spec {
                repo: "git@github.com:o/r.git".into(),
                skill: Some("x".into())
            }
        );
        assert_eq!(parse_spec("https://example.com/o/r").unwrap().skill, None);
        let secretish = format!("https://user:{}@github.com/o/r", "pw-123");
        for bad in [
            "".to_string(),
            "#x".into(),
            "o/r#".into(),
            "o/r#Bad".into(),
            "o/r#a#b".into(),
            "o/r#installed".into(),
            "/local/path".into(),
            secretish.clone(),
            format!("{secretish}#x"),
        ] {
            let err = parse_spec(&bad).unwrap_err();
            assert!(!err.contains("pw-123"), "{err}");
        }
    }

    #[test]
    fn the_selection_round_trips_and_writes_only_on_change() {
        let state = scratch("roundtrip");
        let path = selection_path(&state);
        assert_eq!(load(&path).unwrap(), Selection::default());
        let picks = vec![
            Pick {
                repo: "github-com-o-r-0123abcd".into(),
                skills: Skills::All,
            },
            Pick {
                repo: "github-com-o-s-89abcdef".into(),
                skills: Skills::Named(strings(&["a", "b"])),
            },
        ];
        replace(&state, "box", picks.clone()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""skills": "all""#), "{text}");
        assert_eq!(load(&path).unwrap().sandboxes["box"], picks);
        assert!(has_selection(&state, "box"));
        assert!(!has_selection(&state, "other"));
        // A no-op change leaves the file alone.
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        forget(&state, "other").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        forget(&state, "box").unwrap();
        assert!(load(&path).unwrap().sandboxes.is_empty());
        assert!(!has_selection(&state, "box"));
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn a_selection_this_build_cannot_read_is_reported_and_left_alone() {
        let id = "github-com-o-r-0123abcd";
        for (tag, body) in [
            ("corrupt", "{not json".to_string()),
            ("v2", r#"{"version": 2, "sandboxes": {}}"#.to_string()),
            (
                "bad-word",
                format!(
                    r#"{{"version": 1, "sandboxes": {{"box": [{{"repo": "{id}", "skills": "some"}}]}}}}"#
                ),
            ),
            (
                "bad-id",
                r#"{"version": 1, "sandboxes": {"box": [{"repo": "../x", "skills": "all"}]}}"#
                    .to_string(),
            ),
            (
                "bad-name",
                format!(
                    r#"{{"version": 1, "sandboxes": {{"box": [{{"repo": "{id}", "skills": ["../x"]}}]}}}}"#
                ),
            ),
            (
                "empty-list",
                format!(
                    r#"{{"version": 1, "sandboxes": {{"box": [{{"repo": "{id}", "skills": []}}]}}}}"#
                ),
            ),
            (
                "twice",
                format!(
                    r#"{{"version": 1, "sandboxes": {{"box": [{{"repo": "{id}", "skills": "all"}}, {{"repo": "{id}", "skills": "all"}}]}}}}"#
                ),
            ),
        ] {
            let state = scratch(&format!("unreadable-{tag}"));
            let path = selection_path(&state);
            fs::write(&path, &body).unwrap();
            let err = load(&path).unwrap_err();
            assert!(format!("{err:#}").contains(FILE), "{tag}: {err:#}");
            assert!(forget(&state, "box").is_err(), "{tag}");
            assert!(replace(&state, "box", Vec::new()).is_err(), "{tag}");
            assert!(
                matches!(
                    remove(&state, "box", &strings(&[id])),
                    Err(Error::Failed(_))
                ),
                "{tag}"
            );
            assert!(plan(&state, "box", false).is_err(), "{tag}");
            assert!(list_json(&state, None).is_err(), "{tag}");
            assert!(
                has_selection(&state, "box"),
                "{tag}: never treated as empty"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), body, "{tag}");
            let _ = fs::remove_dir_all(&state);
        }
    }

    #[test]
    fn add_refuses_before_writing_anything() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let state = scratch("refusals");
        let r = cached_repo(&state, "r", &["skills/alpha", "skills/beta"]);
        let s = cached_repo(&state, "s", &["skills/beta", "skills/gamma"]);
        // o/t listed with two refs: ambiguous by source.
        skill_repos::add(&state, "o/t", Some("main"), skill_repos::Method::Auto).unwrap();
        skill_repos::add(&state, "o/t", Some("dev"), skill_repos::Method::Auto).unwrap();
        let path = selection_path(&state);
        let refused = |specs: &[&str]| match add(&state, "box", &strings(specs)) {
            Err(Error::Refused(why)) => why,
            other => panic!("{specs:?}: {other:?}"),
        };
        assert!(refused(&["o/nope"]).contains("no configured skill repo"));
        assert!(refused(&["o/t"]).contains("matches 2"));
        assert!(refused(&["o/r#delta"]).contains("no skill delta"));
        assert!(refused(&["o/r#Bad"]).contains("not a skill name"));
        assert!(refused(&["o/r#alpha", "o/s#beta", "o/r#beta"]).contains("beta"));
        assert!(refused(&["o/r", "o/s"]).contains("beta would come from both"));
        assert!(!path.exists(), "nothing written");

        add(&state, "box", &strings(&["o/r#alpha"])).unwrap();
        add(&state, "box", &strings(&[s.as_str()])).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        // beta is in s (all) and would come from r too.
        assert!(refused(&["o/r#beta"]).contains("beta would come from both"));
        assert!(refused(&["o/r"]).contains("beta would come from both"));
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "left as it was");
        assert_eq!(
            load(&path).unwrap().sandboxes["box"],
            [
                Pick {
                    repo: r.clone(),
                    skills: Skills::Named(strings(&["alpha"]))
                },
                Pick {
                    repo: s.clone(),
                    skills: Skills::All
                },
            ]
        );
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn rm_drops_names_and_entries_and_refuses_a_name_of_an_all_entry() {
        let state = scratch("rm");
        let r = "github-com-o-r-0123abcd";
        let s = "github-com-o-s-89abcdef";
        replace(
            &state,
            "box",
            vec![
                Pick {
                    repo: r.into(),
                    skills: Skills::Named(strings(&["a", "b"])),
                },
                Pick {
                    repo: s.into(),
                    skills: Skills::All,
                },
            ],
        )
        .unwrap();
        let path = selection_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        for specs in [
            vec![format!("{s}#x")],
            vec![format!("{r}#c")],
            vec!["github-com-o-u-00000000".to_string()],
            vec!["o/r#Bad".to_string()],
        ] {
            assert!(
                matches!(remove(&state, "box", &specs), Err(Error::Refused(_))),
                "{specs:?}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), text, "{specs:?}");
        }
        remove(&state, "box", &[format!("{r}#a")]).unwrap();
        assert_eq!(
            load(&path).unwrap().sandboxes["box"][0].skills,
            Skills::Named(strings(&["b"]))
        );
        remove(&state, "box", &[format!("{r}#b")]).unwrap();
        assert_eq!(load(&path).unwrap().sandboxes["box"].len(), 1);
        remove(&state, "box", &[s.to_string()]).unwrap();
        assert!(load(&path).unwrap().sandboxes.is_empty());
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn the_sync_plan_follows_the_caches() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let state = scratch("plan");
        let r = cached_repo(
            &state,
            "r",
            &["skills/alpha", "skills/beta", "skills/Bad_x"],
        );
        let s = cached_repo(&state, "s", &["pack/beta", "pack/gamma"]);
        let gone = skill_repos::add(&state, "o/gone", None, skill_repos::Method::Auto).unwrap();
        let unconfigured = "github-com-o-old-0123abcd".to_string();
        replace(
            &state,
            "box",
            vec![
                Pick {
                    repo: r.clone(),
                    skills: Skills::All,
                },
                Pick {
                    repo: s.clone(),
                    skills: Skills::All,
                },
                Pick {
                    repo: gone.clone(),
                    skills: Skills::All,
                },
                Pick {
                    repo: unconfigured.clone(),
                    skills: Skills::All,
                },
            ],
        )
        .unwrap();
        let p = plan(&state, "box", false).unwrap();
        let commit_r = skill_repos::cached_commit(&skill_repos::cache_root(&state).join(&r))
            .unwrap()
            .0;
        let commit_s = skill_repos::cached_commit(&skill_repos::cache_root(&state).join(&s))
            .unwrap()
            .0;
        assert_eq!(p.uploads.len(), 2);
        let (cr, cs) = (commit_r.as_str(), commit_s.as_str());
        assert_eq!(
            p.uploads[0].archive_args(),
            strings(&[
                "archive",
                "--format=tar",
                cr,
                "--",
                "skills/alpha",
                "skills/beta"
            ])
        );
        assert_eq!(
            p.uploads[0].put_args(),
            strings(&[
                "put",
                r.as_str(),
                cr,
                "skills/alpha=alpha",
                "skills/beta=beta"
            ])
        );
        assert_eq!(
            p.uploads[1].put_args(),
            strings(&["put", s.as_str(), cs, "pack/gamma=gamma"])
        );
        let expected = vec![
            "finish".to_string(),
            "--keep-repo".to_string(),
            gone.clone(),
            format!("alpha\t{r}\t{cr}"),
            format!("beta\t{r}\t{cr}"),
            format!("gamma\t{s}\t{cs}"),
        ];
        assert_eq!(p.finish_args, expected);
        let all = p.warnings.join("\n");
        assert!(
            all.contains(&format!("{r}: skipped Bad_x: bad-name")),
            "{all}"
        );
        assert!(all.contains(&format!("{s}: dup beta")), "{all}");
        assert!(
            all.contains(&format!("aisbx skill-repo fetch {gone}")),
            "{all}"
        );
        assert!(
            all.contains(&format!("{unconfigured} is no longer a configured")),
            "{all}"
        );
        // No selection: a finish with nothing, which empties the sandbox's store.
        let none = plan(&state, "other", false).unwrap();
        assert!(none.uploads.is_empty());
        assert_eq!(none.finish_args, ["finish"]);
        // No token or host path in what reaches the sandbox, apart from skill content.
        let sent = format!("{:?}{:?}", p.finish_args, p.uploads[0].put_args());
        assert!(
            !sent.contains(&state.to_string_lossy().into_owned()),
            "{sent}"
        );
        let _ = fs::remove_dir_all(&state);
    }

    /// A cache as `read_cache` hands it back, built by hand: `(name, content)` found, `(name, why)`
    /// skipped.
    fn cache_of(commit: &str, found: &[(&str, &str)], skipped: &[(&str, &'static str)]) -> Cached {
        Cached {
            dir: PathBuf::from("/nowhere"),
            commit: commit.to_string(),
            found: found
                .iter()
                .map(|(name, content)| Found {
                    name: name.to_string(),
                    path: format!("skills/{name}"),
                    content: content.to_string(),
                })
                .collect(),
            skipped: skipped
                .iter()
                .map(|(name, why)| Skipped {
                    name: name.to_string(),
                    why,
                })
                .collect(),
        }
    }

    fn removed(name: &str, skipped: Option<&'static str>) -> Removed {
        Removed {
            name: name.to_string(),
            skipped,
        }
    }

    #[test]
    fn diff_reports_added_removed_with_the_skip_reason_and_changed() {
        let before = cache_of(
            "1111",
            &[
                ("keep", "t1"),
                ("edit", "t2"),
                ("gone", "t3"),
                ("linky", "t4"),
            ],
            &[],
        );
        let after = cache_of(
            "2222",
            &[
                ("zeta", "t9"),
                ("keep", "t1"),
                ("edit", "t5"),
                ("fresh", "t6"),
            ],
            &[("linky", "symlink")],
        );
        let changes = diff(Some(&before), &after);
        assert_eq!(changes.added, strings(&["fresh", "zeta"]), "sorted");
        assert_eq!(
            changes.removed,
            [removed("gone", None), removed("linky", Some("symlink"))]
        );
        assert_eq!(changes.changed, strings(&["edit"]));
        assert_eq!(
            changes.summary().as_deref(),
            Some("+fresh,zeta  -gone,linky (skipped: symlink)  ~edit")
        );
    }

    #[test]
    fn diff_of_the_same_skills_is_empty_and_without_a_cache_everything_is_added() {
        let one = cache_of("1111", &[("b", "t1"), ("a", "t2")], &[("bad", "dup")]);
        let same = diff(Some(&one), &one);
        assert!(same.is_empty());
        assert_eq!(same.summary(), None);
        // A move with the same content is no change.
        let moved = Cached {
            found: vec![Found {
                name: "a".into(),
                path: "elsewhere/a".into(),
                content: "t2".into(),
            }],
            ..cache_of("2222", &[], &[])
        };
        assert!(diff(Some(&cache_of("1111", &[("a", "t2")], &[])), &moved).is_empty());
        let fresh = diff(None, &one);
        assert_eq!(fresh.added, strings(&["a", "b"]));
        assert!(fresh.removed.is_empty() && fresh.changed.is_empty());
    }

    fn changes_abc() -> SkillChanges {
        SkillChanges {
            added: strings(&["a", "b"]),
            removed: vec![removed("c", None), removed("e", Some("too-big"))],
            changed: strings(&["d"]),
        }
    }

    #[test]
    fn selects_takes_every_name_for_all_and_only_listed_names_otherwise() {
        assert!(selects(&Skills::All, "anything"));
        let named = Skills::Named(strings(&["a", "d"]));
        assert!(selects(&named, "a"));
        assert!(selects(&named, "d"));
        assert!(!selects(&named, "b"));
    }

    #[test]
    fn forecast_with_auto_update_on_installs_updates_and_removes() {
        let all = forecast(&Skills::All, &changes_abc(), true).unwrap();
        assert_eq!(
            all,
            Forecast {
                installs: strings(&["a", "b"]),
                updates: strings(&["d"]),
                removes: strings(&["c", "e"]),
                held: Vec::new(),
            }
        );
        assert_eq!(
            all.line("box"),
            "box: next start installs a,b; updates d; removes c,e"
        );
        let named = forecast(&Skills::Named(strings(&["b", "c"])), &changes_abc(), true).unwrap();
        assert_eq!(named.installs, strings(&["b"]));
        assert!(named.updates.is_empty());
        assert_eq!(named.removes, strings(&["c"]));
    }

    #[test]
    fn forecast_with_auto_update_off_installs_new_ones_and_holds_the_rest() {
        let all = forecast(&Skills::All, &changes_abc(), false).unwrap();
        assert_eq!(
            all,
            Forecast {
                installs: strings(&["a", "b"]),
                updates: Vec::new(),
                removes: Vec::new(),
                held: strings(&["c", "d", "e"]),
            }
        );
        assert_eq!(
            all.line("box"),
            "box: next start installs a,b; held c,d,e (run `aisbx skill sync box` to update)"
        );
        // Only new skills: no sync hint.
        let only_new = SkillChanges {
            added: strings(&["a"]),
            ..SkillChanges::default()
        };
        let line = forecast(&Skills::All, &only_new, false)
            .unwrap()
            .line("box");
        assert_eq!(line, "box: next start installs a");
        assert!(!line.contains("skill sync"), "{line}");
    }

    #[test]
    fn forecast_is_none_for_a_named_pick_whose_names_are_untouched() {
        let untouched = Skills::Named(strings(&["x", "y"]));
        assert_eq!(forecast(&untouched, &changes_abc(), true), None);
        assert_eq!(forecast(&untouched, &changes_abc(), false), None);
        assert_eq!(forecast(&Skills::All, &SkillChanges::default(), true), None);
    }

    #[test]
    fn the_held_status_line_is_read_back() {
        assert_eq!(
            held_line("store 3\nheld 2 alpha,beta\n/h/.claude/skills linked 3\n"),
            Some(("2", "alpha,beta"))
        );
        assert_eq!(held_line("store 3\n/h/.claude/skills linked 3\n"), None);
        assert!(SyncMode::Start { auto_update: false }.holds());
        assert!(!SyncMode::Start { auto_update: true }.holds());
        assert!(!SyncMode::Full.holds());
    }

    #[test]
    fn discovery_gives_each_skill_its_folder_tree_as_content() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let base = scratch("content");
        let work = base.join("repo");
        fs::create_dir_all(&work).unwrap();
        sh_git(&work, &["init", "-q"]);
        skill(&work, "skills/same");
        skill(&work, "skills/edited");
        write(&work.join("skills/edited/ref/notes.md"), "one\n");
        sh_git(&work, &["add", "-A"]);
        sh_git(&work, &["commit", "-qm", "one"]);
        let first = sh_git(&work, &["rev-parse", "HEAD"]);
        write(&work.join("skills/edited/ref/notes.md"), "two\n");
        write(&work.join("README.md"), "outside every skill\n");
        sh_git(&work, &["add", "-A"]);
        sh_git(&work, &["commit", "-qm", "two"]);
        let second = sh_git(&work, &["rev-parse", "HEAD"]);

        let content = |commit: &str, name: &str| {
            discover(&work, commit)
                .unwrap()
                .0
                .into_iter()
                .find(|f| f.name == name)
                .unwrap()
                .content
        };
        let same = content(&first, "same");
        assert_eq!(same.len(), 40, "a tree object id: {same}");
        assert_eq!(
            same,
            sh_git(&work, &["rev-parse", &format!("{first}:skills/same")])
        );
        assert_eq!(content(&second, "same"), same);
        assert_ne!(content(&first, "edited"), content(&second, "edited"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_held_plan_names_the_hold_candidates() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let state = scratch("plan-hold");
        let r = cached_repo(&state, "r", &["skills/alpha", "skills/beta"]);
        let s = cached_repo(&state, "s", &["pack/gamma", "pack/delta"]);
        let gone = skill_repos::add(&state, "o/gone", None, skill_repos::Method::Auto).unwrap();
        replace(
            &state,
            "box",
            vec![
                Pick {
                    repo: r.clone(),
                    skills: Skills::All,
                },
                Pick {
                    repo: s.clone(),
                    skills: Skills::Named(strings(&["gamma", "vanished"])),
                },
                Pick {
                    repo: gone.clone(),
                    skills: Skills::All,
                },
            ],
        )
        .unwrap();
        let commit = |id: &str| {
            skill_repos::cached_commit(&skill_repos::cache_root(&state).join(id))
                .unwrap()
                .0
        };
        let (cr, cs) = (commit(&r), commit(&s));
        let full = plan(&state, "box", false).unwrap();
        let held = plan(&state, "box", true).unwrap();
        assert!(full.uploads.iter().all(|u| !u.skip_installed));
        assert!(!full.finish_args.iter().any(|a| a.starts_with("--hold")));
        assert_eq!(
            held.uploads[0].put_args(),
            strings(&[
                "put",
                "--skip-installed",
                r.as_str(),
                cr.as_str(),
                "skills/alpha=alpha",
                "skills/beta=beta"
            ])
        );
        assert_eq!(
            held.uploads[1].put_args(),
            strings(&[
                "put",
                "--skip-installed",
                s.as_str(),
                cs.as_str(),
                "pack/gamma=gamma"
            ])
        );
        let expected = vec![
            "finish".to_string(),
            "--keep-repo".to_string(),
            gone.clone(),
            "--hold-repo".to_string(),
            format!("{r}={cr}"),
            "--hold".to_string(),
            format!("{s}/gamma={cs}"),
            "--hold".to_string(),
            format!("{s}/vanished={cs}"),
            format!("alpha\t{r}\t{cr}"),
            format!("beta\t{r}\t{cr}"),
            format!("gamma\t{s}\t{cs}"),
        ];
        assert_eq!(held.finish_args, expected);
        // The wanted lines are the full sync's, with the hold flags in front of them.
        let wanted = |p: &Plan| -> Vec<String> {
            p.finish_args
                .iter()
                .filter(|a| a.contains('\t'))
                .cloned()
                .collect()
        };
        assert_eq!(wanted(&held), wanted(&full));
        assert!(
            held.warnings.join("\n").contains(&format!("{s}: vanished")),
            "{:?}",
            held.warnings
        );
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn the_script_is_the_repo_file_and_runs_through_sbx_exec() {
        assert!(SCRIPT.starts_with("#!/bin/sh"));
        assert!(SCRIPT.trim_end().ends_with("exit 0"));
        let argv = sbx::repo_skills_argv("box", &strings(&["finish"]));
        assert_eq!(&argv[..6], ["sbx", "exec", "-i", "box", "sh", "-c"]);
        assert_eq!(argv[6], SCRIPT);
        assert_eq!(&argv[7..], ["sh", "finish"]);
    }

    fn named(names: &[&str]) -> Skills {
        Skills::Named(strings(names))
    }

    fn pick(repo: &str, skills: Skills) -> Pick {
        Pick {
            repo: repo.into(),
            skills,
        }
    }

    #[test]
    fn effective_merges_the_global_picks_with_the_sandbox_own() {
        let (r, s, t) = (
            "github-com-o-r-0123abcd",
            "github-com-o-s-89abcdef",
            "github-com-o-t-00000000",
        );
        let mut sel = Selection {
            global: vec![pick(r, named(&["a", "b"])), pick(s, named(&["x"]))],
            ..Selection::default()
        };
        sel.sandboxes.insert(
            "box".into(),
            vec![
                pick(r, named(&["b", "c"])),
                pick(s, Skills::All),
                pick(t, named(&["z"])),
            ],
        );
        assert_eq!(
            effective(&sel, "box"),
            [
                pick(r, named(&["a", "b", "c"])),
                pick(s, Skills::All),
                pick(t, named(&["z"])),
            ]
        );
        // A sandbox with no entry of its own gets the global picks.
        assert_eq!(effective(&sel, "other"), sel.global);
        // A global `all` is not narrowed by a sandbox's names.
        sel.global = vec![pick(t, Skills::All)];
        assert_eq!(effective(&sel, "box")[0], pick(t, Skills::All));
        assert!(effective(&Selection::default(), "box").is_empty());
    }

    #[test]
    fn global_picks_load_round_trip_and_survive_forget_and_replace() {
        let state = scratch("global");
        let path = selection_path(&state);
        let id = "github-com-o-r-0123abcd";
        let old = format!(
            r#"{{"version": 1, "sandboxes": {{"box": [{{"repo": "{id}", "skills": "all"}}]}}}}"#
        );
        fs::write(&path, &old).unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.global.is_empty());
        assert_eq!(loaded.sandboxes["box"], [pick(id, Skills::All)]);
        assert!(!has_selection(&state, "other"));
        // Written back without the key while it is empty.
        replace(&state, "box", vec![pick(id, named(&["a"]))]).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("global"));

        store(&state, Scope::Global, vec![pick(id, named(&["g"]))]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""global""#), "{text}");
        assert!(has_selection(&state, "any-name"), "every sandbox has it");
        forget(&state, "box").unwrap();
        replace(&state, "new", vec![pick(id, named(&["n"]))]).unwrap();
        replace(&state, "new", Vec::new()).unwrap();
        let after = load(&path).unwrap();
        assert_eq!(after.global, [pick(id, named(&["g"]))]);
        assert!(after.sandboxes.is_empty());
        // A bad global entry makes the file unreadable, like a bad sandbox entry.
        let bad = format!(r#"{{"version": 1, "global": [{{"repo": "{id}", "skills": []}}]}}"#);
        fs::write(&path, &bad).unwrap();
        assert!(load(&path).is_err());
        assert!(store(&state, Scope::Global, Vec::new()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn remove_global_edits_the_global_list_only() {
        let state = scratch("rm-global");
        let (r, s) = ("github-com-o-r-0123abcd", "github-com-o-s-89abcdef");
        store(
            &state,
            Scope::Global,
            vec![pick(r, named(&["a", "b"])), pick(s, Skills::All)],
        )
        .unwrap();
        replace(&state, "box", vec![pick(r, named(&["a"]))]).unwrap();
        let path = selection_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        for specs in [vec![format!("{s}#x")], vec![format!("{r}#c")]] {
            match remove_global(&state, &specs) {
                Err(Error::Refused(why)) => assert!(why.starts_with("global "), "{why}"),
                other => panic!("{specs:?}: {other:?}"),
            }
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
        match remove_global(&state, &[format!("{s}#x")]) {
            Err(Error::Refused(why)) => {
                assert!(why.contains("aisbx skill rm --global"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        remove_global(&state, &[format!("{r}#a"), s.to_string()]).unwrap();
        let sel = load(&path).unwrap();
        assert_eq!(sel.global, [pick(r, named(&["b"]))]);
        assert_eq!(sel.sandboxes["box"], [pick(r, named(&["a"]))]);
        // The sandbox's rm never reaches a global pick.
        assert!(matches!(
            remove(&state, "box", &[format!("{r}#b")]),
            Err(Error::Refused(_))
        ));
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn global_picks_are_planned_and_clashes_with_them_refused() {
        if !git_available() {
            eprintln!("skipped: no git on PATH");
            return;
        }
        let state = scratch("global-plan");
        let r = cached_repo(&state, "r", &["skills/alpha", "skills/beta"]);
        let _s = cached_repo(&state, "s", &["skills/beta", "skills/gamma"]);
        add_global(&state, &strings(&["o/r#alpha"])).unwrap();
        assert!(has_selection(&state, "fresh"));
        let p = plan(&state, "fresh", false).unwrap();
        assert_eq!(p.uploads.len(), 1);
        assert_eq!(p.uploads[0].repo_id, r);
        assert_eq!(
            p.uploads[0].paths,
            [("skills/alpha".to_string(), "alpha".to_string())]
        );

        // Sandbox first, then a global add that clashes with it.
        add(&state, "box", &strings(&["o/s#beta"])).unwrap();
        let path = selection_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        let refused = |res: Result<Vec<String>, Error>| match res {
            Err(Error::Refused(why)) => why,
            other => panic!("{other:?}"),
        };
        let why = refused(add_global(&state, &strings(&["o/r#beta"])));
        assert!(why.contains("beta would come from both"), "{why}");
        assert!(why.contains("(box)"), "{why}");
        assert!(refused(add_global(&state, &strings(&["o/r"]))).contains("beta"));
        assert_eq!(fs::read_to_string(&path).unwrap(), text);

        // The reverse: a global pick, then a sandbox add from another repo.
        add_global(&state, &strings(&["o/s"])).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let why = refused(add(&state, "other", &strings(&["o/r#beta"])));
        assert!(why.contains("beta would come from both"), "{why}");
        assert!(why.contains("(global)"), "{why}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        // The same repo in both lists is no clash: the names are unioned.
        add(&state, "third", &strings(&["o/s#beta"])).unwrap();
        let p = plan(&state, "third", false).unwrap();
        let mut planned: Vec<&str> = p
            .uploads
            .iter()
            .flat_map(|u| u.paths.iter().map(|(_, n)| n.as_str()))
            .collect();
        planned.sort();
        assert_eq!(planned, ["alpha", "beta", "gamma"]);

        // `store` checks the same way and writes nothing on a clash.
        let text = fs::read_to_string(&path).unwrap();
        let clash = store(
            &state,
            Scope::Sandbox("fourth"),
            vec![pick(&r, named(&["beta"]))],
        );
        assert!(matches!(clash, Err(Error::Refused(_))), "{clash:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        // Dropping a sandbox's picks is never refused.
        store(&state, Scope::Sandbox("box"), Vec::new()).unwrap();
        assert!(!load(&path).unwrap().sandboxes.contains_key("box"));
        let _ = fs::remove_dir_all(&state);
    }
}

//! The dashboard's *Repo skills* screen: which skills of the configured skill repos
//! ([`skill_repos`]) go into every sandbox (the selection's `global` list) or into one sandbox
//! (its own entry), as [`skill_select`] stores them.

use std::collections::BTreeMap;
use std::path::Path;

use crate::skill_repos;
use crate::skill_select::{self, Pick, Scope, Selection, Skills};

/// Whose picks the screen edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoScope {
    /// Every sandbox, existing and new: the selection's `global` list.
    Global,
    /// One sandbox's own entry, by its sbx name.
    Sandbox(String),
}

impl RepoScope {
    pub fn as_scope(&self) -> Scope<'_> {
        match self {
            RepoScope::Global => Scope::Global,
            RepoScope::Sandbox(name) => Scope::Sandbox(name),
        }
    }
}

/// One configured repo as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoView {
    pub id: String,
    pub source: String,
    /// The branch or tag it follows; `None` is the remote's default branch.
    pub git_ref: Option<String>,
    /// The cached commit; `None` while there is no cache git can read.
    pub commit: Option<String>,
    /// Why git cannot read a cache, as [`skill_select::read_cache`] says it.
    pub problem: Option<String>,
    /// The skills the cached commit holds, in discovery order.
    pub skills: Vec<String>,
    /// The folders discovery skipped, with the reason.
    pub skipped: Vec<(String, &'static str)>,
}

/// What the scope picks of one repo: all of it, or these names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ticks {
    pub all: bool,
    pub names: Vec<String>,
}

/// A focusable line on the screen, in drawing order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoSkillRow {
    /// "All skills" of the repo with this id.
    All(String),
    /// One discovered skill; `locked` when a global pick gives it (per-sandbox scope only).
    Skill {
        repo: String,
        name: String,
        locked: bool,
    },
    /// Run the sync now (per-sandbox scope only).
    SyncNow,
    Save,
    Close,
}

#[derive(Debug, Clone)]
pub struct RepoSkillsForm {
    pub scope: RepoScope,
    pub focus: usize,
    pub repos: Vec<RepoView>,
    /// This scope's own picks, by repo id (repos on the screen only).
    pub ticks: BTreeMap<String, Ticks>,
    /// The global picks, which lock rows in the per-sandbox scope; empty in the global scope.
    pub global: Vec<Pick>,
    /// Own picks of repos that are no longer configured: written back as they are.
    pub kept: Vec<Pick>,
    /// Why the selection (or the repo list) cannot be read; nothing is written while set.
    pub error: Option<String>,
    /// Ticks changed since the screen opened or was last saved.
    pub dirty: bool,
}

impl RepoSkillsForm {
    /// The form from what was read: the repos and the selection file (or why it cannot be read).
    /// Pure, so tests build it without disk.
    pub fn from_parts(
        scope: RepoScope,
        repos: Vec<RepoView>,
        selection: Result<Selection, String>,
    ) -> Self {
        let mut form = Self {
            scope,
            focus: 0,
            repos,
            ticks: BTreeMap::new(),
            global: Vec::new(),
            kept: Vec::new(),
            error: None,
            dirty: false,
        };
        let selection = match selection {
            Ok(selection) => selection,
            Err(why) => {
                form.error = Some(why);
                return form;
            }
        };
        if matches!(form.scope, RepoScope::Sandbox(_)) {
            form.global = selection.global.clone();
        }
        let own = form.scope.as_scope().picks(&selection);
        for pick in own {
            if !form.repos.iter().any(|r| r.id == pick.repo) {
                form.kept.push(pick);
                continue;
            }
            let ticks = match pick.skills {
                Skills::All => Ticks {
                    all: true,
                    names: Vec::new(),
                },
                Skills::Named(names) => Ticks { all: false, names },
            };
            form.ticks.insert(pick.repo, ticks);
        }
        form
    }

    /// Read the repos, their caches and the selection. Files and git only; never fetches.
    pub fn load(state: &Path, scope: RepoScope) -> Self {
        let config = match skill_repos::load(&skill_repos::config_path(state)) {
            Ok(config) => config,
            Err(e) => return Self::from_parts(scope, Vec::new(), Err(format!("{e:#}"))),
        };
        let repos = config
            .repos
            .iter()
            .filter_map(|repo| {
                let id = repo.id().ok()?;
                let view = match skill_select::read_cache(state, &id) {
                    Ok(cached) => RepoView {
                        id,
                        source: repo.source.clone(),
                        git_ref: repo.git_ref.clone(),
                        commit: Some(cached.commit),
                        problem: None,
                        skills: cached.found.into_iter().map(|f| f.name).collect(),
                        skipped: cached
                            .skipped
                            .into_iter()
                            .map(|s| (s.name, s.why))
                            .collect(),
                    },
                    Err(why) => RepoView {
                        id,
                        source: repo.source.clone(),
                        git_ref: repo.git_ref.clone(),
                        commit: None,
                        problem: Some(why),
                        skills: Vec::new(),
                        skipped: Vec::new(),
                    },
                };
                Some(view)
            })
            .collect();
        let selection =
            skill_select::load(&skill_select::selection_path(state)).map_err(|e| format!("{e:#}"));
        Self::from_parts(scope, repos, selection)
    }

    /// Every focusable line, in drawing order. With an unreadable file there is only *Close*.
    pub fn rows(&self) -> Vec<RepoSkillRow> {
        let mut rows = Vec::new();
        if self.error.is_none() {
            for repo in &self.repos {
                rows.push(RepoSkillRow::All(repo.id.clone()));
                for name in &repo.skills {
                    rows.push(RepoSkillRow::Skill {
                        repo: repo.id.clone(),
                        name: name.clone(),
                        locked: self.locked(&repo.id, Some(name)),
                    });
                }
            }
            if matches!(self.scope, RepoScope::Sandbox(_)) {
                rows.push(RepoSkillRow::SyncNow);
            }
            rows.push(RepoSkillRow::Save);
        }
        rows.push(RepoSkillRow::Close);
        rows
    }

    /// Whether a global pick gives the repo's skill (`None`: all of the repo) in this scope.
    pub fn locked(&self, repo: &str, name: Option<&str>) -> bool {
        self.global
            .iter()
            .find(|p| p.repo == repo)
            .is_some_and(|p| match (&p.skills, name) {
                (Skills::All, _) => true,
                (Skills::Named(names), Some(name)) => names.iter().any(|n| n == name),
                (Skills::Named(_), None) => false,
            })
    }

    /// What the screen says under its title: where the ticked skills go.
    pub fn intro(&self) -> &'static str {
        match self.scope {
            RepoScope::Global => "Ticked skills go into every sandbox, at its next start.",
            RepoScope::Sandbox(_) => {
                "Ticked skills go into this sandbox, beside the ones for all sandboxes (marked \
                 global)."
            }
        }
    }

    /// Why nothing can be saved, when the selection cannot be read.
    pub fn error_lines(&self) -> Vec<String> {
        match &self.error {
            Some(why) => vec![
                format!("The skill selection cannot be read: {why}"),
                "Nothing is saved until you fix or remove the file.".into(),
            ],
            None => Vec::new(),
        }
    }

    /// What ticking the focused row does, unindented; empty where its label says it.
    pub fn notes(&self, row: &RepoSkillRow) -> Vec<String> {
        match row {
            RepoSkillRow::All(repo) if self.locked(repo, None) => vec![LOCKED_NOTE.into()],
            RepoSkillRow::All(_) => vec![
                "When ticked, every skill of this repository goes in, also skills added to it \
                 later. Otherwise, only the skills ticked below."
                    .into(),
            ],
            RepoSkillRow::Skill { locked: true, .. } => vec![LOCKED_NOTE.into()],
            RepoSkillRow::SyncNow => match &self.scope {
                RepoScope::Sandbox(_) => vec![
                    "Puts the saved skills into the sandbox now, and removes the ones no longer \
                     ticked."
                        .into(),
                ],
                RepoScope::Global => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Whether the row is shown ticked: picked by this scope, or locked by a global pick.
    pub fn ticked(&self, row: &RepoSkillRow) -> bool {
        match row {
            RepoSkillRow::All(repo) => {
                self.locked(repo, None) || self.ticks.get(repo).is_some_and(|t| t.all)
            }
            RepoSkillRow::Skill { repo, name, locked } => {
                *locked
                    || self
                        .ticks
                        .get(repo)
                        .is_some_and(|t| t.all || t.names.contains(name))
            }
            _ => false,
        }
    }

    /// Flip a tick. A locked row is refused with what to do instead; unticking one skill of an
    /// `all` pick keeps every other skill the cache holds now.
    pub fn toggle(&mut self, row: &RepoSkillRow) -> Result<(), String> {
        let (repo, name) = match row {
            RepoSkillRow::All(repo) => (repo, None),
            RepoSkillRow::Skill { repo, name, .. } => (repo, Some(name)),
            _ => return Ok(()),
        };
        if self.error.is_some() {
            return Err("the skill selection cannot be read, so nothing can change".into());
        }
        if self.locked(repo, name.map(String::as_str)) {
            return Err(format!(
                "{} is ticked for all sandboxes: {}",
                name.map_or(repo.as_str(), String::as_str),
                CHANGE_GLOBAL
            ));
        }
        let discovered: Vec<String> = self
            .repos
            .iter()
            .find(|r| &r.id == repo)
            .map(|r| r.skills.clone())
            .unwrap_or_default();
        let locked: Vec<String> = discovered
            .iter()
            .filter(|n| self.locked(repo, Some(n.as_str())))
            .cloned()
            .collect();
        let ticks = self.ticks.entry(repo.clone()).or_default();
        match name {
            None => {
                ticks.all = !ticks.all;
                ticks.names.clear();
            }
            Some(name) if ticks.all => {
                ticks.all = false;
                ticks.names = discovered
                    .into_iter()
                    .filter(|n| n != name && !locked.contains(n))
                    .collect();
            }
            Some(name) => match ticks.names.iter().position(|n| n == name) {
                Some(i) => {
                    ticks.names.remove(i);
                }
                None => ticks.names.push(name.clone()),
            },
        }
        self.dirty = true;
        Ok(())
    }

    /// The scope's whole list as the ticks stand: one pick per repo in screen order, then the
    /// picks of repos no longer configured. Pure — what *Save* hands to
    /// [`skill_select::store`].
    pub fn picks(&self) -> Vec<Pick> {
        let mut out: Vec<Pick> = self
            .repos
            .iter()
            .filter_map(|repo| {
                let ticks = self.ticks.get(&repo.id)?;
                let skills = if ticks.all {
                    Skills::All
                } else if ticks.names.is_empty() {
                    return None;
                } else {
                    Skills::Named(ticks.names.clone())
                };
                Some(Pick {
                    repo: repo.id.clone(),
                    skills,
                })
            })
            .collect();
        out.extend(self.kept.iter().cloned());
        out
    }
}

/// Where a pick for all sandboxes is changed, by the action's and the row's words.
const CHANGE_GLOBAL: &str = "change it in Skills (K), under Repo skills for all sandboxes";

/// The note under a row a global pick locks.
pub const LOCKED_NOTE: &str =
    "Ticked for all sandboxes: change it in Skills (K), under Repo skills for all sandboxes.";

/// What the screen says when no skill repository is configured.
pub const NO_REPOS: &str = "No skill repository yet. Add one in a terminal with `aisbx skill-repo \
     add SOURCE`, then `aisbx skill-repo fetch`.";

/// What a repo without a readable copy says under its heading.
pub const NO_COPY: &str = "No readable copy yet: run `aisbx skill-repo fetch` in a terminal.";

impl RepoView {
    /// The repo's heading: its source, then its branch or tag and whether it is fetched.
    pub fn heading(&self) -> (String, String) {
        let mut meta: Vec<&str> = self.git_ref.iter().map(String::as_str).collect();
        if self.commit.is_none() {
            meta.push("not fetched");
        }
        (self.source.clone(), meta.join("  ·  "))
    }

    /// The lines under the repo's skills: why it has no readable copy, and each folder that is
    /// no skill, in words.
    pub fn footer(&self) -> Vec<String> {
        let mut out: Vec<String> = self.problem.iter().map(|_| NO_COPY.to_string()).collect();
        out.extend(self.skipped.iter().map(|(name, why)| {
            format!(
                "{} is not offered: {}",
                name.escape_debug(),
                skip_words(why)
            )
        }));
        out
    }
}

/// A skipped folder's reason ([`skill_select::discover`]'s code) in words.
pub fn skip_words(why: &str) -> &'static str {
    match why {
        "bad-name" => "its name is not a valid skill name",
        "bad-path" => "its path has characters a skill cannot have",
        "symlink" => "it is a link",
        "submodule" => "it is a Git submodule",
        "too-big" => "it is larger than 20 MiB",
        "dup" => "another skill has its name",
        _ => "aisbx cannot use it",
    }
}

/// `aisbx skill sync <sandbox>` — this very binary, the owner's full sync (not a start's).
pub fn sync_argv(sandbox: &str) -> Vec<String> {
    vec![
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "aisbx".to_string()),
        "skill".into(),
        "sync".into(),
        sandbox.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "github-com-o-r-0123abcd";
    const S: &str = "github-com-o-s-89abcdef";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn repo(id: &str, skills: &[&str]) -> RepoView {
        RepoView {
            id: id.into(),
            source: format!("o/{id}"),
            git_ref: None,
            commit: Some("0123456789abcdef".into()),
            problem: None,
            skills: strings(skills),
            skipped: Vec::new(),
        }
    }

    fn pick(repo: &str, skills: Skills) -> Pick {
        Pick {
            repo: repo.into(),
            skills,
        }
    }

    fn skill_row(form: &RepoSkillsForm, name: &str) -> RepoSkillRow {
        form.rows()
            .into_iter()
            .find(|r| matches!(r, RepoSkillRow::Skill { name: n, .. } if n == name))
            .unwrap()
    }

    #[test]
    fn toggles_make_the_picks_and_global_rows_are_locked() {
        let mut sel = Selection {
            global: vec![pick(R, Skills::Named(strings(&["alpha"])))],
            ..Selection::default()
        };
        sel.sandboxes.insert(
            "box".into(),
            vec![
                pick(S, Skills::All),
                pick("github-com-o-gone-00000000", Skills::All),
            ],
        );
        let repos = vec![repo(R, &["alpha", "beta"]), repo(S, &["gamma", "delta"])];
        let mut form =
            RepoSkillsForm::from_parts(RepoScope::Sandbox("box".into()), repos.clone(), Ok(sel));
        assert_eq!(
            form.rows(),
            [
                RepoSkillRow::All(R.into()),
                RepoSkillRow::Skill {
                    repo: R.into(),
                    name: "alpha".into(),
                    locked: true
                },
                RepoSkillRow::Skill {
                    repo: R.into(),
                    name: "beta".into(),
                    locked: false
                },
                RepoSkillRow::All(S.into()),
                RepoSkillRow::Skill {
                    repo: S.into(),
                    name: "gamma".into(),
                    locked: false
                },
                RepoSkillRow::Skill {
                    repo: S.into(),
                    name: "delta".into(),
                    locked: false
                },
                RepoSkillRow::SyncNow,
                RepoSkillRow::Save,
                RepoSkillRow::Close,
            ]
        );
        let alpha = skill_row(&form, "alpha");
        assert!(form.ticked(&alpha), "a global pick shows ticked");
        assert!(form
            .toggle(&alpha)
            .unwrap_err()
            .contains("ticked for all sandboxes"));
        assert!(!form.dirty);

        form.toggle(&skill_row(&form, "beta")).unwrap();
        form.toggle(&skill_row(&form, "gamma")).unwrap();
        assert!(form.dirty);
        assert_eq!(
            form.picks(),
            [
                pick(R, Skills::Named(strings(&["beta"]))),
                pick(S, Skills::Named(strings(&["delta"]))),
                pick("github-com-o-gone-00000000", Skills::All),
            ]
        );
        form.toggle(&RepoSkillRow::All(S.into())).unwrap();
        form.toggle(&skill_row(&form, "beta")).unwrap();
        assert_eq!(
            form.picks(),
            [
                pick(S, Skills::All),
                pick("github-com-o-gone-00000000", Skills::All),
            ]
        );

        let mut sel = Selection {
            global: vec![pick(R, Skills::Named(strings(&["alpha"])))],
            ..Selection::default()
        };
        sel.sandboxes
            .insert("box".into(), vec![pick(S, Skills::All)]);
        let mut global = RepoSkillsForm::from_parts(RepoScope::Global, repos, Ok(sel));
        assert!(!global.rows().contains(&RepoSkillRow::SyncNow));
        let alpha = skill_row(&global, "alpha");
        assert_eq!(
            alpha,
            RepoSkillRow::Skill {
                repo: R.into(),
                name: "alpha".into(),
                locked: false
            }
        );
        global.toggle(&alpha).unwrap();
        global.toggle(&RepoSkillRow::All(S.into())).unwrap();
        assert_eq!(global.picks(), [pick(S, Skills::All)]);
    }

    #[test]
    fn a_global_all_locks_the_whole_repo() {
        let sel = Selection {
            global: vec![pick(R, Skills::All)],
            ..Selection::default()
        };
        let mut form = RepoSkillsForm::from_parts(
            RepoScope::Sandbox("box".into()),
            vec![repo(R, &["alpha"])],
            Ok(sel),
        );
        let all = RepoSkillRow::All(R.into());
        assert!(form.ticked(&all));
        assert!(form.toggle(&all).is_err());
        assert!(form.toggle(&skill_row(&form, "alpha")).is_err());
        assert!(form.picks().is_empty());
    }

    #[test]
    fn an_unreadable_selection_disables_writing() {
        let mut form = RepoSkillsForm::from_parts(
            RepoScope::Global,
            vec![repo(R, &["alpha"])],
            Err("skill-selection.json does not parse".into()),
        );
        assert_eq!(form.rows(), [RepoSkillRow::Close]);
        assert!(form.toggle(&RepoSkillRow::All(R.into())).is_err());
        assert!(!form.dirty);
    }

    #[test]
    fn sync_now_runs_the_full_sync_of_this_binary() {
        let argv = sync_argv("box");
        assert_eq!(&argv[1..], ["skill", "sync", "box"]);
    }

    #[test]
    fn notes_say_what_space_and_enter_do() {
        let sel = Selection {
            global: vec![pick(R, Skills::All)],
            ..Selection::default()
        };
        let form = RepoSkillsForm::from_parts(
            RepoScope::Sandbox("box".into()),
            vec![repo(R, &["alpha"]), repo(S, &["beta"])],
            Ok(sel),
        );
        assert_eq!(form.notes(&RepoSkillRow::All(R.into())), [LOCKED_NOTE]);
        assert_eq!(form.notes(&skill_row(&form, "alpha")), [LOCKED_NOTE]);
        let all = form.notes(&RepoSkillRow::All(S.into())).join(" ");
        assert!(
            all.starts_with("When ticked, ") && all.contains(" Otherwise, "),
            "{all}"
        );
        assert!(form.notes(&skill_row(&form, "beta")).is_empty());
        let sync = form.notes(&RepoSkillRow::SyncNow).join(" ");
        assert!(sync.starts_with("Puts the saved skills"), "{sync}");
        for inner in ["aisbx skill sync", "↵", "cached commit", "global selection"] {
            assert!(!sync.contains(inner) && !all.contains(inner), "{inner}");
            assert!(!LOCKED_NOTE.contains(inner), "{inner}");
        }
        let global =
            RepoSkillsForm::from_parts(RepoScope::Global, Vec::new(), Ok(Selection::default()));
        assert!(global.notes(&RepoSkillRow::SyncNow).is_empty());
    }

    #[test]
    fn a_heading_names_the_source_and_a_footer_says_in_words_what_was_skipped() {
        let mut fetched = repo(R, &["alpha"]);
        assert_eq!(
            fetched.heading(),
            ("o/github-com-o-r-0123abcd".into(), String::new())
        );
        fetched.git_ref = Some("v2".into());
        fetched.skipped = vec![("big".into(), "too-big"), ("odd".into(), "new-code")];
        assert_eq!(fetched.heading().1, "v2");
        assert_eq!(
            fetched.footer(),
            [
                "big is not offered: it is larger than 20 MiB",
                "odd is not offered: aisbx cannot use it"
            ]
        );
        let gone = RepoView {
            commit: None,
            problem: Some(format!("{R} has no cache git can read")),
            ..repo(R, &[])
        };
        assert_eq!(gone.heading().1, "not fetched");
        assert_eq!(gone.footer(), [NO_COPY]);
        for code in [
            "bad-name",
            "bad-path",
            "symlink",
            "submodule",
            "too-big",
            "dup",
        ] {
            assert_ne!(skip_words(code), skip_words("new-code"), "{code}");
        }
    }

    #[test]
    fn the_intro_says_where_the_ticks_go() {
        let global =
            RepoSkillsForm::from_parts(RepoScope::Global, Vec::new(), Ok(Selection::default()));
        assert!(global.intro().contains("every sandbox"));
        let own = RepoSkillsForm::from_parts(
            RepoScope::Sandbox("box".into()),
            Vec::new(),
            Ok(Selection::default()),
        );
        assert!(own.intro().contains("this sandbox"));
        assert!(own.error_lines().is_empty());
        let broken = RepoSkillsForm::from_parts(RepoScope::Global, Vec::new(), Err("bad".into()));
        assert_eq!(
            broken.error_lines()[0],
            "The skill selection cannot be read: bad"
        );
    }
}

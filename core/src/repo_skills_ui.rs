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
    /// The cached commit; `None` while there is no cache git can read.
    pub commit: Option<String>,
    /// Why there is no readable cache, with the `aisbx skill-repo fetch` hint.
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

    /// What space and ↵ do to the focused row, unindented.
    pub fn notes(&self, row: &RepoSkillRow) -> Vec<String> {
        match row {
            RepoSkillRow::All(repo) if self.locked(repo, None) => vec![LOCKED_NOTE.into()],
            RepoSkillRow::All(_) => {
                vec!["space ticks every skill the cached commit holds at each sync".into()]
            }
            RepoSkillRow::Skill { locked: true, .. } => vec![LOCKED_NOTE.into()],
            RepoSkillRow::SyncNow => match &self.scope {
                RepoScope::Sandbox(name) => vec![format!(
                    "↵ runs `aisbx skill sync {name}`: copies what is saved, removes what is not"
                )],
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
            return Err("the selection file cannot be read; nothing can be changed".into());
        }
        if self.locked(repo, name.map(String::as_str)) {
            return Err(format!(
                "{} comes from the global selection; change it in Repo skills for all \
                 sandboxes (K)",
                name.map_or(repo.as_str(), String::as_str)
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

/// The note under a row a global pick locks.
pub const LOCKED_NOTE: &str =
    "from the global selection — change it in K → Repo skills for all sandboxes";

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
        assert!(form.toggle(&alpha).unwrap_err().contains("global"));
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
        assert!(form.notes(&RepoSkillRow::All(S.into()))[0].starts_with("space ticks"));
        assert!(form.notes(&skill_row(&form, "beta")).is_empty());
        assert_eq!(
            form.notes(&RepoSkillRow::SyncNow),
            ["↵ runs `aisbx skill sync box`: copies what is saved, removes what is not"]
        );
        let global =
            RepoSkillsForm::from_parts(RepoScope::Global, Vec::new(), Ok(Selection::default()));
        assert!(global.notes(&RepoSkillRow::SyncNow).is_empty());
    }
}

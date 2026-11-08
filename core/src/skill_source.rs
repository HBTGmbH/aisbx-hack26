//! Where a sandbox's skills come from, besides sbx's own store: one [`SkillSource`] per kind of
//! source, switched on by an addon (the host folders) or by the sandbox's skill selection (the
//! cached skill repos).

use std::path::Path;

use crate::kits::HOST_SKILLS;
use crate::model::{self, KitMounts};
use crate::{sbx, skill_select, state};

/// One kind of skill source. A new one is a variant here plus its entry in [`SOURCES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// The host's global skill folders, mounted read-only and linked into the sandbox by the
    /// `host-skills` kit's hook.
    HostFolder,
    /// Skills of the cached skill repos, chosen per sandbox in `skill-selection.json`
    /// ([`skill_select`]) and copied in by `aisbx skill sync`. No addon, no mount.
    RepoCache,
}

/// Every source, in the order their mounts and install steps come.
pub const SOURCES: &[SkillSource] = &[SkillSource::HostFolder, SkillSource::RepoCache];

impl SkillSource {
    /// The addon that switches this source on; `None` for one a sandbox's selection switches on.
    pub fn addon(self) -> Option<&'static str> {
        match self {
            SkillSource::HostFolder => Some(HOST_SKILLS),
            SkillSource::RepoCache => None,
        }
    }

    /// The folders below home this source mounts, in the order its hook reads them; `None` for an
    /// OS whose layout nobody has looked at.
    fn host_dirs(self, os: &str) -> Option<&'static [&'static str]> {
        let dirs: &'static [&'static str] = match (self, os) {
            (SkillSource::HostFolder, "macos" | "linux") => &[".agents/skills", ".claude/skills"],
            (SkillSource::HostFolder, _) | (SkillSource::RepoCache, _) => return None,
        };
        Some(dirs)
    }

    /// This source's read-only mounts, through the checks the package caches use
    /// ([`model::host_folder_mounts`]): a missing folder is skipped silently, a refused one with a
    /// note naming it.
    pub fn mounts(self, home: Option<&Path>, os: &str) -> KitMounts {
        match self.addon() {
            Some(addon) => model::host_folder_mounts(addon, self.host_dirs(os), "skill", home, os),
            None => KitMounts::default(),
        }
    }

    /// The step that installs this source's skills in a sandbox, run to completion before a
    /// session starts (the kit's startup hook runs the same script but races the agent).
    pub fn install_argv(self, sandbox: &str) -> Vec<String> {
        match self {
            SkillSource::HostFolder => sbx::host_skills_argv(sandbox),
            SkillSource::RepoCache => {
                crate::headless::job_argv(&["skill", "sync", "--start", sandbox])
            }
        }
    }

    /// What headless `run` calls that step in its messages.
    pub fn step_label(self) -> &'static str {
        match self {
            SkillSource::HostFolder => "the host-skills sync",
            SkillSource::RepoCache => "the skill-repo sync",
        }
    }

    /// Added to the create form's reason when the source has nothing to mount.
    fn detection_note(self) -> &'static str {
        match self {
            SkillSource::HostFolder => "skills from the sbx store are still shared (Skills mode)",
            // Never asked: it has no addon, so no create-form row.
            SkillSource::RepoCache => "",
        }
    }
}

/// The source an addon switches on, if it is a source's addon.
fn source_for(addon: &str) -> Option<SkillSource> {
    SOURCES.iter().copied().find(|s| s.addon() == Some(addon))
}

/// The sources these addons switch on, in [`SOURCES`] order.
pub fn active(addons: &[String]) -> impl Iterator<Item = SkillSource> + '_ {
    SOURCES
        .iter()
        .copied()
        .filter(move |s| s.addon().is_some_and(|own| addons.iter().any(|a| a == own)))
}

/// The sources a sandbox's start syncs, in [`SOURCES`] order: those its addons switch on, then
/// [`SkillSource::RepoCache`] when `selected` — the sandbox has a skill selection.
pub fn active_with(addons: &[String], selected: bool) -> Vec<SkillSource> {
    SOURCES
        .iter()
        .copied()
        .filter(|s| match s {
            SkillSource::RepoCache => selected,
            _ => active(addons).any(|a| a == *s),
        })
        .collect()
}

/// Whether the sandbox has a skill selection. Never read under `cargo test`, where the state
/// folder is the real one of whoever runs the suite.
fn has_selection(sandbox: &str) -> bool {
    !cfg!(any(test, feature = "test-support"))
        && skill_select::has_selection(&state::state_dir(), sandbox)
}

/// [`active_with`] for this sandbox, its selection read from the state folder.
pub fn sources_for(sandbox: &str, addons: &[String]) -> Vec<SkillSource> {
    active_with(addons, has_selection(sandbox))
}

/// Whether the addon is a source's and so needs its host folders as extra workspaces at creation.
pub fn needs_mounts(addon: &str) -> bool {
    source_for(addon).is_some()
}

/// The folders below home an addon's source mounts on that OS — `None` for an addon that is no
/// source, or an OS nobody has looked at.
pub fn host_dirs(addon: &str, os: &str) -> Option<&'static [&'static str]> {
    source_for(addon)?.host_dirs(os)
}

/// What the create form adds to an addon's reason when its source would mount nothing.
pub fn detection_note(addon: &str) -> Option<&'static str> {
    source_for(addon).map(SkillSource::detection_note)
}

/// Mounts of every source the addons switch on, in [`SOURCES`] order.
pub fn mounts(addons: &[String], home: Option<&Path>, os: &str) -> KitMounts {
    let mut out = KitMounts::default();
    for source in active(addons) {
        let found = source.mounts(home, os);
        out.mounts.extend(found.mounts);
        out.notes.extend(found.notes);
    }
    out
}

/// Pre-start steps of every source the sandbox's addons and selection switch on, in [`SOURCES`]
/// order.
pub fn install_steps(sandbox: &str, addons: &[String]) -> Vec<Vec<String>> {
    install_steps_with(sandbox, addons, has_selection(sandbox))
}

/// [`install_steps`] with the selection given (`selected`).
pub fn install_steps_with(sandbox: &str, addons: &[String], selected: bool) -> Vec<Vec<String>> {
    active_with(addons, selected)
        .into_iter()
        .map(|s| s.install_argv(sandbox))
        .collect()
}

/// `sbx inspect`'s kit dirs → kit names: each dir's last path component (a trailing `/` allowed).
pub fn addons_from_kit_dirs(dirs: &[String]) -> Vec<String> {
    dirs.iter()
        .filter_map(|dir| Path::new(dir).file_name()?.to_str())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A temp home holding these folders, removed first; `(base to delete, home)`.
    fn temp_home(tag: &str, dirs: &[&str]) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("aisbx-src-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        for dir in dirs {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        (base, home)
    }

    #[test]
    fn the_sources_are_the_host_folder_and_the_repo_cache() {
        assert_eq!(SOURCES, [SkillSource::HostFolder, SkillSource::RepoCache]);
        assert_eq!(SkillSource::HostFolder.addon(), Some("host-skills"));
        assert_eq!(SkillSource::RepoCache.addon(), None);
    }

    #[test]
    fn every_source_is_reached_through_the_shared_functions() {
        for (source, addon) in SOURCES.iter().filter_map(|&s| Some((s, s.addon()?))) {
            let addons = strings(&[addon]);
            assert!(needs_mounts(addon), "{source:?}");
            assert_eq!(active(&addons).collect::<Vec<_>>(), [source]);
            assert_eq!(install_steps("box", &addons), [source.install_argv("box")]);
            assert_eq!(host_dirs(addon, "linux"), source.host_dirs("linux"));
            assert_eq!(detection_note(addon), Some(source.detection_note()));
            assert_eq!(
                mounts(&addons, None, "linux"),
                source.mounts(None, "linux"),
                "{source:?}"
            );
        }
        for other in ["host-npm", "host-pip", "rtk", "host-skills-old", ""] {
            assert!(!needs_mounts(other), "{other}");
            assert_eq!(host_dirs(other, "linux"), None, "{other}");
            assert_eq!(detection_note(other), None, "{other}");
        }
    }

    #[test]
    fn install_steps_only_for_the_host_skills_addon() {
        assert_eq!(
            install_steps("box", &strings(&["host-skills"])),
            [sbx::host_skills_argv("box")]
        );
        assert_eq!(
            install_steps("box", &strings(&["rtk", "host-skills", "host-npm"])),
            [sbx::host_skills_argv("box")]
        );
        for addons in [&[][..], &["host-npm"][..], &["host-skills-old"][..]] {
            assert!(
                install_steps("box", &strings(addons)).is_empty(),
                "{addons:?}"
            );
        }
        let argv = SkillSource::HostFolder.install_argv("box");
        assert_eq!(argv[..3], ["sbx", "exec", "box"]);
        assert_eq!(argv[7..], ["/usr/local/lib/aisbx/host-skills/startup.sh"]);
    }

    #[test]
    #[cfg(unix)]
    fn the_host_folder_mounts_both_folders_in_order() {
        let (base, home) = temp_home("mounts", &[".agents/skills", ".claude/skills"]);
        let real = home.canonicalize().unwrap();
        let ro = |dir: &str| format!("{}:ro", real.join(dir).display());
        for os in ["macos", "linux"] {
            let out = mounts(&strings(&["host-skills"]), Some(home.as_path()), os);
            assert_eq!(
                out.mounts,
                vec![ro(".agents/skills"), ro(".claude/skills")],
                "{os}"
            );
            assert!(out.notes.is_empty(), "{os}: {:?}", out.notes);
            assert_eq!(
                mounts(&strings(&["host-npm"]), Some(home.as_path()), os),
                KitMounts::default()
            );
        }
        let windows = mounts(&strings(&["host-skills"]), Some(home.as_path()), "windows");
        assert_eq!(
            windows.notes,
            ["host-skills: no host skill folders are known on windows, so nothing is mounted"]
        );
        let homeless = mounts(&strings(&["host-skills"]), None, "linux");
        assert_eq!(
            homeless.notes,
            ["host-skills: no home folder (HOME), so nothing is mounted"]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The host-skills sync step, read off a kit list the way headless `run` does.
    fn host_skills_step(dirs: &[String]) -> bool {
        active(&addons_from_kit_dirs(dirs)).any(|s| s == SkillSource::HostFolder)
    }

    #[test]
    fn host_skills_step_only_for_the_kit() {
        let with = strings(&["/repo/kits/claude-anthropic", "/repo/kits/host-skills"]);
        assert!(host_skills_step(&with));
        assert!(host_skills_step(&strings(&["/repo/kits/host-skills/"])));
        let without = strings(&[
            "/repo/kits/claude-anthropic",
            "/repo/kits/host-npm",
            "/repo/kits/host-skills-old",
            "/repo/host-skills/kits/rtk",
        ]);
        assert!(!host_skills_step(&without));
        assert!(!host_skills_step(&[]));
        let argv = sbx::host_skills_argv("box");
        assert_eq!(argv[..3], ["sbx", "exec", "box"]);
        assert_eq!(argv[7..], [sbx::HOST_SKILLS_HOOK]);
    }

    #[test]
    fn the_repo_cache_runs_only_for_a_sandbox_with_a_selection() {
        let none: Vec<String> = Vec::new();
        let host = strings(&["host-skills"]);
        assert!(active_with(&none, false).is_empty());
        assert_eq!(active_with(&none, true), [SkillSource::RepoCache]);
        assert_eq!(
            active_with(&host, true),
            [SkillSource::HostFolder, SkillSource::RepoCache]
        );
        assert_eq!(active_with(&host, false), [SkillSource::HostFolder]);
        assert!(install_steps_with("box", &none, false).is_empty());
        let step = SkillSource::RepoCache.install_argv("box");
        assert_eq!(&step[1..], ["skill", "sync", "--start", "box"]);
        assert!(!step[0].is_empty());
        assert_eq!(
            install_steps_with("box", &host, true),
            [sbx::host_skills_argv("box"), step]
        );
        assert!(!needs_mounts(""));
        assert_eq!(
            SkillSource::RepoCache.mounts(None, "linux"),
            KitMounts::default()
        );
        assert_eq!(
            mounts(&strings(&["host-npm"]), None, "linux"),
            KitMounts::default()
        );
        assert_eq!(sources_for("box", &host), [SkillSource::HostFolder]);
    }

    #[test]
    fn kit_dirs_become_kit_names() {
        let dirs = strings(&[
            "/repo/kits/host-skills/",
            "/repo/kits/claude-anthropic",
            "/repo/host-skills/kits/rtk",
        ]);
        assert_eq!(
            addons_from_kit_dirs(&dirs),
            ["host-skills", "claude-anthropic", "rtk"]
        );
        assert!(addons_from_kit_dirs(&[]).is_empty());
    }
}

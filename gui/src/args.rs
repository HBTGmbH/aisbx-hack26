//! The command line of `aisbx-ui`.

use std::path::PathBuf;

pub const HELP: &str = "\
aisbx-ui — desktop GUI for the aisbx kits (read-only for now; aisbx does the rest)

USAGE:
    aisbx-ui [OPTIONS] [FOLDER]

ARGS:
    FOLDER           show this folder's sandboxes first (default: the current one; at a
                     filesystem root, as from Finder or the Dock, the window asks for one)

OPTIONS:
    -a, --all        list the other folders' sandboxes too (the default)
        --here       start with them collapsed — only this folder's, plus a heading
        --no-update  for sessions started from here: skip the agent update and model-list refresh
    -h, --help       print this help
    -V, --version    print the aisbx-ui version

Set AISBX_HOME to the aisbx checkout if aisbx-ui cannot find its kits/ dir.
";

/// What the window opens with. `show_other: None` keeps whatever the last run left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub show_other: Option<bool>,
    pub auto_update: bool,
    /// The folder to show, as given: `main` makes a relative one absolute.
    pub folder: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    Help,
    Version,
    /// `skill …`, for core's headless code: the one subcommand core's own jobs run as this very
    /// binary (`repo_skills_ui::sync_argv`, the repo-cache skill source's pre-start step).
    Headless(Vec<String>),
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|a| a == "skill") {
        return Ok(Parsed::Headless(args.collect()));
    }
    let mut out = Args {
        show_other: None,
        auto_update: true,
        folder: None,
    };
    for arg in args {
        match arg.as_str() {
            "-a" | "--all" => out.show_other = Some(true),
            "--here" => out.show_other = Some(false),
            "--no-update" => out.auto_update = false,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other if out.folder.is_none() => out.folder = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(Parsed::Run(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    fn args(show_other: Option<bool>, auto_update: bool) -> Result<Parsed, String> {
        Ok(Parsed::Run(Args {
            show_other,
            auto_update,
            folder: None,
        }))
    }

    fn folder(show_other: Option<bool>, dir: &str) -> Result<Parsed, String> {
        Ok(Parsed::Run(Args {
            show_other,
            auto_update: true,
            folder: Some(PathBuf::from(dir)),
        }))
    }

    #[test]
    fn parses_every_option() {
        assert_eq!(run(&[]), args(None, true));
        assert_eq!(run(&["--here"]), args(Some(false), true));
        assert_eq!(run(&["-a"]), args(Some(true), true));
        assert_eq!(run(&["--all"]), args(Some(true), true));
        assert_eq!(run(&["--here", "--all"]), args(Some(true), true));
        assert_eq!(run(&["--no-update"]), args(None, false));
        assert_eq!(run(&["-h"]), Ok(Parsed::Help));
        assert_eq!(run(&["--help", "--bogus"]), Ok(Parsed::Help));
        assert_eq!(run(&["-V"]), Ok(Parsed::Version));
        assert_eq!(run(&["--version"]), Ok(Parsed::Version));
    }

    #[test]
    fn refuses_an_unknown_option() {
        assert_eq!(run(&["--bogus"]), Err("unknown option: --bogus".into()));
        assert_eq!(run(&["x", "-z"]), Err("unknown option: -z".into()));
    }

    #[test]
    fn takes_one_folder_anywhere_on_the_line() {
        assert_eq!(run(&["x"]), folder(None, "x"));
        assert_eq!(run(&["/work/api"]), folder(None, "/work/api"));
        assert_eq!(run(&["--here", "x"]), folder(Some(false), "x"));
        assert_eq!(run(&["x", "--here"]), folder(Some(false), "x"));
        assert_eq!(
            run(&["x", "y"]),
            Err("unexpected argument: y".into()),
            "a second folder"
        );
        assert!(HELP.contains("[FOLDER]"));
    }

    #[test]
    fn the_skill_syncs_core_runs_as_this_binary_go_to_headless() {
        use aisbx_core::{headless, repo_skills_ui, skill_source::SkillSource};
        for argv in [
            repo_skills_ui::sync_argv("box"),
            SkillSource::RepoCache.install_argv("box"),
        ] {
            let tail = argv[1..].to_vec();
            assert_eq!(
                run(&argv[1..].iter().map(String::as_str).collect::<Vec<_>>()),
                Ok(Parsed::Headless(tail.clone()))
            );
            assert!(headless::is_subcommand(&tail[0]), "{tail:?}");
        }
        assert_eq!(
            run(&["skills"]),
            folder(None, "skills"),
            "only the subcommand itself"
        );
    }
}

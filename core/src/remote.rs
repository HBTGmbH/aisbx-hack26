//! Connecting an editor or desktop app to a sandbox over SSH, at `<name>.sbx`. Every target gets
//! the workspace's host path, which is also its path inside the sandbox.

use std::path::Path;

use crate::sbx;

/// An app that can be pointed at a sandbox, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A plain interactive shell — `ssh <name>.sbx`, which needs nothing installed anywhere.
    Terminal,
    VsCode,
    Cursor,
    /// IntelliJ IDEA and the rest of the JetBrains IDEs, through JetBrains Gateway.
    Jetbrains,
    ClaudeDesktop,
}

impl Target {
    pub const ALL: [Target; 5] = [
        Target::Terminal,
        Target::VsCode,
        Target::Cursor,
        Target::Jetbrains,
        Target::ClaudeDesktop,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Target::Terminal => "Terminal (ssh)",
            Target::VsCode => "VS Code",
            Target::Cursor => "Cursor",
            Target::Jetbrains => "IntelliJ IDEA / JetBrains",
            Target::ClaudeDesktop => "Claude Desktop",
        }
    }

    /// The host command that launches it, or `None` where there is nothing reliable to run and the
    /// connection is made inside the app instead.
    pub fn command(self, sandbox: &str, workspace: &Path) -> Option<Vec<String>> {
        let host = sbx::ssh_host(sandbox);
        match self {
            Target::Terminal => Some(vec!["ssh".into(), host]),
            Target::VsCode => Some(vec![
                "code".into(),
                "--remote".into(),
                format!("ssh-remote+{host}"),
                workspace.display().to_string(),
            ]),
            Target::Cursor => Some(vec![
                "cursor".into(),
                "--remote".into(),
                format!("ssh-remote+{host}"),
                workspace.display().to_string(),
            ]),
            Target::Jetbrains | Target::ClaudeDesktop => None,
        }
    }

    /// The launcher this target needs on the host's PATH, if any.
    pub fn launcher(self) -> Option<&'static str> {
        match self {
            Target::Terminal => Some("ssh"),
            Target::VsCode => Some("code"),
            Target::Cursor => Some("cursor"),
            Target::Jetbrains | Target::ClaudeDesktop => None,
        }
    }

    /// Is the launcher installed here? A target without a launcher is always available.
    pub fn available(self) -> bool {
        self.launcher().is_none_or(on_path)
    }

    /// Does the launcher need a terminal window? Only `ssh`: an editor's launcher returns at once.
    pub fn in_terminal(self) -> bool {
        matches!(self, Target::Terminal)
    }

    /// Does this target download a server into the sandbox, so that it needs the `remote-dev` kit
    /// under a locked-down policy?
    pub fn needs_kit(self) -> bool {
        !matches!(self, Target::Terminal)
    }

    /// What to do once the target is chosen — one line per row, shown under it.
    pub fn steps(self, sandbox: &str, workspace: &Path) -> Vec<String> {
        let host = sbx::ssh_host(sandbox);
        let path = workspace.display().to_string();
        // A mountless sandbox's workspace is the image's own folder, not a host path.
        let mountless = workspace == Path::new(crate::model::MOUNTLESS_WORKDIR);
        match self {
            // Worded for both frontends: the TUI runs it in its own terminal, the window opens one.
            Target::Terminal => vec![
                format!("Runs `ssh {host}` in a terminal, starting in the workspace."),
                match mountless {
                    true => format!("The workspace is at {path}."),
                    false => format!("The workspace is at {path}, its host path."),
                },
                format!("Also usable as `sftp {host}` and `scp file {host}:{path}`."),
            ],
            Target::VsCode => vec![
                "Needs the Remote - SSH extension (ms-vscode-remote.remote-ssh).".into(),
                format!("Runs:  code --remote ssh-remote+{host} {path}"),
                "The first connect downloads the VS Code server inside the sandbox.".into(),
            ],
            Target::Cursor => vec![
                "Needs Cursor's Remote - SSH extension.".into(),
                format!("Runs:  cursor --remote ssh-remote+{host} {path}"),
            ],
            Target::Jetbrains => vec![
                "In JetBrains Gateway: New connection → SSH.".into(),
                format!("Host  {host}      User  agent      Port  22"),
                format!("Project  {path}"),
                "Gateway downloads the IDE backend into the sandbox on first connect.".into(),
            ],
            Target::ClaudeDesktop => {
                let mut steps = vec![
                    "In Claude Desktop: environment drop-down → + Add SSH connection.".into(),
                    format!("SSH Host  {host}   — leave SSH Port and Identity File empty."),
                    format!("Then pick the folder {path} in the remote picker."),
                ];
                if cfg!(any(target_os = "macos", windows)) {
                    steps.push(
                        "Or quit it and run `sbx setup ssh --auto-create`, which adds a managed"
                            .into(),
                    );
                    steps.push(
                        "“Docker Sandbox (Claude)” to its own mountless sandbox, ssh-claude."
                            .into(),
                    );
                }
                steps
            }
        }
    }
}

/// Is this program on the host's PATH? The same lookup as [`crate::terminal::start_detached`].
fn on_path(program: &str) -> bool {
    use crate::terminal::{Os, TermEnv};
    crate::terminal::on_path(Os::current(), program, &TermEnv::from_process()).is_some()
}

/// The kit that opens the egress an editor's server download needs.
pub const REMOTE_DEV_KIT: &str = "remote-dev";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_remote_target_opens_the_workspaces_host_path() {
        let ws = PathBuf::from("/Users/bob/src/my-project");
        for target in Target::ALL {
            if let Some(argv) = target.command("claude-litellm-my-project", &ws) {
                if target != Target::Terminal {
                    assert!(
                        argv.contains(&ws.display().to_string()),
                        "{target:?} → {argv:?}"
                    );
                }
                assert!(
                    argv.iter()
                        .any(|a| a.contains("claude-litellm-my-project.sbx")),
                    "{target:?} → {argv:?}"
                );
            }
            let steps = target.steps("claude-litellm-my-project", &ws).join(" ");
            assert!(
                steps.contains(".sbx") || steps.contains(&ws.display().to_string()),
                "{target:?} → {steps}"
            );
        }
    }

    #[test]
    fn the_terminal_row_says_the_session_starts_in_the_workspace() {
        let steps = Target::Terminal
            .steps("s", Path::new("/Users/bob/src/my-project"))
            .join(" ");
        assert!(steps.contains("starting in the workspace"), "{steps}");
        assert!(!steps.contains("/home/agent"), "{steps}");
        assert!(steps.contains("Runs `ssh s.sbx` in a terminal"), "{steps}");
        assert!(!steps.contains("this terminal"), "{steps}");
        assert!(steps.contains("its host path"), "{steps}");
        let mountless = Target::Terminal
            .steps("s", Path::new(crate::model::MOUNTLESS_WORKDIR))
            .join(" ");
        assert!(!mountless.contains("host path"), "{mountless}");
    }

    #[test]
    fn only_the_server_installing_targets_need_the_egress_kit() {
        assert!(!Target::Terminal.needs_kit());
        for target in [Target::VsCode, Target::Cursor, Target::Jetbrains] {
            assert!(target.needs_kit(), "{target:?}");
        }
    }

    #[test]
    fn only_the_ssh_session_wants_a_terminal_window() {
        for target in Target::ALL {
            assert_eq!(
                target.in_terminal(),
                target == Target::Terminal,
                "{target:?}"
            );
        }
    }

    #[test]
    fn targets_without_a_launcher_are_always_offered() {
        assert!(Target::Jetbrains.available());
        assert!(Target::ClaudeDesktop.available());
        assert!(Target::Jetbrains.command("s", Path::new("/p")).is_none());
    }
}

//! The list screen's actions, one per action key, for both frontends. [`crate::app::App::can`]
//! says whether one can run now, [`crate::app::App::perform`] runs it. Navigation, quitting, the
//! tabs and `m` / `M` are not actions: the list screen's key handler keeps them.

use crate::input::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Start,
    Shell,
    New,
    EditKits,
    Auth,
    Secrets,
    Connect,
    StartDir,
    Skills,
    RepoSkills,
    Folders,
    Settings,
    Approvals,
    KeepRunning,
    RefreshModels,
    Prune,
    Refresh,
    Rename,
    Rebuild,
    Stop,
    Delete,
    PublishPort,
    UnpublishPort,
    CheckHost,
    ToggleOther,
    Help,
}

/// Why an action does not run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing to say: the key does nothing here (an action on the New row).
    Silent,
    /// Not wrong, just nothing to do — said on the status line.
    Info(String),
    /// Said as an error.
    Error(String),
}

impl Action {
    pub const ALL: [Action; 26] = [
        Action::Start,
        Action::Shell,
        Action::New,
        Action::EditKits,
        Action::Auth,
        Action::Secrets,
        Action::Connect,
        Action::StartDir,
        Action::Skills,
        Action::RepoSkills,
        Action::Folders,
        Action::Settings,
        Action::Approvals,
        Action::KeepRunning,
        Action::RefreshModels,
        Action::Prune,
        Action::Refresh,
        Action::Rename,
        Action::Rebuild,
        Action::Stop,
        Action::Delete,
        Action::PublishPort,
        Action::UnpublishPort,
        Action::CheckHost,
        Action::ToggleOther,
        Action::Help,
    ];

    /// The action's own key on the list screen, and its button text.
    fn spec(self) -> (KeyCode, &'static str) {
        use KeyCode::Char;
        match self {
            Action::Start => (KeyCode::Enter, "Start"),
            Action::Shell => (Char('x'), "Shell"),
            Action::New => (Char('n'), "New sandbox"),
            Action::EditKits => (Char('e'), "Edit kits"),
            Action::Auth => (Char('A'), "Account"),
            Action::Secrets => (Char('S'), "Secrets"),
            Action::Connect => (Char('o'), "Connect"),
            Action::StartDir => (Char('w'), "Start folder"),
            Action::Skills => (Char('K'), "Skills"),
            Action::RepoSkills => (Char('i'), "Repo skills"),
            Action::Folders => (Char('F'), "Host folders"),
            Action::Settings => (Char('T'), "Settings"),
            Action::Approvals => (Char('y'), "Approvals"),
            Action::KeepRunning => (Char('b'), "Keep running"),
            Action::RefreshModels => (Char('L'), "Refresh models"),
            Action::Prune => (Char('P'), "Prune"),
            Action::Refresh => (KeyCode::F(5), "Refresh"),
            Action::Rename => (Char('r'), "Rename"),
            Action::Rebuild => (Char('R'), "Rebuild"),
            Action::Stop => (Char('s'), "Stop"),
            Action::Delete => (Char('d'), "Delete"),
            Action::PublishPort => (Char('p'), "Publish port"),
            Action::UnpublishPort => (Char('u'), "Unpublish port"),
            Action::CheckHost => (Char('c'), "Check host"),
            Action::ToggleOther => (Char('a'), "Sandboxes in other folders"),
            Action::Help => (Char('?'), "Help"),
        }
    }

    /// The action's own key on the list screen.
    pub fn key(self) -> KeyEvent {
        KeyEvent::from(self.spec().0)
    }

    /// The action a key on the list screen means, aliases included: Ctrl-R refreshes like F5
    /// (while a plain `r` renames), and Delete deletes like `d`.
    pub fn from_key(key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let code = match key.code {
            KeyCode::Char('r') if ctrl => KeyCode::F(5),
            KeyCode::Delete => KeyCode::Char('d'),
            // Ctrl-C quits, and is not `c`.
            KeyCode::Char('c') if ctrl => return None,
            code => code,
        };
        Action::ALL.into_iter().find(|a| a.spec().0 == code)
    }

    /// The button text.
    pub fn title(self) -> &'static str {
        self.spec().1
    }

    /// What the action does, in one line — for a help sheet or a tooltip, in any frontend.
    pub fn summary(self) -> &'static str {
        match self {
            Action::Start => "Start the sandbox's agent",
            Action::Shell => "Open a shell inside the sandbox",
            Action::New => "Create a new sandbox for this folder",
            Action::EditKits => "Add or remove the sandbox's kits",
            Action::Auth => "Change the account or LiteLLM connection the sandbox uses",
            Action::Secrets => "Manage the stored tokens, keys and connections",
            Action::Connect => "Connect a terminal, an editor or a desktop app over SSH",
            Action::StartDir => "Choose the folder the agent starts in",
            Action::Skills => "Manage the shared skills",
            Action::RepoSkills => "Pick the skills from skill repositories this sandbox gets",
            Action::Folders => "Choose the folders every sandbox mounts",
            Action::Settings => "Change sbx's settings on this computer, such as telemetry",
            Action::Approvals => "Answer the network requests waiting for approval",
            Action::KeepRunning => "Keep the sandbox running without a session, or stop doing so",
            Action::RefreshModels => "Fetch the gateway's model list again",
            Action::Prune => "Remove every stopped sandbox",
            Action::Refresh => "Read the sandboxes from sbx again",
            Action::Rename => "Change the sandbox's label",
            Action::Rebuild => "Rebuild the sandbox from its kits, keeping its settings",
            Action::Stop => "Stop the sandbox",
            Action::Delete => "Remove the sandbox",
            Action::PublishPort => "Make a sandbox port reachable from this computer",
            Action::UnpublishPort => "Stop publishing a sandbox port",
            Action::CheckHost => "Check whether the sandbox may reach a host",
            Action::ToggleOther => "Show or hide the other folders' sandboxes",
            Action::Help => "Show the keys and what they do",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_its_own_key_and_the_key_names_it_back() {
        let mut keys: Vec<KeyEvent> = Action::ALL.iter().map(|a| a.key()).collect();
        for action in Action::ALL {
            assert_eq!(Action::from_key(action.key()), Some(action), "{action:?}");
            assert!(!action.title().is_empty());
        }
        keys.dedup();
        assert_eq!(keys.len(), Action::ALL.len());
    }

    #[test]
    fn every_action_has_a_one_line_summary_of_its_own() {
        let mut summaries: Vec<&str> = Action::ALL.iter().map(|a| a.summary()).collect();
        for (action, summary) in Action::ALL.iter().zip(&summaries) {
            assert!(!summary.is_empty() && !summary.contains('\n'), "{action:?}");
        }
        summaries.sort_unstable();
        summaries.dedup();
        assert_eq!(summaries.len(), Action::ALL.len());
    }

    #[test]
    fn the_aliases_and_the_ctrl_guard_are_kept() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |c| KeyEvent::from(KeyCode::Char(c));
        for (key, want) in [
            (KeyCode::Delete.into(), Some(Action::Delete)),
            (ctrl('r'), Some(Action::Refresh)),
            (KeyCode::F(5).into(), Some(Action::Refresh)),
            (plain('r'), Some(Action::Rename)),
            (plain('R'), Some(Action::Rebuild)),
            (ctrl('c'), None),
            (KeyCode::Up.into(), None),
            (KeyCode::Esc.into(), None),
            (KeyCode::Tab.into(), None),
        ] {
            assert_eq!(Action::from_key(key), want, "{key:?}");
        }
        for c in "qj1mMg\0".chars() {
            assert_eq!(Action::from_key(plain(c)), None, "{c:?}");
        }
    }
}

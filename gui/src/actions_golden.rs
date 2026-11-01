//! Golden tests: list actions sent to the window as egui events reach the same prompt,
//! confirmation, job and session as the same keys sent to core's `on_key`. In one frame, the
//! window drops the keys after a key that opens or closes a window.

use std::path::PathBuf;

use aisbx_core::actions::Action;
use aisbx_core::app::{App, ConnectRow, Handoff, Modal, Screen, StartDirRow};
use aisbx_core::input::KeyCode;
use aisbx_core::kits::Catalog;
use aisbx_core::remote::Target;
use aisbx_core::terminal::TermEnv;
use aisbx_core::{model, sbx, terminal};
use eframe::egui::{self, Key, Modifiers};

use crate::form_golden::{events, pressed, typed, Core, Driver, Events, Win};
use crate::{actions, overview, theme};

/// Put empty `ssh`, `code` and `cursor` files, also under their Windows names, first on PATH, so
/// Connect finds them on every host. Nothing runs them under test.
fn launchers_on_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join("aisbx-ui-launchers");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["ssh", "code", "cursor", "ssh.exe", "code.cmd", "cursor.cmd"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(dir).chain(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
}

/// A workspace that exists, for the start-folder picker, which reads it.
fn workspace() -> PathBuf {
    let root = std::env::temp_dir().join("aisbx-ui-actions-test");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    root
}

/// `box`, running, in its existing workspace, which the window shows; selected.
fn on_box() -> App {
    launchers_on_path();
    let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
    sb.running = true;
    sb.workspace = workspace();
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
    app.set_folder(workspace()).unwrap();
    app.select(0);
    app
}

/// `box` stopped, so prune has a candidate.
fn stopped() -> App {
    let mut app = on_box();
    app.sandboxes[0].running = false;
    app
}

/// What the keys led to: the modal (a job as every step's argv), the screen and the status.
fn outcome(app: &App) -> String {
    let job = |pending: &aisbx_core::app::PendingJob| {
        let steps: Vec<_> = pending.steps.iter().map(|s| (&s.label, &s.argv)).collect();
        format!("{} {steps:?}", pending.title)
    };
    let modal = match &app.modal {
        Some(Modal::Running(_, pending)) => format!("job {}", job(pending)),
        Some(Modal::Confirm(c)) => format!("confirm {:?} {:?} {}", c.title, c.body, job(&c.job)),
        Some(Modal::Prompt(p)) => format!("prompt {:?} {:?}", p.ask, p.input.value),
        Some(Modal::Help) => "help".into(),
        None => "none".into(),
    };
    let status = app.status.as_ref().map(|(t, e, _)| (t, e));
    format!(
        "{modal} {:?} {status:?}",
        std::mem::discriminant(&app.screen)
    )
}

/// What a session left: the argv opened (recorded under test), and whether no terminal was
/// found.
type Opened = (Vec<Vec<String>>, bool);

/// Run `script` through core's `on_key` and through the window's frames, and assert the same
/// outcome and opened argv. Returns the outcome, core's handoff and what was opened.
fn same(
    setup: &dyn Fn() -> App,
    script: &dyn Fn(&mut dyn Driver),
) -> (String, Option<Handoff>, Opened) {
    terminal::opened();
    let mut core = Core(setup());
    script(&mut core);
    let handoff = core.0.handoff.take();
    let no_terminal = match handoff.clone() {
        Some(h) => core.0.open_session(terminal::Launch::start(h)).is_err(),
        None => false,
    };
    let want = (outcome(&core.0), (terminal::opened(), no_terminal));
    let mut win = Win::new(setup());
    script(&mut win);
    // The window takes a handoff and opens it once its (here empty) prelude is done.
    win.frame(Vec::new());
    let opened = (terminal::opened(), win.session.no_terminal().is_some());
    let got = (outcome(&win.app), opened);
    assert_eq!(got, want);
    (got.0, handoff, got.1)
}

/// ↓ on the Connect screen until the cursor is on `row`.
fn connect_to(d: &mut dyn Driver, row: ConnectRow) {
    for _ in 0..10 {
        let Screen::Connect(form) = &d.app().screen else {
            panic!("Connect is open")
        };
        if form.focused() == row {
            return;
        }
        d.press(KeyCode::Down);
    }
    panic!("no {row:?}");
}

fn target_row(target: Target) -> ConnectRow {
    let at = Target::ALL.iter().position(|t| *t == target);
    ConnectRow::Target(at.unwrap())
}

#[test]
fn connect_vs_code_starts_the_editor_directly() {
    let (outcome, handoff, (opened, no_terminal)) = same(&on_box, &|d| {
        d.press(KeyCode::Char('o'));
        connect_to(d, target_row(Target::VsCode));
        d.press(KeyCode::Enter);
    });
    let Some(Handoff::Connect { argv, terminal, .. }) = handoff else {
        panic!("a connection")
    };
    assert!(!terminal && !no_terminal);
    assert_eq!(
        Some(argv.clone()),
        Target::VsCode.command("box", &workspace())
    );
    // The bare argv, not wrapped in a terminal. Its program is `code.cmd` on Windows.
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(opened[0][0].starts_with("code"), "{opened:?}");
    assert_eq!(opened[0][1..], argv[1..]);
    assert!(outcome.contains("box: `code` started"), "{outcome}");
}

#[test]
fn connect_terminal_opens_ssh_in_a_terminal_window() {
    let (_, handoff, (opened, no_terminal)) = same(&on_box, &|d| {
        d.press(KeyCode::Char('o'));
        d.press(KeyCode::Enter);
    });
    let Some(Handoff::Connect { argv, terminal, .. }) = handoff else {
        panic!("a connection")
    };
    assert!(terminal);
    assert_eq!(argv, ["ssh".to_string(), sbx::ssh_host("box")]);
    // Wrapped in this host's terminal, or the dialog if the host has none.
    match terminal::launch_argv(terminal::Os::current(), &argv, &TermEnv::from_process()) {
        Ok(wrapped) => {
            assert_ne!(wrapped, argv, "not the bare argv");
            assert_eq!(opened, vec![wrapped]);
        }
        Err(err) => assert!(opened.is_empty() && no_terminal, "{err}"),
    }
}

#[test]
fn connect_sets_up_ssh_first() {
    let unset = || {
        let mut app = on_box();
        app.ssh_ready = false;
        app
    };
    let (outcome, _, _) = same(&unset, &|d| {
        d.press(KeyCode::Char('o'));
        connect_to(d, ConnectRow::SetupSsh);
        d.press(KeyCode::Enter);
    });
    let setup = format!("{:?}", sbx::setup_ssh_argv());
    assert!(
        outcome.starts_with("job") && outcome.contains(&setup),
        "{outcome}"
    );
}

#[test]
fn start_folder_start_here_is_the_same_handoff() {
    let (_, handoff, (opened, no_terminal)) = same(&on_box, &|d| {
        d.press(KeyCode::Char('w'));
        for _ in 0..10 {
            let Screen::StartDir(picker) = &d.app().screen else {
                panic!("the picker is open")
            };
            let rows = picker.rows();
            if rows[picker.focus.min(rows.len() - 1)] == StartDirRow::Start {
                break;
            }
            d.press(KeyCode::Down);
        }
        d.press(KeyCode::Enter);
    });
    assert!(matches!(handoff, Some(Handoff::Agent { ref name, .. }) if name == "box"));
    assert!(opened.len() == 1 || no_terminal);
}

#[test]
fn the_port_and_host_prompts_give_the_same_job() {
    for (key, value, want) in [
        ('p', "8080", "job"),
        ('u', "8080", "job"),
        ('c', "example.com", "checking example.com…"),
    ] {
        let (prompt, _, _) = same(&on_box, &|d| d.press(KeyCode::Char(key)));
        assert!(prompt.starts_with("prompt"), "{key}: {prompt}");
        let (outcome, _, _) = same(&on_box, &|d| {
            d.press(KeyCode::Char(key));
            d.type_text(value);
            d.press(KeyCode::Enter);
        });
        assert!(outcome.contains(want), "{key}: {outcome}");
        if want == "job" {
            assert!(outcome.contains("8080"), "{key}: {outcome}");
        }
    }
}

#[test]
fn keep_running_rebuild_and_prune_confirm_the_same_job() {
    for (key, setup) in [
        ('b', &on_box as &dyn Fn() -> App),
        ('R', &on_box),
        ('P', &stopped),
    ] {
        let (confirm, _, _) = same(setup, &|d| d.press(KeyCode::Char(key)));
        assert!(confirm.starts_with("confirm"), "{key}: {confirm}");
        let (outcome, _, _) = same(setup, &|d| {
            d.press(KeyCode::Char(key));
            d.press(KeyCode::Char('y'));
        });
        assert!(outcome.starts_with("job"), "{key}: {outcome}");
    }
}

#[test]
fn help_opens_with_question_mark_and_closes_through_core() {
    let (outcome, _, _) = same(&on_box, &|d| d.press(KeyCode::Char('?')));
    assert!(outcome.starts_with("help"), "{outcome}");
    let mut w = Win::new(on_box());
    w.frame(typed('?'));
    w.frame(Vec::new());
    assert!(
        w.drawn.iter().any(|t| t == "Actions and keys"),
        "the sheet is drawn"
    );
    let sections = crate::help::action_sections();
    let headings: Vec<&str> = sections.iter().map(|(h, _)| *h).collect();
    assert_eq!(
        headings,
        ["Sandbox", "Setup", "Access", "Manage", "This host"]
    );
    let listed: Vec<(Action, &crate::help::ActionRow)> = (sections.iter())
        .zip(actions::sections())
        .flat_map(|((_, rows), (_, group))| group.iter().copied().zip(rows))
        .collect();
    assert_eq!(listed.len(), Action::ALL.len());
    for (action, (keys, title, summary)) in &listed {
        assert_eq!(keys[0], actions::key_label(*action), "{action:?}");
        assert_eq!((*title, *summary), (action.title(), action.summary()));
    }
    let keys_of = |a: Action| (listed.iter().find(|(b, _)| *b == a)).map(|(_, r)| r.0.clone());
    let delete = ["d".to_string(), "Delete".to_string()];
    assert_eq!(keys_of(Action::Delete), Some(delete.to_vec()));
    let refresh = ["F5".to_string(), crate::help::command_key("Cmd-R")];
    assert_eq!(keys_of(Action::Refresh), Some(refresh.to_vec()));
    // The sheet scrolls: its first rows are on screen, each key a keycap of its own.
    for (keys, _, summary) in &sections[0].1 {
        assert!(w.drawn.contains(&keys[0]) && w.drawn.iter().any(|t| t == summary));
    }
    for close in [
        events(KeyCode::Esc),
        events(KeyCode::Enter),
        typed('?'),
        typed('q'),
    ] {
        w.frame(Vec::new());
        assert!(matches!(w.app.modal, Some(Modal::Help)));
        w.frame(close.clone());
        assert!(w.app.modal.is_none(), "{close:?}");
        w.frame(typed('?'));
    }
    w.frame(events(KeyCode::Down));
    assert!(matches!(w.app.modal, Some(Modal::Help)), "↓ scrolls");
}

/// Each row: the title, the summary, then the keys, the last keys ending at one right edge. The
/// sheet opens at its full height: its Close stays where it was on the first frame.
#[test]
fn the_help_sheet_leads_with_the_task_and_ends_each_row_with_its_keys() {
    let mut w = Win::new(on_box());
    // Tall enough that the sheet scrolls nothing out of view: a row out of view draws no text.
    w.size = egui::vec2(1200.0, 2400.0);
    w.frame(typed('?'));
    // The frame `?` is pressed in opens the sheet; the next one is its first.
    w.frame(Vec::new());
    let close = |w: &Win| {
        (w.drawn_at.iter())
            .find(|(t, _)| t == "Close")
            .map(|(_, r)| *r)
    };
    let opened = close(&w);
    assert!(opened.is_some(), "the sheet is drawn on its first frame");
    (0..20).for_each(|_| w.frame(Vec::new()));
    assert_eq!(
        close(&w),
        opened,
        "the sheet's height on its first frame and later"
    );
    // Only the sheet's column: the window beside it draws some of the same words.
    let heading = (w.drawn_at.iter())
        .find(|(t, _)| t == "Actions and keys")
        .unwrap()
        .1;
    let sheet = heading.left() - 1.0..=heading.left() + crate::help::SHEET_W;
    // Where `text` is drawn on the sheet's line at `y`, right of `right_of`: the leftmost, or
    // with `last` the rightmost, because *Delete* is both a title and a key on one line.
    let on_line = |text: &str, y: f32, right_of: f32, last: bool| {
        let found = (w.drawn_at.iter())
            .filter(|(_, r)| sheet.contains(&r.left()) && sheet.contains(&r.right()))
            .filter(|(t, r)| t == text && (r.center().y - y).abs() < 8.0 && r.left() > right_of)
            .map(|(_, r)| *r);
        let pick = |a: egui::Rect, b: egui::Rect| match (b.left() > a.left()) == last {
            true => b,
            false => a,
        };
        (found.reduce(pick))
            .unwrap_or_else(|| panic!("{text:?} on the line at {y}: {:?}", w.drawn_at))
    };
    let summary = |text: &str| {
        (w.drawn_at.iter())
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{text:?} on the sheet"))
    };
    let mut ends = Vec::new();
    for (_, rows) in crate::help::action_sections() {
        for (keys, title, what) in rows {
            let what = summary(what);
            let title = on_line(title, what.center().y, 0.0, false);
            let key = on_line(keys.last().unwrap(), what.center().y, what.right(), true);
            assert!(title.right() < what.left(), "{title:?} before {what:?}");
            ends.push(key.right());
        }
    }
    for (task, input) in crate::help::WINDOW_KEYS {
        let task = summary(task);
        if let crate::help::Input::Keys(keys) = input {
            let last = crate::help::command_key(keys.last().unwrap());
            ends.push(on_line(&last, task.center().y, task.right(), true).right());
        }
    }
    let (min, max) = (ends.iter()).fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
    assert!(max - min < 1.0, "the keys end at one edge: {ends:?}");
}

// --- same-frame cases ----------------------------------------------------------------------

fn nothing_started(w: &Win) -> bool {
    w.app.handoff.is_none()
        && !w.session.busy()
        && !w.session.dialog_open()
        && terminal::opened().is_empty()
}

#[test]
fn o_then_enter_opens_connect_only() {
    let mut w = Win::new(on_box());
    terminal::opened();
    w.frame([typed('o'), events(KeyCode::Enter)].concat());
    let Screen::Connect(form) = &w.app.screen else {
        panic!("Connect is open")
    };
    assert_eq!(form.focus, 0);
    assert!(nothing_started(&w));
}

#[test]
fn enter_on_a_target_then_d_deletes_nothing() {
    let mut w = Win::new(on_box());
    w.frame(typed('o'));
    terminal::opened();
    w.frame([events(KeyCode::Enter), typed('d')].concat());
    assert!(w.app.modal.is_none(), "no confirmation");
    let went = !terminal::opened().is_empty() || w.session.dialog_open();
    assert!(went, "the connection went");
}

#[test]
fn escape_on_connect_then_x_opens_no_shell() {
    let mut w = Win::new(on_box());
    w.frame(typed('o'));
    terminal::opened();
    w.frame([events(KeyCode::Esc), typed('x')].concat());
    assert!(matches!(w.app.screen, Screen::List));
    assert!(nothing_started(&w));
}

#[test]
fn question_mark_then_d_opens_help_only() {
    let mut w = Win::new(on_box());
    w.frame([typed('?'), typed('d')].concat());
    assert!(matches!(w.app.modal, Some(Modal::Help)));
}

#[test]
fn escape_in_help_then_x_opens_no_shell() {
    let mut w = Win::new(on_box());
    w.frame(typed('?'));
    terminal::opened();
    w.frame([events(KeyCode::Esc), typed('x')].concat());
    assert!(w.app.modal.is_none());
    assert!(nothing_started(&w));
}

#[test]
fn prune_then_y_and_rebuild_then_enter_start_no_job() {
    for keys in [
        [typed('P'), typed('y')].concat(),
        [typed('R'), events(KeyCode::Enter)].concat(),
    ] {
        let mut w = Win::new(stopped());
        w.frame(keys.clone());
        let confirm = matches!(w.app.modal, Some(Modal::Confirm(_)));
        assert!(confirm, "{keys:?}: {}", outcome(&w.app));
    }
}

/// The middle of the drawn text that starts with `starts`, in the last frame.
fn text_at(w: &Win, starts: &str) -> egui::Pos2 {
    let found = w.drawn_at.iter().find(|(t, _)| t.starts_with(starts));
    found
        .unwrap_or_else(|| panic!("{starts} is not drawn"))
        .1
        .center()
}

/// A click at `pos`: pressed in one frame, released in the next.
fn click(w: &mut Win, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    w.frame(vec![egui::Event::PointerMoved(pos), button(true)]);
    w.frame(vec![button(false)]);
    w.frame(Vec::new());
}

/// A click on the SSH endpoint line does nothing; one on an app selects it, and its *Open*
/// connects it.
#[test]
fn a_click_on_the_endpoint_line_does_nothing() {
    let mut w = Win::new(on_box());
    w.frame(typed('o'));
    w.frame(Vec::new());
    terminal::opened();
    let host = text_at(&w, "box.sbx");
    click(&mut w, host);
    let Screen::Connect(form) = &w.app.screen else {
        panic!("Connect is still open")
    };
    assert_eq!(form.focus, 0);
    assert!(w.app.modal.is_none() && w.app.status.is_none());
    assert!(nothing_started(&w));

    let cursor = text_at(&w, "Cursor");
    click(&mut w, cursor);
    assert!(nothing_started(&w), "a click on the app selects it");
    let Screen::Connect(form) = &w.app.screen else {
        panic!("Connect is still open")
    };
    let at = Target::ALL.iter().position(|t| *t == Target::Cursor);
    assert_eq!(form.rows()[form.focus], ConnectRow::Target(at.unwrap()));
    let open = (w.drawn_at.iter())
        .filter(|(t, _)| t == crate::connect::OPEN)
        .map(|(_, r)| r.center())
        .min_by_key(|p| (p.y - cursor.y).abs() as i32)
        .expect("an Open button");
    click(&mut w, open);
    let want = Target::Cursor.command("box", &workspace()).unwrap();
    let opened = terminal::opened();
    assert_eq!(opened.len(), 1, "its Open connected it");
    assert_eq!(opened[0][1..], want[1..]);
}

#[test]
fn every_action_is_offered_and_no_hint_says_in_aisbx() {
    let mut offered: Vec<Action> = (actions::sections().into_iter())
        .flat_map(|(_, group)| group.iter().copied())
        .collect();
    offered.sort_by_key(|a| Action::ALL.iter().position(|b| b == a));
    assert_eq!(offered, Action::ALL);
    let mut sb = model::sample("box", Some("claude-token"), &[]);
    // Every optional Overview row that names a key.
    sb.running = true;
    sb.keep_running = true;
    sb.unresponsive = true;
    sb.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
    sb.cpus = Some(2);
    sb.start_dir = Some(PathBuf::from("gone"));
    sb.other_checkout = Some(PathBuf::from("/elsewhere"));
    let mut other = model::sample("far", Some("claude-anthropic"), &[]);
    other.workspace = PathBuf::from("/elsewhere");
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone(), other]);
    app.ssh_ready = false;
    let rows = format!("{:?}", overview::rows(&app, &sb));
    assert!(!rows.contains("in aisbx"), "{rows}");
    let mut w = Win::new(app);
    for cell in 0..w.app.cells.len() {
        w.app.select(cell);
        w.frame(Vec::new());
        let drawn = w.drawn.join("\n");
        assert!(!drawn.contains("in aisbx"), "{drawn}");
    }
}

/// `?` reaches core as `?`, whatever modifier the layout types it with.
#[test]
fn the_question_mark_key_is_help() {
    for modifiers in [Modifiers::NONE, Modifiers::SHIFT] {
        let mut w = Win::new(on_box());
        w.frame(vec![pressed(Key::Questionmark, modifiers)]);
        assert!(matches!(w.app.modal, Some(Modal::Help)), "{modifiers:?}");
    }
    let _: Events = Vec::new();
}

/// The header offers the host's actions on the list screen, and only *Refresh* on another one;
/// the count of the requests waiting is a button that opens them, as `y` does.
#[test]
fn the_header_offers_the_host_actions_on_the_list_only() {
    let mut app = on_box();
    app.approvals = vec![aisbx_core::testkit::held("box", "example.org:443")];
    let mut w = Win::new(app);
    w.frame(Vec::new());
    let drawn = |w: &Win, title: &str| w.drawn.iter().any(|t| t == title);
    for action in actions::HOST {
        assert!(drawn(&w, action.title()), "{action:?}: {:?}", w.drawn);
    }

    let count = text_at(&w, "⚑ 1 to approve");
    click(&mut w, count);
    assert!(
        matches!(w.app.screen, Screen::Approvals(_)),
        "{:?}",
        w.app.screen
    );
    assert!(drawn(&w, "Refresh"));
    for action in [Action::New, Action::Prune, Action::Help] {
        assert!(!drawn(&w, action.title()), "{action:?} off the list");
    }
}

// --- the window's own ways to an action ------------------------------------------------------

/// Assert that `key` through core and a click on `label`, drawn where `found` accepts it, reach
/// the same outcome. The window is tall enough for the whole Overview. Returns the window's app.
fn click_is_key(
    setup: &dyn Fn() -> App,
    key: KeyCode,
    label: &str,
    found: &dyn Fn(&Win, egui::Rect) -> bool,
) -> App {
    let mut core = setup();
    core.on_key(aisbx_core::input::KeyEvent::from(key));
    let mut w = Win::new(setup());
    w.size = egui::vec2(1200.0, 1600.0);
    w.frame(Vec::new());
    let at = (w.drawn_at.iter())
        .find(|(t, r)| t == label && found(&w, *r))
        .unwrap_or_else(|| panic!("{label} is not drawn there: {:?}", w.drawn_at))
        .1
        .center();
    click(&mut w, at);
    assert_eq!(outcome(&w.app), outcome(&core), "{label}");
    w.app
}

/// On the sandbox's page, right of the list.
fn panel(r: egui::Rect) -> bool {
    r.center().x > crate::shell::LIST_W
}

/// Where the page draws `text`: the last of its copies right of the list.
fn on_page(w: &Win, text: &str) -> Option<egui::Rect> {
    (w.drawn_at.iter())
        .rfind(|(t, r)| t == text && panel(*r))
        .map(|(_, r)| *r)
}

/// Above the page's toolbar: the head and its callout.
fn in_head(w: &Win, r: egui::Rect) -> bool {
    let start = on_page(w, main_action(w)).expect("the toolbar's main action");
    panel(r) && r.bottom() <= start.top()
}

/// The toolbar's filled button, as the selected sandbox's state words it: *Start*, or *New
/// session* on a running one.
fn main_action(w: &Win) -> &'static str {
    actions::title(&w.app, Action::Start)
}

/// Under the tab strip: the open tab's body.
fn in_tab(w: &Win, r: egui::Rect) -> bool {
    let tabs = on_page(w, "Overview").expect("the tab strip");
    panel(r) && r.top() >= tabs.bottom()
}

/// The list is a navigator down the left, the agent under the name. The page beside it has the
/// name as its title, the agent's colour down the head's edge, kit badges and the tabs in one line.
#[test]
fn the_list_is_a_navigator_beside_the_sandboxs_page() {
    let mut app = on_box();
    app.sandboxes[0].addons = vec!["jdk25".into(), "rtk".into()];
    let mut w = Win::new(app);
    w.frame(Vec::new());
    let left = crate::shell::LIST_W;
    assert!(
        left < crate::roster::NARROW,
        "the list starts in its two-line layout"
    );
    let all = |t: &str| -> Vec<egui::Rect> {
        (w.drawn_at.iter())
            .filter(|(d, _)| d == t)
            .map(|(_, r)| *r)
            .collect()
    };
    let one = |t: &str, list: bool| -> egui::Rect {
        let found = all(t).into_iter().find(|r| (r.center().x < left) == list);
        found.unwrap_or_else(|| panic!("{t} (list {list}): {:?}", w.drawn_at))
    };
    let (row, title) = (one("box", true), one("box", false));
    assert!(title.height() > row.height() * 1.4, "{title:?} {row:?}");
    let agent = w.app.sandboxes[0].type_label(&w.app.catalog);
    let under = one(&agent, true);
    assert!(under.top() >= row.bottom() - 1.0 && (under.left() - row.left()).abs() < 1.0);
    for kit in ["JDK25", "RTK"] {
        one(kit, false);
    }
    let dark = w.ctx.global_style().visuals.dark_mode;
    let hue = theme::agent_color(&w.app.sandboxes[0].agent, dark);
    let bar = (w.fills.iter())
        .find(|(r, c)| *c == hue && r.width() <= 4.0 && r.height() > title.height());
    let bar = bar
        .unwrap_or_else(|| panic!("no bar in {hue:?}: {:?}", w.fills))
        .0;
    assert!(
        bar.left() > left && bar.right() < title.left(),
        "{bar:?} {title:?}"
    );
    let tabs: Vec<egui::Rect> = (aisbx_core::insight::Tab::ALL.iter())
        .map(|t| one(t.title(), false))
        .collect();
    assert!(
        tabs.windows(2)
            .all(|p| p[1].left() > p[0].right() && (p[1].center().y - p[0].center().y).abs() < 1.0),
        "one line: {tabs:?}"
    );
}

/// On a wide window, nothing on the page runs past `shell::PAGE_W` from its left edge. The state
/// pill ends at that edge, level with the first line of a title that wraps.
#[test]
fn the_page_stops_at_a_readable_width() {
    let mut app = on_box();
    app.sandboxes[0].label = "a label that wraps ".repeat(8).trim().to_string();
    let mut w = Win::new(app);
    w.size = egui::vec2(1680.0, 900.0);
    // A layout sized from the frame before: a few frames at the new size.
    (0..3).for_each(|_| w.frame(Vec::new()));
    let texts: Vec<egui::Rect> = (w.drawn_at.iter())
        .map(|(_, r)| *r)
        .filter(|r| panel(*r) && r.top() > 60.0)
        .collect();
    let start = texts.iter().map(|r| r.left()).fold(f32::MAX, f32::min);
    let edge = start + crate::shell::PAGE_W + 1.0;
    for r in &texts {
        assert!(r.right() <= edge, "{r:?} runs past {edge}");
    }
    let pill = actions::state_label(&w.app.sandboxes[0]);
    let pill = (w.drawn_at.iter())
        .find(|(t, _)| t.ends_with(&pill))
        .unwrap_or_else(|| panic!("the state pill: {:?}", w.drawn_at))
        .1;
    assert!(
        pill.right() > edge - 30.0,
        "{pill:?} ends at the edge, {edge}"
    );
    let title = (w.drawn_at.iter())
        .find(|(t, r)| t.starts_with("a label that wraps") && panel(*r))
        .unwrap()
        .1;
    assert!(
        title.right() < pill.left(),
        "{title:?} wraps before the pill"
    );
    assert!(title.height() > 50.0, "{title:?} is on more than one line");
    assert!(
        (pill.center().y - title.top()) < 24.0,
        "{pill:?} level with {title:?}"
    );
}

#[test]
fn the_new_rows_page_lists_the_wizards_steps() {
    let mut app = on_box();
    app.select(app.here);
    let mut w = Win::new(app);
    w.frame(Vec::new());
    let on_page = |t: &str| {
        (w.drawn_at.iter())
            .find(|(d, r)| d == t && r.center().x > crate::shell::LIST_W)
            .unwrap_or_else(|| panic!("{t}: {:?}", w.drawn_at))
            .1
    };
    let mut above = on_page("New sandbox").bottom();
    for (step, gloss) in actions::STEP_GLOSSES {
        let title = on_page(step.title());
        assert!(title.top() >= above, "{step:?} in order");
        assert!(on_page(gloss).left() > title.right(), "{gloss}");
        above = title.bottom();
    }
    assert!(on_page("Set up a sandbox").top() > above);
    let steps: Vec<_> = actions::STEP_GLOSSES.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        steps,
        aisbx_core::create::CreateStep::ALL,
        "every step, once"
    );
}

#[test]
fn the_header_fills_new_sandbox_at_its_edge() {
    let mut w = Win::new(on_box());
    w.frame(Vec::new());
    let header = |t: &str| {
        (w.drawn_at.iter())
            .find(|(d, r)| d == t && r.center().y < 46.0)
            .unwrap_or_else(|| panic!("{t} is not in the header: {:?}", w.drawn_at))
            .1
    };
    let new = header("New sandbox");
    for action in actions::HOST.iter().filter(|a| **a != Action::New) {
        assert!(header(action.title()).right() < new.left(), "{action:?}");
    }
    let dark = w.ctx.global_style().visuals.dark_mode;
    let filled =
        (w.fills.iter()).any(|(r, c)| r.contains(new.center()) && *c == theme::accent(dark));
    assert!(filled, "New sandbox is filled in the ink");
}

#[test]
fn the_page_head_says_what_a_sandbox_needs_and_its_button_fixes_it() {
    let drifted = || {
        let mut app = on_box();
        app.sandboxes[0].drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        app
    };
    let stuck = || {
        let mut app = on_box();
        app.sandboxes[0].unresponsive = true;
        app
    };
    // In the head: the toolbar under it has a *Stop* button too.
    let rebuild = click_is_key(&drifted, KeyCode::Char('R'), "Rebuild", &in_head);
    assert!(matches!(rebuild.modal, Some(Modal::Confirm(_))));
    let stop = click_is_key(&stuck, KeyCode::Char('s'), "Stop", &in_head);
    assert!(matches!(stop.modal, Some(Modal::Running(..))));
}

/// `box` holding `n` requests for approval, the first to `host0:443`.
fn holding(n: usize) -> App {
    let mut app = on_box();
    app.approvals = (0..n)
        .map(|i| aisbx_core::testkit::held("box", &format!("host{i}:443")))
        .collect();
    app
}

/// The job a modal runs: its title and every step's argv — nothing of the screen under it.
fn job_of(app: &App) -> String {
    match &app.modal {
        Some(Modal::Running(_, pending)) => {
            let argv: Vec<_> = pending.steps.iter().map(|s| &s.argv).collect();
            format!("{} {argv:?}", pending.title)
        }
        _ => "no job".into(),
    }
}

/// The callout's *Allow* and *Dismiss* run the same job as `a` and `d` on the approvals screen,
/// for that request, and the page stays where it was.
#[test]
fn a_waiting_request_is_answered_in_the_callout() {
    for (label, key) in [("Allow", 'a'), ("Dismiss", 'd')] {
        let mut core = holding(2);
        core.on_key(aisbx_core::input::KeyEvent::from(KeyCode::Char('y')));
        core.on_key(aisbx_core::input::KeyEvent::from(KeyCode::Down));
        core.on_key(aisbx_core::input::KeyEvent::from(KeyCode::Char(key)));
        assert!(job_of(&core).contains("host1:443"), "{}", job_of(&core));

        let mut w = Win::new(holding(2));
        w.size = egui::vec2(1200.0, 1600.0);
        w.frame(Vec::new());
        let host = (w.drawn_at.iter())
            .find(|(t, r)| t == "host1:443" && in_head(&w, *r))
            .unwrap_or_else(|| panic!("host1:443 is not in the callout: {:?}", w.drawn_at))
            .1;
        let at = (w.drawn_at.iter())
            .find(|(t, r)| {
                t == label && in_head(&w, *r) && (r.center().y - host.center().y).abs() < 4.0
            })
            .unwrap_or_else(|| panic!("no {label} on host1's line: {:?}", w.drawn_at))
            .1;
        assert!(at.left() > host.right(), "{label} at the right of its line");
        click(&mut w, at.center());
        assert_eq!(job_of(&w.app), job_of(&core), "{label}");
        assert!(matches!(w.app.screen, Screen::List), "the page stays");
    }
}

#[test]
fn the_callout_says_why_only_beyond_the_default_deny() {
    let mut app = holding(2);
    app.approvals[1].detail =
        "Protocol: TCP\nResource type: domain\napproval required by policy \"egress\"".into();
    let mut w = Win::new(app);
    w.size = egui::vec2(1200.0, 1600.0);
    w.frame(Vec::new());
    let in_callout = |t: &str| (w.drawn_at.iter()).any(|(d, r)| d.contains(t) && in_head(&w, *r));
    assert!(
        in_callout("approval required by policy \"egress\""),
        "{:?}",
        w.drawn_at
    );
    assert!(!in_callout("default deny"), "{:?}", w.drawn_at);
}

/// The callout lists the first [`actions::CALLOUT_REQUESTS`] requests and counts the rest beside
/// *All requests*, which opens the approvals screen as `y` does. Without a rest, it has no button.
#[test]
fn the_callout_counts_the_requests_it_does_not_list() {
    let shown = actions::CALLOUT_REQUESTS;
    let many = || holding(shown + 2);
    let mut w = Win::new(many());
    w.size = egui::vec2(1200.0, 1600.0);
    w.frame(Vec::new());
    let in_callout = |t: &str| (w.drawn_at.iter()).any(|(d, r)| d == t && in_head(&w, *r));
    for i in 0..shown {
        assert!(in_callout(&format!("host{i}:443")), "{i}: {:?}", w.drawn_at);
    }
    assert!(!in_callout(&format!("host{shown}:443")));
    assert!(in_callout("and 2 more"), "{:?}", w.drawn_at);
    let all = click_is_key(&many, KeyCode::Char('y'), "All requests", &in_head);
    assert!(matches!(all.screen, Screen::Approvals(_)));

    let mut w = Win::new(holding(shown));
    w.frame(Vec::new());
    assert!(!(w.drawn_at.iter()).any(|(d, r)| d == "All requests" && in_head(&w, *r)));
}

#[test]
fn the_callout_fixes_what_the_rows_tooltip_names() {
    use aisbx_core::model::CardNote;
    for note in [
        CardNote::Unresponsive,
        CardNote::NoToken,
        CardNote::Approvals(2),
        CardNote::KitsChanged(aisbx_core::model::DriftKind::Changed),
    ] {
        let (_, _, fix, _) = actions::attention(note);
        let (_, tooltip) = crate::roster::note_text(note);
        let key = format!(" {}", actions::key_label(fix));
        assert!(
            tooltip.contains(&key),
            "{note:?}: {tooltip} does not name {key}"
        );
    }
}

/// The New row's page has no sandbox actions. Its one button opens the wizard, as `n` does.
#[test]
fn the_new_rows_page_sets_up_a_sandbox() {
    let on_new = || {
        let mut app = on_box();
        app.select(app.here);
        app
    };
    let set_up = click_is_key(&on_new, KeyCode::Char('n'), "Set up a sandbox", &|_, r| {
        panel(r)
    });
    assert!(matches!(set_up.screen, Screen::Create(_)));
    let mut w = Win::new(on_new());
    w.frame(Vec::new());
    for action in actions::PRIMARY {
        let drawn = (w.drawn_at.iter()).any(|(t, r)| t == action.title() && panel(*r));
        assert!(!drawn, "{action:?} is not offered on the New row");
    }
}

#[test]
fn an_overview_rows_button_does_what_its_key_does() {
    let drifted = || {
        let mut app = on_box();
        app.sandboxes[0].drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        app
    };
    let connect = click_is_key(&on_box, KeyCode::Char('o'), "Connect", &in_tab);
    assert!(matches!(connect.screen, Screen::Connect(_)));
    let rebuild = click_is_key(&drifted, KeyCode::Char('R'), "Rebuild", &in_tab);
    assert!(matches!(rebuild.modal, Some(Modal::Confirm(_))));
}

/// Each fix is a button instead. The check skips "a" and "A": "a" is also an English article.
#[test]
fn no_overview_row_names_a_list_key() {
    let mut sb = model::sample("box", Some("claude-token"), &[]);
    sb.running = true;
    sb.keep_running = true;
    sb.unresponsive = true;
    sb.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
    sb.cpus = Some(2);
    sb.start_dir = Some(PathBuf::from("gone"));
    sb.other_checkout = Some(PathBuf::from("/elsewhere"));
    sb.auth = Some(model::Auth::Token {
        name: "work".into(),
        placeholder: sbx::named_token_placeholder("work"),
        stored: false,
    });
    let mut app = App::for_test(Catalog::load().unwrap(), vec![sb.clone()]);
    app.approvals = vec![aisbx_core::testkit::held("box", "example.org:443")];
    for ssh_ready in [true, false] {
        app.ssh_ready = ssh_ready;
        let rows = overview::rows(&app, &sb);
        let keys: Vec<char> = Action::ALL
            .iter()
            .filter_map(|a| match a.key().code {
                KeyCode::Char(c) if !matches!(c, 'a' | 'A') => Some(c),
                _ => None,
            })
            .collect();
        for row in &rows {
            let texts = row.value.iter().chain(&row.gloss).map(|p| p.text.as_str());
            for text in texts {
                let words = text.split(|c: char| !c.is_alphanumeric() && c != '?');
                for word in words {
                    let mut chars = word.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        assert!(!keys.contains(&c), "{}: {text}", row.label);
                    }
                }
            }
        }
        let acting = |label: &str| (rows.iter().find(|r| r.label == label)).and_then(|r| r.action);
        assert_eq!(acting("SSH").map(|a| a.0), Some(Action::Connect));
        assert_eq!(acting("⟳ Update").map(|a| a.0), Some(Action::Rebuild));
        // A waiting request is in the head's callout and on the map, so it has no row.
        assert!(!rows.iter().any(|r| r.label == "⚑ Network"));
        assert_eq!(acting("Account").map(|a| a.0), Some(Action::Auth));
        assert_eq!(acting("Opens in").map(|a| a.0), Some(Action::StartDir));
        assert_eq!(acting("Lifecycle").map(|a| a.0), Some(Action::KeepRunning));
    }
}

#[test]
fn the_right_click_menu_shows_each_key_as_a_shortcut() {
    let mut w = Win::new(on_box());
    w.frame(Vec::new());
    let row = (w.drawn_at.iter())
        .find(|(t, r)| t == "box" && !panel(*r))
        .unwrap()
        .1
        .center();
    let button = |pressed| egui::Event::PointerButton {
        pos: row,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    w.frame(vec![egui::Event::PointerMoved(row), button(true)]);
    w.frame(vec![button(false)]);
    w.frame(Vec::new());
    let at = |t: &str| (w.drawn_at.iter()).rfind(|(d, _)| d == t).map(|(_, r)| *r);
    let (title, key) = (
        at("Rebuild").unwrap(),
        at("R").expect("the key is drawn apart"),
    );
    assert!(key.left() > title.right() && (key.center().y - title.center().y).abs() < 2.0);
}

/// *Start* on a stopped sandbox, *New session* on a running one, in a row's menu too. A click
/// opens the agent either way, as Enter does.
#[test]
fn the_main_button_is_worded_for_the_sandboxs_state() {
    let mut w = Win::new(stopped());
    (0..2).for_each(|_| w.frame(Vec::new()));
    assert!(on_page(&w, "Start").is_some(), "{:?}", w.drawn);
    assert!(on_page(&w, actions::NEW_SESSION).is_none(), "{:?}", w.drawn);

    let mut w = Win::new(on_box());
    (0..2).for_each(|_| w.frame(Vec::new()));
    assert!(on_page(&w, "Start").is_none(), "{:?}", w.drawn);
    let at = on_page(&w, actions::NEW_SESSION).expect("New session on a running sandbox");
    let sb = w.app.selected();
    assert_eq!(
        actions::title_for(sb, Action::Start),
        actions::NEW_SESSION,
        "the row's menu"
    );
    aisbx_core::terminal::opened();
    click(&mut w, at.center());
    let opened = aisbx_core::terminal::opened();
    let agent = opened
        .iter()
        .any(|argv| argv.iter().any(|a| a.contains("box")));
    assert!(
        agent || w.session.dialog_open(),
        "a click opens the agent: {opened:?}"
    );
}

/// In a low and a tall window: the group menus follow *Stop* on the toolbar's line, no group
/// action is a button above the tabs, and the tabs follow the toolbar. An item acts as its key.
#[test]
fn the_pages_groups_are_menus_on_its_toolbar() {
    for size in [egui::vec2(960.0, 600.0), egui::vec2(1200.0, 1100.0)] {
        let mut w = Win::new(on_box());
        w.size = size;
        (0..2).for_each(|_| w.frame(Vec::new()));
        let start = on_page(&w, main_action(&w)).unwrap();
        let tabs = on_page(&w, "Overview").unwrap();
        let mut left = on_page(&w, "Stop").unwrap().right();
        for (label, group) in actions::GROUPS {
            let menu = actions::group_menu(label);
            let at = on_page(&w, &menu).unwrap_or_else(|| panic!("{menu}: {:?}", w.drawn));
            let line = (at.center().y - start.center().y).abs();
            assert!(line < 2.0, "{size:?}: {menu} on the toolbar's line");
            assert!(at.left() > left, "{size:?}: {menu} in order");
            left = at.right();
            for action in group.iter() {
                let button = (w.drawn_at.iter())
                    .any(|(t, r)| t == action.title() && panel(*r) && r.bottom() <= tabs.top());
                assert!(!button, "{size:?}: {action:?} is a button over the tabs");
            }
        }
        let gap = tabs.top() - start.bottom();
        assert!(gap < 50.0, "{size:?}: the tabs follow the toolbar, {gap}");
    }
    let mut w = Win::new(on_box());
    let manage = on_page(&w, &actions::group_menu("Manage")).unwrap();
    click(&mut w, manage.center());
    (0..12).for_each(|_| w.frame(Vec::new()));
    let rename = on_page(&w, Action::Rename.title()).expect("Rename in the Manage menu");
    click(&mut w, rename.center());
    w.frame(Vec::new());
    let mut core = on_box();
    core.on_key(Action::Rename.key());
    assert!(matches!(core.modal, Some(Modal::Prompt(_))));
    assert_eq!(outcome(&w.app), outcome(&core));
}

#[test]
fn a_groups_menu_names_its_actions() {
    let app = on_box();
    for (_, group) in actions::GROUPS {
        let tip = actions::group_tooltip(&app, group);
        for action in group.iter() {
            assert!(tip.contains(action.title()), "{action:?}: {tip}");
        }
    }
}

/// The host's actions are a submenu, so the whole menu fits in a window of the minimum size.
#[test]
fn the_right_click_menu_groups_the_actions_as_the_page_does() {
    let mut w = Win::new(on_box());
    w.size = egui::vec2(960.0, 600.0);
    w.frame(Vec::new());
    // The list's row, left of the page's title of the same name.
    let row = (w.drawn_at.iter())
        .filter(|(t, _)| t == "box")
        .map(|(_, r)| r.center())
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .unwrap();
    let button = |pressed| egui::Event::PointerButton {
        pos: row,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    w.frame(vec![egui::Event::PointerMoved(row), button(true)]);
    w.frame(vec![button(false)]);
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    // The menu is drawn over the page, so its copy of a title is the last one drawn.
    let at = |w: &Win, t: &str| (w.drawn_at.iter()).rfind(|(d, _)| d == t).map(|(_, r)| *r);
    let order = [
        actions::NEW_SESSION,
        "Stop",
        "Setup",
        "Edit kits",
        "Access",
        "Connect",
        "Manage",
        "Rename",
        "Delete",
        actions::HOST_LABEL,
    ];
    let ys: Vec<f32> = (order.iter())
        .map(|t| {
            at(&w, t)
                .unwrap_or_else(|| panic!("{t} in the menu"))
                .center()
                .y
        })
        .collect();
    assert!(
        ys.windows(2).all(|p| p[0] < p[1]),
        "{order:?} top to bottom: {ys:?}"
    );
    let host = at(&w, actions::HOST_LABEL).unwrap();
    assert!(
        host.bottom() <= w.size.y,
        "the whole menu is on screen: {host:?}"
    );
    let count = |w: &Win, t: &str| w.drawn.iter().filter(|d| *d == t).count();
    assert_eq!(count(&w, "Prune"), 1, "the header's");
    w.frame(vec![egui::Event::PointerMoved(host.center())]);
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    assert_eq!(count(&w, "Prune"), 2, "and the submenu's");
}

#[test]
fn a_prompt_names_its_answer_and_puts_it_at_the_right() {
    let mut w = Win::new(on_box());
    w.app.perform(Action::Rename);
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    assert!(!w.drawn.iter().any(|t| t == "OK"), "{:?}", w.drawn);
    let at = |t: &str| (w.drawn_at.iter()).rfind(|(d, _)| d == t).map(|(_, r)| *r);
    let cancel = at("Cancel").expect("Cancel");
    let rename = (w.drawn_at.iter())
        .rfind(|(t, r)| t == "Rename" && (r.center().y - cancel.center().y).abs() < 2.0)
        .expect("Rename on Cancel's line")
        .1;
    assert!(rename.left() > cancel.right());
    let ink = theme::accent(w.ctx.global_style().visuals.dark_mode);
    let filled = (w.fills.iter()).any(|(r, c)| r.contains(rename.center()) && *c == ink);
    assert!(filled, "Rename is filled in the ink");
    click(&mut w, rename.center());
    assert!(w.app.modal.is_none(), "answered");
}

#[test]
fn a_confirmation_puts_its_answer_at_the_right_filled() {
    let mut w = Win::new(on_box());
    w.app.perform(Action::Delete);
    // A window is invisible in its first frame, then fades in.
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    let cancel = (w.drawn_at.iter())
        .find(|(t, _)| t == "Cancel")
        .expect("Cancel")
        .1;
    let delete = (w.drawn_at.iter())
        .find(|(t, r)| t == "Delete" && (r.center().y - cancel.center().y).abs() < 2.0)
        .expect("the answer on Cancel's line")
        .1;
    assert!(delete.left() > cancel.right());
    let dark = w.ctx.global_style().visuals.dark_mode;
    let danger = theme::danger(dark);
    let filled = (w.fills.iter()).any(|(r, c)| r.contains(delete.center()) && *c == danger);
    assert!(filled, "Delete is filled in the danger colour");
}

#[test]
#[cfg(unix)]
fn a_failed_jobs_hint_shows_without_a_fallback() {
    use aisbx_core::app::{PendingJob, PostJob};
    use aisbx_core::job::{Job, Step};
    let job = Job::spawn("Publishing", vec![Step::new("false", vec!["false".into()])]);
    for _ in 0..200 {
        if job.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(job.is_finished(), "`false` ends at once");
    let hint = "Port grammar: [[HOST_IP:]HOST_PORT:]SANDBOX_PORT[/PROTOCOL].";
    let pending = PendingJob {
        title: "Publishing".into(),
        steps: Vec::new(),
        post: PostJob::default(),
        fallback: None,
        fallback_hint: Some(hint.into()),
    };
    let mut w = Win::new(on_box());
    w.app.modal = Some(Modal::Running(job, pending));
    // A window is invisible in its first frame, then fades in.
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    assert!(w.drawn.iter().any(|t| t == hint), "{:?}", w.drawn);
    assert!(!w.drawn.iter().any(|t| t == crate::modal::FALLBACK));
    assert!(w.drawn.iter().any(|t| t == "Failed"), "{:?}", w.drawn);
    assert!(
        w.drawn.iter().any(|t| t == "false failed (exit code 1)"),
        "{:?}",
        w.drawn
    );
}

/// Checked in a Connect step, a Secrets heading, a toast and a failed job's hint.
#[test]
#[cfg(unix)]
fn the_window_draws_cores_code_as_code_never_as_backticks() {
    use aisbx_core::app::{PendingJob, PostJob};
    use aisbx_core::job::{Job, Step};
    let ticked = |w: &Win| {
        (w.drawn.iter())
            .filter(|t| t.contains('`'))
            .cloned()
            .collect::<Vec<_>>()
    };
    let mono = |w: &Win, code: &str| w.mono.iter().any(|m| m == code);
    let mut w = Win::new(on_box());
    w.frame(typed('o'));
    connect_to(&mut w, target_row(Target::Terminal));
    w.frame(Vec::new());
    assert!(
        mono(&w, &format!("ssh {}", sbx::ssh_host("box"))),
        "{:?}",
        w.mono
    );
    assert_eq!(ticked(&w), Vec::<String>::new());
    let mut w = Win::new(on_box());
    w.app.secrets = Some(aisbx_core::testkit::sample_secrets());
    w.frame(typed('S'));
    w.frame(Vec::new());
    assert!(mono(&w, "credentials:"), "{:?}", w.mono);
    assert_eq!(ticked(&w), Vec::<String>::new());
    let mut w = Win::new(on_box());
    w.app
        .set_error("SSH is not set up yet — run `sbx setup ssh` from the row above first");
    (0..3).for_each(|_| w.frame(Vec::new()));
    assert!(mono(&w, "sbx setup ssh"), "{:?} {:?}", w.mono, w.drawn);
    assert_eq!(ticked(&w), Vec::<String>::new());
    let job = Job::spawn("Adding", vec![Step::new("false", vec!["false".into()])]);
    for _ in 0..200 {
        if job.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let pending = PendingJob {
        title: "Adding".into(),
        steps: Vec::new(),
        post: PostJob::default(),
        fallback: None,
        fallback_hint: Some("sbx refuses `sbx kit add` for this kit.".into()),
    };
    let mut w = Win::new(on_box());
    w.app.modal = Some(Modal::Running(job, pending));
    (0..12).for_each(|_| w.frame(Vec::new()));
    assert!(
        mono(&w, "$ false") && mono(&w, "sbx kit add"),
        "{:?}",
        w.mono
    );
    assert_eq!(ticked(&w), Vec::<String>::new());
}

/// A job of three steps where the second is `second` — the first done, the last to come — frozen
/// for the job window's tests.
fn three_steps(second: aisbx_core::job::StepState) -> aisbx_core::job::Job {
    use aisbx_core::job::{Job, StepState, StepView};
    use std::time::Duration;
    let mut printed = vec!["── PREPARE IMAGE".to_string(), "Resolving kits".to_string()];
    if second == StepState::Failed {
        printed.push("error: hook exited 1".into());
    }
    let step =
        |label: &str, command: &str, state, output: Vec<String>, secs: Option<f32>| StepView {
            label: label.into(),
            command: command.into(),
            state,
            output,
            elapsed: secs.map(Duration::from_secs_f32),
        };
    let steps = vec![
        step(
            "sbx secret set-custom",
            "sbx secret set-custom --host gw",
            StepState::Done,
            vec!["Stored".into()],
            Some(0.42),
        ),
        step(
            "sbx create box",
            "sbx create --name box",
            second,
            printed,
            Some(40.2),
        ),
        step(
            "write the label",
            "sbx exec box",
            StepState::Waiting,
            Vec::new(),
            None,
        ),
    ];
    Job::frozen("Creating box", steps, Some("pulling the image"))
}

/// The window with `job` running in it, faded in.
fn on_job(job: aisbx_core::job::Job) -> Win {
    use aisbx_core::app::{PendingJob, PostJob};
    let pending = PendingJob {
        title: job.title.clone(),
        steps: Vec::new(),
        post: PostJob::default(),
        fallback: None,
        fallback_hint: None,
    };
    let mut w = Win::new(on_box());
    w.app.modal = Some(Modal::Running(job, pending));
    // A window is invisible in its first frame, then fades in.
    (0..12).for_each(|_| w.frame(Vec::new()));
    w
}

#[test]
fn a_running_job_lists_its_steps_and_shows_the_running_ones_output() {
    let w = on_job(three_steps(aisbx_core::job::StepState::Running));
    let has = |t: &str| w.drawn.iter().any(|d| d == t);
    assert!(has("Step 2 of 3"), "{:?}", w.drawn);
    for label in ["sbx secret set-custom", "sbx create box", "write the label"] {
        assert!(has(label), "{label}: {:?}", w.drawn);
    }
    assert!(has("0.4 s") && has("40 s"), "{:?}", w.drawn);
    assert!(
        has("pulling the image"),
        "the running step's note: {:?}",
        w.drawn
    );
    let mono = |m: &str| w.mono.iter().any(|t| t == m);
    assert!(
        mono("$ sbx create --name box") && mono("Resolving kits"),
        "{:?}",
        w.mono
    );
    assert!(!mono("Stored"), "a finished step's output is folded");
    let color = |t: &str| (w.colored.iter()).find(|(d, _)| d == t).map(|(_, c)| *c);
    let visuals = w.ctx.global_style().visuals.clone();
    assert_eq!(color("sbx create box"), Some(visuals.strong_text_color()));
    assert_eq!(color("write the label"), Some(visuals.weak_text_color()));
}

#[test]
fn a_click_on_a_finished_step_opens_its_output() {
    let mut w = on_job(three_steps(aisbx_core::job::StepState::Running));
    let row = |w: &Win| {
        (w.drawn_at.iter())
            .find(|(t, _)| t == "sbx secret set-custom")
            .map(|(_, r)| *r)
            .expect("the finished step")
    };
    let at = row(&w).center();
    click(&mut w, at);
    w.frame(Vec::new());
    assert!(w.mono.iter().any(|m| m == "Stored"), "{:?}", w.mono);
    assert!(w
        .mono
        .iter()
        .any(|m| m == "$ sbx secret set-custom --host gw"));
    let at = row(&w).center();
    click(&mut w, at);
    w.frame(Vec::new());
    assert!(!w.mono.iter().any(|m| m == "Stored"), "{:?}", w.mono);
}

/// The reason names the step and its exit code, not its command line again.
#[test]
fn a_failed_step_is_marked_and_its_reason_is_short() {
    let w = on_job(three_steps(aisbx_core::job::StepState::Failed));
    let has = |t: &str| w.drawn.iter().any(|d| d == t);
    assert!(has("Failed at step 2 of 3"), "{:?}", w.drawn);
    assert!(has("sbx create box failed (exit code 1)"), "{:?}", w.drawn);
    let danger = theme::danger(w.ctx.global_style().visuals.dark_mode);
    let color = |t: &str| (w.colored.iter()).find(|(d, _)| d == t).map(|(_, c)| *c);
    assert_eq!(color("sbx create box"), Some(danger));
    assert_eq!(color("error: hook exited 1"), Some(danger));
    assert!(!has("pulling the image"), "a failed job has no note");
}

#[test]
fn the_steps_to_come_past_three_are_counted() {
    use aisbx_core::job::{Job, StepState, StepView};
    let step = |n: usize, state| StepView {
        label: format!("step {n}"),
        command: format!("sbx {n}"),
        state,
        output: Vec::new(),
        elapsed: (state == StepState::Running).then(|| std::time::Duration::from_secs(3)),
    };
    let steps = std::iter::once(step(0, StepState::Running))
        .chain((1..=7).map(|n| step(n, StepState::Waiting)))
        .collect();
    let w = on_job(Job::frozen("Rebuilding box", steps, None));
    let has = |t: &str| w.drawn.iter().any(|d| d == t);
    assert!(has("step 3") && !has("step 4"), "{:?}", w.drawn);
    assert!(has("and 4 more steps"), "{:?}", w.drawn);
}

#[test]
#[cfg(unix)]
fn a_failed_job_offers_its_fallback_as_a_rebuild() {
    use aisbx_core::app::{PendingJob, PostJob};
    use aisbx_core::job::{Job, Step};
    let job = Job::spawn("Adding", vec![Step::new("false", vec!["false".into()])]);
    for _ in 0..200 {
        if job.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let fallback = PendingJob {
        title: "Rebuilding box with the new kits".into(),
        steps: vec![Step::new("sbx rm", vec!["true".into()])],
        post: PostJob::default(),
        fallback: None,
        fallback_hint: None,
    };
    let pending = PendingJob {
        title: "Adding".into(),
        steps: Vec::new(),
        post: PostJob::default(),
        fallback: Some(Box::new(fallback)),
        fallback_hint: None,
    };
    let mut w = Win::new(on_box());
    w.app.modal = Some(Modal::Running(job, pending));
    for _ in 0..12 {
        w.frame(Vec::new());
    }
    assert!(crate::modal::FALLBACK.starts_with(Action::Rebuild.title()));
    let at = |t: &str| (w.drawn_at.iter()).find(|(d, _)| d == t).map(|(_, r)| *r);
    let (close, rebuild) = (
        at("Close").expect("Close"),
        at(crate::modal::FALLBACK).expect("the fallback"),
    );
    assert!(rebuild.left() > close.right() && (rebuild.center().y - close.center().y).abs() < 2.0);
    let danger = theme::danger(w.ctx.global_style().visuals.dark_mode);
    let filled = (w.fills.iter()).any(|(r, c)| r.contains(rebuild.center()) && *c == danger);
    assert!(filled, "filled in the danger colour");
    click(&mut w, rebuild.center());
    let started = |p: &PendingJob| p.title == "Rebuilding box with the new kits";
    assert!(matches!(&w.app.modal, Some(Modal::Running(_, p)) if started(p)));
}

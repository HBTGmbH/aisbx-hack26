//! The window: the sbx gate, then the list of the host's sandboxes down the left and the selected
//! one's page beside it — its actions and details tabs — the folder chooser, core's modals and
//! the sessions being prepared.

use std::path::Path;
use std::time::Duration;

use aisbx_core::actions::Action;
use aisbx_core::app::{App, Screen};
use aisbx_core::{kits, model, sbx};
use eframe::egui::{self, Key, Modifiers, RichText};

use crate::args::Args;
use crate::chooser::{self, Chooser};
use crate::session::{self, Session};
use crate::{
    account, actions, approvals, connect, details, form, modal, roster, secrets, skills, startdir,
    theme, toast, wizard,
};

enum State {
    /// Why the dashboard cannot open, and whether sbx itself was not found.
    Blocked {
        message: String,
        no_sbx: bool,
    },
    Ready(Box<App>),
}

pub struct GuiApp {
    state: State,
    args: Args,
    /// The folder chooser, while it is open.
    chooser: Option<Chooser>,
    session: Session,
}

impl GuiApp {
    pub fn new(args: Args) -> Self {
        let mut state = open(&args);
        let chooser = start(&mut state, args.folder.as_deref());
        Self {
            state,
            args,
            chooser,
            session: Session::default(),
        }
    }
}

/// Which folder the window opens on: FOLDER when given, or the chooser with the error as a toast;
/// else the process's own, except at a filesystem root, which opens the chooser.
fn start(state: &mut State, folder: Option<&Path>) -> Option<Chooser> {
    let State::Ready(app) = state else {
        return None;
    };
    match folder {
        Some(dir) => match app.set_folder(dir.to_path_buf()) {
            Ok(()) => None,
            Err(e) => {
                app.set_error(format!("{e:#}"));
                Some(Chooser::new(&app.cwd))
            }
        },
        None => chooser::opens_at_start(false, &app.cwd).then(|| Chooser::new(&app.cwd)),
    }
}

/// The TUI's start, in its order: the catalog, then the sbx gate, then the `App`.
fn open(args: &Args) -> State {
    let blocked = |e: anyhow::Error, no_sbx| State::Blocked {
        message: format!("{e:#}"),
        no_sbx,
    };
    let catalog = match kits::Catalog::load() {
        Ok(catalog) => catalog,
        Err(e) => return blocked(e, false),
    };
    let version = match sbx::checked_version() {
        Ok(version) => version,
        Err(e) => {
            let no_sbx = e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            });
            return blocked(e, no_sbx);
        }
    };
    match App::new(catalog, args.show_other, version, true, args.auto_update) {
        Ok(mut app) => {
            app.disable_telemetry_by_default();
            State::Ready(Box::new(app))
        }
        Err(e) => blocked(e, false),
    }
}

impl eframe::App for GuiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let State::Ready(app) = &mut self.state {
            app.tick();
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let retry = match &mut self.state {
            State::Blocked { message, no_sbx } => blocked(ui, message, *no_sbx),
            State::Ready(app) => {
                list(ui, app, &mut self.chooser, &mut self.session);
                false
            }
        };
        if retry {
            self.state = open(&self.args);
            self.chooser = start(&mut self.state, self.args.folder.as_deref());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let State::Ready(app) = &mut self.state {
            let _ = app.registry.save();
        }
    }
}

/// The error screen. Returns whether *Retry* was clicked.
fn blocked(ui: &mut egui::Ui, message: &str, no_sbx: bool) -> bool {
    egui::CentralPanel::default()
        .show(ui, |ui| {
            ui.heading("aisbx-ui cannot open the dashboard");
            ui.add_space(8.0);
            ui.label(RichText::new(message).color(ui.visuals().error_fg_color));
            if no_sbx {
                ui.add_space(8.0);
                ui.label(
                    "A GUI started from Finder, the Dock or a desktop menu does not get your \
                     shell's PATH. Start aisbx-ui from a shell, or through bin/unix/aisbx-ui \
                     (bin\\windows\\aisbx-ui.cmd), so it finds sbx.",
                );
            }
            ui.add_space(12.0);
            ui.button("Retry").clicked()
        })
        .inner
}

/// Who answers this frame's keys, decided once before anything answers one: the topmost window
/// (the session dialog, then the chooser, then core's modal), else a form screen, whose keys are
/// all core's, else the grid while it is shown and no widget has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keys {
    Dialog,
    Chooser,
    Modal,
    Form,
    Grid,
    Free,
}

fn keys_owner(ctx: &egui::Context, app: &App, chooser: bool, session: &Session) -> Keys {
    if session.dialog_open() {
        Keys::Dialog
    } else if chooser {
        Keys::Chooser
    } else if app.modal.is_some() {
        Keys::Modal
    } else if form_screen(&app.screen) {
        Keys::Form
    } else if matches!(app.screen, Screen::List) && ctx.memory(|m| m.focused()).is_none() {
        Keys::Grid
    } else {
        Keys::Free
    }
}

/// A screen whose keys are all core's: a form, or one of the lists core moves the cursor over.
fn form_screen(screen: &Screen) -> bool {
    match screen {
        Screen::Create(_)
        | Screen::Kits(_)
        | Screen::StartDir(_)
        | Screen::AuthEdit(_)
        | Screen::Secrets(_)
        | Screen::Skills(_)
        | Screen::RepoSkills(_)
        | Screen::Approvals(_)
        | Screen::Connection(_)
        | Screen::ConnSwitch(_)
        | Screen::Folders(_)
        | Screen::SandboxFolders(_)
        | Screen::Settings(_)
        | Screen::Connect(_) => true,
        Screen::List => false,
    }
}

pub(crate) fn list(
    ui: &mut egui::Ui,
    app: &mut App,
    chooser: &mut Option<Chooser>,
    session: &mut Session,
) {
    let ctx = ui.ctx().clone();
    let keys = keys_owner(&ctx, app, chooser.is_some(), session);
    let busy = session.busy();
    let moved = frame_keys(&ctx, app, chooser, keys, busy);
    let no_window = matches!(keys, Keys::Grid | Keys::Free) && chooser.is_none();
    let dark = ui.visuals().dark_mode;
    let mut open_chooser = false;
    let mut host = None;
    let frame =
        egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(14, 8));
    egui::Panel::top("header").frame(frame).show(ui, |ui| {
        ui.horizontal(|ui| {
            let title = theme::strong(RichText::new(format!("🗁 {}", model::tilde(&app.cwd))))
                .size(theme::SECTION);
            let folder = ui
                .add(egui::Button::new(title).frame_when_inactive(false))
                .on_hover_text("Show another folder's sandboxes first (Ctrl-O / Cmd-O)");
            if folder.clicked() {
                open_chooser = true;
            }
            host_running(ui, app, dark);
            ui.label(
                RichText::new(format!("sbx {}", app.sbx_version))
                    .small()
                    .weak(),
            );
            if app.loading {
                ui.add(egui::Spinner::new())
                    .on_hover_text("Reading the sandboxes");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let list = matches!(app.screen, Screen::List);
                host = actions::header(ui, app, busy, list, no_window);
                let waiting = app.approvals.len();
                if waiting > 0 {
                    let warn = theme::warn(dark);
                    let count =
                        theme::strong(RichText::new(format!("⚑ {waiting} to approve"))).color(warn);
                    let pill = egui::Button::new(count)
                        .fill(theme::tint(warn, dark))
                        .stroke(egui::Stroke::new(1.0, warn.gamma_multiply(0.6)))
                        .corner_radius(12);
                    let tip = toast::approvals_tip(app);
                    ui.add_space(8.0);
                    let response = ui
                        .add_enabled(no_window, pill)
                        .on_hover_text(tip.clone())
                        .on_disabled_hover_text(tip);
                    if response.clicked() {
                        host = Some(Action::Approvals);
                    }
                }
            });
        });
    });
    if open_chooser && no_window {
        *chooser = Some(Chooser::new(&app.cwd));
    }
    if let Some(action) = host {
        actions::run(app, action, busy);
    }
    session::strip(ui, app, session);
    screen(ui, app, moved, busy);
    toast::toast(&ctx, app);
    let covered = matches!(keys, Keys::Dialog | Keys::Chooser);
    modal::show(&ctx, app, keys == Keys::Modal, covered);
    if let Some(ch) = chooser {
        if !chooser::show(&ctx, app, ch, keys == Keys::Dialog) {
            *chooser = None;
        }
    }
    session::dialog(&ctx, session);
    session::take(app, session);
    session::finish(app, session);
}

/// The host's sandboxes beside the folder: a dot, filled while any runs, and how many run.
pub(crate) fn host_running(ui: &mut egui::Ui, app: &App, dark: bool) {
    let Some((running, all)) = app.host_running() else {
        return;
    };
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    let painter = ui.painter();
    match running {
        0 => painter.circle_stroke(
            rect.center(),
            3.5,
            egui::Stroke::new(1.5, theme::stopped(dark)),
        ),
        _ => painter.circle_filled(rect.center(), 4.0, theme::running(dark)),
    };
    let text = format!("{running} of {all} running");
    ui.label(RichText::new(text).weak())
        .on_hover_text("The sandboxes on this host, in this folder and every other");
    ui.add_space(8.0);
}

/// Hand out this frame's keys in the order pressed, before anything is drawn, to the grid, a modal
/// or core's form; leave the rest for the widget with the keyboard. Drop every key after one that
/// opens or closes a window, answers a modal, changes the screen or leaves a session, and after
/// Enter or Esc left to a window. Returns whether the selection moved.
fn frame_keys(
    ctx: &egui::Context,
    app: &mut App,
    chooser: &mut Option<Chooser>,
    keys: Keys,
    busy: bool,
) -> bool {
    let open = |app: &App, chooser: &Option<Chooser>| {
        let modal = app.modal.as_ref().map(std::mem::discriminant);
        let screen = std::mem::discriminant(&app.screen);
        (modal, chooser.is_some(), screen, app.handoff.is_some())
    };
    let (cursor, before) = (app.cursor, open(app, chooser));
    // A form's Tab is core's: the header's buttons, which egui would hand it to, never keep it.
    if keys == Keys::Form {
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }
    let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
    let mut kept = Vec::with_capacity(events.len());
    let mut done = false;
    for event in events {
        use egui::Event::{Key as Pressed, Paste, Text};
        if done && matches!(event, Pressed { .. } | Text(_) | Paste(_)) {
            continue;
        }
        // A form's keys are all core's, text included; none is left for a widget.
        if keys == Keys::Form && matches!(event, Pressed { .. } | Text(_) | Paste(_)) {
            for key in form::form_key(&event) {
                app.on_key(form::along_buttons(app, key));
                if open(app, chooser) != before {
                    done = true;
                    break;
                }
            }
            continue;
        }
        let Pressed {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        else {
            kept.push(event);
            continue;
        };
        let choose = key == Key::O && modifiers.matches_logically(Modifiers::COMMAND);
        let left = match keys {
            Keys::Grid | Keys::Free if choose => {
                *chooser = Some(Chooser::new(&app.cwd));
                false
            }
            Keys::Grid => !actions::key(app, key, modifiers, busy),
            Keys::Modal => {
                !(modal::confirm_key(app, key, modifiers) || modal::help_key(app, key, modifiers))
            }
            Keys::Form | Keys::Dialog | Keys::Chooser | Keys::Free => true,
        };
        if left {
            kept.push(event);
        }
        let answers =
            matches!(key, Key::Enter | Key::Escape) || (keys, key) == (Keys::Free, Key::Space);
        done = (left && answers) || open(app, chooser) != before;
    }
    ctx.input_mut(|i| i.events = kept);
    app.cursor != cursor
}

/// How wide the list starts, and how narrow and wide a drag can make it. Narrower than
/// `roster::NARROW`, so the list starts in its two-line layout: a navigator, not a table.
pub const LIST_W: f32 = 340.0;
pub(crate) const LIST_MIN: f32 = 260.0;
const LIST_MAX: f32 = 720.0;

/// The widest the sandbox's page runs: past it, a value and its label sit too far apart to read
/// as one line.
pub(crate) const PAGE_W: f32 = 860.0;

/// The window's main screen: the list as a navigator down the left, on the ground, and the
/// selected sandbox's page beside it, raised: its head, actions and details tabs.
fn list_and_page(ui: &mut egui::Ui, app: &mut App, moved: bool, busy: bool) {
    let dark = ui.visuals().dark_mode;
    let list = egui::Frame::side_top_panel(ui.style())
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(8, 6));
    egui::Panel::left("sandboxes")
        .resizable(true)
        .default_size(LIST_W)
        .size_range(LIST_MIN..=LIST_MAX)
        .frame(list)
        .show(ui, |ui| roster::list(ui, app, moved, busy));
    let page = egui::Frame::central_panel(ui.style())
        .fill(theme::raised(dark))
        .inner_margin(egui::Margin {
            left: 28,
            right: 20,
            top: 16,
            bottom: 8,
        });
    let mut chosen = None;
    let mut asked = None;
    egui::CentralPanel::default().frame(page).show(ui, |ui| {
        // A column of its own: the panel's ui is as wide as the panel already, and cannot shrink.
        let size = egui::vec2(PAGE_W.min(ui.available_width()), ui.available_height());
        let column = egui::Layout::top_down(egui::Align::Min);
        ui.allocate_ui_with_layout(size, column, |ui| {
            ui.set_width(size.x);
            chosen = actions::head(ui, app, busy);
            actions::panel(ui, app, busy);
            details::strip(ui, app);
            egui::ScrollArea::vertical()
                .id_salt(("details-scroll", app.tab.index()))
                .auto_shrink(false)
                .show(ui, |ui| {
                    if let Some(clicked) = details::body(ui, app) {
                        asked = Some(clicked);
                    }
                });
        });
    });
    match chosen {
        Some(actions::Asked::Action(action)) => actions::run(app, action, busy),
        Some(actions::Asked::Answer(id, allow)) => app.answer_approval(&id, allow),
        None => {}
    }
    if let Some(clicked) = asked {
        clicked.run(app, busy);
    }
}

/// What the window shows under the header: the grid, or the form or list core has open.
fn screen(ui: &mut egui::Ui, app: &mut App, moved: bool, busy: bool) {
    match &app.screen {
        Screen::List => list_and_page(ui, app, moved, busy),
        Screen::Create(_) => wizard::show(ui, app),
        Screen::Kits(_) => crate::kits::show(ui, app),
        Screen::StartDir(_) => startdir::show(ui, app),
        Screen::AuthEdit(_) => account::show_auth(ui, app),
        Screen::ConnSwitch(_) => account::show_switch(ui, app),
        Screen::Secrets(_) => secrets::show_secrets(ui, app),
        Screen::Connection(_) => secrets::show_connection(ui, app),
        Screen::Skills(_) => skills::show_store(ui, app),
        Screen::RepoSkills(_) => skills::show_repo(ui, app),
        Screen::Approvals(_) => approvals::show(ui, app),
        Screen::Connect(_) => connect::show(ui, app),
        Screen::Folders(_) => crate::folders::show(ui, app),
        Screen::SandboxFolders(_) => crate::folders::show_sandbox(ui, app),
        Screen::Settings(_) => crate::settings::show(ui, app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::app::Modal;
    use aisbx_core::kits::Catalog;

    #[test]
    fn the_header_counts_the_hosts_running_sandboxes() {
        let mut app = mixed();
        app.sandboxes[1].running = true;
        let mut w = crate::form_golden::Win::new(app);
        assert!(
            w.drawn.iter().any(|t| t == "1 of 3 running"),
            "{:?}",
            w.drawn
        );
        w.app.sandboxes.clear();
        w.frame(Vec::new());
        assert!(
            !w.drawn.iter().any(|t| t.contains("running")),
            "{:?}",
            w.drawn
        );
    }

    fn mixed() -> App {
        let sandboxes = ["one", "two", "three"]
            .iter()
            .map(|n| model::sample(n, Some("claude-anthropic"), &[]))
            .collect();
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    fn frame(ctx: &egui::Context, app: &mut App, chooser: &mut Option<Chooser>, key: Option<Key>) {
        let events = key
            .map(|key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .into_iter()
            .collect();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 760.0),
            )),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| list(ui, app, chooser, &mut Session::default()))
            .textures_delta
            .clear();
    }

    const NONE: Modifiers = Modifiers::NONE;

    /// A window under test, one frame per call.
    struct Win {
        ctx: egui::Context,
        app: App,
        chooser: Option<Chooser>,
        session: Session,
    }

    impl Win {
        fn new(app: App) -> Self {
            let (ctx, chooser, session) = (egui::Context::default(), None, Session::default());
            Win {
                ctx,
                app,
                chooser,
                session,
            }
        }

        /// One frame with these keys pressed, in order — a typed `x` where the key is `None`.
        fn keys(&mut self, keys: &[(Option<Key>, Modifiers)]) {
            let event = |&(key, modifiers): &(Option<Key>, Modifiers)| match key {
                Some(key) => egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                None => egui::Event::Text("x".into()),
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 760.0),
                )),
                events: keys.iter().map(event).collect(),
                ..Default::default()
            };
            let (app, chooser, session) = (&mut self.app, &mut self.chooser, &mut self.session);
            let mut output = self.ctx.run_ui(input, |ui| list(ui, app, chooser, session));
            output.textures_delta.clear();
        }

        fn key(&mut self, key: Key) {
            self.keys(&[(Some(key), NONE)]);
        }
    }

    #[test]
    fn enter_on_a_focused_button_reaches_that_button_only() {
        let mut reached = Vec::new();
        for tabs in 1..=32 {
            let mut w = Win::new(mixed());
            aisbx_core::terminal::opened();
            for _ in 0..tabs {
                w.key(Key::Tab);
            }
            w.keys(&[]);
            w.key(Key::Enter);
            let started = w.app.handoff.is_some() || w.session.busy() || w.session.dialog_open();
            let started = started || !aisbx_core::terminal::opened().is_empty();
            let window = match &w.app.modal {
                Some(Modal::Prompt(_)) => "prompt",
                Some(Modal::Confirm(_)) => "confirm",
                Some(Modal::Help) => "help",
                _ if w.chooser.is_some() => "folder",
                _ => continue,
            };
            assert!(
                !started,
                "{tabs} tabs: ↵ opened {window} and started a session"
            );
            reached.push(window);
        }
        let all = ["folder", "confirm", "help"];
        assert!(all.iter().all(|w| reached.contains(w)), "{reached:?}");
    }

    /// Keys after one that opens a window are dropped: neither the grid nor the window gets them.
    #[test]
    fn arrows_and_actions_in_one_frame_act_in_order() {
        let run = |keys: &[Key]| {
            let mut w = Win::new(mixed());
            w.app.sandboxes.iter_mut().for_each(|sb| sb.running = true);
            w.keys(&[]);
            w.keys(&keys.iter().map(|k| (Some(*k), NONE)).collect::<Vec<_>>());
            assert_eq!(w.app.cursor, 1, "{keys:?}");
            w
        };
        let w = run(&[Key::ArrowRight, Key::S, Key::ArrowRight, Key::R]);
        assert!(
            matches!(&w.app.modal, Some(Modal::Running(job, _)) if job.title == "Stopping two")
        );
        // Opened in a terminal (recorded under test), or shown in the dialog on a host without one.
        let w = run(&[Key::ArrowRight, Key::Enter]);
        let opened = aisbx_core::terminal::opened().concat().join(" ");
        let shown = format!("{opened} {}", w.session.no_terminal().unwrap_or_default());
        assert!(shown.contains("--name two"), "{shown}");
        // The confirmation opens on Cancel, and stays there a frame later.
        let mut w = run(&[Key::ArrowRight, Key::D, Key::ArrowRight, Key::Enter]);
        w.keys(&[]);
        let Some(Modal::Confirm(confirm)) = &w.app.modal else {
            panic!("no confirmation")
        };
        assert!(confirm.body.concat().contains("two") && !confirm.yes);
    }

    fn pressed(keys: &[Key]) -> Vec<(Option<Key>, Modifiers)> {
        keys.iter().map(|k| (Some(*k), NONE)).collect()
    }

    #[test]
    fn the_keys_after_the_folder_shortcut_are_dropped() {
        for key in [Key::Enter, Key::D] {
            let mut w = Win::new(mixed());
            w.keys(&[]);
            aisbx_core::terminal::opened();
            w.keys(&[(Some(Key::O), Modifiers::COMMAND), (Some(key), NONE)]);
            assert!(w.chooser.is_some() && w.app.modal.is_none(), "{key:?}");
            assert!(w.app.handoff.is_none() && !w.session.busy() && !w.session.dialog_open());
            assert!(aisbx_core::terminal::opened().is_empty(), "{key:?}");
        }
    }

    /// Each of these frames cancels: the keys act in order, with the choice selected at that time.
    #[test]
    fn a_confirmation_answers_its_keys_in_order() {
        let frames: [&[Key]; 3] = [
            &[Key::Enter, Key::ArrowRight],
            &[Key::Escape, Key::Y],
            &[Key::Tab, Key::Tab, Key::Enter],
        ];
        for keys in frames {
            let mut w = Win::new(mixed());
            w.app.perform(Action::Delete);
            w.keys(&[]);
            w.keys(&pressed(keys));
            assert!(w.app.modal.is_none(), "{keys:?} did not cancel");
        }
    }

    /// After `a` unfolds *Sandboxes in other folders*, Up goes to the New row, where `s` stops nothing.
    #[test]
    fn the_arrows_after_unfolding_move_over_the_new_rows() {
        let sandboxes = ["one", "two", "three", "four"].map(|n| {
            let mut sb = model::sample(n, Some("claude-anthropic"), &[]);
            sb.running = true;
            if matches!(n, "three" | "four") {
                sb.workspace = "/other".into();
            }
            sb
        });
        let mut w = Win::new(App::for_test(Catalog::load().unwrap(), sandboxes.into()));
        if w.app.show_other {
            w.key(Key::A);
        }
        w.keys(&[]);
        assert_eq!((w.app.here, w.app.show_other), (2, false));
        w.keys(&pressed(&[Key::A, Key::ArrowUp, Key::S]));
        assert_eq!(
            (w.app.cursor, w.app.show_other),
            (2, true),
            "on the New row"
        );
        assert!(w.app.modal.is_none(), "nothing stopped");
    }

    #[test]
    fn tab_never_lands_on_a_row_and_esc_gives_the_keyboard_back_to_the_list() {
        let mut w = Win::new(mixed());
        aisbx_core::terminal::opened();
        for _ in 0..64 {
            w.key(Key::Tab);
            w.keys(&[]);
            let focused = w.ctx.memory(|m| m.focused()).unwrap();
            let rect = w.ctx.read_response(focused).unwrap().rect;
            assert!(
                !crate::roster::ROW_H.contains(&rect.height()),
                "a sandbox's row has the keyboard"
            );
        }
        w.key(Key::Escape);
        w.keys(&pressed(&[Key::ArrowRight, Key::X]));
        let opened = aisbx_core::terminal::opened().concat().join(" ");
        let shown = format!("{opened} {}", w.session.no_terminal().unwrap_or_default());
        assert!(shown.contains("exec") && shown.contains("two"), "{shown}");
    }

    #[test]
    fn keys_reach_only_the_topmost_window() {
        let mut w = Win::new(mixed());
        w.app.perform(Action::Delete);
        w.keys(&[(Some(Key::O), Modifiers::COMMAND)]);
        assert!(w.chooser.is_none(), "no chooser over the confirmation");
        w.chooser = Some(Chooser::new(&w.app.cwd));
        for key in [Key::Y, Key::Enter, Key::Escape] {
            w.key(key);
            assert!(
                matches!(w.app.modal, Some(Modal::Confirm(_))),
                "{key:?} answered it"
            );
        }
    }

    /// Tab, Shift-Tab and the arrows move the choice and the keyboard focus together.
    #[test]
    fn the_confirmation_answers_what_it_shows_selected() {
        let mut w = Win::new(mixed());
        let yes = |app: &App| matches!(&app.modal, Some(Modal::Confirm(c)) if c.yes);
        // Cancel has the keyboard from the start: Space clicks it.
        w.app.perform(Action::Delete);
        w.key(Key::F1);
        w.key(Key::Space);
        assert!(w.app.modal.is_none(), "Space clicked Cancel");
        w.app.perform(Action::Delete);
        w.key(Key::F1);
        for (key, modifiers, want) in [
            (Key::Tab, NONE, true),
            (Key::Tab, Modifiers::SHIFT, false),
            (Key::ArrowRight, NONE, true),
            (Key::ArrowLeft, NONE, false),
        ] {
            w.keys(&[(Some(key), modifiers)]);
            assert_eq!(yes(&w.app), want, "{key:?}");
        }
        w.key(Key::Enter);
        assert!(w.app.modal.is_none(), "↵ answered Cancel, the selected one");
        w.app.perform(Action::Delete);
        for key in [Key::F1, Key::Tab, Key::Space] {
            w.key(key);
        }
        assert!(
            matches!(w.app.modal, Some(Modal::Running(..))),
            "Space confirmed"
        );
    }

    /// Tab moves the prompt onto Cancel, an idle frame keeps it there, and ↵ cancels.
    #[test]
    fn tab_takes_the_prompt_onto_its_buttons_and_leaves_it_there() {
        let mut w = Win::new(mixed());
        w.app.perform(Action::Rename);
        for key in [
            Some(Key::F1),
            None,
            Some(Key::Tab),
            Some(Key::F1),
            Some(Key::Enter),
        ] {
            w.keys(&[(key, NONE)]);
        }
        assert!(w.app.modal.is_none());
        assert_eq!(
            w.app.selected().unwrap().label,
            "one",
            "cancelled, not renamed"
        );
    }

    #[test]
    fn a_frame_with_a_modal_open_forwards_no_arrow_key() {
        let mut w = Win::new(mixed());
        w.app.perform(Action::Rename);
        assert!(w.app.modal.is_some());
        let before = w.app.cursor;
        for key in [Key::ArrowRight, Key::ArrowDown, Key::End, Key::S] {
            w.key(key);
        }
        assert_eq!(w.app.cursor, before);
        assert!(w.app.handoff.is_none() && !w.session.busy());
    }

    #[test]
    fn a_frame_after_start_takes_the_handoff_and_holds_a_launch() {
        let mut w = Win::new(mixed());
        w.app.perform(Action::Start);
        assert!(w.app.handoff.is_some());
        session::take(&mut w.app, &mut w.session);
        assert!(w.app.handoff.is_none());
        let launch = w.session.launch().expect("a launch is held");
        assert_eq!(launch.handoff().name(), "one");
        assert!(w.session.busy());
        let busy = actions::enabled(&w.app, Action::Start, w.session.busy());
        assert_eq!(busy, Err("another session is being prepared".into()));
        // The next frame opens it: recorded under test, or the dialog on a host without a terminal.
        w.key(Key::F1);
        assert!(!w.session.busy());
        let opened = aisbx_core::terminal::opened();
        assert!(
            (opened.len() == 1 && w.app.status.is_some()) || w.session.dialog_open(),
            "{opened:?}"
        );
    }

    #[test]
    fn enter_on_the_new_row_opens_the_wizard_and_esc_goes_back_to_the_list() {
        let mut w = Win::new(mixed());
        w.app.select(w.app.here);
        w.key(Key::Enter);
        assert!(
            matches!(w.app.screen, Screen::Create(_)),
            "↵ on the New row"
        );
        w.key(Key::Escape);
        assert!(matches!(w.app.screen, Screen::List));
    }

    #[test]
    fn a_frame_with_the_chooser_open_forwards_no_arrow_key_and_esc_closes_it() {
        let ctx = egui::Context::default();
        let mut app = mixed();
        let mut chooser = Some(Chooser::new(&app.cwd));
        frame(&ctx, &mut app, &mut chooser, None);
        let before = app.cursor;
        for key in [Key::ArrowRight, Key::ArrowDown, Key::End] {
            frame(&ctx, &mut app, &mut chooser, Some(key));
        }
        assert!(chooser.is_some(), "still open");
        assert_eq!(app.cursor, before);

        frame(&ctx, &mut app, &mut chooser, Some(Key::Escape));
        assert!(chooser.is_none());
        assert_eq!(app.cwd, std::path::PathBuf::from("/proj"));
        frame(&ctx, &mut app, &mut chooser, Some(Key::ArrowRight));
        assert_ne!(app.cursor, before);
    }

    /// The start rule on a fresh app: the app afterwards, and whether the chooser opened.
    fn started(folder: Option<&Path>) -> (Box<App>, bool) {
        let mut state = State::Ready(Box::new(mixed()));
        let chooser = start(&mut state, folder).is_some();
        match state {
            State::Ready(app) => (app, chooser),
            State::Blocked { .. } => unreachable!(),
        }
    }

    #[test]
    fn a_folder_argument_that_fails_opens_the_chooser_with_the_error() {
        let (app, chooser) = started(Some(Path::new("/no/such/aisbx/folder")));
        assert!(chooser);
        assert_eq!(app.cwd, std::path::PathBuf::from("/proj"));
        let (text, error, _) = app.status.clone().unwrap();
        assert!(error && text.contains("/no/such/aisbx/folder"), "{text}");

        // A folder that exists opens directly; no FOLDER, not at a root: no chooser either.
        let dir = std::env::temp_dir();
        let (app, chooser) = started(Some(&dir));
        assert_eq!((app.cwd.clone(), chooser), (dir, false));
        assert!(!started(None).1);
    }
}

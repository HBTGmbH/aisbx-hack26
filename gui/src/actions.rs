//! The list actions the window offers: the header's and the page's buttons, the row menu, and the
//! TUI's keys while the list has the keyboard. Core decides each action (`Action::from_key`,
//! `App::can`, `App::perform`); this module draws them and turns egui's keys into core's.

use aisbx_core::actions::{Action, Refusal};
use aisbx_core::app::App;
use aisbx_core::create::CreateStep;
use aisbx_core::input::{KeyCode, KeyEvent, KeyModifiers};
use aisbx_core::model::{self, CardNote};
use eframe::egui::{self, RichText};

use crate::{page, roster, theme};

/// The header's actions: the host's, which no selected sandbox changes. *Approvals* shows as a
/// count while a request waits (`shell`), and *Other folders* is the list's heading.
pub const HOST: [Action; 8] = [
    Action::New,
    Action::Secrets,
    Action::Skills,
    Action::Folders,
    Action::Settings,
    Action::Prune,
    Action::Help,
    Action::Refresh,
];

/// The page's first row of actions: what is done with the selected sandbox most.
pub const PRIMARY: [Action; 3] = [Action::Start, Action::Shell, Action::Stop];

/// The page's other actions on the selected sandbox, in labelled groups: setup, access and
/// management. *Delete*, the one that cannot be undone, comes last.
pub const GROUPS: [(&str, &[Action]); 3] = [
    (
        "Setup",
        &[
            Action::EditKits,
            Action::Auth,
            Action::StartDir,
            Action::RepoSkills,
            Action::RefreshModels,
        ],
    ),
    (
        "Access",
        &[
            Action::Connect,
            Action::PublishPort,
            Action::UnpublishPort,
            Action::CheckHost,
            Action::KeepRunning,
        ],
    ),
    ("Manage", &[Action::Rename, Action::Rebuild, Action::Delete]),
];

/// The host's actions as a row's menu and the help sheet list them under [`HOST_LABEL`]: the
/// header's, *Approvals* and *Other folders*, the most used first.
pub const HOST_GROUP: [Action; 10] = [
    Action::New,
    Action::Secrets,
    Action::Skills,
    Action::Folders,
    Action::Settings,
    Action::Approvals,
    Action::Prune,
    Action::ToggleOther,
    Action::Refresh,
    Action::Help,
];

/// The label over [`HOST_GROUP`].
pub const HOST_LABEL: &str = "This host";

/// Every action once, in the window's one order: [`PRIMARY`] with no label, each of [`GROUPS`]
/// under its label, then [`HOST_GROUP`]. A row's menu and the help sheet list the actions so.
pub fn sections() -> Vec<(Option<&'static str>, &'static [Action])> {
    let mut out: Vec<(Option<&'static str>, &'static [Action])> = vec![(None, &PRIMARY)];
    out.extend(
        GROUPS
            .iter()
            .map(|(label, actions)| (Some(*label), *actions)),
    );
    out.push((Some(HOST_LABEL), &HOST_GROUP));
    out
}

/// Why a disabled action is disabled — core's own words, or what selecting a sandbox changes.
pub fn tooltip(refusal: Refusal) -> String {
    match refusal {
        Refusal::Silent => "select a sandbox".into(),
        Refusal::Info(text) | Refusal::Error(text) => text,
    }
}

/// Can `action` run now? `busy`: a session is being prepared, and only one is at a time.
pub fn enabled(app: &App, action: Action, busy: bool) -> Result<(), String> {
    if busy && matches!(action, Action::Start | Action::Shell) {
        return Err("another session is being prepared".into());
    }
    app.can(action).map_err(tooltip)
}

/// The key that runs `action`, as the TUI's footer writes it.
pub fn key_label(action: Action) -> String {
    match action.key().code {
        KeyCode::Enter => "↵".into(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// How a button looks: `Primary` filled in the ink (*Start*, *New sandbox*), `Quiet` framed only
/// under the pointer, `Plain` for the rest, *Delete* in the danger colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    Primary,
    Plain,
    Quiet,
}

/// The page's first row and the main buttons are this tall: an easy target.
const TALL: f32 = 30.0;

/// The room between the page's title and its state pill, and the least the title keeps.
const PILL_GAP: f32 = 16.0;
const TITLE_MIN_W: f32 = 160.0;

/// The page head's text stands this far right of the agent's colour bar.
const HEAD_INDENT: f32 = 16.0;

/// An action's button text for the selected sandbox, worded for its state: *New session* for
/// *Start* on a running one, *Auto-stop* for *Keep running* on one that keeps running.
pub fn title(app: &App, action: Action) -> &'static str {
    title_for(app.selected(), action)
}

/// [`title`] for `sb`, which a right-click may name before it is selected.
pub fn title_for(sb: Option<&model::Sandbox>, action: Action) -> &'static str {
    match action {
        Action::Start if sb.is_some_and(|sb| sb.running) => NEW_SESSION,
        Action::KeepRunning if sb.is_some_and(|sb| sb.keep_running) => "Auto-stop",
        _ => action.title(),
    }
}

/// *Start*'s text on a running sandbox ([`title_for`]).
pub const NEW_SESSION: &str = "New session";

/// A button's tooltip: what the action does — on a running sandbox *Start* opens one more session
/// of its agent — and its key.
fn hover_text(app: &App, action: Action) -> String {
    let what = match action {
        Action::Start if app.selected().is_some_and(|sb| sb.running) => {
            "Open one more session of the sandbox's agent, in a terminal window"
        }
        _ => action.summary(),
    };
    format!("{what} ({})", key_label(action))
}

/// A sandbox's right-click menu: the groups of [`sections`], each key as a shortcut, the host's
/// group as a submenu. An action stays enabled on a sandbox that is not selected, because
/// choosing it selects that sandbox first. Returns the action chosen.
pub fn row_menu(
    ui: &mut egui::Ui,
    app: &App,
    sb: &model::Sandbox,
    selected: bool,
    busy: bool,
) -> Option<Action> {
    let mut chosen = None;
    ui.spacing_mut().item_spacing.y = MENU_GAP;
    for (i, (label, group)) in sections().into_iter().enumerate() {
        if i > 0 {
            ui.separator();
        }
        match label {
            Some(HOST_LABEL) => {
                ui.menu_button(HOST_LABEL, |ui| {
                    ui.spacing_mut().item_spacing.y = MENU_GAP;
                    chosen = menu_items(ui, app, sb, group, selected, busy).or(chosen);
                });
            }
            Some(label) => {
                ui.label(RichText::new(label).small().weak());
                chosen = menu_items(ui, app, sb, group, selected, busy).or(chosen);
            }
            None => chosen = menu_items(ui, app, sb, group, selected, busy).or(chosen),
        }
    }
    chosen
}

/// The room between two rows of [`row_menu`].
const MENU_GAP: f32 = 2.0;

/// One group's items of [`row_menu`]. Returns the action chosen.
fn menu_items(
    ui: &mut egui::Ui,
    app: &App,
    sb: &model::Sandbox,
    group: &[Action],
    selected: bool,
    busy: bool,
) -> Option<Action> {
    let dark = ui.visuals().dark_mode;
    let mut chosen = None;
    for &action in group {
        let state = enabled(app, action, busy);
        let mut text = RichText::new(title_for(Some(sb), action));
        if action == Action::Delete {
            text = text.color(theme::danger(dark));
        }
        let item = egui::Button::new(text).shortcut_text(key_label(action));
        let response = ui.add_enabled(state.is_ok() || !selected, item);
        let response = match state {
            Err(why) if selected => {
                response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t))
            }
            _ => response,
        };
        if response.clicked() {
            chosen = Some(action);
            ui.close();
        }
    }
    chosen
}

/// One action as a button, disabled with its reason. Returns whether it was clicked.
fn button(ui: &mut egui::Ui, app: &App, action: Action, busy: bool, look: Look) -> bool {
    labelled(ui, app, action, title(app, action), busy, look)
}

/// [`button`] with its own text.
fn labelled(
    ui: &mut egui::Ui,
    app: &App,
    action: Action,
    label: &str,
    busy: bool,
    look: Look,
) -> bool {
    let state = enabled(app, action, busy);
    let dark = ui.visuals().dark_mode;
    let text = RichText::new(label);
    let widget = match (look, action) {
        (Look::Primary, _) => egui::Button::new(theme::strong(text).color(theme::on_accent(dark)))
            .fill(theme::accent(dark))
            .min_size(egui::vec2(84.0, TALL)),
        (_, Action::Delete) => egui::Button::new(text.color(theme::danger(dark))),
        (Look::Quiet, _) => egui::Button::new(text).frame_when_inactive(false),
        (Look::Plain, _) => egui::Button::new(text),
    };
    let response = ui
        .add_enabled(state.is_ok(), widget)
        .on_hover_text(hover_text(app, action));
    let response = match state {
        Ok(()) => response,
        Err(why) => response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t)),
    };
    response.clicked()
}

/// The header's host actions in [`HOST`]'s order, right-aligned, *New sandbox* filled at the
/// right edge. Only on the list screen (`list`), disabled while a window covers it (`free` off);
/// *Refresh* shows and works on every screen. Returns the one clicked.
pub fn header(ui: &mut egui::Ui, app: &App, busy: bool, list: bool, free: bool) -> Option<Action> {
    let mut chosen = None;
    // Right to left: the first drawn is rightmost.
    let order = [Action::New]
        .into_iter()
        .chain(HOST.into_iter().rev().filter(|a| *a != Action::New));
    for action in order {
        let refresh = action == Action::Refresh;
        if !list && !refresh {
            continue;
        }
        let look = match action {
            Action::New => Look::Primary,
            _ => Look::Quiet,
        };
        let clicked = ui
            .add_enabled_ui(free || refresh, |ui| button(ui, app, action, busy, look))
            .inner;
        if clicked {
            chosen = Some(action);
        }
        if action == Action::New && list {
            ui.add_space(6.0);
        }
    }
    chosen
}

/// What a row's note asks of you, as the page's head says it: a title, why it matters, and the
/// action that fixes it with its button's text. For requests, that action lists them all.
pub fn attention(note: CardNote) -> (String, String, Action, &'static str) {
    match note {
        CardNote::Unresponsive => (
            "Not responding".into(),
            "The sandbox stopped answering. Stop it, then start it again.".into(),
            Action::Stop,
            "Stop",
        ),
        CardNote::NoToken => (
            "No setup token".into(),
            "Its setup token is not stored, so the agent cannot sign in.".into(),
            Action::Auth,
            "Store a token",
        ),
        CardNote::Approvals(n) => (
            format!(
                "{n} request{} waiting for approval",
                if n == 1 { "" } else { "s" }
            ),
            format!(
                "Allow adds a rule that lets {} through for this sandbox alone; Dismiss keeps it \
                 blocked until the agent asks again.",
                if n == 1 { "it" } else { "one" },
            ),
            Action::Approvals,
            "All requests",
        ),
        CardNote::KitsChanged(kind) => (
            kind.title().into(),
            "A rebuild applies it.".into(),
            Action::Rebuild,
            "Rebuild",
        ),
    }
}

/// What a click on the page's head asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// A list action: the callout's fix, or *Set up a sandbox* on the New row.
    Action(Action),
    /// An answer to one of the sandbox's waiting requests, by its id: *Allow* (`true`) or
    /// *Dismiss* — core's `App::answer_approval`.
    Answer(String, bool),
}

/// How many of a sandbox's waiting requests its callout lists; the rest are counted beside the
/// button that opens the screen listing them all.
pub const CALLOUT_REQUESTS: usize = 3;

/// A request's line in the callout this tall: an item's button and room around it.
const REQUEST_H: f32 = 36.0;

/// A request's answers in the callout, as the approvals screen draws them: *Allow* filled,
/// rightmost, then *Dismiss* — core's option and its text.
pub const CALLOUT_ANSWERS: [(&str, bool); 2] = [("Allow", true), ("Dismiss", false)];

/// The state pill's text: core's status parts, the first one capitalised, as a window's status
/// reads — `Running · 1 session`, `Stopped · 3 days ago`.
pub fn state_label(sb: &model::Sandbox) -> String {
    let text = sb.status_parts().join(" · ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// The pill's padding and its text size.
const PILL_PAD: egui::Vec2 = egui::vec2(10.0, 4.0);
const PILL_TEXT: f32 = 13.0;

fn pill_text(sb: &model::Sandbox) -> String {
    let dot = if sb.running { "●" } else { "○" };
    format!("{dot}  {}", state_label(sb))
}

/// How wide the state pill is drawn.
fn pill_width(ui: &egui::Ui, sb: &model::Sandbox) -> f32 {
    let font = egui::FontId::new(PILL_TEXT, theme::strong_family());
    let galley = (ui.painter()).layout_no_wrap(pill_text(sb), font, egui::Color32::PLACEHOLDER);
    galley.size().x + 2.0 * PILL_PAD.x
}

/// The page's state pill: whether the sandbox runs, and the facts core puts beside that — in the
/// running green on its badge fill, or quiet on a button's grey while it is stopped.
fn state_pill(ui: &mut egui::Ui, sb: &model::Sandbox) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (text, fill, stroke) = match sb.running {
        true => {
            let green = theme::running(dark);
            (
                green,
                theme::badge_fill(green, dark),
                green.gamma_multiply(0.5),
            )
        }
        false => (
            ui.visuals().weak_text_color(),
            ui.visuals().widgets.inactive.weak_bg_fill,
            theme::line(dark),
        ),
    };
    let font = egui::FontId::new(PILL_TEXT, theme::strong_family());
    let galley = ui.painter().layout_no_wrap(pill_text(sb), font, text);
    let size = galley.size() + 2.0 * PILL_PAD;
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        size.y / 2.0,
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + PILL_PAD, galley, text);
    let label = state_label(sb);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    response
}

/// The page's head: the label with its state pill, the agent line, the kits, and a callout while
/// the sandbox needs something, with the fix as a button. On the New row: what a new sandbox is
/// and the button that starts one. Returns what a click asked for.
pub fn head(ui: &mut egui::Ui, app: &App, busy: bool) -> Option<Asked> {
    let dark = ui.visuals().dark_mode;
    let Some(sb) = app.selected() else {
        ui.label(theme::strong(
            RichText::new("New sandbox").size(theme::TITLE),
        ));
        ui.add_space(2.0);
        ui.scope(|ui| {
            ui.set_max_width(crate::form::PARA_W);
            ui.add(
                egui::Label::new(format!(
                    "Set up a sandbox for {}: the agent, how it reaches its model, and its kits. \
                     Nothing is created until the last step.",
                    model::tilde(&app.cwd)
                ))
                .wrap(),
            );
        });
        ui.add_space(14.0);
        steps(ui);
        ui.add_space(14.0);
        let set_up = labelled(
            ui,
            app,
            Action::New,
            "Set up a sandbox",
            busy,
            Look::Primary,
        );
        return set_up.then_some(Asked::Action(Action::New));
    };
    let agent = theme::agent_color(&sb.agent, dark);
    let block = ui.horizontal_top(|ui| {
        ui.add_space(HEAD_INDENT);
        let pill = pill_width(ui, sb);
        let text_w = (ui.available_width() - pill - PILL_GAP).max(TITLE_MIN_W);
        ui.vertical(|ui| {
            ui.set_max_width(text_w);
            let title = theme::strong(RichText::new(&sb.label).size(theme::TITLE));
            ui.add(egui::Label::new(title).wrap());
            ui.label(RichText::new(sb.type_label(&app.catalog)).color(agent));
            if !sb.kit_badges().is_empty() {
                ui.add_space(2.0);
                roster::kit_row(ui, sb);
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.add_space(2.0);
            state_pill(ui, sb);
        });
    });
    let edge = block.response.rect;
    let bar = egui::Rect::from_min_size(edge.min, egui::vec2(4.0, edge.height()));
    ui.painter().rect_filled(bar, 2.0, agent);
    let note = sb.card_note(app.approvals_for(&sb.name))?;
    let (title, mut why, fix, label) = attention(note);
    if let (Some(drift), CardNote::KitsChanged(_)) = (&sb.drift, note) {
        why = drift.reason();
    }
    let colour = match note.is_danger() {
        true => theme::danger(dark),
        false => theme::warn(dark),
    };
    ui.add_space(8.0);
    let mut asked = None;
    egui::Frame::new()
        .fill(theme::tint(colour, dark))
        .stroke(egui::Stroke::new(1.0, colour.gamma_multiply(0.6)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let CardNote::Approvals(_) = note {
                ui.label(theme::strong(RichText::new(title)).color(colour));
                ui.add(
                    egui::Label::new(RichText::new(why).color(ui.visuals().text_color())).wrap(),
                );
                asked = approvals_callout(ui, app, sb, (fix, label), busy);
                return;
            }
            ui.horizontal(|ui| {
                ui.label(theme::strong(RichText::new(title)).color(colour));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if fix_button(ui, app, fix, label, busy) {
                        asked = Some(Asked::Action(fix));
                    }
                });
            });
            ui.add(egui::Label::new(RichText::new(why).color(ui.visuals().text_color())).wrap());
        });
    asked
}

/// The callout's fix as a button, disabled with core's reason. Returns whether it was clicked.
fn fix_button(ui: &mut egui::Ui, app: &App, fix: Action, label: &str, busy: bool) -> bool {
    let state = enabled(app, fix, busy);
    let response = ui
        .add_enabled(state.is_ok(), egui::Button::new(label))
        .on_hover_text(format!("{} ({})", fix.summary(), key_label(fix)));
    let response = match state {
        Ok(()) => response,
        Err(why) => response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t)),
    };
    response.clicked()
}

/// The requests `sb` holds, one line each in its callout: the destination, sbx's reason unless it
/// is the default deny, and the answers [`CALLOUT_ANSWERS`]. Past [`CALLOUT_REQUESTS`] the rest
/// are counted beside `all`. Returns what a click asked for.
fn approvals_callout(
    ui: &mut egui::Ui,
    app: &App,
    sb: &model::Sandbox,
    all: (Action, &str),
    busy: bool,
) -> Option<Asked> {
    let dark = ui.visuals().dark_mode;
    // Not the weak colour: it falls under 4.5:1 contrast on the warning's tint.
    let (strong, text) = (ui.visuals().strong_text_color(), ui.visuals().text_color());
    let requests = app.approvals_of(&sb.name);
    let mut asked = None;
    ui.add_space(6.0);
    for request in requests.iter().take(CALLOUT_REQUESTS) {
        let width = ui.available_width();
        let (rect, line) =
            ui.allocate_exact_size(egui::vec2(width, REQUEST_H), egui::Sense::hover());
        let rule = egui::Stroke::new(1.0, theme::line(dark));
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, rule);
        let notes = app.approval_notes(&request.id).join(" ");
        // Each answer in a child UI of its own, which moves nothing after it.
        let mut right = rect.right();
        for (text, allow) in CALLOUT_ANSWERS {
            let option = if allow { "allow" } else { "dismiss" };
            let look = match allow {
                true => crate::form::Look::Primary,
                false => crate::form::Look::Plain,
            };
            let size = page::button_size(ui, text);
            let top = rect.center().y - size.y / 2.0;
            let at = egui::Rect::from_min_size(egui::pos2(right - size.x, top), size);
            let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(at));
            let offered = request.offers(option);
            let button = page::button(text, look, dark).min_size(size);
            let response = cell.add_enabled(offered, button);
            let response = match offered {
                true => response.on_hover_text(theme::with_code(ui, &notes, |t| t)),
                false => response.on_disabled_hover_text(format!(
                    "sbx does not offer “{option}” for this request"
                )),
            };
            if response.clicked() {
                asked = Some(Asked::Answer(request.id.clone(), allow));
            }
            right = at.left() - 6.0;
        }
        let room = right - 12.0 - rect.left();
        // A reason worth reading keeps its share of the line from a long destination.
        let reason = !request.default_deny();
        let font = egui::FontId::monospace(theme::BODY - 1.0);
        let title_room = if reason { room * 0.6 } else { room };
        let title = page::one_line(ui, request.title.clone(), font, strong, title_room);
        let mut cut = title.elided;
        let at = egui::pos2(rect.left(), rect.center().y - title.size().y / 2.0);
        let after = at.x + title.size().x + 10.0;
        ui.painter().galley(at, title, strong);
        let left = rect.left() + room - after;
        if reason && left > 24.0 {
            let font = egui::FontId::proportional(theme::SMALL + 0.5);
            let reason = page::one_line(ui, request.reason(), font, text, left);
            cut |= reason.elided;
            let at = egui::pos2(after, rect.center().y - reason.size().y / 2.0);
            ui.painter().galley(at, reason, text);
        }
        if cut {
            let whole = format!("{}  {}", request.title, request.reason());
            line.on_hover_text(whole);
        }
    }
    let hidden = requests.len().saturating_sub(CALLOUT_REQUESTS);
    if hidden > 0 {
        let rule = egui::Stroke::new(1.0, theme::line(dark));
        let top = ui.cursor().top() + 0.5;
        ui.painter().hline(ui.max_rect().x_range(), top, rule);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("and {hidden} more")).color(text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (fix, label) = all;
                if fix_button(ui, app, fix, label, busy) {
                    asked = Some(Asked::Action(fix));
                }
            });
        });
    }
    asked
}

/// What each of the wizard's steps asks, in its order: the New row's page lists them.
pub const STEP_GLOSSES: [(CreateStep, &str); 7] = [
    (CreateStep::Provider, "who serves the model"),
    (
        CreateStep::Connection,
        "which LiteLLM gateway, and its model list",
    ),
    (CreateStep::Harness, "the agent, and its account"),
    (
        CreateStep::Models,
        "which models a gateway offers the agent",
    ),
    (CreateStep::Kits, "what is built into the sandbox"),
    (CreateStep::Workspace, "its label, start folder and options"),
    (
        CreateStep::Review,
        "what sbx will create, before anything is",
    ),
];

/// The wizard's steps as its rail numbers them — an outlined mark with the step's number — each
/// with its title and what it asks ([`STEP_GLOSSES`]).
fn steps(ui: &mut egui::Ui) {
    let weak = ui.visuals().weak_text_color();
    let text = ui.visuals().text_color();
    for (step, gloss) in STEP_GLOSSES {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            let painter = ui.painter();
            painter.circle_stroke(rect.center(), 10.0, egui::Stroke::new(1.2, weak));
            let number = step.number().to_string();
            let font = egui::FontId::proportional(theme::SMALL);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                number,
                font,
                text,
            );
            ui.add_space(4.0);
            ui.label(theme::strong(RichText::new(step.title())));
            ui.label(RichText::new(gloss).color(weak));
        });
    }
}

/// A group's menu on the page's toolbar: its label, and a mark that it opens.
pub fn group_menu(label: &str) -> String {
    format!("{label} ▾")
}

/// What a group's menu holds, as its tooltip says before it opens.
pub fn group_tooltip(app: &App, actions: &[Action]) -> String {
    let titles: Vec<&str> = actions.iter().map(|a| title(app, *a)).collect();
    titles.join(", ")
}

/// The page's toolbar for the selected sandbox: [`PRIMARY`] with *Start* filled, a rule, then a
/// menu per group of [`GROUPS`]. Draws nothing on the New row: [`head`] offers its action.
pub fn panel(ui: &mut egui::Ui, app: &mut App, busy: bool) {
    let Some(sb) = app.selected() else {
        return;
    };
    let mut chosen = None;
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().interact_size.y = TALL;
        for action in PRIMARY {
            let look = match action {
                Action::Start => Look::Primary,
                _ => Look::Plain,
            };
            if button(ui, app, action, busy, look) {
                chosen = Some(action);
            }
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        for (heading, actions) in GROUPS {
            let menu = ui.menu_button(group_menu(heading), |ui| {
                ui.spacing_mut().item_spacing.y = MENU_GAP;
                menu_items(ui, app, sb, actions, true, busy)
            });
            if let Some(Some(action)) = menu.inner {
                chosen = Some(action);
            }
            menu.response.on_hover_text(group_tooltip(app, actions));
        }
    });
    ui.add_space(14.0);
    if let Some(action) = chosen {
        app.perform(action);
    }
}

/// A key egui reports, as core's key — only the keys the actions answer to.
fn core_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<KeyEvent> {
    let ctrl = match modifiers.ctrl || modifiers.command {
        true => KeyModifiers::CONTROL,
        false => KeyModifiers::NONE,
    };
    let code = match key {
        egui::Key::Enter => KeyCode::Enter,
        egui::Key::F5 => KeyCode::F(5),
        egui::Key::Delete => KeyCode::Delete,
        egui::Key::Questionmark => KeyCode::Char('?'),
        other => {
            let mut name = other.name().chars();
            let (Some(c), None) = (name.next(), name.next()) else {
                return None;
            };
            if !c.is_ascii_alphabetic() {
                return None;
            }
            KeyCode::Char(match modifiers.shift {
                true => c.to_ascii_uppercase(),
                false => c.to_ascii_lowercase(),
            })
        }
    };
    Some(KeyEvent::new(code, ctrl))
}

/// Run `action` as its key would: refused with core's words, or — Start and Shell while a session
/// is being prepared — with that.
pub fn run(app: &mut App, action: Action, busy: bool) {
    match enabled(app, action, busy) {
        Err(why) if busy && matches!(action, Action::Start | Action::Shell) => app.set_info(why),
        _ => app.perform(action),
    }
}

/// The keys `1`…`7`, which open the details tabs in `Tab::ALL`'s order.
pub const TAB_KEYS: [egui::Key; 7] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
];

/// One of the list's keys, as the TUI's list answers it: an arrow moves over the rows, `1`…`7`
/// open a details tab, an action runs on the sandbox selected now. Returns false for a key the
/// list does not take.
pub fn key(app: &mut App, key: egui::Key, modifiers: egui::Modifiers, busy: bool) -> bool {
    let tab = TAB_KEYS.iter().position(|k| *k == key);
    match roster::NAV_KEYS.iter().find(|(k, _)| *k == key) {
        Some((_, code)) => app.on_key(KeyEvent::from(*code)),
        None if tab.is_some() && modifiers.is_none() => {
            let digit = char::from_digit(tab.unwrap_or(0) as u32 + 1, 10).unwrap_or('1');
            app.on_key(KeyEvent::from(KeyCode::Char(digit)));
        }
        None => match core_key(key, modifiers).and_then(Action::from_key) {
            Some(action) => run(app, action, busy),
            _ => return false,
        },
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::kits::Catalog;
    use aisbx_core::model;

    /// Every action in the order [`sections`] lists them.
    fn listed() -> Vec<Action> {
        sections()
            .into_iter()
            .flat_map(|(_, group)| group.iter().copied())
            .collect()
    }

    #[test]
    fn the_sections_list_every_action_once() {
        let mut listed = listed();
        let count = listed.len();
        assert_eq!(listed[..PRIMARY.len()], PRIMARY);
        let panel: Vec<Action> = PRIMARY.into_iter().chain(secondary()).collect();
        assert_eq!(listed[..panel.len()], panel[..], "the page's order");
        assert_eq!(listed[panel.len()..], HOST_GROUP, "the host's last");
        let order = |a: &Action| Action::ALL.iter().position(|b| b == a);
        listed.sort_by_key(order);
        listed.dedup();
        assert_eq!(listed.len(), count, "no action twice");
        assert_eq!(listed, Action::ALL, "no action left out");
        let host: Vec<Action> = HOST
            .into_iter()
            .chain([Action::Approvals, Action::ToggleOther])
            .collect();
        assert!(
            HOST_GROUP.iter().all(|a| host.contains(a)),
            "the header's own"
        );
    }

    /// Every action of [`GROUPS`], in their order.
    fn secondary() -> impl Iterator<Item = Action> {
        GROUPS
            .into_iter()
            .flat_map(|(_, actions)| actions.iter().copied())
    }

    /// The places: the header, the page's first row, its groups, the approvals count and the
    /// *Other folders* heading.
    #[test]
    fn every_action_has_one_place() {
        let groups: Vec<Action> = secondary().collect();
        let mut placed: Vec<Action> = [HOST.as_slice(), &PRIMARY, &groups]
            .concat()
            .into_iter()
            .chain([Action::Approvals, Action::ToggleOther])
            .collect();
        let count = placed.len();
        let order = |a: &Action| Action::ALL.iter().position(|b| b == a);
        placed.sort_by_key(order);
        placed.dedup();
        assert_eq!(placed.len(), count, "no action twice");
        assert_eq!(placed, Action::ALL, "no action left out");
    }

    #[test]
    fn delete_is_last_and_every_group_has_actions() {
        assert_eq!(secondary().last(), Some(Action::Delete));
        assert!(GROUPS.iter().all(|(_, actions)| !actions.is_empty()));
    }

    #[test]
    fn the_tab_strip_wraps_into_an_even_grid() {
        use crate::details::tab_grid;
        assert_eq!(tab_grid(700.0, 90.0), (7, 1));
        assert_eq!(tab_grid(560.0, 90.0), (4, 2), "six and one would be uneven");
        assert_eq!(tab_grid(370.0, 90.0), (4, 2));
        assert_eq!(tab_grid(200.0, 90.0), (2, 4));
        assert_eq!(tab_grid(10.0, 90.0), (1, 7));
    }

    #[test]
    fn the_tabs_sit_on_one_line_while_they_fit() {
        use crate::details::tab_cells;
        let titles = [60.0, 70.0, 50.0, 36.0, 58.0, 72.0, 56.0];
        let line = tab_cells(700.0, &titles);
        assert!(line.iter().all(|c| c.top() == 0.0), "{line:?}");
        assert!(line.windows(2).all(|p| p[1].left() == p[0].right()));
        assert!(line
            .iter()
            .zip(titles)
            .all(|(c, w)| c.width() > w && c.width() < w + 30.0));
        let grid = tab_cells(300.0, &titles);
        let widths: Vec<f32> = grid.iter().map(|c| c.width()).collect();
        assert!(
            widths.windows(2).all(|p| p[0] == p[1]),
            "even cells: {widths:?}"
        );
        assert!(grid.iter().any(|c| c.top() > 0.0), "more than one line");
        assert!(grid.iter().all(|c| c.right() <= 300.0 + 0.01));
    }

    #[test]
    fn a_disabled_action_says_why_in_core_words() {
        assert_eq!(tooltip(Refusal::Silent), "select a sandbox");
        let stopped = "box is already stopped".to_string();
        assert_eq!(tooltip(Refusal::Info(stopped.clone())), stopped);
        assert_eq!(tooltip(Refusal::Error("gone".into())), "gone");

        let sandboxes = vec![model::sample("box", Some("claude-anthropic"), &[])];
        let app = App::for_test(Catalog::load().unwrap(), sandboxes);
        assert_eq!(app.cursor, 0, "on the sandbox");
        assert_eq!(enabled(&app, Action::Stop, false), Err(stopped));
        assert_eq!(enabled(&app, Action::Start, false), Ok(()));
        let busy = Err("another session is being prepared".to_string());
        assert_eq!(enabled(&app, Action::Start, true), busy);
        assert_eq!(enabled(&app, Action::Rename, true), Ok(()));
    }

    #[test]
    fn keys_map_onto_core_keys_with_their_modifiers() {
        use egui::{Key, Modifiers as M};
        for (key, modifiers, want) in [
            (Key::Enter, M::NONE, Some(Action::Start)),
            (Key::X, M::NONE, Some(Action::Shell)),
            (Key::R, M::NONE, Some(Action::Rename)),
            (Key::R, M::CTRL, Some(Action::Refresh)),
            (Key::R, M::SHIFT, Some(Action::Rebuild)),
            (Key::F5, M::NONE, Some(Action::Refresh)),
            (Key::Delete, M::NONE, Some(Action::Delete)),
            (Key::Questionmark, M::NONE, Some(Action::Help)),
            (Key::Questionmark, M::SHIFT, Some(Action::Help)),
            (Key::ArrowUp, M::NONE, None),
        ] {
            let got = core_key(key, modifiers).and_then(Action::from_key);
            assert_eq!(got, want, "{key:?} {modifiers:?}");
        }
        let labels = [Action::Start, Action::Refresh, Action::Stop].map(key_label);
        assert_eq!(labels, ["↵", "F5", "s"]);
    }
}

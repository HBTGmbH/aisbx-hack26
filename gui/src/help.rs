//! The help sheet (`?`, core's `Modal::Help`): every list action with its title, core's summary and
//! its keys, grouped as a row's menu groups them, then the window's own keys. The task stands on
//! the left, its keys as keycaps at the right edge, as a menu shows its shortcuts.

use aisbx_core::actions::Action;
use aisbx_core::app::App;
use aisbx_core::input::{KeyCode, KeyEvent};
use eframe::egui::{self, RichText};

use crate::{actions, theme};

/// What a row of the sheet's *Window* part is done with: keys, each drawn as a keycap, or a
/// pointer gesture, in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Keys(&'static [&'static str]),
    Pointer(&'static str),
}

/// The window's own keys: what moves, opens and closes, beside the actions. `Cmd` stands for
/// the platform's command key (`command_key`).
pub const WINDOW_KEYS: [(&str, Input); 9] = [
    ("Move the selection", Input::Keys(&["←", "→", "↑", "↓"])),
    (
        "Jump to the first or last sandbox, or a page on",
        Input::Keys(&["Home", "End", "PageUp", "PageDown"]),
    ),
    (
        "Show a details tab, in the strip's order",
        Input::Keys(&["1", "2", "3", "4", "5", "6", "7"]),
    ),
    ("Start a sandbox", Input::Pointer("Double-click its row")),
    (
        "Show a sandbox's actions",
        Input::Pointer("Right-click its row"),
    ),
    (
        "Show another folder's sandboxes first",
        Input::Keys(&["Cmd-O"]),
    ),
    (
        "Go back, close, or give the keyboard back to the list",
        Input::Keys(&["Esc"]),
    ),
    (
        "Move between a form's rows",
        Input::Keys(&["Tab", "Shift-Tab"]),
    ),
    ("Close this sheet", Input::Keys(&["?", "Esc", "↵", "q"])),
];

/// The platform's name for the command key: Cmd on macOS, Ctrl elsewhere. The window takes
/// either (`actions::core_key`); the sheet names the one this keyboard has.
pub fn command_key(key: &str) -> String {
    match (key.strip_prefix("Cmd-"), cfg!(target_os = "macos")) {
        (Some(rest), false) => format!("Ctrl-{rest}"),
        _ => key.to_string(),
    }
}

/// The keys that run `action`, a keycap each: its own, then its aliases.
pub fn keys(action: Action) -> Vec<String> {
    let own = actions::key_label(action);
    match action {
        Action::Delete => vec![own, "Delete".into()],
        Action::Refresh => vec![own, command_key("Cmd-R")],
        Action::Start
        | Action::Shell
        | Action::New
        | Action::EditKits
        | Action::Auth
        | Action::Secrets
        | Action::Connect
        | Action::StartDir
        | Action::Skills
        | Action::RepoSkills
        | Action::Folders
        | Action::Approvals
        | Action::KeepRunning
        | Action::RefreshModels
        | Action::Prune
        | Action::Rename
        | Action::Rebuild
        | Action::Stop
        | Action::PublishPort
        | Action::UnpublishPort
        | Action::CheckHost
        | Action::ToggleOther
        | Action::Help => vec![own],
    }
}

/// One action's row of the sheet: its keys, title and core's summary.
pub type ActionRow = (Vec<String>, &'static str, &'static str);

/// The sheet's action rows in [`actions::sections`]' groups, each under its heading; the first
/// group, which has no label there, as *Sandbox*.
pub fn action_sections() -> Vec<(&'static str, Vec<ActionRow>)> {
    actions::sections()
        .into_iter()
        .map(|(label, group)| {
            let rows = group
                .iter()
                .map(|&a| (keys(a), a.title(), a.summary()))
                .collect();
            (label.unwrap_or("Sandbox"), rows)
        })
        .collect()
}

/// A key drawn as a key: framed and raised, in the monospace face.
fn keycap(ui: &mut egui::Ui, key: &str) {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(theme::raised(dark))
        .stroke(egui::Stroke::new(1.0, theme::line(dark)))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(key).monospace());
        });
}

/// How wide the sheet is at most: a page's column (`page::COLUMN`).
pub const SHEET_W: f32 = 760.0;
/// The width of an action's title, so that every summary starts on one line down the sheet.
const TITLE_W: f32 = 132.0;
/// The room between a row's task and its keys, and around a row's text.
const GAP: f32 = 16.0;
const ROW_H: f32 = 28.0;

/// What a row is done with, as the sheet draws it: its keys, each a keycap, or a pointer gesture
/// in words.
enum Shortcut<'a> {
    Keys(&'a [String]),
    Pointer(&'a str),
}

/// One row of the sheet, the full width: the task on the left — an action's title in its own
/// column, then what it does in the weak colour — and what does it at the right edge, so the
/// keys line up down the sheet. A row of the window's own keys has a task and no title.
/// `stripe`: every other row lies on the stripe.
fn row(ui: &mut egui::Ui, stripe: bool, title: Option<&str>, task: &str, does: Shortcut) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::hover(),
    );
    if stripe {
        (ui.painter()).rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
    }
    let inner = rect.shrink2(egui::vec2(8.0, 0.0));
    let layout = egui::Layout::right_to_left(egui::Align::Center);
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(layout));
    ui.spacing_mut().item_spacing.x = 4.0;
    // Right to left: the last key first, so that they read in their order.
    match does {
        Shortcut::Keys(keys) => keys.iter().rev().for_each(|key| keycap(&mut ui, key)),
        Shortcut::Pointer(gesture) => {
            ui.label(gesture);
        }
    }
    ui.add_space(GAP);
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(title) = title {
            let size = egui::vec2(TITLE_W, ROW_H);
            ui.allocate_ui_with_layout(
                size,
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(TITLE_W);
                    ui.add(egui::Label::new(title).truncate());
                },
            );
        }
        let task = match title {
            Some(_) => RichText::new(task).weak(),
            None => RichText::new(task),
        };
        ui.add(egui::Label::new(task).truncate());
    });
}

/// A section of the sheet: its heading, then its rows, striped.
fn section(ui: &mut egui::Ui, heading: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    ui.label(theme::strong(RichText::new(heading).size(theme::SECTION)));
    ui.add_space(4.0);
    ui.spacing_mut().item_spacing.y = 2.0;
    add(ui);
}

/// Draw the sheet. *Close* closes it as Esc does, through core. `covered`: another window is open
/// over it.
pub fn show(ctx: &egui::Context, app: &mut App, covered: bool) {
    let close = egui::Modal::new(egui::Id::unique("aisbx-help"))
        .show(ctx, |ui| {
            if covered {
                ui.disable();
            }
            ui.set_width(SHEET_W.min(ctx.content_rect().width() - 64.0));
            ui.heading("Actions and keys");
            ui.add_space(2.0);
            let tall = ctx.content_rect().height() * 0.7;
            egui::ScrollArea::vertical()
                .id_salt("help-scroll")
                .max_height(tall)
                .min_scrolled_height(tall)
                .show(ui, |ui| {
                    for (heading, rows) in action_sections() {
                        section(ui, heading, |ui| {
                            for (i, (keys, title, summary)) in rows.iter().enumerate() {
                                row(ui, i % 2 == 1, Some(title), summary, Shortcut::Keys(keys));
                            }
                        });
                    }
                    section(ui, "Window", |ui| {
                        for (i, (task, input)) in WINDOW_KEYS.into_iter().enumerate() {
                            let keys: Vec<String>;
                            let does = match input {
                                Input::Keys(k) => {
                                    keys = k.iter().map(|k| command_key(k)).collect();
                                    Shortcut::Keys(&keys)
                                }
                                Input::Pointer(gesture) => Shortcut::Pointer(gesture),
                            };
                            row(ui, i % 2 == 1, None, task, does);
                        }
                    });
                });
            ui.add_space(8.0);
            // A lone button at the right, as every dialog's answer is.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Button::new("Close").min_size(egui::vec2(84.0, 30.0)))
                    .clicked()
            })
            .inner
        })
        .inner;
    if close && !covered {
        app.on_key(KeyEvent::from(KeyCode::Esc));
    }
}

//! The host folders screen (`F`): the folders every sandbox mounts beside its workspace, each
//! with its access and purpose, and the fields that add a folder or change one. Browse… opens
//! core's folder picker for the folder field. Core keeps the list and saves it.

use aisbx_core::app::{App, FoldersRow, Hit, Screen, FOLDERS_NOTE};
use aisbx_core::folders::Access;
use aisbx_core::input::{KeyCode, KeyEvent};
use aisbx_core::model;
use eframe::egui;

use crate::controls::{self, Input, Mark};
use crate::form::{self, Look, Rows};
use crate::page::{self, Item, State};
use crate::{theme, wizard};

/// The widest an input box is drawn, as on the wizard.
const FIELD: f32 = 520.0;

/// A folder's buttons and the key each one sends: `↵` puts it in the fields, `d` drops it.
const ROW_ACTIONS: [(&str, KeyCode, Look); 2] = [
    ("Edit", KeyCode::Enter, Look::Plain),
    ("Delete", KeyCode::Char('d'), Look::Danger),
];

pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Folders(folders) = &app.screen else {
        return;
    };
    let rows = folders.rows();
    let focus = folders.focus.min(rows.len() - 1);
    let focused = rows[focus];
    let mut list = Rows::new(ui, "folders", focus);
    let index = |row: FoldersRow| form::index_of(&rows, row);
    let dark = ui.visuals().dark_mode;
    let mut picked = None;
    let mut action = None;
    let mut access = None;
    let mut key = None;

    // Save says what it does: the fields add a folder, or change the one being edited.
    let save = match folders.editing {
        Some(_) => "Save changes",
        None => "Add folder",
    };
    page::bar(ui, "folders", |ui| {
        list.buttons(
            ui,
            &[
                (save, index(FoldersRow::Save), false),
                ("Close", index(FoldersRow::Close), false),
            ],
        );
    });
    page::body(ui, "folders", "Host folders", FOLDERS_NOTE, |ui| {
        let weak = ui.visuals().weak_text_color();
        if let Some(error) = &folders.error {
            page::paragraph(ui, error, theme::danger(dark));
        }
        page::heading(ui, "Folders", "");
        if folders.folders.is_empty() {
            page::paragraph(ui, "None yet: add one below.", weak);
        } else {
            let labels: Vec<(&str, Look)> = ROW_ACTIONS.iter().map(|(l, _, k)| (*l, *k)).collect();
            let reserve = page::actions_width(ui, &labels);
            page::group(ui, |ui| {
                for (n, f) in folders.folders.iter().enumerate() {
                    // The folders are the screen's first rows.
                    let i = n;
                    // A path, as the field below takes it: in the code face.
                    let title = format!("`{}`", model::tilde(&f.path));
                    let item = Item {
                        title: &title,
                        mark: None,
                        meta: &f.purpose,
                        state: Some(State {
                            text: wizard::sentence(f.access.label()),
                            color: access_color(f.access, ui.visuals()),
                            value: false,
                        }),
                        actions: &labels,
                        reserve,
                        selected: list.is_focused(i),
                        rule: n > 0,
                    };
                    // No notes under the selected one: its buttons say what it offers, and the
                    // page's note says when a change takes effect.
                    let got = page::item(ui, item, list.follows(i), |_| {});
                    if got.item {
                        picked = Some(i);
                    }
                    if let Some(a) = got.action {
                        action = Some((i, ROW_ACTIONS[a].1));
                    }
                }
            });
        }
        let heading = match folders.editing {
            Some(_) => "Change the folder",
            None => "Add a folder",
        };
        page::heading_with(ui, heading, "", |ui| {
            if folders.editing.is_some() {
                ui.add_space(8.0);
                let new = egui::Button::new("Add a new one instead").sense(egui::Sense::CLICK);
                if ui.add(new).clicked() {
                    key = Some(KeyCode::Char('n'));
                }
            }
        });
        let width = ui.available_width().min(FIELD);
        for (row, label, input, placeholder) in [
            (
                FoldersRow::Path,
                "Folder",
                &folders.path,
                "/path/to/folder or ~/folder",
            ),
            (
                FoldersRow::Purpose,
                "Purpose",
                &folders.purpose,
                "what the agent finds there",
            ),
        ] {
            let gloss = folders.help(row).text;
            let response = controls::labelled(ui, label, &gloss, None, |ui| {
                let field = Input {
                    label,
                    input,
                    masked: false,
                    placeholder,
                    focused: focused == row,
                    problem: false,
                    monospace: row == FoldersRow::Path,
                    suggested: false,
                };
                controls::input_box(ui, field, width)
            });
            list.take(&response, index(row));
            if row == FoldersRow::Path {
                ui.add_space(4.0);
                list.button(ui, "Browse…", index(FoldersRow::Browse), Look::Plain)
                    .on_hover_text(folders.help(FoldersRow::Browse).text);
                let at = rows
                    .iter()
                    .position(|r| *r == FoldersRow::Access)
                    .unwrap_or(0);
                ui.add_space(4.0);
                ui.label(theme::strong(egui::RichText::new("Access").size(13.5)));
                // Side by side: two answers to one question.
                let half = (width - ui.spacing().item_spacing.x) / 2.0;
                ui.horizontal_top(|ui| {
                    for value in Access::ALL {
                        let item = controls::Item {
                            mark: Mark::Radio,
                            title: &wizard::sentence(value.label()),
                            detail: match value {
                                Access::Read => "nothing in the sandbox can change it",
                                Access::Write => "the agent can change and delete what is in it",
                            },
                            badges: Vec::new(),
                            color: theme::accent(dark),
                            checked: folders.access == value,
                            enabled: true,
                            focused: focused == FoldersRow::Access && folders.access == value,
                        };
                        if controls::check_item(ui, item, half).clicked() {
                            access = Some((at, value));
                        }
                    }
                });
            }
        }
    });
    if let Some((i, code)) = action {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(code));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some((at, value)) = access {
        app.click(Hit::FormFocus(at));
        let current = matches!(&app.screen, Screen::Folders(f) if f.access == value);
        if !current {
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        }
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    } else if let Some(code) = key {
        // Off the text fields first, or the key is typed into the one the cursor is on.
        app.click(Hit::FormFocus(index(FoldersRow::Save).unwrap_or(0)));
        app.on_key(KeyEvent::from(code));
    }
}

/// How a folder's access reads in the list: read-write, where the agent can delete, in the
/// warning colour; read-only muted.
fn access_color(access: Access, visuals: &egui::Visuals) -> egui::Color32 {
    match access {
        Access::Read => visuals.weak_text_color(),
        Access::Write => theme::warn(visuals.dark_mode),
    }
}

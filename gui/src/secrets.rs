//! The secrets screen (`S`) and the LiteLLM connection dialog it opens (`n`, Enter on a
//! connection). Core decides every row and what is said about it. No value is ever shown: a key is
//! typed into a field drawn as dots and reaches sbx only on stdin.

use aisbx_core::app::CreateRow;
use aisbx_core::app::{App, ConnMode, ConnRow, Empty, Hit, Screen, SecretRow};
use aisbx_core::input::{KeyCode, KeyEvent};
use aisbx_core::{create, litellm};
use eframe::egui::{self, RichText};

use crate::controls::{self, Input};
use crate::form::{self, Look, Rows};
use crate::page::{self, Item, State};
use crate::{theme, wizard};

/// The widest an input box of the connection dialog is drawn, as on the wizard.
const FIELD: f32 = 460.0;

/// What a row says where sbx shows no value.
fn empty_text(empty: Empty) -> &'static str {
    match empty {
        Empty::Unknown => "…",
        Empty::NotListed => "not listed by sbx",
        Empty::SignedIn => "signed in",
        Empty::NotSignedIn => "not signed in",
        Empty::NotStored => "⚠ not stored",
    }
}

/// What can be done with a secret's row, as buttons under it: the key each one sends (`↵` stores
/// or replaces, opens a connection; `d` deletes or signs out), in core's words for the row.
pub fn row_actions(app: &App, row: &SecretRow) -> Vec<(&'static str, KeyCode, Look)> {
    let stored = app.secret_view(row).stored.is_some();
    let (edit, delete) = (KeyCode::Enter, KeyCode::Char('d'));
    match row {
        SecretRow::Connection(_) => vec![
            ("Edit", edit, Look::Plain),
            ("Delete", delete, Look::Danger),
        ],
        SecretRow::LitellmOrphan(_) => vec![("Delete the key", delete, Look::Danger)],
        // Written by a sign-in inside a sandbox, never here: only signing out is.
        SecretRow::Oauth | SecretRow::AgyLogin if stored => {
            vec![("Sign out", delete, Look::Danger)]
        }
        SecretRow::Oauth | SecretRow::AgyLogin => Vec::new(),
        _ if stored => vec![
            ("Replace", edit, Look::Plain),
            ("Delete", delete, Look::Danger),
        ],
        SecretRow::Close => Vec::new(),
        _ => vec![("Store", edit, Look::Primary)],
    }
}

/// Draw the secrets screen: one group per kind of secret, each secret with its state and the
/// buttons its keys stand for; a click on a secret selects it, a button acts on its own secret.
pub fn show_secrets(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Secrets(secrets) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = view.secret_rows();
    let focus = secrets.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "secrets", focus);
    let close = form::index_of(&rows, SecretRow::Close);
    let dark = ui.visuals().dark_mode;
    let mut groups: Vec<(Option<&'static str>, Vec<usize>)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if *row == SecretRow::Close {
            continue;
        }
        let heading = App::secret_heading(row);
        match groups.last_mut() {
            Some((h, items)) if *h == heading => items.push(i),
            _ => groups.push((heading, vec![i])),
        }
    }
    let mut key = None;
    let mut picked = None;
    let mut action = None;

    page::bar(ui, "secrets", |ui| {
        ui.horizontal(|ui| {
            list.button(ui, "Close", close, Look::Plain);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Anywhere on the screen, as `n` is.
                let new = egui::Button::new("New LiteLLM connection")
                    .min_size(egui::vec2(72.0, 26.0))
                    .sense(egui::Sense::CLICK);
                if ui.add(new).clicked() {
                    key = Some(KeyCode::Char('n'));
                }
            });
        });
    });
    page::body(
        ui,
        "secrets",
        "Stored secrets",
        aisbx_core::app::SECRETS_NOTE,
        |ui| {
            let weak = ui.visuals().weak_text_color();
            if view.secrets.is_none() {
                page::paragraph(ui, "Reading `sbx secret ls`…", weak);
            }
            for (heading, items) in &groups {
                page::heading(ui, heading.unwrap_or("Other"), "");
                // Every item keeps room for the group's widest buttons, so the values line up.
                let reserve = (items.iter())
                    .map(|i| {
                        let actions = row_actions(view, &rows[*i]);
                        let labels: Vec<(&str, Look)> =
                            actions.iter().map(|(l, _, look)| (*l, *look)).collect();
                        page::actions_width(ui, &labels)
                    })
                    .fold(0.0, f32::max);
                page::group(ui, |ui| {
                    for (n, i) in items.iter().enumerate() {
                        let row = &rows[*i];
                        let secret = view.secret_view(row);
                        let (text, color, value) = match (&secret.stored, secret.empty) {
                            (Some(masked), _) => (masked.clone(), theme::running(dark), true),
                            (None, Empty::NotStored) => (
                                empty_text(Empty::NotStored).to_string(),
                                theme::warn(dark),
                                false,
                            ),
                            (None, Empty::SignedIn) => (
                                empty_text(Empty::SignedIn).to_string(),
                                theme::running(dark),
                                false,
                            ),
                            (None, empty) => (empty_text(empty).to_string(), weak, false),
                        };
                        let actions = row_actions(view, row);
                        let labels: Vec<(&str, Look)> =
                            actions.iter().map(|(l, _, look)| (*l, *look)).collect();
                        let item = Item {
                            title: &secret.name,
                            mark: None,
                            meta: &secret.note,
                            state: Some(State { text, color, value }),
                            actions: &labels,
                            reserve,
                            selected: list.is_focused(*i),
                            rule: n > 0,
                        };
                        let got = page::item(ui, item, list.follows(*i), |ui| {
                            page::notes(ui, &view.secret_notes(row).join(" "));
                        });
                        if got.item {
                            picked = Some(*i);
                        }
                        if let Some(a) = got.action {
                            action = Some((*i, actions[a].1));
                        }
                    }
                });
            }
            // Custom secrets that are not aisbx's: listed, but never touched from here.
            let mut foreign = view.foreign_secret_lines();
            let how = foreign.pop();
            if !foreign.is_empty() {
                page::heading(ui, "Other custom secrets", how.as_deref().unwrap_or(""));
                page::group(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(20, 8))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            for line in foreign {
                                ui.label(RichText::new(line).monospace().color(weak));
                            }
                        });
                });
            }
        },
    );
    if let Some((i, code)) = action {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(code));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    } else if let Some(code) = key {
        app.on_key(KeyEvent::from(code));
    }
}

/// Draw the connection dialog and pass a click on a row or a button to core.
pub fn show_connection(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Connection(conn) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = conn.rows();
    let focus = conn.focus.min(rows.len() - 1);
    let focused = rows[focus];
    let mut list = Rows::new(ui, "connection", focus);
    let index = |row: ConnRow| form::index_of(&rows, row);
    let editing = match &conn.mode {
        ConnMode::Edit(name) => Some(name.as_str()),
        ConnMode::New => None,
    };

    page::bar(ui, "connection", |ui| {
        if let Some(name) = editing {
            let users = view.litellm_users(name);
            let text = match users.len() {
                0 => "No sandbox is on this connection.".to_string(),
                n => format!(
                    "On it: {} — Save names what reaches {} and what needs R.",
                    users.join(", "),
                    if n == 1 { "it" } else { "them" }
                ),
            };
            form::para(ui, &text);
            form::para(
                ui,
                "A new host needs the key again — the stored one targets the old host only.",
            );
            ui.add_space(4.0);
        }
        list.buttons(
            ui,
            &[
                ("Save", index(ConnRow::Save), false),
                ("Cancel", index(ConnRow::Cancel), false),
            ],
        );
    });
    let title = match editing {
        Some(name) => format!("LiteLLM connection — {name}"),
        None => "New LiteLLM connection".to_string(),
    };
    let subtitle = "The key goes to the host keychain as one global sbx secret; every sandbox on \
                    the connection sees only its placeholder, which the proxy swaps for the key.";
    page::body(ui, "connection", &title, subtitle, |ui| {
        let fields = &conn.fields;
        let width = ui.available_width().min(FIELD);
        if let Some(name) = editing {
            ui.add_space(6.0);
            ui.label(theme::strong(RichText::new("Name").size(13.5)));
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).monospace().strong());
                ui.label(RichText::new("a new name is a new connection").weak());
            });
        }
        for row in &rows {
            let (label, input, placeholder, gloss) = match row {
                ConnRow::Name => (
                    "Name",
                    &fields.name,
                    "my-gateway",
                    create::field_gloss(CreateRow::LitellmName).to_string(),
                ),
                ConnRow::Url => {
                    let url = fields.url.value.trim();
                    let gloss = match litellm::check_url(url) {
                        Ok(_) => format!("Host {}: the key goes to it only.", litellm::host(url)),
                        Err(_) => create::field_gloss(CreateRow::LitellmUrl).to_string(),
                    };
                    ("URL", &fields.url, "https://litellm.example.com", gloss)
                }
                ConnRow::Key => match editing {
                    Some(_) => (
                        "Key",
                        &fields.key,
                        "empty keeps the stored key",
                        "A new key reaches every sandbox on it at its next request.".to_string(),
                    ),
                    None => (
                        "Key",
                        &fields.key,
                        "paste the gateway's key",
                        create::field_gloss(CreateRow::LitellmKey).to_string(),
                    ),
                },
                // In the bottom panel.
                ConnRow::Save | ConnRow::Cancel => continue,
            };
            let at = index(*row);
            let response = controls::labelled(ui, label, &wizard::sentence(&gloss), None, |ui| {
                let field = Input {
                    label,
                    input,
                    masked: row.masked(),
                    placeholder,
                    focused: focused == *row,
                    problem: false,
                    monospace: *row == ConnRow::Url,
                    suggested: false,
                };
                controls::input_box(ui, field, width)
            });
            list.take(&response, at);
        }
    });
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_value_says_why_it_is_empty() {
        assert_eq!(empty_text(Empty::NotListed), "not listed by sbx");
        assert_eq!(empty_text(Empty::NotStored), "⚠ not stored");
        assert_eq!(empty_text(Empty::Unknown), "…");
        assert_eq!(empty_text(Empty::SignedIn), "signed in");
        assert_eq!(empty_text(Empty::NotSignedIn), "not signed in");
    }
}

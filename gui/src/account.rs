//! `A` on a sandbox: which Anthropic account it is on (`Screen::AuthEdit`), or on a LiteLLM sandbox
//! which connection (`Screen::ConnSwitch`). Core words every state and effect. A token or key is
//! typed into a field drawn as dots and reaches sbx only on stdin.

use aisbx_core::app::{AccountChoice, App, AuthRow, ConnSwitchRow, CreateRow, Screen};
use aisbx_core::create;
use aisbx_core::input::KeyCode;
use aisbx_core::litellm;
use aisbx_core::model::Auth;
use eframe::egui::{self, RichText};

use crate::controls::{self, Badge, Input, Item, Mark, Tile};
use crate::form::{self, Rows};
use crate::{page, theme, wizard};

/// What a chosen tile still needs, set in under it, as on the wizard.
fn nested(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        ui.vertical(|ui| add(ui));
    });
}

/// The title over both forms — what it changes, for which sandbox — and its sbx name under it.
fn heading(app: &App, what: &str, sandbox: &str) -> (String, String) {
    let label = app
        .sandboxes
        .iter()
        .find(|s| s.name == sandbox)
        .map_or(sandbox, |s| s.label.as_str());
    (format!("{what} — {label}"), format!("sbx name: {sandbox}"))
}

/// Draw the account form and pass a click on a row or a button to core.
pub fn show_auth(ui: &mut egui::Ui, app: &mut App) {
    let Screen::AuthEdit(auth) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = auth.rows();
    let focus = auth.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "auth", focus);
    let index = |row: AuthRow| form::index_of(&rows, row);
    let dark = ui.visuals().dark_mode;

    page::bar(ui, "auth", |ui| {
        let color = match auth.needs_recreate() {
            true => theme::warn(dark),
            false => ui.visuals().text_color(),
        };
        let effect = auth.effect().join(" ");
        ui.add(egui::Label::new(theme::with_code(ui, &effect, |t| t.color(color))).wrap());
        ui.add_space(4.0);
        list.buttons(
            ui,
            &[
                ("Apply", index(AuthRow::Apply), auth.needs_recreate()),
                ("Cancel", index(AuthRow::Cancel), false),
            ],
        );
    });
    let (title, subtitle) = heading(view, "Anthropic account", &auth.sandbox);
    page::body(ui, "auth", &title, &subtitle, |ui| {
        page::heading(ui, "Now", "");
        let users = auth
            .current
            .token_name()
            .map_or(0, |name| view.token_users(name));
        let (text, note) = create::auth_now(&auth.current, users);
        let color = match &auth.current {
            Auth::Login => None,
            Auth::Token { stored: false, .. } => Some(theme::danger(dark)),
            Auth::Token { .. } => Some(theme::running(dark)),
        };
        ui.label(form::with_hint(ui, &text, color, &note));

        // The wizard's controls: a tile per kind of account, under the chosen one what it needs.
        page::heading(ui, "Change to", "");
        let width = ui.available_width();
        let on_token = auth.account.on_token();
        let claude = theme::agent_color("claude", dark);
        let weak = ui.visuals().weak_text_color();
        let notes = create::account_notes(&auth.account).join(" ");
        let tag = wizard::sentence(create::LOGIN_TAG);
        let login = Tile {
            title: "/login (OAuth)",
            blurb: &wizard::sentence(create::LOGIN_HINT),
            detail: "",
            badges: vec![Badge {
                text: &tag,
                color: theme::running(dark),
            }],
            color: claude,
            selected: !on_token,
            focused: list.focused(index(AuthRow::Oauth)),
        };
        let response = controls::tile(ui, login, width, 0.0);
        list.take(&response, index(AuthRow::Oauth));
        if !on_token {
            nested(ui, |ui| page::paragraph(ui, &notes, weak));
        }
        ui.add_space(8.0);
        let token = Tile {
            title: "Setup token",
            blurb: &wizard::sentence(create::token_hint(on_token, auth.tokens.len())),
            detail: "",
            badges: Vec::new(),
            color: claude,
            selected: on_token,
            focused: list.focused(index(AuthRow::UseToken)),
        };
        let response = controls::tile(ui, token, width, 0.0);
        list.take(&response, index(AuthRow::UseToken));
        if !on_token {
            return;
        }
        nested(ui, |ui| {
            let width = ui.available_width().min(560.0);
            let mut radio = |ui: &mut egui::Ui, row: AuthRow, title: &str, detail: &str, on| {
                let item = Item {
                    mark: Mark::Radio,
                    title,
                    detail,
                    badges: Vec::new(),
                    color: theme::accent(dark),
                    checked: on,
                    enabled: true,
                    focused: list.focused(index(row)),
                };
                let response = controls::check_item(ui, item, width);
                list.take(&response, index(row));
            };
            for (i, name) in auth.tokens.iter().enumerate() {
                let users = wizard::sentence(&create::token_users(view.token_users(name)));
                let on = auth.account == AccountChoice::Named(name.clone());
                radio(ui, AuthRow::TokenPick(i), name, &users, on);
            }
            let new = auth.account == AccountChoice::New;
            radio(
                ui,
                AuthRow::TokenNew,
                "New token…",
                "Paste a new token and name it",
                new,
            );
            let fields = [
                (AuthRow::Name, "Name", &auth.token_name, "personal"),
                (
                    AuthRow::Value,
                    "Token",
                    &auth.token,
                    "paste the setup token",
                ),
            ];
            for (row, label, input, placeholder) in fields {
                let Some(at) = index(row) else {
                    continue;
                };
                let response = controls::labelled(ui, label, "", None, |ui| {
                    let field = Input {
                        label,
                        input,
                        masked: row.masked(),
                        placeholder,
                        focused: list.is_focused(at),
                        problem: false,
                        monospace: false,
                        suggested: false,
                    };
                    controls::input_box(ui, field, width)
                });
                list.take(&response, Some(at));
            }
            ui.add_space(6.0);
            page::paragraph(ui, &notes, weak);
        });
    });
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

/// Draw the connection move and pass a click on a row or a button to core.
pub fn show_switch(ui: &mut egui::Ui, app: &mut App) {
    let Screen::ConnSwitch(switch) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = switch.rows();
    let focus = switch.focus.min(rows.len() - 1);
    let focused = rows[focus];
    let mut list = Rows::new(ui, "conn-switch", focus);
    let index = |row: ConnSwitchRow| form::index_of(&rows, row);
    let dark = ui.visuals().dark_mode;
    let moving = switch.moving();

    let mut step = None;
    page::bar(ui, "conn-switch", |ui| {
        let (note, color) = match moving {
            true => (
                "Apply rebuilds the sandbox on that connection: its kit is baked into the container.",
                theme::warn(dark),
            ),
            false => ("No changes.", ui.visuals().text_color()),
        };
        ui.add(egui::Label::new(RichText::new(note).color(color)).wrap());
        ui.add_space(4.0);
        list.buttons(
            ui,
            &[
                ("Apply", index(ConnSwitchRow::Apply), moving),
                ("Cancel", index(ConnSwitchRow::Cancel), false),
            ],
        );
    });
    let (title, subtitle) = heading(view, "LiteLLM connection", &switch.sandbox);
    page::body(ui, "conn-switch", &title, &subtitle, |ui| {
        page::heading(ui, "Now", "");
        let now = match switch.current.as_deref() {
            Some(cur) => match switch.connections.iter().find(|c| c.name == cur) {
                Some(c) => format!("LiteLLM {cur} · {}", litellm::host(&c.url)),
                None => format!("LiteLLM {cur} — no longer exists"),
            },
            None => "no LiteLLM connection".to_string(),
        };
        ui.label(theme::strong(RichText::new(now)));

        // The connection as a stepper: its arrows are ← and → on the row, as core takes them.
        page::heading(ui, "Move to", "");
        let mut notes = Vec::new();
        let main = match switch.picked() {
            Some(conn) => {
                if switch.current.as_deref() == Some(conn.name.as_str()) {
                    notes.push("current");
                }
                if view.litellm_key_missing(&conn.name) {
                    notes.push("key missing");
                }
                format!("{} · {}", conn.name, litellm::host(&conn.url))
            }
            None => "New connection…".to_string(),
        };
        let conn = index(ConnSwitchRow::Conn);
        let field = ui.visuals().extreme_bg_color;
        let picker = ui.horizontal(|ui| {
            let arrow = |text: &str| {
                egui::Button::new(RichText::new(text).size(18.0)).min_size(egui::vec2(34.0, 30.0))
            };
            let back = ui.add(arrow("‹")).on_hover_text("The connection before it");
            egui::Frame::new()
                .fill(field)
                .stroke(egui::Stroke::new(1.0, theme::line(dark)))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.set_min_width(280.0);
                    ui.label(theme::strong(RichText::new(&main)));
                });
            let next = ui.add(arrow("›")).on_hover_text("The connection after it");
            if !notes.is_empty() {
                ui.label(RichText::new(wizard::sentence(&notes.join(" · "))).weak());
            }
            if back.clicked() {
                step = Some(KeyCode::Left);
            }
            if next.clicked() {
                step = Some(KeyCode::Right);
            }
        });
        if list.focused(conn) {
            controls::focus_ring(ui, picker.response.rect);
        }
        if switch.picked().is_none() {
            let typed = &switch.new_conn;
            let width = ui.available_width().min(460.0);
            let fields = [
                (ConnSwitchRow::Name, "Name", &typed.name, "my-gateway"),
                (
                    ConnSwitchRow::Url,
                    "URL",
                    &typed.url,
                    "https://litellm.example.com",
                ),
                (
                    ConnSwitchRow::Key,
                    "Key",
                    &typed.key,
                    "paste the gateway's key",
                ),
            ];
            for (row, label, input, placeholder) in fields {
                let gloss = match row {
                    ConnSwitchRow::Name => create::field_gloss(CreateRow::LitellmName),
                    ConnSwitchRow::Url => create::field_gloss(CreateRow::LitellmUrl),
                    _ => create::field_gloss(CreateRow::LitellmKey),
                };
                let response =
                    controls::labelled(ui, label, &wizard::sentence(gloss), None, |ui| {
                        let field = Input {
                            label,
                            input,
                            masked: row.masked(),
                            placeholder,
                            focused: focused == row,
                            problem: false,
                            monospace: row == ConnSwitchRow::Url,
                            suggested: false,
                        };
                        controls::input_box(ui, field, width)
                    });
                list.take(&response, index(row));
            }
        }
    });
    if let Some(code) = step {
        if let Some(at) = form::index_of(&switch.rows(), ConnSwitchRow::Conn) {
            app.click(aisbx_core::app::Hit::FormFocus(at));
            app.on_key(aisbx_core::input::KeyEvent::from(code));
            return;
        }
    }
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

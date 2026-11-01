//! The Connect screen (`o`): the sandbox's SSH endpoint, *Set up SSH now* until `sbx setup ssh`
//! has run, and one item per app that can connect, all in core's words (`App::connect_view`). An
//! app's *Open* is Enter on it; a click on the app only selects it.

use aisbx_core::app::{App, ConnectRow, Hit, Screen, TargetLaunch};
use aisbx_core::input::{KeyCode, KeyEvent};
use eframe::egui::{self, RichText};

use crate::controls::{self, Tone};
use crate::form::{self, Look, Rows};
use crate::page::{self, Item, State};
use crate::theme;

/// The button of an app the window starts itself.
pub const OPEN: &str = "Open";

/// Draw the screen and pass a click on an app, its *Open*, *Set up SSH* or *Close* to core.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Connect(connect) = &app.screen else {
        return;
    };
    let Some(view) = app.connect_view() else {
        return;
    };
    let rows = connect.rows();
    let focus = connect.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "connect", focus);
    let dark = ui.visuals().dark_mode;
    let close = form::index_of(&rows, ConnectRow::Cancel);
    let mut picked = None;
    let mut open = None;

    page::bar(ui, "connect", |ui| {
        list.buttons(ui, &[("Close", close, false)]);
    });
    let title = format!("Connect to {}", view.label);
    let subtitle = format!("{} {}", view.workspace[0], view.workspace[1]);
    page::body(ui, "connect", &title, &subtitle, |ui| {
        page::heading(ui, "SSH endpoint", "");
        page::group(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(20, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let host = RichText::new(&view.host).monospace().strong();
                        ui.label(host);
                        ui.label(RichText::new(view.state).weak());
                    });
                });
        });
        if view.ssh_missing {
            ui.add_space(8.0);
            controls::banner(ui, Tone::Warn, &format!("⚠ {}", view.ssh_warning.join(" ")));
            ui.add_space(4.0);
            if let Some(i) = form::index_of(&rows, ConnectRow::SetupSsh) {
                page::group(ui, |ui| {
                    let item = Item {
                        title: "Set up SSH now",
                        mark: None,
                        meta: "",
                        state: None,
                        actions: &[("Set up SSH", Look::Primary)],
                        reserve: 0.0,
                        selected: list.is_focused(i),
                        rule: false,
                    };
                    let got = page::item(ui, item, list.follows(i), |ui| {
                        page::notes(ui, &view.ssh_notes.join(" "));
                    });
                    if got.item {
                        picked = Some(i);
                    }
                    if got.action.is_some() {
                        open = Some(i);
                    }
                });
            }
        }

        page::heading(ui, "Connect with", "");
        // Every app keeps an *Open*'s room, so a warning ends where the others' buttons start.
        let reserve = page::actions_width(ui, &[(OPEN, Look::Plain)]);
        page::group(ui, |ui| {
            for (n, target) in view.targets.iter().enumerate() {
                let Some(i) = form::index_of(&rows, target.row) else {
                    continue;
                };
                let (meta, state, look) = match target.launch {
                    TargetLaunch::Here => ("", None, Some(Look::Plain)),
                    TargetLaunch::NotOnPath(_) => {
                        let why = State {
                            text: format!("⚠ {}", target.status),
                            color: theme::warn(dark),
                            value: false,
                        };
                        ("", Some(why), Some(Look::Inert))
                    }
                    TargetLaunch::InApp => (target.status.as_str(), None, None),
                };
                let actions: Vec<(&str, Look)> = look.map(|l| (OPEN, l)).into_iter().collect();
                let item = Item {
                    title: target.title,
                    mark: None,
                    meta,
                    state,
                    actions: &actions,
                    reserve,
                    selected: list.is_focused(i),
                    rule: n > 0,
                };
                let got = page::item(ui, item, list.follows(i), |ui| {
                    for step in &target.steps {
                        let color = ui.visuals().text_color();
                        ui.label(theme::with_code(ui, step, |t| t.color(color)));
                    }
                    if !target.kit_warning.is_empty() {
                        let warning = format!("⚠ {}", target.kit_warning.join(" "));
                        controls::banner(ui, Tone::Warn, &warning);
                    }
                });
                if got.item {
                    picked = Some(i);
                }
                if got.action.is_some() {
                    open = Some(i);
                }
            }
        });
    });
    if let Some(i) = open {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(KeyCode::Enter));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

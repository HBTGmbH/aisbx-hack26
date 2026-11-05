//! The settings screen (`T`): the sbx settings for this host that aisbx offers, each with its
//! state, what it means and one button that turns it the other way, in core's words.

use aisbx_core::app::{
    App, Hit, Screen, SettingTone, SettingsRow, SETTINGS, SETTINGS_NOTE, SKILLS_DEFAULT_LABEL,
    SKILLS_DEFAULT_NOTE,
};
use aisbx_core::create::{skills_mode_name, SKILLS_MODES};
use aisbx_core::input::{KeyCode, KeyEvent};
use eframe::egui;

use crate::form::{self, Look, Rows};
use crate::page::{self, Item, State};
use crate::theme;

/// Draw the screen and pass a click on a setting, its button or *Close* to core.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Settings(settings) = &app.screen else {
        return;
    };
    let rows = settings.rows();
    let focus = settings.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "settings", focus);
    let dark = ui.visuals().dark_mode;
    let weak = ui.visuals().weak_text_color();
    let close = form::index_of(&rows, SettingsRow::Close);
    // One button per setting, named for the value it would set.
    let buttons: Vec<String> = (0..SETTINGS.len())
        .map(|n| match settings.value(n) {
            Some(value) => format!("Turn {}", SETTINGS[n].word(!value)),
            None => "Change".into(),
        })
        .collect();
    // The widest button, so every setting's state ends in one column.
    let reserve = (buttons.iter())
        .map(|b| page::actions_width(ui, &[(b.as_str(), Look::Plain)]))
        .fold(0.0, f32::max);
    let mut picked = None;
    let mut change = None;
    let mut choose = None;
    let skills = form::index_of(&rows, SettingsRow::SkillsDefault);
    // The skills row's three modes, the current one inert.
    let current = settings.skills_mode();
    let mode_labels: Vec<String> = (SKILLS_MODES.iter())
        .map(|m| capitalised(skills_mode_name(*m)))
        .collect();
    let modes: Vec<(&str, Look)> = (SKILLS_MODES.iter().zip(&mode_labels))
        .map(|(m, label)| match Some(*m) == current {
            true => (label.as_str(), Look::Inert),
            false => (label.as_str(), Look::Plain),
        })
        .collect();
    // Every item keeps the widest button row's room, so the states end in one column.
    let reserve = reserve.max(page::actions_width(ui, &modes));

    page::bar(ui, "settings", |ui| {
        list.buttons(ui, &[("Close", close, false)]);
    });
    page::body(
        ui,
        "settings",
        "Settings",
        "sbx's own settings for this host",
        |ui| {
            page::heading(ui, "sbx on this computer", "");
            page::group(ui, |ui| {
                for (n, spec) in SETTINGS.iter().enumerate() {
                    let Some(i) = form::index_of(&rows, SettingsRow::Setting(n)) else {
                        continue;
                    };
                    let (state, meaning) = settings.view(n);
                    let colour = match spec.tone(settings.value(n)) {
                        SettingTone::Calm => theme::running(dark),
                        SettingTone::Warn => theme::warn(dark),
                        SettingTone::Plain => ui.visuals().text_color(),
                        SettingTone::Unknown => weak,
                    };
                    let actions = [(buttons[n].as_str(), Look::Plain)];
                    let title = capitalised(spec.label);
                    let item = Item {
                        title: &title,
                        mark: None,
                        meta: &meaning,
                        state: Some(State {
                            text: state.to_string(),
                            color: colour,
                            value: false,
                        }),
                        actions: &actions,
                        reserve,
                        selected: list.is_focused(i),
                        rule: n > 0,
                    };
                    let follow = list.follows(i);
                    let got = page::item(ui, item, follow, |ui| page::notes(ui, spec.note));
                    if got.item {
                        picked = Some(i);
                    }
                    if got.action.is_some() {
                        change = Some(i);
                    }
                }
                if let Some(i) = skills {
                    let (state, meaning, tone) = settings.skills_view();
                    let title = capitalised(SKILLS_DEFAULT_LABEL);
                    let item = Item {
                        title: &title,
                        mark: None,
                        meta: &meaning,
                        state: Some(State {
                            text: state.to_string(),
                            color: match tone {
                                SettingTone::Calm => theme::running(dark),
                                SettingTone::Warn => theme::warn(dark),
                                SettingTone::Plain => ui.visuals().text_color(),
                                SettingTone::Unknown => weak,
                            },
                            value: false,
                        }),
                        actions: &modes,
                        reserve,
                        selected: list.is_focused(i),
                        rule: true,
                    };
                    let follow = list.follows(i);
                    let got =
                        page::item(ui, item, follow, |ui| page::notes(ui, SKILLS_DEFAULT_NOTE));
                    if got.item {
                        picked = Some(i);
                    }
                    if let Some(value) = got.action {
                        choose = Some((i, value));
                    }
                }
            });
            page::notes(ui, SETTINGS_NOTE);
        },
    );
    if let Some((i, value)) = choose {
        app.click(Hit::FormChoice(i, value));
    } else if let Some(i) = change {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(KeyCode::Enter));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

/// `clipboard copy` as an item's title: *Clipboard copy*.
fn capitalised(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

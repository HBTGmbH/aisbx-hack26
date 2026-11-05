//! The start-folder picker: which folder inside the workspace the agent opens in, for an existing
//! sandbox or for the create form that opened it. The listing is core's (`StartDirForm::dirs`,
//! read when core moves to a level); this module never reads a directory itself.

use std::path::Path;

use aisbx_core::app::{App, Screen, StartDirForm, StartDirRow, StartDirTarget};
use aisbx_core::{model, startdir};
use eframe::egui::{self, RichText};

use crate::form::{self, Rows};
use crate::page;

/// Draw the picker and pass a click on a folder or a button to core. Leaving it for the create
/// form hands the form back with every answer kept (core's `leave_start_dir`).
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::StartDir(picker) = &app.screen else {
        return;
    };
    let rows = picker.rows();
    let focus = picker.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "startdir", focus);
    let index = |row: StartDirRow| form::index_of(&rows, row);
    let chosen = picker.chosen();
    let host = matches!(
        picker.target,
        StartDirTarget::HostFolder(_) | StartDirTarget::Folder(_)
    );
    let target = match &picker.target {
        StartDirTarget::Sandbox(name) => format!("For sandbox {name}."),
        StartDirTarget::New(_) => "For the new sandbox.".to_string(),
        StartDirTarget::HostFolder(_) => "For the host folders list.".to_string(),
        StartDirTarget::Folder(_) => "The new sandbox mounts it read-only.".to_string(),
    };
    let use_label = match &picker.target {
        StartDirTarget::HostFolder(_) => "Use this folder",
        StartDirTarget::Folder(_) => "Mount this folder",
        StartDirTarget::Sandbox(_) | StartDirTarget::New(_) => "Use, don't start",
    };

    page::bar(ui, "startdir", |ui| {
        let buttons: Vec<(&str, Option<usize>, bool)> = rows
            .iter()
            .filter_map(|row| match row {
                StartDirRow::Start => Some(("Start here", index(*row), false)),
                StartDirRow::Use => Some((use_label, index(*row), false)),
                StartDirRow::Cancel => Some(("Cancel", index(*row), false)),
                StartDirRow::Up | StartDirRow::Dir(_) => None,
            })
            .collect();
        debug_assert_eq!(buttons.len(), picker.buttons());
        list.buttons(ui, &buttons);
    });
    let title = match &picker.target {
        StartDirTarget::HostFolder(_) => "Pick a host folder".to_string(),
        StartDirTarget::Folder(_) => "Pick a folder to mount".to_string(),
        StartDirTarget::Sandbox(_) | StartDirTarget::New(_) => {
            format!("Start folder — {}", picker.label)
        }
    };
    page::body(ui, "startdir", &title, &target, |ui| {
        if host {
            form::section(ui, "Folder", "");
            ui.label(
                RichText::new(model::tilde(&picker.here()))
                    .monospace()
                    .strong(),
            );
            if matches!(picker.target, StartDirTarget::HostFolder(_)) {
                form::para(
                    ui,
                    "Hidden folders are not listed: type their path in the field instead.",
                );
            }
        } else {
            mounted_and_opens(ui, picker, chosen.as_deref());
        }
        form::section(
            ui,
            "Browse",
            "a click goes into a folder, .. comes back out",
        );
        if picker.dirs.is_empty() {
            form::para(ui, "(no subfolders below this one)");
        }
        for row in &rows {
            let text = match row {
                StartDirRow::Up if picker.widens() => {
                    let parent = picker.root.parent().map(model::tilde).unwrap_or_default();
                    format!("←  ..   mount {parent} instead")
                }
                StartDirRow::Up => "←  ..".to_string(),
                StartDirRow::Dir(i) => format!("→  {}", picker.dirs[*i]),
                // In the bottom panel.
                StartDirRow::Start | StartDirRow::Use | StartDirRow::Cancel => continue,
            };
            list.row(ui, index(*row), RichText::new(text).monospace());
        }
    });
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

/// What a sandbox's picker shows above the folders: the project folder and where the agent starts.
fn mounted_and_opens(ui: &mut egui::Ui, picker: &StartDirForm, chosen: Option<&Path>) {
    form::section(ui, "Project folder", "");
    ui.label(RichText::new(model::tilde(&picker.root)).monospace());
    form::section(ui, "Agent starts in", "");
    let opens = startdir::describe(&picker.root, chosen);
    ui.label(RichText::new(opens).monospace().strong());
}

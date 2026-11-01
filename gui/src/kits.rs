//! The kits editor: which addon kits one existing sandbox is built from and, on a gateway backend,
//! its model pins — with what *Apply* will do, in core's words (`KitsForm::effect`).

use aisbx_core::app::{App, KitsEffect, KitsRow, RebuildCause, Screen};
use aisbx_core::model;
use eframe::egui;

use aisbx_core::create;

use crate::controls::{self, Badge, Item, Mark};
use crate::form::{self, Rows};
use crate::{page, theme, wizard};

/// What the *Host caches* group says under its heading: the editor ticks them as any kit, while
/// the wizard also greys out a cache this host does not have.
const HOST_CACHES_GLOSS: &str = "Read-only, from this host.";

/// Draw the editor and pass a click on a row or a button to core.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Kits(kits) = &app.screen else {
        return;
    };
    let view: &App = app;
    let catalog = &view.catalog;
    let rows = kits.rows(catalog);
    let focus = kits.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "kits", focus);
    let sandbox = view.sandboxes.iter().find(|s| s.name == kits.sandbox);
    let dark = ui.visuals().dark_mode;
    let index = |row: KitsRow| form::index_of(&rows, row);
    let other_checkout = sandbox.is_some_and(|sb| sb.other_checkout.is_some());
    let effect = kits.effect(catalog, other_checkout);

    page::bar(ui, "kits", |ui| {
        // The note sits right above the buttons it is about.
        let color = match effect {
            KitsEffect::Rebuild(
                RebuildCause::Models | RebuildCause::Kits | RebuildCause::OtherCheckout,
            )
            | KitsEffect::KitAddRefused(_) => theme::warn(dark),
            KitsEffect::KitAdd | KitsEffect::NoChanges => ui.visuals().text_color(),
        };
        let note = effect.note(kits.history()).join(" ");
        ui.add(egui::Label::new(theme::with_code(ui, &note, |t| t.color(color))).wrap());
        ui.add_space(4.0);
        list.buttons(
            ui,
            &[
                ("Apply", index(KitsRow::Apply), kits.removing()),
                ("Cancel", index(KitsRow::Cancel), false),
            ],
        );
    });
    let label = sandbox.map(|s| s.label.as_str()).unwrap_or(&kits.sandbox);
    let backend = sandbox.map(|sb| {
        sb.backend
            .as_deref()
            .and_then(|b| catalog.backend(b))
            .map(|b| b.kit.display_name.clone())
            .unwrap_or_else(|| sb.agent.clone())
    });
    let subtitle = backend
        .map(|b| format!("Built on {b}, which stays — a new sandbox is how to change it."))
        .unwrap_or_default();
    page::body(ui, "kits", &format!("Kits — {label}"), &subtitle, |ui| {
        // The wizard's items, badges and groups; what Apply would change is a badge too.
        let add_later = wizard::sentence(create::ADD_LATER);
        let addons = rows.iter().filter_map(|row| match row {
            KitsRow::Addon(i) => Some(*i),
            _ => None,
        });
        let host = |i: &usize| model::is_host_folder_kit(&catalog.addons[*i].kit.name);
        let (hosts, others): (Vec<usize>, Vec<usize>) = addons.partition(host);
        let others_gloss = format!("{}.", wizard::sentence(create::ADD_LATER_GLOSS));
        for (title, gloss, group) in [
            (wizard::HOST_CACHES, HOST_CACHES_GLOSS, hosts),
            (wizard::ADDITIONAL_KITS, others_gloss.as_str(), others),
        ] {
            if group.is_empty() {
                continue;
            }
            controls::section(ui, title, gloss);
            // Two to a line where the window is wide enough, as on the wizard's Kits step.
            const GAP: f32 = 12.0;
            let avail = ui.available_width();
            let columns = if avail >= 700.0 { 2 } else { 1 };
            let width = (avail - GAP * (columns - 1) as f32) / columns as f32;
            for chunk in group.chunks(columns) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for &i in chunk {
                        let row = KitsRow::Addon(i);
                        let addon = &catalog.addons[i];
                        let (was, now) = (kits.original[i], kits.addons[i]);
                        let mut badges = Vec::new();
                        match (was, now) {
                            (false, true) => badges.push(Badge {
                                text: "Adding",
                                color: theme::accent(dark),
                            }),
                            (true, false) => badges.push(Badge {
                                text: "Removing",
                                color: theme::danger(dark),
                            }),
                            _ => {}
                        }
                        if addon.create_time_reason().is_none() {
                            badges.push(Badge {
                                text: &add_later,
                                color: theme::running(dark),
                            });
                        }
                        let item = Item {
                            mark: Mark::Check,
                            title: &addon.kit.display_name,
                            detail: addon.kit.first_sentence(),
                            badges,
                            color: theme::kit_color(&addon.kit.name, dark),
                            checked: now,
                            enabled: true,
                            focused: list.focused(index(row)),
                        };
                        // Core's help for the kit is the tooltip, as on the wizard.
                        let help = wizard::tooltip(&kits.help(catalog, row));
                        let response = controls::check_item(ui, item, width).on_hover_text(help);
                        list.take(&response, index(row));
                    }
                });
                ui.add_space(4.0);
            }
        }
        // While `headroom` is ticked, chosen by the agent sbx reports.
        let headroom = catalog.addons.iter().position(|a| a.kit.name == "headroom");
        if let (Some(sb), Some(i)) = (sandbox, headroom) {
            if kits.addons[i] {
                form::warn(ui, aisbx_core::kits::headroom_warning(&sb.agent));
            }
        }
        if kits.models_offered {
            form::model_section(
                ui,
                &mut list,
                view,
                &kits.models,
                kits.model_pick,
                &index,
                KitsRow::Model,
                KitsRow::ModelDefault,
                KitsRow::ModelPick,
            );
        }
    });
    if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

//! The approvals screen (`y`): the requests that sandboxes hold because no rule allows them, one
//! group per sandbox, each with *Allow* and *Dismiss*, in core's words. A click on a request only
//! moves core's cursor: allowing one takes its *Allow* button, `a` or Enter.

use aisbx_core::app::{App, ApprovalRow, Hit, Screen, APPROVALS_EMPTY, APPROVALS_NOTE};
use aisbx_core::input::{KeyCode, KeyEvent};
use eframe::egui;

use crate::form::{self, Look, Rows};
use crate::page::{self, Item};
use crate::theme;

/// A request's answers, as its buttons: the keys `a` and `d`.
pub const ANSWERS: [(&str, KeyCode, Look); 2] = [
    ("Allow", KeyCode::Char('a'), Look::Primary),
    ("Dismiss", KeyCode::Char('d'), Look::Plain),
];

/// Draw the screen and pass a click on a request, one of its buttons or *Close* to core.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Approvals(approvals) = &app.screen else {
        return;
    };
    let view: &App = app;
    let rows = view.approval_rows();
    let focus = approvals.focus.min(rows.len() - 1);
    let mut list = Rows::new(ui, "approvals", focus);
    let dark = ui.visuals().dark_mode;
    let close = form::index_of(&rows, ApprovalRow::Close);
    let answers: Vec<(&str, Look)> = ANSWERS.iter().map(|(l, _, look)| (*l, *look)).collect();
    let mut groups: Vec<(&str, Vec<(usize, &str)>)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let ApprovalRow::Request(id) = row else {
            continue;
        };
        let Some(request) = view.approval(id) else {
            continue;
        };
        match groups.last_mut() {
            Some((sandbox, items)) if *sandbox == request.sandbox => items.push((i, id)),
            _ => groups.push((request.sandbox.as_str(), vec![(i, id.as_str())])),
        }
    }
    let mut picked = None;
    let mut answer = None;

    page::bar(ui, "approvals", |ui| {
        list.buttons(ui, &[("Close", close, false)]);
    });
    page::body(
        ui,
        "approvals",
        "Network requests waiting for approval",
        APPROVALS_NOTE,
        |ui| {
            if groups.is_empty() {
                page::heading(ui, "Nothing is waiting", "");
                page::paragraph(ui, APPROVALS_EMPTY, ui.visuals().weak_text_color());
            }
            for (sandbox, items) in &groups {
                let label = view.sandbox_label(sandbox);
                let gloss = if label == *sandbox { "" } else { sandbox };
                page::heading(ui, &label, gloss);
                page::group(ui, |ui| {
                    for (n, (i, id)) in items.iter().enumerate() {
                        let Some(request) = view.approval(id) else {
                            continue;
                        };
                        let reason = aisbx_core::app::approval_reason(request);
                        let item = Item {
                            title: &request.title,
                            mark: Some(("⚑", theme::warn(dark))),
                            meta: &reason,
                            state: None,
                            actions: &answers,
                            reserve: 0.0,
                            selected: list.is_focused(*i),
                            rule: n > 0,
                        };
                        let follow = list.follows(*i);
                        let got = page::item(ui, item, follow, |ui| {
                            page::notes(ui, &view.approval_notes(id).join(" "));
                        });
                        if got.item {
                            picked = Some(*i);
                        }
                        if let Some(a) = got.action {
                            answer = Some((*i, ANSWERS[a].1));
                        }
                    }
                });
            }
        },
    );
    if let Some((i, code)) = answer {
        app.click(Hit::FormFocus(i));
        app.on_key(KeyEvent::from(code));
    } else if let Some(i) = picked {
        app.click(Hit::FormFocus(i));
    } else if let Some(hit) = list.clicked {
        app.click(hit);
    }
}

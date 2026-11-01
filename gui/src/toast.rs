//! Messages as toasts, and the approvals count's tooltip. The message is core's `app.status`, which
//! `App::tick` clears, so the window keeps no timer. An error's close button is
//! `App::dismiss_status`.

use aisbx_core::app::App;
use eframe::egui::{self, Align2, Color32};

use crate::theme;

/// The most waiting requests the approvals tooltip names before it says how many more there are.
const TIP_MAX: usize = 8;

/// What the toast shows: the text, its colour (`None` for the plain text colour), and whether it
/// has a × — errors only.
pub fn style(app: &App, dark: bool) -> Option<(&str, Option<Color32>, bool)> {
    let (text, error, _) = app.status.as_ref()?;
    Some(match error {
        true => (text.as_str(), Some(theme::danger(dark)), true),
        false => (text.as_str(), None, false),
    })
}

/// Draw the toast in the window's bottom-right corner, if there is a message.
pub fn toast(ctx: &egui::Context, app: &mut App) {
    let dark = ctx.global_style().visuals.dark_mode;
    let Some((text, color, closable)) = style(app, dark) else {
        return;
    };
    let mut dismiss = false;
    egui::Area::new(egui::Id::unique("toast"))
        .anchor(Align2::RIGHT_BOTTOM, [-16.0, -16.0])
        // Above the chooser, which can open together with an error.
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(440.0);
                ui.horizontal(|ui| {
                    ui.label(theme::with_code(ui, text, |t| match color {
                        Some(color) => t.color(color),
                        None => t,
                    }));
                    if closable {
                        let close = ui.small_button("×").on_hover_text("Dismiss");
                        close.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, "Dismiss")
                        });
                        dismiss = close.clicked();
                    }
                });
            });
        });
    if dismiss {
        app.dismiss_status();
    }
}

/// The approvals count's tooltip: each waiting request's sandbox and destination, at most
/// [`TIP_MAX`] of them, and where to answer them.
pub fn approvals_tip(app: &App) -> String {
    let n = app.approvals.len();
    let mut lines = vec![format!(
        "{n} request{} waiting for approval:",
        if n == 1 { "" } else { "s" }
    )];
    for approval in app.approvals.iter().take(TIP_MAX) {
        lines.push(format!("• {} — {}", approval.sandbox, approval.title));
    }
    if n > TIP_MAX {
        lines.push(format!("and {} more", n - TIP_MAX));
    }
    lines.push("Click to answer them, or press y.".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::kits::Catalog;
    use aisbx_core::sbx::Approval;

    fn app() -> App {
        App::for_test(Catalog::load().unwrap(), Vec::new())
    }

    #[test]
    fn an_error_toast_is_red_and_closable_and_an_info_one_is_plain() {
        let mut app = app();
        assert_eq!(style(&app, true), None);
        app.set_info("saved");
        assert_eq!(style(&app, true), Some(("saved", None, false)));
        app.set_error("boom");
        for dark in [true, false] {
            let red = Some(theme::danger(dark));
            assert_eq!(style(&app, dark), Some(("boom", red, true)));
        }
    }

    fn approval(sandbox: &str, title: &str) -> Approval {
        Approval {
            id: "network:1".into(),
            sandbox: sandbox.into(),
            title: title.into(),
            detail: String::new(),
            options: Vec::new(),
        }
    }

    #[test]
    fn the_approvals_tip_names_eight_requests_then_counts_the_rest() {
        let mut app = app();
        app.approvals = vec![approval("api", "example.org:443")];
        assert_eq!(
            approvals_tip(&app),
            "1 request waiting for approval:\n• api — example.org:443\nClick to answer them, or press y."
        );
        app.approvals = (0..11)
            .map(|i| approval(&format!("sb-{i}"), "example.org:443"))
            .collect();
        let tip = approvals_tip(&app);
        let lines: Vec<&str> = tip.lines().collect();
        assert_eq!(lines[0], "11 requests waiting for approval:");
        assert_eq!(lines.iter().filter(|l| l.starts_with("• ")).count(), 8);
        assert_eq!(lines[8], "• sb-7 — example.org:443");
        assert_eq!(lines[9], "and 3 more");
        assert_eq!(lines[10], "Click to answer them, or press y.");
    }
}

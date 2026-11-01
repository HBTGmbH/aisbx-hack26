//! Sessions: the window's end of core's `Handoff`. The window starts core's `terminal::Launch`,
//! shows its pre-start steps in a strip, then opens the session through `App::open_session`. With
//! no terminal, a dialog shows the command to run by hand.

use aisbx_core::app::{App, Handoff};
use aisbx_core::terminal::{self, Launch, LaunchError, Os};
use eframe::egui::{self, Key};

#[derive(Default)]
pub struct Session {
    /// The session being prepared — one at a time.
    launch: Option<Launch>,
    /// The command of a session no terminal was found for, while its dialog is open.
    no_terminal: Option<String>,
}

impl Session {
    /// A session is being prepared: Start and Shell wait for it.
    pub fn busy(&self) -> bool {
        self.launch.is_some()
    }

    /// The *No terminal found* dialog is open.
    pub fn dialog_open(&self) -> bool {
        self.no_terminal.is_some()
    }

    #[cfg(test)]
    pub fn launch(&self) -> Option<&Launch> {
        self.launch.as_ref()
    }

    #[cfg(test)]
    pub fn no_terminal(&self) -> Option<&str> {
        self.no_terminal.as_deref()
    }
}

/// Take the app's handoff, if there is one and no session is being prepared, and start
/// preparing it.
pub fn take(app: &mut App, session: &mut Session) {
    if session.launch.is_some() {
        return;
    }
    if let Some(handoff) = app.handoff.take() {
        session.launch = Some(Launch::start(handoff));
    }
}

/// Open the prepared session once its pre-start steps are done.
pub fn finish(app: &mut App, session: &mut Session) {
    if !session.launch.as_ref().is_some_and(Launch::is_finished) {
        return;
    }
    let Some(launch) = session.launch.take() else {
        return;
    };
    let command = terminal::display_command(Os::current(), &launch.handoff().argv());
    if let Err(LaunchError::NoTerminal) = app.open_session(launch) {
        session.no_terminal = Some(command);
    }
}

/// The strip's line: *Preparing <label> — step i/n <label>*, worded for what is being prepared.
pub fn line(app: &App, launch: &Launch) -> String {
    let doing = match launch.handoff() {
        Handoff::Agent { .. } => "Preparing",
        Handoff::Shell { .. } => "Opening a shell in",
        Handoff::Connect { terminal: true, .. } => "Connecting to",
        Handoff::Connect {
            terminal: false, ..
        } => "Starting an editor on",
        Handoff::Edit { .. } => "Opening",
    };
    let label = app.sandbox_label(launch.handoff().name());
    match launch.progress() {
        Some(p) => format!("{doing} {label} — step {}/{} {}", p.index, p.total, p.label),
        None => format!("{doing} {label}…"),
    }
}

/// The preparing strip at the bottom of the window, while a session is being prepared.
pub fn strip(ui: &mut egui::Ui, app: &App, session: &Session) {
    let Some(launch) = &session.launch else {
        return;
    };
    let text = line(app, launch);
    egui::Panel::bottom("preparing").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label(text);
        });
    });
}

/// The *No terminal found* dialog: the session command to copy, and how to name a terminal.
pub fn dialog(ctx: &egui::Context, session: &mut Session) {
    let Some(command) = &session.no_terminal else {
        return;
    };
    let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
    egui::Modal::new(egui::Id::unique("aisbx-no-terminal")).show(ctx, |ui| {
        ui.set_max_width(560.0);
        ui.heading("No terminal found");
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                "aisbx-ui found no terminal window to open the session in. Set AISBX_TERMINAL \
                 to the command that opens one and runs the words after it (for example \
                 `xterm -e`), then start aisbx-ui again — or run this in a terminal:",
            )
            .wrap(),
        );
        ui.add_space(6.0);
        let mut shown = command.as_str();
        ui.add(
            egui::TextEdit::multiline(&mut shown)
                .code_editor()
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(command.clone());
            }
            close |= ui.button("Close").clicked();
        });
    });
    if close {
        session.no_terminal = None;
    }
}

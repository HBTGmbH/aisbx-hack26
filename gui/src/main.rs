//! aisbx-ui — a desktop GUI (egui/eframe) on the same `aisbx-core` as the `aisbx` TUI.

// No console window of its own on Windows; `-h` / `-V` then print nowhere visible from cmd.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod actions;
#[cfg(test)]
mod actions_golden;
mod approvals;
mod args;
mod boundary;
mod chooser;
mod connect;
mod controls;
mod details;
#[cfg(test)]
mod details_golden;
mod folders;
mod form;
#[cfg(test)]
mod form_golden;
mod help;
mod kits;
mod modal;
mod overview;
mod page;
mod resources;
mod roster;
#[cfg(test)]
mod screens_golden;
mod secrets;
mod session;
mod shell;
mod skills;
mod startdir;
mod table;
mod theme;
mod toast;
mod wizard;

use args::Parsed;

fn main() -> anyhow::Result<()> {
    let mut args = match args::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Help) => {
            print!("{}", args::HELP);
            return Ok(());
        }
        Ok(Parsed::Version) => {
            println!("{}", aisbx_core::version());
            return Ok(());
        }
        // As `aisbx` does: before anything the window sets up.
        Ok(Parsed::Headless(args)) => aisbx_core::headless::exit(&args),
        Err(e) => anyhow::bail!("{e}\n\n{}", args::HELP),
    };
    // A relative FOLDER is the shell's: taken against the process's working directory, which
    // the window itself never changes.
    args.folder = args
        .folder
        .map(|dir| std::path::absolute(&dir).unwrap_or(dir));
    // Before any thread: the local UTC offset is only readable single-threaded.
    aisbx_core::state::init_clock();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("aisbx-ui")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([960.0, 600.0])
            .with_maximized(true),
        ..Default::default()
    };
    eframe::run_native(
        "aisbx-ui",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(eframe::egui::ThemePreference::System);
            theme::install(&cc.egui_ctx);
            Ok(Box::new(shell::GuiApp::new(args)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("aisbx-ui: {e}"))
}

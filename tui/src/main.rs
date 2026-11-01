//! aisbx — a Ratatui dashboard for this repo's Docker Sandboxes kits.
//! It lists the sandboxes for the current folder, and starts, creates, edits and deletes them.

// At the crate root, so the renderer names these as `crate::app`, `crate::sbx` and so on.
use aisbx_core::{
    actions, app, create, find, folders, geom, headless, input, insight, job, kits, litellm, model,
    models, repo_skills_ui, roster, sbx, startdir, state, stats,
};
// Only the tests use it: create_wizard_golden removes the token kit it writes.
#[cfg(test)]
use aisbx_core::tokens;

#[cfg(test)]
mod create_wizard_golden;
mod keys;
mod theme;
mod ui;

use std::io;
use std::process::{Command, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;

use crate::app::{App, Handoff};

const HELP: &str = "\
aisbx — Docker Sandboxes dashboard for the aisbx kits

USAGE:
    aisbx [OPTIONS]                        the dashboard
    aisbx up|run|status|stop|rm …          headless, for scripts (below)
    aisbx skill-repo add|rm|ls|fetch …     the configured skill repositories (below)

OPTIONS:
    -a, --all        start with the other folders' sandboxes expanded (the default)
        --here       start with them collapsed — only this folder's, plus a heading
        --no-mouse   start with mouse reporting off (press M to toggle it any time)
        --no-update  skip the dashboard's own pre-start steps: the agent update and, on the
                     LiteLLM backends, the model-list refresh. The kits' startup hook still
                     runs whenever the sandbox starts and may fetch the list (at most once a day)
    -h, --help       print this help
    -V, --version    print the aisbx version

The dashboard lists every sandbox on the host, this folder's first: press n to create one
for the folder you are in, or a to fold the other folders away. Whichever you last chose is
remembered for the next run.
Starting a sandbox updates the agent in it first (`claude update`, `codex update`, `agy update`,
npm for OpenCode and Pi), since an agent's own self-update applies only to the *next* launch.
It costs about two seconds when there is nothing to fetch, and never blocks the start;
--no-update skips it for the run.
On a LiteLLM backend the start also refreshes the gateway's model list (at most once a day;
L forces it). --no-update skips that step too, but not the kits' own startup hook.
Set AISBX_HOME to point at the aisbx checkout if aisbx cannot find its kits/ dir.
";

fn main() -> ExitCode {
    // Before any dashboard setup: its Ctrl-C handler would swallow the interrupt `aisbx run` needs.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| headless::is_subcommand(a)) {
        return ExitCode::from(headless::main(&args));
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aisbx: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // `None` keeps whatever the last run left in the registry.
    let mut show_other = None;
    let mut mouse = true;
    let mut auto_update = true;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-a" | "--all" => show_other = Some(true),
            "--here" | "--folder" => show_other = Some(false),
            "--no-mouse" => mouse = false,
            "--no-update" => auto_update = false,
            // No effect: still accepted, so an alias or a script that passes it keeps working.
            "--wizard" => {}
            "-h" | "--help" => {
                print!("{HELP}\n{}", headless::HELP);
                return Ok(());
            }
            "-V" | "--version" => {
                println!("{}", aisbx_core::version());
                return Ok(());
            }
            other => anyhow::bail!("unknown option: {other}\n\n{HELP}"),
        }
    }

    // Before any thread starts: the local UTC offset is readable only while single-threaded.
    state::init_clock();

    let catalog = kits::Catalog::load()?;
    let version = sbx::checked_version()?;

    // Ctrl-C must not end the dashboard during a session. Exec resets the handler for the child.
    let _ = ctrlc::set_handler(|| {});
    let stop = stop_signals();

    let mut app = App::new(catalog, show_other, version, mouse, auto_update)?;
    let mut terminal = ratatui::init();
    mouse_off_on_panic();
    let mut mouse_on = false;

    let result = event_loop(&mut terminal, &mut app, &mut mouse_on, &stop);
    if mouse_on {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    let _ = app.registry.save();
    result
}

/// A flag that SIGTERM and SIGHUP set (Unix). The event loop then ends the dashboard as `q` does.
fn stop_signals() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
        let _ = signal_hook::flag::register(signal, Arc::clone(&stop));
    }
    stop
}

/// Turns mouse reporting off after a panic. ratatui's own panic hook does not know about it.
fn mouse_off_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        previous(info);
    }));
}

/// Turn mouse reporting on or off to match `app.mouse`.
fn sync_mouse(want: bool, on: &mut bool) -> io::Result<()> {
    if want == *on {
        return Ok(());
    }
    if want {
        execute!(io::stdout(), EnableMouseCapture)?;
    } else {
        execute!(io::stdout(), DisableMouseCapture)?;
    }
    *on = want;
    Ok(())
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mouse_on: &mut bool,
    stop: &AtomicBool,
) -> Result<()> {
    while !app.quit && !stop.load(Ordering::Relaxed) {
        sync_mouse(app.mouse, mouse_on)?;
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(120))? {
            // Drain the queue before a redraw: a paste arrives as one key event per character.
            // The budget stops a stuck key from starving the refresh.
            let mut budget = 512;
            loop {
                match event::read()? {
                    Event::Key(key) if suspends(&key) => suspend(terminal, app.mouse, mouse_on)?,
                    Event::Key(key) => app.on_key(keys::key(key)),
                    Event::Mouse(mouse) => app.on_mouse(keys::mouse(mouse)),
                    Event::Resize(_, _) => terminal.autoresize()?,
                    _ => {}
                }
                budget -= 1;
                if budget == 0 || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.tick();

        if let Some(request) = app.handoff.take() {
            handoff(terminal, app, &request, mouse_on)?;
        }
    }
    Ok(())
}

/// Does `key` suspend the dashboard? Ctrl-Z on Unix: raw mode delivers it as a key, not a signal.
fn suspends(key: &event::KeyEvent) -> bool {
    cfg!(unix)
        && key.kind == event::KeyEventKind::Press
        && key.code == event::KeyCode::Char('z')
        && key.modifiers == event::KeyModifiers::CONTROL
}

/// Stop the dashboard until the shell's `fg` continues it. The terminal goes back to the shell
/// as `q` leaves it, and is taken again and redrawn on the way back.
#[cfg(unix)]
fn suspend(
    terminal: &mut ratatui::DefaultTerminal,
    mouse: bool,
    mouse_on: &mut bool,
) -> Result<()> {
    sync_mouse(false, mouse_on)?;
    ratatui::restore();
    signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)?;
    // Here once the process is continued.
    *terminal = ratatui::init();
    terminal.clear()?;
    sync_mouse(mouse, mouse_on)?;
    Ok(())
}

#[cfg(not(unix))]
fn suspend(_: &mut ratatui::DefaultTerminal, _: bool, _: &mut bool) -> Result<()> {
    Ok(())
}

/// Give the terminal to another program for the length of a session, then take it back.
fn handoff(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    request: &Handoff,
    mouse_on: &mut bool,
) -> Result<()> {
    let argv = request.argv();
    let name = request.name().to_string();
    sync_mouse(false, mouse_on)?;
    ratatui::restore();

    for step in request.prelude() {
        run_prelude(step);
    }
    println!("\n→ {}\n", argv.join(" "));
    let status = spawn_inherited(&argv);

    *terminal = ratatui::init();
    terminal.clear()?;

    match status {
        Ok(status) if status.success() && matches!(request, Handoff::Edit { .. }) => {
            match app.edit_ended(&name) {
                (text, false) => app.set_info(text),
                (text, true) => app.set_error(text),
            }
        }
        Ok(status) => match session_end(&name, request.what(), &argv, status) {
            (text, false) => app.set_info(text),
            (text, true) => app.set_error(text),
        },
        Err(e) => app.set_error(format!("could not start `{}`: {e}", argv[0])),
    }
    app.refresh();
    Ok(())
}

/// How the status line words the end of a session, and whether that is an error. Ctrl-C
/// (SIGINT, exit code 130, Windows `STATUS_CONTROL_C_EXIT`) is not an error.
fn session_end(name: &str, what: &str, argv: &[String], status: ExitStatus) -> (String, bool) {
    if status.success() {
        return (format!("{name} {what} ended"), false);
    }
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    const CONTROL_C_EXIT: i32 = 0xC000_013A_u32 as i32;
    if signal == Some(2) || matches!(status.code(), Some(130 | CONTROL_C_EXIT)) {
        return (format!("{name} {what} interrupted (Ctrl-C)"), false);
    }
    let how = match (status.code(), signal) {
        (Some(code), _) => code.to_string(),
        (None, Some(signal)) => format!("signal {signal}"),
        (None, None) => "a signal".to_string(),
    };
    (format!("`{}` exited with {how}", argv.join(" ")), true)
}

/// Run one pre-start step, and never let it stop the session. See [`app::Handoff::prelude`].
fn run_prelude(argv: &[String]) {
    println!("\n→ {}\n", argv.join(" "));
    match spawn_inherited(argv) {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "aisbx: pre-start step exited with {} — starting the sandbox anyway",
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| c.to_string())
        ),
        Err(e) => {
            eprintln!("aisbx: could not run a pre-start step ({e}) — starting the sandbox anyway")
        }
    }
}

fn spawn_inherited(argv: &[String]) -> io::Result<std::process::ExitStatus> {
    let (program, args) = argv
        .split_first()
        .expect("every handoff argv names a program");
    Command::new(program).args(args).status()
}

#[cfg(test)]
mod tests {
    use super::session_end;
    use std::process::ExitStatus;

    /// The status of a process that exited with `code`.
    #[cfg(unix)]
    fn exited(code: i32) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exited(code: i32) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(code as u32)
    }

    #[test]
    fn a_session_ended_with_ctrl_c_is_interrupted_not_failed() {
        let argv = ["sbx", "run", "--name", "box"].map(String::from);
        let end = |status| session_end("box", "session", &argv, status);
        assert_eq!(end(exited(0)), ("box session ended".to_string(), false));
        let interrupted = ("box session interrupted (Ctrl-C)".to_string(), false);
        assert_eq!(end(exited(130)), interrupted, "exit 130");
        let failed = |how: &str| (format!("`sbx run --name box` exited with {how}"), true);
        assert_eq!(end(exited(1)), failed("1"));
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                end(ExitStatus::from_raw(2)),
                interrupted,
                "killed by SIGINT"
            );
            assert_eq!(end(ExitStatus::from_raw(9)), failed("signal 9"));
        }
        #[cfg(windows)]
        assert_eq!(end(exited(0xC000_013A_u32 as i32)), interrupted);
    }

    #[test]
    #[cfg(unix)]
    fn sigterm_and_sighup_ask_the_dashboard_to_stop() {
        use std::sync::atomic::Ordering;
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
            let stop = super::stop_signals();
            assert!(!stop.load(Ordering::SeqCst));
            signal_hook::low_level::raise(signal).unwrap();
            assert!(stop.load(Ordering::SeqCst), "{signal}");
        }
    }

    #[test]
    fn ctrl_z_suspends_and_nothing_else_does() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            super::suspends(&key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            cfg!(unix)
        );
        assert!(!super::suspends(&key(
            KeyCode::Char('z'),
            KeyModifiers::NONE
        )));
        assert!(!super::suspends(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        let mut release = key(KeyCode::Char('z'), KeyModifiers::CONTROL);
        release.kind = KeyEventKind::Release;
        assert!(!super::suspends(&release));
    }

    #[test]
    fn the_no_update_help_names_the_startup_hook() {
        let option: String = super::HELP
            .lines()
            .skip_while(|l| !l.trim_start().starts_with("--no-update"))
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(option.contains("model-list refresh"), "{option}");
        assert!(option.contains("startup hook still"), "{option}");
    }
}
